//! Running the phases of an extraction on several threads.
//!
//! rust-analyzer's database can be read from several threads at once. A clone of
//! [`RootDatabase`] shares the analysis state with the original, so every worker
//! gets a clone of its own and no thread writes to the database while the others
//! read from it.
//!
//! An extraction runs in three phases:
//!
//! - the definition maps of every crate, on the workers, dependencies first;
//! - the walk of each member crate, on the calling thread, because the syntax
//!   trees a walk builds cannot move to another thread;
//! - the inactive ranges of the modules the walks found, on the workers. They
//!   are the most expensive query of the analysis: reading them validates and
//!   infers every body of the module, which also leaves the types in place for
//!   the dependency pass that follows.

use std::collections::HashSet;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};

use blastr_model::{CancelToken, Location, Progress};
use ra_ap_hir::{AnyDiagnostic, Crate, Module, attach_db, db::HirDatabase};
use ra_ap_ide::RootDatabase;

use crate::locate::{Locator, Paths};
use crate::{Error, check_cancel};

/// Everything the phases of one extraction share.
pub(crate) struct Pipeline<'a> {
    /// The database the project was loaded into; every worker clones it.
    pub(crate) database: &'a RootDatabase,
    /// The number of workers; `None` for one per core.
    pub(crate) threads: Option<usize>,
    /// The paths of the files, for the locators of the workers.
    pub(crate) paths: &'a Paths,
    /// Asks the caller to stop.
    pub(crate) cancel: &'a CancelToken,
    /// Tells the caller how far the extraction is.
    pub(crate) progress: &'a dyn Progress,
}

impl Pipeline<'_> {
    /// Computes the definition maps, walks the `members` member crates with
    /// `walk` and reads the inactive ranges of the modules the walks return.
    ///
    /// `walk` runs on the calling thread, which must have the database
    /// attached.
    pub(crate) fn run<T>(
        &self,
        members: usize,
        mut walk: impl FnMut(usize) -> Result<(T, Vec<Module>), Error>,
    ) -> Result<(Vec<T>, Vec<Location>), Error> {
        let crates = {
            let _span = tracing::info_span!("def maps").entered();
            let crates = dependencies_first(self.database);
            let defined = self.parallel(&crates, |db, krate| {
                ra_ap_hir::crate_def_map(db, krate.base());
                Some(())
            });
            if !defined {
                return Err(Error::Internal(
                    "a definition map of the project could not be computed".to_owned(),
                ));
            }
            crates.len()
        };
        tracing::debug!(crates, "definition maps computed");

        let span = tracing::info_span!("walk").entered();
        let mut walks = Vec::with_capacity(members);
        let mut modules = Vec::new();
        for member in 0..members {
            check_cancel(self.cancel)?;
            self.progress
                .report("extract", member as u64, Some(members as u64));
            let (walked, found) =
                catch_unwind(AssertUnwindSafe(|| walk(member))).map_err(|_| {
                    Error::Internal("a crate of the project could not be walked".to_owned())
                })??;
            walks.push(walked);
            modules.extend(found);
        }
        drop(span);

        let _span = tracing::info_span!("inactive ranges", modules = modules.len()).entered();
        let inactive = Mutex::new(Vec::new());
        let done = AtomicU64::new(0);
        let total = modules.len() as u64;
        let (paths, progress) = (self.paths, self.progress);
        self.parallel(&modules, |db, module| {
            let ranges = inactive_ranges(db, *module, &Locator::new(db, paths));
            lock(&inactive).extend(ranges);
            let done = done.fetch_add(1, Ordering::Relaxed) + 1;
            progress.report("extract", done, Some(total));
            Some(())
        });
        check_cancel(self.cancel)?;
        Ok((walks, lock(&inactive).split_off(0)))
    }

    /// Runs `work` on every item, each on the first worker that is free, and
    /// returns false when a piece of work panicked, which stops the others.
    fn parallel<I: Sync>(
        &self,
        items: &[I],
        work: impl Fn(&dyn HirDatabase, &I) -> Option<()> + Sync,
    ) -> bool {
        let available = std::thread::available_parallelism().map_or(1, |count| count.get());
        let threads = self.threads.filter(|count| *count > 0).unwrap_or(available);
        let next = AtomicUsize::new(0);
        let failed = AtomicBool::new(false);
        let cancel = self.cancel;
        std::thread::scope(|scope| {
            for index in 0..threads.min(items.len()) {
                let database = self.database.clone();
                let (next, failed, work) = (&next, &failed, &work);
                // A thread that cannot be started leaves its share to the others.
                _ = std::thread::Builder::new()
                    .name(format!("blastr-extract-{index}"))
                    .spawn_scoped(scope, move || {
                        let db: &dyn HirDatabase = &database;
                        attach_db(db, || {
                            while !failed.load(Ordering::Relaxed) && !cancel.is_cancelled() {
                                let Some(item) = items.get(next.fetch_add(1, Ordering::Relaxed))
                                else {
                                    return;
                                };
                                // rust-analyzer uses unwinding to cancel a query and
                                // can panic on code it cannot analyse.
                                if catch_unwind(AssertUnwindSafe(|| work(db, item))).is_err() {
                                    failed.store(true, Ordering::Relaxed);
                                }
                            }
                        });
                    });
            }
        });
        !failed.load(Ordering::Relaxed)
    }
}

/// Every crate of the crate graph, each after the crates it depends on.
fn dependencies_first(db: &dyn HirDatabase) -> Vec<Crate> {
    fn visit(db: &dyn HirDatabase, krate: Crate, seen: &mut HashSet<Crate>, out: &mut Vec<Crate>) {
        if seen.insert(krate) {
            for dependency in krate.dependencies(db) {
                visit(db, dependency.krate, seen, out);
            }
            out.push(krate);
        }
    }
    let mut seen = HashSet::new();
    let mut out = Vec::new();
    for krate in Crate::all(db) {
        visit(db, krate, &mut seen, &mut out);
    }
    out
}

/// Locks a mutex, ignoring a panic of another worker.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(|error| error.into_inner())
}

/// The ranges of the code of `module` that the configuration disables.
///
/// rust-analyzer reports them among the diagnostics of the module, which also
/// validate every body of the module; that is the price of the answer, and the
/// caller runs it for several modules at once.
fn inactive_ranges(db: &dyn HirDatabase, module: Module, locator: &Locator<'_>) -> Vec<Location> {
    let mut diagnostics = Vec::new();
    let done = catch_unwind(AssertUnwindSafe(|| {
        module.diagnostics(db, &mut diagnostics, false);
    }));
    if done.is_err() {
        return Vec::new();
    }
    diagnostics
        .into_iter()
        .filter_map(|diagnostic| match diagnostic {
            AnyDiagnostic::InactiveCode(code) => {
                Some(code.node.map(|pointer| pointer.text_range()))
            }
            _ => None,
        })
        .filter_map(|range| {
            catch_unwind(AssertUnwindSafe(|| {
                locator.location(range.file_id, range.value)
            }))
            .ok()
            .flatten()
        })
        .collect()
}
