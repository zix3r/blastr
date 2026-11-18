//! Blastr: the window, and the analysis engine it starts beside it.
//!
//! The window is built for the Windows GUI subsystem, so it never has a
//! console and nothing can be printed to one. An optional path opens a
//! repository; unknown options are ignored rather than refused, because there
//! is nowhere to report them.

#![windows_subsystem = "windows"]

use std::ffi::OsString;
use std::path::PathBuf;

fn main() {
    blastr_gui::run(start_directory(std::env::args_os().skip(1)));
}

/// The directory to look for a repository in: the first argument that is not
/// an option, or `None` for the current directory.
fn start_directory(arguments: impl Iterator<Item = OsString>) -> Option<PathBuf> {
    arguments
        .filter(|argument| !is_option(argument))
        .map(PathBuf::from)
        .next()
}

/// Whether an argument is written as an option rather than as a path.
fn is_option(argument: &OsString) -> bool {
    argument.to_string_lossy().starts_with('-')
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Arguments as the system hands them over.
    fn arguments(text: &[&str]) -> std::vec::IntoIter<OsString> {
        text.iter()
            .map(OsString::from)
            .collect::<Vec<_>>()
            .into_iter()
    }

    #[test]
    fn no_argument_opens_the_current_directory() {
        assert_eq!(start_directory(arguments(&[])), None);
    }

    #[test]
    fn a_path_is_the_directory_to_open() {
        assert_eq!(
            start_directory(arguments(&["/home/dev/app"])),
            Some(PathBuf::from("/home/dev/app"))
        );
    }

    #[test]
    fn options_are_ignored_and_the_first_path_wins() {
        assert_eq!(start_directory(arguments(&["--help"])), None);
        assert_eq!(
            start_directory(arguments(&["--workspace", "C:\\work", "other"])),
            Some(PathBuf::from("C:\\work"))
        );
    }
}
