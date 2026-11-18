//! Finding the user's editor on this machine, and opening a file in it at a
//! line.
//!
//! Editors are not started by bare name. On Windows the installers put a
//! `code.cmd` shim on `PATH` next to, or instead of, the real `Code.exe`, and
//! Rust's [`std::process::Command`] neither resolves a name through `PATH` nor
//! runs a `.cmd` at all. So the program is looked up here — `PATH` first, then
//! the places the installers use — and preferred as a real executable over a
//! script. Arguments are passed as separate entries: a path with a space in it
//! is one argument, never re-split.
//!
//! Every launch starts the editor without a console window, because the window
//! is a graphical program with no console of its own; and every failure is
//! reported to the window, which says what went wrong and where to change it.

use std::path::{Path, PathBuf};
use std::process::{Command as Process, Stdio};

use serde::{Deserialize, Serialize};

/// The editor Blastr opens files in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub(crate) enum Editor {
    /// The first editor found on this machine.
    #[default]
    Auto,
    /// Visual Studio Code.
    VsCode,
    /// Visual Studio Code Insiders.
    VsCodeInsiders,
    /// Cursor.
    Cursor,
    /// Zed.
    Zed,
    /// RustRover.
    RustRover,
    /// Sublime Text.
    Sublime,
    /// Notepad++.
    NotepadPlusPlus,
    /// A command line the user wrote.
    Custom,
}

/// Windows' `CREATE_NO_WINDOW`: start the child without a console of its own.
#[cfg(windows)]
const CREATE_NO_WINDOW: u32 = 0x0800_0000;

/// The placeholder for the absolute path of the file.
pub(crate) const FILE: &str = "{file}";
/// The placeholder for the line the caret should be on.
pub(crate) const LINE: &str = "{line}";
/// The placeholder for the column the caret should be on.
pub(crate) const COLUMN: &str = "{column}";

impl Editor {
    /// The editors that can be looked for, in the order they are reported.
    pub(crate) const DETECTABLE: [Editor; 7] = [
        Editor::VsCode,
        Editor::VsCodeInsiders,
        Editor::Cursor,
        Editor::Zed,
        Editor::RustRover,
        Editor::Sublime,
        Editor::NotepadPlusPlus,
    ];

    /// The name the editor is shown under.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Editor::Auto => "Automatic",
            Editor::VsCode => "VS Code",
            Editor::VsCodeInsiders => "VS Code Insiders",
            Editor::Cursor => "Cursor",
            Editor::Zed => "Zed",
            Editor::RustRover => "RustRover",
            Editor::Sublime => "Sublime Text",
            Editor::NotepadPlusPlus => "Notepad++",
            Editor::Custom => "Custom command",
        }
    }

    /// The program names the editor is installed under, in the order they are
    /// looked for on `PATH`.
    fn names(self) -> &'static [&'static str] {
        match self {
            Editor::Auto | Editor::Custom => &[],
            Editor::VsCode => &["code"],
            Editor::VsCodeInsiders => &["code-insiders"],
            Editor::Cursor => &["cursor"],
            Editor::Zed => &["zed", "zeditor"],
            Editor::RustRover => &["rustrover"],
            Editor::Sublime => &["subl"],
            Editor::NotepadPlusPlus => &["notepad++"],
        }
    }

    /// The arguments that open `file` at `line` and `column`.
    ///
    /// Each editor spells "this position" its own way; a name is never turned
    /// into a shell word here, so a path with a space in it stays one argument.
    fn arguments(self, file: &Path, line: u32, column: u32) -> Vec<String> {
        let position = format!("{}:{line}:{column}", file.display());
        let path = file.display().to_string();
        match self {
            Editor::VsCode | Editor::VsCodeInsiders | Editor::Cursor => {
                vec!["-g".to_owned(), position]
            }
            Editor::Zed | Editor::Sublime => vec![position],
            Editor::RustRover => vec!["--line".to_owned(), line.to_string(), path],
            Editor::NotepadPlusPlus => vec![format!("-n{line}"), path],
            Editor::Auto | Editor::Custom => Vec::new(),
        }
    }

    /// The places a normal installation of this editor puts its program, aside
    /// from `PATH`.
    fn installs(self, env: &Env) -> Vec<PathBuf> {
        let mut found = Vec::new();
        for (directory, names) in self.install_dirs(env) {
            found.extend(scan(directory.as_deref(), names));
        }
        found
    }

    /// The directories this editor's installers use, with the program names in
    /// them.
    #[cfg(windows)]
    fn install_dirs(self, env: &Env) -> Vec<(Option<PathBuf>, &'static [&'static str])> {
        let local = || env.local.clone().map(|dir| dir.join("Programs"));
        match self {
            Editor::VsCode => vec![
                (local(), &["Microsoft VS Code/Code.exe"]),
                (env.programs.clone(), &["Microsoft VS Code/Code.exe"]),
            ],
            Editor::VsCodeInsiders => vec![
                (local(), &["Microsoft VS Code Insiders/Code - Insiders.exe"]),
                (
                    env.programs.clone(),
                    &["Microsoft VS Code Insiders/Code - Insiders.exe"],
                ),
            ],
            Editor::Cursor => vec![
                (local(), &["cursor/Cursor.exe"]),
                (env.programs.clone(), &["cursor/Cursor.exe"]),
            ],
            Editor::Zed => vec![
                (env.local.clone(), &["Zed/zed.exe"]),
                (local(), &["Zed/zed.exe"]),
                (env.programs.clone(), &["Zed/zed.exe"]),
            ],
            Editor::RustRover => vec![
                (
                    env.local
                        .clone()
                        .map(|dir| dir.join("JetBrains").join("Toolbox").join("apps")),
                    &["RustRover"],
                ),
                (
                    env.programs.clone().map(|dir| dir.join("JetBrains")),
                    &["bin/rustrover64.exe"],
                ),
            ],
            Editor::Sublime => vec![
                (local(), &["Sublime Text/subl.exe"]),
                (
                    env.programs.clone(),
                    &["Sublime Text/subl.exe", "Sublime Text 3/subl.exe"],
                ),
            ],
            Editor::NotepadPlusPlus => vec![
                (env.programs.clone(), &["Notepad++/notepad++.exe"]),
                (env.programs_x86.clone(), &["Notepad++/notepad++.exe"]),
            ],
            Editor::Auto | Editor::Custom => Vec::new(),
        }
    }

    /// The directories this editor's installers use on this platform.
    #[cfg(not(windows))]
    fn install_dirs(self, env: &Env) -> Vec<(Option<PathBuf>, &'static [&'static str])> {
        vec![(
            env.home.clone().map(|dir| dir.join(".local/bin")),
            self.names(),
        )]
    }
}

/// A program that was found on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Detected {
    /// Which editor it is.
    pub editor: Editor,
    /// The program to start.
    pub program: PathBuf,
}

/// A program and the arguments that open a file in it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Launch {
    /// The program to start.
    pub program: PathBuf,
    /// Its arguments, each one passed as written.
    pub arguments: Vec<String>,
}

impl Launch {
    /// The command line as it would be typed, for showing under the setting.
    pub(crate) fn display(&self) -> String {
        let mut words = vec![self.program.display().to_string()];
        words.extend(self.arguments.iter().cloned());
        words
            .iter()
            .map(|word| quote(word))
            .collect::<Vec<_>>()
            .join(" ")
    }
}

/// A word of a command line, quoted when it holds a space.
fn quote(word: &str) -> String {
    if word.is_empty() {
        return "\"\"".to_owned();
    }
    if word.contains(' ') || word.contains('\t') {
        format!("\"{word}\"")
    } else {
        word.to_owned()
    }
}

/// Where programs are looked for.
#[derive(Debug, Clone, Default)]
struct Env {
    /// The directories of `PATH`.
    path: Vec<PathBuf>,
    /// `LOCALAPPDATA`, where per-user installations live.
    #[cfg_attr(not(windows), allow(dead_code))]
    local: Option<PathBuf>,
    /// `ProgramFiles`.
    #[cfg_attr(not(windows), allow(dead_code))]
    programs: Option<PathBuf>,
    /// `ProgramFiles(x86)`.
    #[cfg_attr(not(windows), allow(dead_code))]
    programs_x86: Option<PathBuf>,
    /// The user's home directory, where a per-user installation lands on the
    /// platforms that have no `LOCALAPPDATA`.
    #[cfg_attr(windows, allow(dead_code))]
    home: Option<PathBuf>,
}

impl Env {
    /// The places this machine looks for programs.
    fn current() -> Env {
        Env {
            path: std::env::var_os("PATH")
                .map(|value| std::env::split_paths(&value).collect())
                .unwrap_or_default(),
            local: var_path("LOCALAPPDATA"),
            programs: var_path("ProgramFiles"),
            programs_x86: var_path("ProgramFiles(x86)"),
            home: var_path("HOME").or_else(|| var_path("USERPROFILE")),
        }
    }
}

/// The value of an environment variable that names a directory.
fn var_path(name: &str) -> Option<PathBuf> {
    let value = std::env::var_os(name)?;
    if value.is_empty() {
        return None;
    }
    Some(PathBuf::from(value))
}

/// The executables a program can be, in the order they are preferred.
///
/// Windows finds `code` as `code.exe` or as the `code.cmd` shim its installer
/// puts on `PATH`; the real executable is preferred, and a script is only used
/// when nothing better is there.
#[cfg(windows)]
const SUFFIXES: &[&str] = &["exe", "cmd", "bat"];
/// On other platforms a program is its own name.
#[cfg(not(windows))]
const SUFFIXES: &[&str] = &[""];

/// The first program called `name` in `dirs`.
fn search(name: &str, dirs: &[PathBuf]) -> Option<PathBuf> {
    for directory in dirs {
        for suffix in SUFFIXES {
            let file = if suffix.is_empty() {
                name.to_string()
            } else {
                format!("{name}.{suffix}")
            };
            let candidate = directory.join(file);
            if candidate.is_file() {
                return Some(candidate);
            }
        }
    }
    None
}

/// The programs named `names` under the subdirectories of `dir`.
///
/// Installers lay their program out one, two or three levels down
/// (`JetBrains/RustRover 2024.1/bin/rustrover64.exe`,
/// `JetBrains/Toolbox/apps/RustRover/ch-0/241.1/bin/rustrover64.exe`), so the
/// listing is followed that far and no further.
fn scan(dir: Option<&Path>, names: &[&'static str]) -> Vec<PathBuf> {
    let Some(dir) = dir else {
        return Vec::new();
    };
    let mut found = Vec::new();
    for name in names {
        found.push(dir.join(name));
    }
    let Ok(entries) = std::fs::read_dir(dir) else {
        return found;
    };
    for entry in entries.flatten() {
        let first = entry.path();
        for name in names {
            found.push(first.join(name));
            found.push(first.join("bin").join(name));
        }
        let Ok(children) = std::fs::read_dir(&first) else {
            continue;
        };
        for child in children.flatten() {
            let second = child.path();
            for name in names {
                found.push(second.join(name));
                found.push(second.join("bin").join(name));
            }
        }
    }
    found
}

/// Where `editor` is installed on this machine, if it is.
fn find_with(editor: Editor, env: &Env) -> Option<PathBuf> {
    for name in editor.names() {
        if let Some(program) = search(name, &env.path) {
            return Some(program);
        }
    }
    editor
        .installs(env)
        .into_iter()
        .find(|candidate| candidate.is_file())
}

/// Every editor installed on this machine, in [`Editor::DETECTABLE`] order.
///
/// Reading the file system, so this runs on the engine thread, never while the
/// window draws.
pub(crate) fn detect() -> Vec<Detected> {
    let env = Env::current();
    Editor::DETECTABLE
        .into_iter()
        .filter_map(|editor| find_with(editor, &env).map(|program| Detected { editor, program }))
        .collect()
}

/// The editor a setting names, and the command line it runs.
///
/// The name is returned with the launch so that a failure can say which editor
/// could not be started. `detected` is what [`detect`] found.
pub(crate) fn resolve(
    editor: Editor,
    custom: &str,
    detected: &[Detected],
    file: &Path,
    line: u32,
    column: u32,
) -> Result<(String, Launch), String> {
    if editor == Editor::Custom {
        let launch = custom_launch(custom, file, line, column)?;
        return Ok(("the custom command".to_owned(), launch));
    }
    let chosen = match editor {
        Editor::Auto => detected.first().ok_or_else(|| {
            "No editor was found on this machine. Choose one in Settings (Ctrl ,).".to_owned()
        })?,
        wanted => detected
            .iter()
            .find(|candidate| candidate.editor == wanted)
            .ok_or_else(|| {
                format!(
                    "{} was not found on this machine. Choose another editor in Settings (Ctrl ,).",
                    wanted.label()
                )
            })?,
    };
    Ok((
        chosen.editor.label().to_owned(),
        Launch {
            program: chosen.program.clone(),
            arguments: chosen.editor.arguments(file, line, column),
        },
    ))
}

/// Splits a command line into words, keeping a quoted run together.
///
/// Double quotes group words; a backslash before a double quote keeps it. The
/// result is a list of arguments, so a path is never taken apart again.
fn words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut any = false;
    let mut characters = text.chars().peekable();
    while let Some(character) = characters.next() {
        match character {
            '"' => {
                quoted = !quoted;
                any = true;
            }
            '\\' if quoted && characters.peek() == Some(&'"') => {
                word.push('"');
                characters.next();
            }
            character if character.is_whitespace() && !quoted => {
                if any || !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                    any = false;
                }
            }
            character => {
                word.push(character);
                any = true;
            }
        }
    }
    if any || !word.is_empty() {
        words.push(word);
    }
    words
}

/// The command a custom template runs, with its placeholders filled in.
///
/// The template needs a `{file}` placeholder: a command that does not name the
/// file would open the editor without it.
pub(crate) fn custom_launch(
    template: &str,
    file: &Path,
    line: u32,
    column: u32,
) -> Result<Launch, String> {
    let template = template.trim();
    if template.is_empty() {
        return Err("Write the command the editor is started with, such as \
             `code -g {file}:{line}:{column}`."
            .to_owned());
    }
    if !template.contains(FILE) {
        return Err(format!(
            "The command needs a `{FILE}` placeholder, so that it knows which file to open."
        ));
    }
    let mut words = words(template);
    if words.is_empty() {
        return Err("The command has no program to start.".to_owned());
    }
    let program = fill(&words.remove(0), file, line, column);
    let arguments = words
        .iter()
        .map(|word| fill(word, file, line, column))
        .collect();
    Ok(Launch {
        program: PathBuf::from(program),
        arguments,
    })
}

/// One word of a template with its placeholders filled in.
fn fill(word: &str, file: &Path, line: u32, column: u32) -> String {
    word.replace(FILE, &file.display().to_string())
        .replace(LINE, &line.to_string())
        .replace(COLUMN, &column.to_string())
}

/// Opens `file` at `line` in the configured editor.
///
/// Nothing waits for the editor: the program is started and left to the
/// desktop. A failure is returned as a sentence the window shows, so that a
/// missing or broken editor is never silent again.
pub(crate) fn open(
    editor: Editor,
    custom: &str,
    detected: &[Detected],
    file: &Path,
    line: u32,
    column: u32,
) -> Result<(), String> {
    let (name, launch) = resolve(editor, custom, detected, file, line, column)?;
    spawn(&launch).map_err(|error| {
        format!("Could not start {name}: {error}. Change the editor in Settings (Ctrl ,).")
    })
}

/// Starts a program without waiting for it, and without a console window.
fn spawn(launch: &Launch) -> std::io::Result<()> {
    let mut command = if needs_shell(&launch.program) {
        // `cmd.exe` is what runs a `.cmd` or `.bat`, and it needs the argument
        // as it would be typed: `cmd /C` takes the rest of its line.
        let mut command = Process::new("cmd");
        command.arg("/C").arg(&launch.program);
        command
    } else {
        Process::new(&launch.program)
    };
    command
        .args(&launch.arguments)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn().map(|_| ())
}

/// Whether the program is a script the shell has to run.
///
/// Windows resolves `.cmd` and `.bat` through `cmd.exe` only; the window's own
/// process cannot start one directly.
fn needs_shell(program: &Path) -> bool {
    if !cfg!(windows) {
        return false;
    }
    program
        .extension()
        .and_then(|extension| extension.to_str())
        .is_some_and(|extension| {
            extension.eq_ignore_ascii_case("cmd") || extension.eq_ignore_ascii_case("bat")
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A file path with a space in it, as a workspace under `Program Files`
    /// would be.
    fn spaced() -> PathBuf {
        PathBuf::from("C:/code/my project/src/lib.rs")
    }

    #[test]
    fn every_editor_gets_the_arguments_it_understands() {
        assert_eq!(
            Editor::VsCode.arguments(&spaced(), 42, 7),
            vec!["-g", "C:/code/my project/src/lib.rs:42:7"]
        );
        assert_eq!(
            Editor::Cursor.arguments(&spaced(), 42, 7),
            vec!["-g", "C:/code/my project/src/lib.rs:42:7"]
        );
        assert_eq!(
            Editor::Zed.arguments(&spaced(), 42, 7),
            vec!["C:/code/my project/src/lib.rs:42:7"]
        );
        assert_eq!(
            Editor::Sublime.arguments(&spaced(), 42, 7),
            vec!["C:/code/my project/src/lib.rs:42:7"]
        );
        assert_eq!(
            Editor::RustRover.arguments(&spaced(), 42, 7),
            vec!["--line", "42", "C:/code/my project/src/lib.rs"]
        );
        assert_eq!(
            Editor::NotepadPlusPlus.arguments(&spaced(), 42, 7),
            vec!["-n42", "C:/code/my project/src/lib.rs"]
        );
    }

    #[test]
    fn a_path_with_a_space_stays_one_argument() {
        let arguments = Editor::RustRover.arguments(&spaced(), 3, 1);
        assert_eq!(arguments.len(), 3);
        assert_eq!(arguments[2], "C:/code/my project/src/lib.rs");
    }

    #[test]
    fn a_custom_command_fills_in_every_placeholder() {
        let launch = custom_launch(
            "code -g {file}:{line}:{column}",
            Path::new("C:/code/a.rs"),
            12,
            3,
        )
        .expect("a usable command");
        assert_eq!(launch.program, Path::new("code"));
        assert_eq!(launch.arguments, vec!["-g", "C:/code/a.rs:12:3"]);
    }

    #[test]
    fn a_quoted_path_in_a_custom_command_is_one_word() {
        let launch = custom_launch(
            "rider64.exe --line {line} \"{file}\"",
            Path::new("C:/code/my project/src/lib.rs"),
            8,
            1,
        )
        .expect("a usable command");
        assert_eq!(launch.program, Path::new("rider64.exe"));
        assert_eq!(
            launch.arguments,
            vec!["--line", "8", "C:/code/my project/src/lib.rs"]
        );
    }

    #[test]
    fn a_custom_command_must_name_the_file_and_a_program() {
        let file = Path::new("/code/a.rs");
        let missing_file = custom_launch("code --new-window", file, 1, 1)
            .expect_err("a command without a file is useless");
        assert!(missing_file.contains(FILE), "{missing_file}");
        let empty = custom_launch("   ", file, 1, 1).expect_err("an empty command");
        assert!(empty.contains("Write the command"), "{empty}");
        let quotes = custom_launch("\"{file}\"", file, 1, 1).expect("a quoted program");
        assert_eq!(quotes.program, Path::new("/code/a.rs"));
        assert!(quotes.arguments.is_empty());
    }

    #[test]
    fn a_command_line_is_split_on_spaces_but_not_inside_quotes() {
        assert_eq!(words("code -g a.rs"), vec!["code", "-g", "a.rs"]);
        assert_eq!(
            words("  zed   \"C:/my code/a.rs\"  "),
            vec!["zed", "C:/my code/a.rs"]
        );
        assert_eq!(words(""), Vec::<String>::new());
        assert_eq!(words("\"\""), vec![""]);
    }

    #[test]
    fn a_shown_command_line_quotes_the_words_that_need_it() {
        let launch = Launch {
            program: PathBuf::from("C:/Program Files/Code.exe"),
            arguments: vec!["-g".to_owned(), "C:/code/a.rs:4:1".to_owned()],
        };
        assert_eq!(
            launch.display(),
            "\"C:/Program Files/Code.exe\" -g C:/code/a.rs:4:1"
        );
    }

    /// A temporary directory with a program in it, and the `PATH` that finds
    /// it.
    fn with_program(name: &str) -> (tempfile::TempDir, Env) {
        let directory = tempfile::tempdir().expect("a temporary directory");
        std::fs::write(directory.path().join(name), "").expect("a written program");
        let env = Env {
            path: vec![directory.path().to_path_buf()],
            ..Env::default()
        };
        (directory, env)
    }

    #[test]
    fn a_program_on_the_path_is_found() {
        let (directory, env) = with_program(if cfg!(windows) { "code.exe" } else { "code" });
        let found = find_with(Editor::VsCode, &env).expect("the program is on the path");
        assert_eq!(
            found,
            directory
                .path()
                .join(if cfg!(windows) { "code.exe" } else { "code" })
        );
    }

    #[test]
    fn an_editor_that_is_not_installed_is_not_found() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        let env = Env {
            path: vec![directory.path().to_path_buf()],
            ..Env::default()
        };
        assert_eq!(find_with(Editor::Cursor, &env), None);
        assert_eq!(find_with(Editor::Auto, &env), None);
        assert_eq!(find_with(Editor::Custom, &env), None);
    }

    #[test]
    fn the_first_directory_of_the_path_wins() {
        let first = tempfile::tempdir().expect("a temporary directory");
        let second = tempfile::tempdir().expect("a temporary directory");
        let name = if cfg!(windows) { "zed.exe" } else { "zed" };
        std::fs::write(first.path().join(name), "").expect("a written program");
        std::fs::write(second.path().join(name), "").expect("a written program");
        let env = Env {
            path: vec![first.path().to_path_buf(), second.path().to_path_buf()],
            ..Env::default()
        };
        assert_eq!(find_with(Editor::Zed, &env), Some(first.path().join(name)));
    }

    #[cfg(windows)]
    #[test]
    fn a_real_executable_is_preferred_to_a_command_shim() {
        let directory = tempfile::tempdir().expect("a temporary directory");
        std::fs::write(directory.path().join("code.cmd"), "").expect("a written shim");
        let env = Env {
            path: vec![directory.path().to_path_buf()],
            ..Env::default()
        };
        assert_eq!(
            find_with(Editor::VsCode, &env),
            Some(directory.path().join("code.cmd")),
            "a shim is used when it is all there is"
        );

        std::fs::write(directory.path().join("code.exe"), "").expect("a written program");
        assert_eq!(
            find_with(Editor::VsCode, &env),
            Some(directory.path().join("code.exe")),
            "the executable is preferred to the shim"
        );
    }

    #[cfg(windows)]
    #[test]
    fn only_a_script_is_started_through_the_shell() {
        assert!(needs_shell(Path::new("C:/bin/code.cmd")));
        assert!(needs_shell(Path::new("C:/bin/build.BAT")));
        assert!(!needs_shell(Path::new("C:/bin/Code.exe")));
    }

    #[cfg(not(windows))]
    #[test]
    fn nothing_is_started_through_the_shell_here() {
        assert!(!needs_shell(Path::new("/usr/bin/code")));
    }

    #[test]
    fn a_chosen_editor_that_was_not_found_says_so() {
        let detected = vec![Detected {
            editor: Editor::VsCode,
            program: PathBuf::from("/usr/bin/code"),
        }];
        let (name, launch) = resolve(Editor::Auto, "", &detected, Path::new("/code/a.rs"), 1, 1)
            .expect("the first editor is used");
        assert_eq!(name, "VS Code");
        assert_eq!(launch.program, Path::new("/usr/bin/code"));

        let error = resolve(Editor::Zed, "", &detected, Path::new("/code/a.rs"), 1, 1)
            .expect_err("Zed is not installed");
        assert!(error.contains("Zed"), "{error}");
        assert!(error.contains("Settings"), "{error}");

        let none = resolve(Editor::Auto, "", &[], Path::new("/code/a.rs"), 1, 1)
            .expect_err("no editor at all");
        assert!(none.contains("No editor"), "{none}");
    }

    #[test]
    fn a_custom_command_is_used_as_written() {
        let (name, launch) = resolve(
            Editor::Custom,
            "vi +{line} {file}",
            &[],
            Path::new("/code/a.rs"),
            5,
            1,
        )
        .expect("a usable command");
        assert_eq!(name, "the custom command");
        assert_eq!(launch.program, Path::new("vi"));
        assert_eq!(launch.arguments, vec!["+5", "/code/a.rs"]);
    }
}
