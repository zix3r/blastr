//! Loading a Cargo project into rust-analyzer.
//!
//! The project is loaded in two steps: [`ProjectWorkspace::load`] asks Cargo
//! about the workspace (which also tells us which targets exist and which
//! packages are members), and `load_workspace` turns that into a database and a
//! virtual file system. The build scripts and the procedural-macro server are
//! only used when the caller allows execution.

use std::collections::{HashMap, HashSet};
use std::path::Path;
use std::process::Command;

use blastr_model::{CancelToken, CrateInfo, ExecMode, Progress, TargetKind};
use ra_ap_hir::{Crate, ProcMacros};
use ra_ap_ide::{AnalysisHost, RootDatabase};
use ra_ap_load_cargo::{LoadCargoConfig, ProcMacroServerChoice, ProjectFolders, load_workspace};
use ra_ap_paths::{AbsPathBuf, Utf8PathBuf};
use ra_ap_project_model::{
    CargoConfig, CargoFeatures, CargoWorkspace, ProjectManifest, ProjectWorkspace,
    ProjectWorkspaceKind, RustLibSource, TargetKind as CargoTargetKind,
};
use ra_ap_vfs::{FileId, VfsPath};

use crate::check_cancel;
use crate::locate::Root;
use crate::{Error, LoadOptions, Workspace};

/// One loaded workspace-member crate: its metadata and its crate root.
#[derive(Debug, Clone)]
pub(crate) struct MemberCrate {
    /// The crate as the graph reports it.
    pub(crate) info: CrateInfo,
    /// The root file, so the crate can be found again in the database.
    pub(crate) root_file: FileId,
}

/// A Cargo target of a workspace member, before the project is loaded.
struct Target {
    /// The Cargo package name, with dashes.
    package: String,
    /// The kind of target.
    target_kind: TargetKind,
    /// The target name, as `cargo test --test <name>` expects it.
    target_name: String,
    /// The crate name as it appears in paths: underscores, no dashes.
    name: String,
    /// The crate root file.
    root: AbsPathBuf,
}

impl Workspace {
    /// Loads the project whose `Cargo.toml` is in `manifest_dir`.
    ///
    /// `root` is the directory that `RelPath`s in the result are relative to
    /// (the repository root, or its snapshot); `manifest_dir` is `root` or below
    /// it. `cfg(test)` is always enabled for the crates of the workspace.
    ///
    /// A load that comes back without the procedural macros a crate of the
    /// workspace depends on is tried again; what it lost is returned with the
    /// project rather than refused, see [`Workspace::load_once`].
    pub fn load(
        root: &Path,
        manifest_dir: &Path,
        options: &LoadOptions,
        progress: &dyn Progress,
        cancel: &CancelToken,
    ) -> Result<Workspace, Error> {
        let _span = tracing::info_span!("load").entered();
        let mut attempt = 1;
        loop {
            let workspace = Workspace::load_once(root, manifest_dir, options, progress, cancel)?;
            if workspace.missing_proc_macros.is_empty() || attempt >= LOAD_ATTEMPTS {
                return Ok(workspace);
            }
            attempt += 1;
            // Whatever made the macros unavailable (a build that raced another
            // cargo process, a server that was still starting) is often gone by
            // the next attempt.
            tracing::warn!("the load came back without some procedural macros; loading again");
        }
    }

    /// One attempt at [`Workspace::load`].
    ///
    /// rust-analyzer reports a crate whose procedural macros could not be loaded
    /// by expanding every call of them to nothing. Nothing in its API fails, so
    /// a load that ran into that would quietly produce a graph without the items
    /// those macros generate: without a derive's impls, or without the modules a
    /// `automod::dir!` names, which is how a whole test target goes missing.
    ///
    /// Which macros load is not up to Blastr (the dylibs are built by Cargo and
    /// expanded by server processes), so a load cannot fix a dylib that is not
    /// there. It does not fail either: the missing crates are recorded in
    /// [`Workspace::missing_proc_macros`] so that the result can say what it
    /// does not contain, and no front end is left without an analysis.
    ///
    /// Only the procedural-macro crates that a crate of this workspace depends
    /// on directly count. Those are the ones whose macros can expand in the
    /// code Blastr extracts; a macro crate deeper in the dependency graph only
    /// expands inside a dependency crate, whose items are not part of the graph
    /// anyway, and one that Cargo never builds for this configuration (another
    /// platform, an inactive feature) cannot affect anything at all.
    fn load_once(
        root: &Path,
        manifest_dir: &Path,
        options: &LoadOptions,
        progress: &dyn Progress,
        cancel: &CancelToken,
    ) -> Result<Workspace, Error> {
        check_cancel(cancel)?;
        progress.report("manifest", 0, None);

        let manifest_file = manifest_dir.join("Cargo.toml");
        if !manifest_file.is_file() {
            return Err(Error::NoManifest {
                dir: manifest_dir.to_path_buf(),
            });
        }
        let manifest = ProjectManifest::from_manifest_file(absolute(&manifest_file)?)
            .map_err(|error| Error::Cargo(format!("{error:#}")))?;

        let mut cargo_config = CargoConfig {
            features: features(options),
            target: options.target.clone(),
            sysroot: Some(RustLibSource::Discover),
            set_test: true,
            ..CargoConfig::default()
        };
        if let Some(target_dir) = &options.target_dir {
            cargo_config
                .extra_env
                .insert("CARGO_TARGET_DIR".to_owned(), Some(path_text(target_dir)));
        }

        progress.report("metadata", 0, None);
        let mut workspace = ProjectWorkspace::load(manifest, &cargo_config, &|_message: String| {
            progress.report("metadata", 0, None);
        })
        .map_err(|error| Error::Cargo(format!("{error:#}")))?;
        check_cancel(cancel)?;

        let sysroot = &workspace.sysroot;
        if sysroot.rust_lib_src_root().is_none() || sysroot.is_rust_lib_src_empty() {
            return Err(Error::MissingRustSrc);
        }

        let toolchain = toolchain_version(&workspace)?;

        if options.exec == ExecMode::Full {
            progress.report("build scripts", 0, None);
            let build_scripts = workspace
                .run_build_scripts(&cargo_config, &|_message: String| {
                    progress.report("build scripts", 0, None);
                })
                .map_err(|error| Error::Cargo(format!("{error:#}")))?;
            workspace.set_build_scripts(build_scripts);
            check_cancel(cancel)?;
        }

        let targets = member_targets(&workspace);
        let load_config = LoadCargoConfig {
            // The build scripts were run above, and only when the caller allows
            // execution, so `load_workspace` must not run them again.
            load_out_dirs_from_check: false,
            with_proc_macro_server: match options.exec {
                ExecMode::Full => ProcMacroServerChoice::Sysroot,
                ExecMode::Restricted => ProcMacroServerChoice::None,
            },
            prefill_caches: false,
            num_worker_threads: 1,
            proc_macro_processes: PROC_MACRO_PROCESSES,
        };

        progress.report("database", 0, None);
        // The file-set configuration the loading code uses to partition the
        // virtual file system into source roots. It is rebuilt here from the
        // same workspace, with the same arguments, so it is identical to the
        // one `load_workspace` uses; `Workspace::apply` needs it to put a newly
        // added file into the source root of its package.
        let project_folders = ProjectFolders::new(std::slice::from_ref(&workspace), &[], None);
        // The proc-macro server is only needed while macros are expanded, which
        // happens during `extract`. The expansions are cached in the database
        // and the macros it knows keep the server alive, so the client handle
        // itself is not stored and `Workspace` holds no process handle.
        let (db, vfs, _proc_macros) =
            load_workspace(workspace, &cargo_config.extra_env, &load_config)
                .map_err(|error| Error::Cargo(format!("{error:#}")))?;
        check_cancel(cancel)?;
        // Every crate whose macros did not load contributes nothing to the
        // graph, so the load records which ones a crate of this workspace
        // depends on; the analysis reports them as a limit. Without execution
        // they are not expected to load at all.
        let missing_proc_macros = if options.exec == ExecMode::Full {
            missing_proc_macros(&db)
        } else {
            Vec::new()
        };

        let crates = loaded_targets(root, &vfs, targets);
        progress.report("database", 1, Some(1));

        Ok(Workspace {
            host: AnalysisHost::with_database(db),
            vfs,
            root: root.to_path_buf(),
            source_roots: project_folders.source_root_config,
            toolchain,
            crates,
            restricted: options.exec == ExecMode::Restricted,
            missing_proc_macros,
        })
    }
}

/// The number of procedural-macro server processes to run.
///
/// Expanding a macro needs the server, and a single server expands one call at
/// a time, so several of them let the workers of an extraction expand derives
/// without waiting for each other.
const PROC_MACRO_PROCESSES: usize = 4;

/// How often [`Workspace::load`] tries before it keeps what it got.
///
/// Only a load that lost the procedural macros a crate depends on is tried
/// again, so the second attempt costs nothing in the normal case.
const LOAD_ATTEMPTS: u32 = 2;

/// One crate of the database, reduced to what decides which macros are checked.
#[derive(Debug, Clone)]
struct CrateNode {
    /// The crate is a member of this workspace, so Blastr extracts its items.
    local: bool,
    /// The crate is a procedural-macro crate, so its macros expand in whoever
    /// calls them.
    proc_macro: bool,
    /// The indices of the crates it depends on directly.
    depends_on: Vec<usize>,
}

/// The indices of the procedural-macro crates a local crate depends on
/// directly, in order.
///
/// A macro can only expand in the crate that calls it, and only local crates
/// are extracted, so these are the only macro crates whose failure can make the
/// graph incomplete. A macro crate that only a dependency uses expands inside
/// it, and one that Cargo never builds for this configuration expands nowhere;
/// neither changes the graph.
fn direct_proc_macro_dependencies(nodes: &[CrateNode]) -> Vec<usize> {
    let mut selected = Vec::new();
    for node in nodes.iter().filter(|node| node.local) {
        for &dependency in &node.depends_on {
            if nodes[dependency].proc_macro && !selected.contains(&dependency) {
                selected.push(dependency);
            }
        }
    }
    selected.sort_unstable();
    selected
}

/// The crates whose procedural macros a local crate needs but did not get,
/// sorted and without duplicates.
///
/// Called after a load that was allowed to run build scripts and procedural
/// macros, where a missing dylib is a failure of the environment and not the
/// configuration the caller asked for. An empty list means every macro a local
/// crate can expand arrived and the graph is complete.
fn missing_proc_macros(db: &RootDatabase) -> Vec<String> {
    let all = Crate::all(db);
    let index: HashMap<Crate, usize> = all
        .iter()
        .enumerate()
        .map(|(position, krate)| (*krate, position))
        .collect();
    let nodes: Vec<CrateNode> = all
        .iter()
        .map(|krate| CrateNode {
            local: krate.origin(db).is_local(),
            proc_macro: ProcMacros::get_for_crate(db, krate.base()).is_some(),
            depends_on: krate
                .dependencies(db)
                .into_iter()
                .filter_map(|dependency| index.get(&dependency.krate).copied())
                .collect(),
        })
        .collect();

    let mut failures = Vec::new();
    for selected in direct_proc_macro_dependencies(&nodes) {
        let krate = all[selected];
        let Some(macros) = ProcMacros::get_for_crate(db, krate.base()) else {
            continue;
        };
        if macros.get_error().is_some() {
            // Any failure counts: a crate whose dylib was not built, or was not
            // loaded, expands to nothing, so the items its macros generate are
            // missing from the graph either way.
            failures.push(display_name(krate, db));
        }
    }
    failures.sort();
    failures.dedup();
    failures
}

/// The name of a crate as the project spells it.
fn display_name(krate: Crate, db: &RootDatabase) -> String {
    krate
        .display_name(db)
        .map(|name| name.canonical_name().as_str().to_owned())
        .unwrap_or_else(|| "an unnamed crate".to_owned())
}

/// The Cargo features to enable.
fn features(options: &LoadOptions) -> CargoFeatures {
    if options.all_features {
        CargoFeatures::All
    } else {
        CargoFeatures::Selected {
            features: options.features.clone(),
            no_default_features: false,
        }
    }
}

/// `rustc --version` of the toolchain the project was loaded with.
fn toolchain_version(workspace: &ProjectWorkspace) -> Result<String, Error> {
    let program = rustc_command(workspace);
    let mut command = Command::new(&program);
    command.arg("--version");
    let program = program.to_string_lossy().into_owned();
    let output = command
        .output()
        .map_err(|error| Error::Toolchain(format!("`{program} --version` failed: {error}")))?;
    if !output.status.success() {
        return Err(Error::Toolchain(format!(
            "`{program} --version` exited with {}",
            output.status
        )));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    let first_line = stdout.lines().next().unwrap_or_default().trim();
    if first_line.is_empty() {
        return Err(Error::Toolchain(format!(
            "`{program} --version` printed nothing"
        )));
    }
    Ok(first_line.to_owned())
}

/// The `rustc` of the loaded toolchain: the one in the sysroot if it is there,
/// the one in `PATH` otherwise.
fn rustc_command(workspace: &ProjectWorkspace) -> std::ffi::OsString {
    if let Some(root) = workspace.sysroot.root() {
        let sysroot: &Path = root.as_ref();
        let rustc = sysroot
            .join("bin")
            .join(format!("rustc{}", std::env::consts::EXE_SUFFIX));
        if rustc.is_file() {
            return rustc.into_os_string();
        }
    }
    std::ffi::OsString::from("rustc")
}

/// Every target of every workspace member, sorted by
/// `(package, target kind, target name)`.
fn member_targets(workspace: &ProjectWorkspace) -> Vec<Target> {
    let ProjectWorkspaceKind::Cargo { cargo, .. } = &workspace.kind else {
        return Vec::new();
    };
    let duplicated = duplicated_package_names(cargo);
    let mut targets = Vec::new();
    for package in cargo.packages() {
        if !cargo[package].is_member {
            continue;
        }
        let data = &cargo[package];
        let name = package_selector(
            &data.name,
            &data.version.to_string(),
            duplicated.contains(&data.name),
        );
        for &target in &data.targets {
            let target_data = &cargo[target];
            targets.push(Target {
                package: name.clone(),
                target_kind: target_kind(target_data.kind),
                target_name: target_data.name.clone(),
                name: crate_name(&target_data.name),
                root: target_data.root.clone(),
            });
        }
    }
    targets.sort_by(|left, right| target_key(left).cmp(&target_key(right)));
    targets
}

/// The names of the packages that more than one package of the project has.
///
/// A dependency and a member may share a name, and a package may appear twice at
/// two versions; Cargo then needs the version to tell them apart.
fn duplicated_package_names(cargo: &CargoWorkspace) -> HashSet<String> {
    let mut seen = HashSet::new();
    let mut duplicated = HashSet::new();
    for package in cargo.packages() {
        let name = &cargo[package].name;
        if !seen.insert(name.clone()) {
            duplicated.insert(name.clone());
        }
    }
    duplicated
}

/// How Cargo is asked for a package: its name, or `name@version` when more than
/// one package of the project has that name.
fn package_selector(name: &str, version: &str, duplicated: bool) -> String {
    if duplicated {
        format!("{name}@{version}")
    } else {
        name.to_owned()
    }
}

type TargetKey<'a> = (&'a str, u8, &'a str, &'a str, &'a str);

/// The sort key of a target: package, target kind, target name, and the crate
/// name and root as tie breakers so that the order is total.
fn target_key(target: &Target) -> TargetKey<'_> {
    (
        &target.package,
        kind_rank(target.target_kind),
        &target.target_name,
        &target.name,
        target.root.as_str(),
    )
}

/// The order target kinds are sorted in: the declaration order of
/// [`TargetKind`].
fn kind_rank(kind: TargetKind) -> u8 {
    match kind {
        TargetKind::Lib => 0,
        TargetKind::Bin => 1,
        TargetKind::Test => 2,
        TargetKind::Example => 3,
        TargetKind::Bench => 4,
        TargetKind::ProcMacro => 5,
        TargetKind::BuildScript => 6,
        TargetKind::Other => 7,
    }
}

fn target_kind(kind: CargoTargetKind) -> TargetKind {
    match kind {
        CargoTargetKind::Lib {
            is_proc_macro: false,
        } => TargetKind::Lib,
        CargoTargetKind::Lib {
            is_proc_macro: true,
        } => TargetKind::ProcMacro,
        CargoTargetKind::Bin => TargetKind::Bin,
        CargoTargetKind::Example => TargetKind::Example,
        CargoTargetKind::Test => TargetKind::Test,
        CargoTargetKind::Bench => TargetKind::Bench,
        CargoTargetKind::BuildScript => TargetKind::BuildScript,
        CargoTargetKind::Other => TargetKind::Other,
    }
}

/// The crate name as code sees it: dashes become underscores.
fn crate_name(target_name: &str) -> String {
    target_name.replace('-', "_")
}

/// Keeps the targets whose crate root is inside the root and known to the
/// virtual file system.
fn loaded_targets(root: &Path, vfs: &ra_ap_vfs::Vfs, targets: Vec<Target>) -> Vec<MemberCrate> {
    let root = Root::new(root);
    let mut crates = Vec::with_capacity(targets.len());
    for target in targets {
        let root_path: &Path = target.root.as_ref();
        let Some(root_file) = root.relative(root_path) else {
            continue;
        };
        let path = VfsPath::from(target.root);
        let Some((file_id, _excluded)) = vfs.file_id(&path) else {
            continue;
        };
        crates.push(MemberCrate {
            info: CrateInfo {
                name: target.name,
                package: target.package,
                target_kind: target.target_kind,
                target_name: target.target_name,
                root_file,
            },
            root_file: file_id,
        });
    }
    crates
}

/// The absolute form of `path`, as the loading code needs it.
pub(crate) fn absolute(path: &Path) -> Result<AbsPathBuf, Error> {
    let absolute = std::path::absolute(path).map_err(|error| {
        Error::Cargo(format!(
            "cannot resolve the path `{}`: {error}",
            path.display()
        ))
    })?;
    let utf8 = Utf8PathBuf::from_path_buf(absolute).map_err(|path| {
        Error::Cargo(format!("the path `{}` is not valid UTF-8", path.display()))
    })?;
    AbsPathBuf::try_from(utf8)
        .map_err(|path| Error::Cargo(format!("the path `{path}` is not absolute")))
}

/// The text of `path`, for a command-line environment variable.
fn path_text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[cfg(test)]
mod tests {
    use super::{
        CrateNode, crate_name, direct_proc_macro_dependencies, kind_rank, package_selector,
    };
    use blastr_model::TargetKind;

    /// One crate of the graph: local or not, a procedural macro or not, and
    /// what it depends on directly.
    fn node(local: bool, proc_macro: bool, depends_on: &[usize]) -> CrateNode {
        CrateNode {
            local,
            proc_macro,
            depends_on: depends_on.to_vec(),
        }
    }

    #[test]
    fn only_macros_a_local_crate_depends_on_directly_are_checked() {
        // 0: the workspace member that is extracted;
        // 1: a macro it calls;
        // 2: a dependency that is not extracted; 3: a macro only 2 calls;
        // 4: a macro Cargo never builds for this configuration, used by nobody.
        let nodes = [
            node(true, false, &[1, 2]),
            node(false, true, &[]),
            node(false, false, &[3]),
            node(false, true, &[]),
            node(false, true, &[]),
        ];
        assert_eq!(direct_proc_macro_dependencies(&nodes), [1]);
    }

    #[test]
    fn a_macro_of_another_member_is_checked_once() {
        // Two members call the same macro crate, which is a dependency of both.
        let nodes = [
            node(true, false, &[2]),
            node(true, false, &[2]),
            node(false, true, &[]),
        ];
        assert_eq!(direct_proc_macro_dependencies(&nodes), [2]);
    }

    #[test]
    fn no_local_crate_means_no_macro_is_checked() {
        let nodes = [node(false, false, &[1]), node(false, true, &[])];
        assert!(direct_proc_macro_dependencies(&nodes).is_empty());
    }

    #[test]
    fn crate_names_use_underscores() {
        assert_eq!(crate_name("x01_direct_call"), "x01_direct_call");
        assert_eq!(crate_name("my-crate"), "my_crate");
    }

    #[test]
    fn a_unique_package_name_needs_no_version() {
        assert_eq!(package_selector("serde", "1.0.0", false), "serde");
    }

    #[test]
    fn a_duplicated_package_name_carries_its_version() {
        assert_eq!(package_selector("gix", "0.90.0", true), "gix@0.90.0");
        assert_eq!(package_selector("gix", "1.0.0", true), "gix@1.0.0");
    }

    #[test]
    fn target_kinds_are_ordered_by_their_declaration() {
        assert!(kind_rank(TargetKind::Lib) < kind_rank(TargetKind::Bin));
        assert!(kind_rank(TargetKind::Bin) < kind_rank(TargetKind::Test));
        assert!(kind_rank(TargetKind::Bench) < kind_rank(TargetKind::Other));
    }
}
