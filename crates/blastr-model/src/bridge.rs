//! The bridge between AI agents and the window.
//!
//! `blastr-mcp` (the MCP server an agent starts) connects to the open window
//! over a per-user local socket and asks it things. One JSON message per line,
//! in both directions:
//!
//! 1. The server sends [`ToWindow::Hello`]; the window answers
//!    [`FromWindow::Hello`].
//! 2. The server sends [`ToWindow::Call`]s, each with its own id; the window
//!    answers each with one [`FromWindow::Reply`] carrying the same id. Calls
//!    may overlap; replies come in any order.
//! 3. While a call runs a job, the window sends [`FromWindow::Progress`] for
//!    it. Whenever the window's result changes, for any reason, it sends
//!    [`FromWindow::Changed`].
//!
//! The window does the work with its own state and engine, exactly as if the
//! user had clicked: what an agent opens, compares or selects is what the
//! window shows.

use std::path::PathBuf;

use serde::{Deserialize, Serialize};

use crate::protocol::Report;
use crate::{Analysis, AnalysisOptions, Assume, Location, RevSpec, SymbolPath};

/// Bumped when a message changes shape, so that a `blastr-mcp` and a window
/// from different builds refuse each other clearly.
pub const BRIDGE_VERSION: u32 = 1;

/// Where the window listens: a named pipe on Windows, a socket file on other
/// systems. One per user, so two users of one machine never meet.
pub fn socket_path() -> PathBuf {
    let user = std::env::var("USERNAME")
        .or_else(|_| std::env::var("USER"))
        .unwrap_or_default();
    let name = format!("blastr-agents-{}", sanitise(&user));
    if cfg!(windows) {
        PathBuf::from(format!(r"\\.\pipe\{name}"))
    } else {
        let dir = std::env::var_os("XDG_RUNTIME_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(std::env::temp_dir);
        dir.join(format!("{name}.sock"))
    }
}

/// Keeps letters, digits, `-` and `_`, so any user name makes a valid name.
fn sanitise(text: &str) -> String {
    text.chars()
        .filter(|c| c.is_ascii_alphanumeric() || *c == '-' || *c == '_')
        .collect()
}

/// A message from `blastr-mcp` to the window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ToWindow {
    /// Starts the conversation.
    Hello {
        /// [`BRIDGE_VERSION`] of the sender.
        version: u32,
        /// The agent, as its MCP client names itself (`claude-code`), for the
        /// status bar.
        client: String,
    },
    /// Asks the window something.
    Call {
        /// Chosen by the sender; the reply carries it back.
        id: u64,
        /// What to do.
        call: Call,
    },
    /// Stops the job a call started, if it is still running.
    Cancel {
        /// The id of that call.
        id: u64,
    },
}

/// What an agent can ask the window.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "call")]
pub enum Call {
    /// What is open and what is shown. Answers [`Answer::Status`] at once.
    Status,
    /// Opens a repository in the window. When the window asks the user whether
    /// to trust it, the reply waits for the answer. Answers [`Answer::Status`].
    Open {
        /// Any path inside the repository.
        path: PathBuf,
    },
    /// Compares two revisions of the open repository and waits for the result.
    /// Answers [`Answer::Status`].
    Compare {
        /// The comparison, as the compare chip makes it.
        spec: RevSpec,
    },
    /// Changes the open repository's analysis options, as Settings does, and
    /// runs the comparison again. A field left `None` keeps its value.
    /// Answers [`Answer::Status`].
    SetOptions {
        /// Cargo features to enable.
        features: Option<Vec<String>>,
        /// Enable every feature.
        all_features: Option<bool>,
        /// Target triple; `Some("")` means the host.
        target: Option<String>,
        /// Ignore patterns.
        ignore: Option<Vec<String>>,
    },
    /// The result shown in the window. Waits while a comparison or a refresh
    /// after a save is running, so a call made after a save sees it.
    /// Answers [`Answer::Analysis`].
    Analysis,
    /// Symbols of the open repository whose name matches, ranked as the
    /// window's symbol search ranks them. Answers [`Answer::Symbols`].
    FindSymbol {
        /// What to look for.
        query: String,
        /// At most this many.
        limit: u32,
    },
    /// Traces an assumed change of one symbol in the working tree, shows it in
    /// the window and waits for it. Answers [`Answer::Analysis`].
    WhatIf {
        /// The symbol.
        symbol: SymbolPath,
        /// The change to assume.
        assume: Assume,
    },
    /// Leaves a what-if and shows the comparison again. Answers
    /// [`Answer::Status`].
    LeaveWhatIf,
    /// Selects something in the window: a change by its index in the
    /// analysis' changes, or an affected item by its symbol. Answers
    /// [`Answer::Done`].
    Show {
        /// What to select.
        target: ShowTarget,
    },
}

/// What [`Call::Show`] selects.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ShowTarget {
    /// A change, by its index in the analysis' changes.
    Change(u32),
    /// An affected item, or a changed symbol, by its path.
    Symbol(SymbolPath),
}

/// A message from the window to `blastr-mcp`.
///
/// Built once, written to the socket and dropped, so the size of its
/// variants does not matter.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
#[allow(clippy::large_enum_variant)]
pub enum FromWindow {
    /// Answers [`ToWindow::Hello`].
    Hello {
        /// [`BRIDGE_VERSION`] of the window.
        version: u32,
        /// The Blastr version of the window.
        blastr: String,
    },
    /// Answers one [`ToWindow::Call`].
    Reply {
        /// The id of the call.
        id: u64,
        /// The answer, or why there is none, in words for the agent.
        result: Result<Answer, String>,
    },
    /// How far the job a call started has got.
    Progress {
        /// The id of the call.
        id: u64,
        /// The engine's report.
        report: Report,
    },
    /// The window's result changed: a new comparison, a refresh after a save,
    /// a what-if, or another repository.
    Changed,
}

/// The answer to a [`Call`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case", tag = "answer")]
pub enum Answer {
    /// See [`Status`].
    Status(Status),
    /// The result shown, with what it describes.
    Analysis {
        /// What is open and compared.
        status: Status,
        /// The result; `None` when nothing has been compared yet.
        analysis: Option<Box<Analysis>>,
    },
    /// Matching symbols, best first.
    Symbols {
        /// The symbols found.
        hits: Vec<SymbolHit>,
    },
    /// The call was carried out.
    Done,
}

/// What the window has open and shows.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct Status {
    /// The open repository's root; `None` on the start screen.
    pub repository: Option<PathBuf>,
    /// The branch `HEAD` is on.
    pub branch: Option<String>,
    /// The comparison chosen, as the window labels its sides
    /// (`HEAD`, `Working tree`, `ecb87b2`).
    pub comparison: Option<(String, String)>,
    /// The comparison chosen, as a spec that [`Call::Compare`] accepts.
    pub spec: Option<RevSpec>,
    /// The options the repository is analysed with.
    pub options: Option<AnalysisOptions>,
    /// Ignore patterns of the repository.
    pub ignore: Vec<String>,
    /// Whether a what-if is shown, and of which symbol.
    pub what_if: Option<(SymbolPath, Assume)>,
    /// Whether a job is running (a comparison, a refresh or a what-if).
    pub busy: bool,
    /// Whether the working tree is compared and saves refresh the result.
    pub watching: bool,
    /// The last failure shown, when the window shows one instead of a result.
    pub failure: Option<String>,
}

/// One symbol a search found.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SymbolHit {
    /// Its path.
    pub symbol: SymbolPath,
    /// Its kind, as the window names it (`fn`, `struct`, `field`).
    pub kind: String,
    /// Where it is defined.
    pub location: Location,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_survives_a_round_trip_through_json() {
        let message = ToWindow::Call {
            id: 7,
            call: Call::Compare {
                spec: RevSpec {
                    rev: Some("HEAD~1".into()),
                    ..RevSpec::default()
                },
            },
        };
        let line = serde_json::to_string(&message).expect("serialises");
        assert!(!line.contains('\n'), "one message per line");
        let back: ToWindow = serde_json::from_str(&line).expect("parses");
        assert_eq!(back, message);
    }

    #[test]
    fn every_answer_survives_a_round_trip_through_json() {
        let hit = SymbolHit {
            symbol: SymbolPath::new("demo::run", crate::SymbolKind::Function),
            kind: "fn".into(),
            location: Location {
                file: crate::RelPath("src/lib.rs".into()),
                span: crate::Span {
                    start: 0,
                    end: 3,
                    start_line: 1,
                    end_line: 1,
                },
            },
        };
        let answers = [
            Answer::Status(Status::default()),
            Answer::Analysis {
                status: Status::default(),
                analysis: None,
            },
            Answer::Symbols { hits: vec![hit] },
            Answer::Done,
        ];
        for answer in answers {
            let message = FromWindow::Reply {
                id: 1,
                result: Ok(answer),
            };
            let line = serde_json::to_string(&message).expect("serialises");
            let back: FromWindow = serde_json::from_str(&line).expect("parses");
            assert_eq!(back, message);
        }
    }

    #[test]
    fn the_socket_is_per_user_and_has_a_valid_name() {
        let path = socket_path();
        let name = path.file_name().expect("a name").to_string_lossy();
        assert!(name.starts_with("blastr-agents-"));
        assert!(!name.contains(' '));
    }
}
