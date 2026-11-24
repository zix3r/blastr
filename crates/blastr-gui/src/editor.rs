//! Opening a file in the user's editor.
//!
//! The editor is one command line with `{file}`, `{line}` and `{column}`
//! placeholders. It starts out as the first editor found on the machine; with
//! no command the file opens in the program the system associates with it.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Editors that are looked for, in order, with the command that opens a file
/// at a position.
const KNOWN: &[(&str, &str)] = &[
    ("code", "-g {file}:{line}:{column}"),
    ("cursor", "-g {file}:{line}:{column}"),
    ("zed", "{file}:{line}:{column}"),
    ("subl", "{file}:{line}:{column}"),
    ("rustrover", "--line {line} {file}"),
    ("notepad++", "-n{line} {file}"),
];

/// Where installers put the editors that do not add themselves to `PATH`,
/// below `%LOCALAPPDATA%\Programs` or `%ProgramFiles%`.
#[cfg(windows)]
const INSTALLS: &[(&str, &str)] = &[
    ("code", "Microsoft VS Code/Code.exe"),
    ("cursor", "cursor/Cursor.exe"),
    ("zed", "Zed/zed.exe"),
    ("notepad++", "Notepad++/notepad++.exe"),
];

/// The command of the first editor found on this machine.
pub(crate) fn detect() -> Option<String> {
    KNOWN.iter().find_map(|(name, arguments)| {
        let program = find(name)?;
        Some(format!(
            "{} {arguments}",
            quote(&program.display().to_string())
        ))
    })
}

/// Finds a program by name on `PATH`, or where its installer puts it.
fn find(name: &str) -> Option<PathBuf> {
    let suffixes: &[&str] = if cfg!(windows) {
        &[".exe", ".cmd", ".bat"]
    } else {
        &[""]
    };
    let path = std::env::var_os("PATH").unwrap_or_default();
    let on_path = std::env::split_paths(&path).find_map(|directory| {
        suffixes
            .iter()
            .map(|suffix| directory.join(format!("{name}{suffix}")))
            .find(|candidate| candidate.is_file())
    });
    #[cfg(windows)]
    let on_path = on_path.or_else(|| {
        let roots: Vec<PathBuf> = [("LOCALAPPDATA", "Programs"), ("ProgramFiles", "")]
            .iter()
            .filter_map(|(variable, below)| {
                Some(PathBuf::from(std::env::var_os(variable)?).join(below))
            })
            .collect();
        INSTALLS
            .iter()
            .filter(|(known, _)| *known == name)
            .flat_map(|(_, file)| roots.iter().map(move |root| root.join(file)))
            .find(|candidate| candidate.is_file())
    });
    on_path
}

/// A word in quotes when it holds white space, so that it stays one word.
fn quote(word: &str) -> String {
    if word.contains(char::is_whitespace) {
        format!("\"{word}\"")
    } else {
        word.to_owned()
    }
}

/// Splits a command line into words; double quotes keep white space inside
/// one word.
fn words(text: &str) -> Vec<String> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    let mut started = false;
    for character in text.chars() {
        match character {
            '"' => {
                quoted = !quoted;
                started = true;
            }
            character if character.is_whitespace() && !quoted => {
                if started {
                    words.push(std::mem::take(&mut word));
                    started = false;
                }
            }
            character => {
                word.push(character);
                started = true;
            }
        }
    }
    if started {
        words.push(word);
    }
    words
}

/// The program and arguments `template` asks for to open `file` at a position.
fn command_line(
    template: &str,
    file: &Path,
    line: u32,
    column: u32,
) -> Result<Vec<String>, String> {
    if !template.contains("{file}") {
        return Err("The editor command needs a `{file}` placeholder.".to_owned());
    }
    let file = file.display().to_string();
    let words: Vec<String> = words(template)
        .iter()
        .map(|word| {
            word.replace("{file}", &file)
                .replace("{line}", &line.to_string())
                .replace("{column}", &column.to_string())
        })
        .collect();
    if words.is_empty() {
        return Err("The editor command has no program to start.".to_owned());
    }
    Ok(words)
}

/// Why `template` cannot open a file, when it cannot.
pub(crate) fn problem(template: &str) -> Option<String> {
    if template.trim().is_empty() {
        return None;
    }
    command_line(template, Path::new("a.rs"), 1, 1).err()
}

/// Opens `file` at `line` and `column` with `template`, or with the program
/// the system associates with it when `template` is empty.
pub(crate) fn open(template: &str, file: &Path, line: u32, column: u32) -> Result<(), String> {
    let mut words = if template.trim().is_empty() {
        let opener = if cfg!(windows) {
            "explorer"
        } else if cfg!(target_os = "macos") {
            "open"
        } else {
            "xdg-open"
        };
        vec![opener.to_owned(), file.display().to_string()]
    } else {
        command_line(template, file, line, column)?
    };
    let name = words.remove(0);
    // A bare name such as `code` is a `.cmd` shim on Windows, which the
    // system only finds once it is spelled out.
    let program = if name.contains(['/', '\\']) {
        PathBuf::from(&name)
    } else {
        find(&name).unwrap_or_else(|| PathBuf::from(&name))
    };
    let mut command = Command::new(&program);
    command
        .args(&words)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null());
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt as _;
        /// Starts a console program without a console window.
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        command.creation_flags(CREATE_NO_WINDOW);
    }
    command.spawn().map(drop).map_err(|error| {
        format!("Could not start `{name}`: {error}. Check the editor in Settings.")
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn placeholders_are_filled_and_a_spaced_path_stays_one_word() {
        let file = Path::new("C:/code/my project/src/lib.rs");
        assert_eq!(
            command_line("code -g {file}:{line}:{column}", file, 12, 3).expect("a command"),
            vec!["code", "-g", "C:/code/my project/src/lib.rs:12:3"]
        );
        assert_eq!(
            command_line(
                "\"C:/Program Files/Ed/ed.exe\" --line {line} \"{file}\"",
                file,
                8,
                1
            )
            .expect("a command"),
            vec![
                "C:/Program Files/Ed/ed.exe",
                "--line",
                "8",
                "C:/code/my project/src/lib.rs"
            ]
        );
    }

    #[test]
    fn a_program_path_with_spaces_is_quoted() {
        assert_eq!(
            quote("C:/Program Files/Code.exe"),
            "\"C:/Program Files/Code.exe\""
        );
        assert_eq!(quote("code"), "code");
        assert_eq!(words(&quote("C:/a b/c.exe")), vec!["C:/a b/c.exe"]);
        assert_eq!(words("a \"\" b"), vec!["a", "", "b"]);
    }

    #[test]
    fn a_command_without_a_file_is_refused() {
        assert!(problem("code -g").is_some());
        assert!(
            problem("").is_none(),
            "empty opens with the system's program"
        );
        assert!(problem("zed {file}").is_none());
    }
}
