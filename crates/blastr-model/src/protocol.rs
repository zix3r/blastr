//! The messages the window and the analysis engine exchange.
//!
//! One message is one frame: a little-endian `u32` byte count, followed by
//! that many `postcard` bytes. The window writes [`Request`]s to the engine's
//! standard input and reads [`Response`]s from its standard output; the
//! engine's standard error carries its logs.
//!
//! The protocol is versioned as a whole: [`PROTOCOL_VERSION`] changes whenever
//! a message changes shape, and the window refuses an engine that speaks
//! another version instead of misreading its messages.

use std::io::{self, Read, Write};
use std::path::PathBuf;

use serde::Serialize;
use serde::de::DeserializeOwned;

use crate::{
    Analysis, AnalysisOptions, AnalysisParams, Assume, DiffResult, RevSpec, RevisionGraph,
    SymbolPath,
};

/// Version of the message protocol.
///
/// The window and the engine are installed together, so they always share it;
/// a mismatch means two different builds are on the machine, and the window
/// says so instead of reading messages it does not understand.
///
/// 2: opening a repository no longer carries options read from a file; the
/// window sends the user's settings with every job instead.
pub const PROTOCOL_VERSION: u32 = 2;

/// Largest frame either side accepts, in bytes.
///
/// The result of a large comparison is the biggest message there is, and it
/// stays far below this; the limit only stops a broken stream from being read
/// as a frame of several gigabytes.
pub const MAX_FRAME_BYTES: u32 = 512 * 1024 * 1024;

/// Which kind of job a message belongs to.
///
/// The window runs two kinds of job side by side, each numbered from one, so a
/// bare number would not say which job is meant.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
pub enum JobKind {
    /// A comparison of two revisions.
    Compare,
    /// A hypothetical change to one symbol.
    WhatIf,
}

/// One job: its kind and the number the window gave it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, serde::Deserialize)]
pub struct JobId {
    /// Which kind of job it is.
    pub kind: JobKind,
    /// The window's number for it, increasing within a kind.
    pub generation: u64,
}

/// What the window asks the engine to do.
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub enum Request {
    /// Starts the conversation; the engine answers with [`Response::Hello`].
    Hello {
        /// The protocol version the window speaks.
        protocol: u32,
    },
    /// Finds the repository above a path and reads its identity.
    Open {
        /// The folder the user picked.
        path: PathBuf,
    },
    /// Compares two revisions and traces the impact.
    Compare {
        /// Identifies the job.
        id: JobId,
        /// The working tree of the repository.
        repo: PathBuf,
        /// The two sides to compare.
        spec: RevSpec,
        /// The settings to analyse under.
        options: AnalysisOptions,
        /// The parameters of the impact analysis.
        params: AnalysisParams,
    },
    /// Traces what an assumed change of one symbol would reach.
    WhatIf {
        /// Identifies the job.
        id: JobId,
        /// The working tree of the repository.
        repo: PathBuf,
        /// The symbol assumed to change.
        symbol: SymbolPath,
        /// The kind of change assumed.
        assume: Assume,
        /// The settings to analyse under.
        options: AnalysisOptions,
        /// The parameters of the impact analysis.
        params: AnalysisParams,
    },
    /// Stops a job at its next cancellation check.
    Cancel {
        /// The job to stop.
        id: JobId,
    },
    /// Asks what the cache holds.
    CacheStatus,
    /// Empties the cache.
    CacheClear,
    /// Ends the engine; the window sends this when it closes.
    Shutdown,
}

impl Request {
    /// The job this request belongs to, when it belongs to one.
    pub fn job(&self) -> Option<JobId> {
        match self {
            Request::Hello { .. }
            | Request::Open { .. }
            | Request::CacheStatus
            | Request::CacheClear
            | Request::Shutdown => None,
            Request::Compare { id, .. } | Request::WhatIf { id, .. } | Request::Cancel { id } => {
                Some(*id)
            }
        }
    }
}

/// Why the engine could not do something, in the words the window shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Failure {
    /// What went wrong.
    pub message: String,
    /// What to do next, when there is something useful to say.
    pub hint: Option<String>,
}

impl Failure {
    /// A failure with a message and no hint.
    pub fn new(message: impl Into<String>) -> Failure {
        Failure {
            message: message.into(),
            hint: None,
        }
    }
}

/// What the cache holds and where it lives: revision graphs, the analyses
/// computed from them and materialised snapshots.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct CacheStatus {
    /// Number of cached graphs.
    pub graphs: u64,
    /// Total size of the cached graphs, in bytes.
    pub graph_bytes: u64,
    /// Number of cached analyses: comparisons whose impact analysis is
    /// stored and reused instead of computed again.
    pub results: u64,
    /// Total size of the cached analyses, in bytes.
    pub result_bytes: u64,
    /// Number of materialised snapshots.
    pub snapshots: u64,
    /// Total size of the snapshots on disk, in bytes.
    pub snapshot_bytes: u64,
    /// The cache directory.
    pub cache_dir: PathBuf,
    /// Directory holding materialised snapshots.
    pub snapshots_dir: PathBuf,
}

/// A commit the window names in its title bar.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Commit {
    /// Full object id, in hexadecimal.
    pub id: String,
    /// Abbreviated object id.
    pub short: String,
}

/// What the window shows about the repository it has open.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Repository {
    /// The root of the working tree.
    pub root: PathBuf,
    /// The directory name, shown as the repository's name.
    pub name: String,
    /// The branch `HEAD` is on; `None` when it is detached.
    pub branch: Option<String>,
    /// The commit `HEAD` points to; `None` in a repository without commits.
    pub head: Option<Commit>,
    /// Whether something tracked is modified or staged.
    pub dirty: bool,
}

/// A repository that was opened.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct OpenedRepository {
    /// The repository itself.
    pub repository: Repository,
}

/// How far a job has got.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, serde::Deserialize)]
pub struct Report {
    /// The phase the engine is in, such as `extract head`.
    pub phase: String,
    /// Units of the phase that are done.
    pub done: u64,
    /// Units the phase has in all, when the engine knows.
    pub total: Option<u64>,
}

/// What the engine answers.
///
/// The variants differ a lot in size, which costs nothing here: a message is
/// built once, written to a pipe and dropped.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Serialize, serde::Deserialize)]
pub enum Response {
    /// The engine's build, answering [`Request::Hello`].
    Hello {
        /// The protocol version the engine speaks.
        protocol: u32,
        /// The engine's build: its version and a fingerprint of the executable.
        build: String,
    },
    /// A repository was opened, or could not be.
    Opened {
        /// The repository, or why it cannot be used.
        result: Result<OpenedRepository, Failure>,
    },
    /// A job's progress.
    Progress {
        /// The job reporting.
        id: JobId,
        /// How far it has got.
        report: Report,
    },
    /// The classified changes of a comparison, before its impact.
    Changes {
        /// The job they belong to.
        id: JobId,
        /// The changes.
        diff: DiffResult,
    },
    /// A comparison finished, or was refreshed after a save.
    Result {
        /// The job it belongs to.
        id: JobId,
        /// The analysis.
        analysis: Box<Analysis>,
        /// The graph of the newer side, for symbol search.
        graph: Option<Box<RevisionGraph>>,
        /// How long the engine took, in milliseconds.
        elapsed_ms: u64,
    },
    /// A comparison failed.
    Failed {
        /// The job it belongs to.
        id: JobId,
        /// What went wrong.
        failure: Failure,
        /// Whether the comparison was already showing a result: a failure
        /// while watching then keeps that result on screen.
        watching: bool,
    },
    /// A watching session noticed a save and started recomputing.
    Refreshing {
        /// The job it belongs to.
        id: JobId,
    },
    /// The base side of a watching session moved: it has to be built again.
    BaseMoved {
        /// The job it belongs to.
        id: JobId,
    },
    /// A hypothetical change finished.
    WhatIfResult {
        /// The job it belongs to.
        id: JobId,
        /// The hypothetical analysis.
        analysis: Box<Analysis>,
        /// How long the engine took, in milliseconds.
        elapsed_ms: u64,
    },
    /// A hypothetical change failed.
    WhatIfFailed {
        /// The job it belongs to.
        id: JobId,
        /// What went wrong.
        failure: Failure,
    },
    /// What the cache holds, answering [`Request::CacheStatus`] and
    /// [`Request::CacheClear`].
    Cache {
        /// The state of the cache, or why it could not be read.
        result: Result<CacheStatus, Failure>,
    },
}

/// Encodes one message as a frame.
pub fn encode_frame<T: Serialize>(message: &T) -> io::Result<Vec<u8>> {
    let body = postcard::to_allocvec(message)
        .map_err(|error| invalid_input(format!("could not encode a message: {error}")))?;
    let length = u32::try_from(body.len())
        .ok()
        .filter(|length| *length <= MAX_FRAME_BYTES)
        .ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                format!(
                    "a message of {} bytes is larger than the {} byte limit",
                    body.len(),
                    MAX_FRAME_BYTES
                ),
            )
        })?;
    let mut frame = Vec::with_capacity(4 + body.len());
    frame.extend_from_slice(&length.to_le_bytes());
    frame.extend_from_slice(&body);
    Ok(frame)
}

/// Writes one message to `writer`.
pub fn write_message<W: Write, T: Serialize>(writer: &mut W, message: &T) -> io::Result<()> {
    let frame = encode_frame(message)?;
    writer.write_all(&frame)?;
    writer.flush()
}

/// Reads one message from `reader`.
///
/// `Ok(None)` means the other side closed the stream cleanly, which is how the
/// engine learns that the window is gone.
pub fn read_message<R: Read, T: DeserializeOwned>(reader: &mut R) -> io::Result<Option<T>> {
    let Some(body) = read_frame(reader)? else {
        return Ok(None);
    };
    postcard::from_bytes(&body)
        .map(Some)
        .map_err(|error| invalid_data(format!("could not decode a message: {error}")))
}

/// Reads one frame from `reader`; `Ok(None)` at the end of the stream.
pub fn read_frame<R: Read>(reader: &mut R) -> io::Result<Option<Vec<u8>>> {
    let Some(header) = read_length(reader)? else {
        return Ok(None);
    };
    if header > MAX_FRAME_BYTES {
        return Err(invalid_data(format!(
            "a frame of {header} bytes is larger than the {MAX_FRAME_BYTES} byte limit"
        )));
    }
    let mut body = vec![0; header as usize];
    reader.read_exact(&mut body).map_err(|error| {
        if error.kind() == io::ErrorKind::UnexpectedEof {
            invalid_data(format!(
                "the stream ended in the middle of a frame of {header} bytes"
            ))
        } else {
            error
        }
    })?;
    Ok(Some(body))
}

/// Reads the four length bytes of a frame; `Ok(None)` when the stream ended
/// before the first of them.
fn read_length<R: Read>(reader: &mut R) -> io::Result<Option<u32>> {
    let mut header = [0u8; 4];
    let mut filled = 0;
    while filled < header.len() {
        match reader.read(&mut header[filled..]) {
            Ok(0) if filled == 0 => return Ok(None),
            Ok(0) => {
                return Err(invalid_data(
                    "the stream ended in the middle of a frame length",
                ));
            }
            Ok(read) => filled += read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
    Ok(Some(u32::from_le_bytes(header)))
}

/// An error caused by a message that is not what the protocol says.
fn invalid_data(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message.into())
}

/// An error caused by a value that cannot be sent.
fn invalid_input(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

#[cfg(test)]
mod tests {
    use crate::{AnalysisOptions, ExecMode, RevSpec};

    use super::*;

    fn compare() -> Request {
        Request::Compare {
            id: JobId {
                kind: JobKind::Compare,
                generation: 7,
            },
            repo: PathBuf::from("/tmp/project"),
            spec: RevSpec {
                rev: Some("HEAD~2".to_owned()),
                ..RevSpec::default()
            },
            options: AnalysisOptions {
                features: vec!["fast".to_owned()],
                all_features: false,
                target: None,
                exec: ExecMode::Restricted,
                manifest_dir: None,
                watch: true,
            },
            params: AnalysisParams::default(),
        }
    }

    #[test]
    fn every_request_survives_a_round_trip() {
        let requests = [
            Request::Hello {
                protocol: PROTOCOL_VERSION,
            },
            Request::Open {
                path: PathBuf::from("C:\\projects\\app"),
            },
            compare(),
            Request::WhatIf {
                id: JobId {
                    kind: JobKind::WhatIf,
                    generation: 3,
                },
                repo: PathBuf::from("/tmp/project"),
                symbol: crate::SymbolPath {
                    path: "app::total".to_owned(),
                    kind: crate::SymbolKind::Function,
                    disambiguator: 1,
                },
                assume: Assume::VariantRemoved,
                options: AnalysisOptions::default(),
                params: AnalysisParams::default(),
            },
            Request::Cancel {
                id: JobId {
                    kind: JobKind::Compare,
                    generation: 7,
                },
            },
            Request::CacheStatus,
            Request::CacheClear,
            Request::Shutdown,
        ];
        for request in requests {
            let frame = encode_frame(&request).expect("a frame");
            let mut bytes = frame.as_slice();
            let decoded: Request = read_message(&mut bytes)
                .expect("a message")
                .expect("a frame, not the end");
            assert_eq!(decoded, request);
            assert!(bytes.is_empty(), "the whole frame is consumed");
        }
    }

    #[test]
    fn a_failure_response_survives_a_round_trip() {
        let response = Response::Failed {
            id: JobId {
                kind: JobKind::Compare,
                generation: 1,
            },
            failure: Failure {
                message: "the project could not be loaded".to_owned(),
                hint: Some("check that `cargo metadata` works".to_owned()),
            },
            watching: true,
        };
        let frame = encode_frame(&response).expect("a frame");
        let mut bytes = frame.as_slice();
        let decoded: Response = read_message(&mut bytes)
            .expect("a message")
            .expect("a frame, not the end");
        assert_eq!(decoded, response);
    }

    #[test]
    fn a_frame_starts_with_its_length() {
        let frame = encode_frame(&Request::CacheStatus).expect("a frame");
        let length = u32::from_le_bytes(frame[..4].try_into().expect("four bytes"));
        assert_eq!(length as usize, frame.len() - 4);
    }

    #[test]
    fn a_closed_stream_is_the_end_and_not_an_error() {
        let mut empty = [].as_slice();
        assert!(read_frame(&mut empty).expect("no error").is_none());
        let message: Option<Request> = read_message(&mut empty).expect("no error");
        assert!(message.is_none());
    }

    #[test]
    fn a_truncated_frame_is_an_error() {
        let frame = encode_frame(&Request::CacheStatus).expect("a frame");
        for cut in [2, 4, frame.len() - 1] {
            let mut partial = &frame[..cut];
            let error = read_frame(&mut partial).expect_err("a truncated frame");
            assert_eq!(error.kind(), io::ErrorKind::InvalidData, "cut at {cut}");
        }
    }

    #[test]
    fn an_oversized_frame_is_refused_before_it_is_read() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&(MAX_FRAME_BYTES + 1).to_le_bytes());
        stream.extend_from_slice(&[0; 8]);
        let error = read_frame(&mut stream.as_slice()).expect_err("an oversized frame");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        assert!(error.to_string().contains("larger than"), "{error}");
    }

    #[test]
    fn a_message_written_to_a_stream_can_be_read_back() {
        let mut buffer = Vec::new();
        write_message(&mut buffer, &compare()).expect("a written message");
        write_message(&mut buffer, &Request::Shutdown).expect("a written message");
        let mut bytes = buffer.as_slice();
        let first: Request = read_message(&mut bytes)
            .expect("a message")
            .expect("a frame");
        let second: Request = read_message(&mut bytes)
            .expect("a message")
            .expect("a frame");
        assert_eq!(first, compare());
        assert_eq!(second, Request::Shutdown);
        let end: Option<Request> = read_message(&mut bytes).expect("the end");
        assert!(end.is_none());
    }

    #[test]
    fn bytes_that_are_not_a_message_are_an_error() {
        let mut stream = Vec::new();
        stream.extend_from_slice(&4u32.to_le_bytes());
        stream.extend_from_slice(&[0xff; 4]);
        let error = read_message::<_, Request>(&mut stream.as_slice()).expect_err("not a message");
        assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    }
}
