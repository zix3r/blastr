//! The analysis engine: a `blastr-engine` process beside the window.
//!
//! Requests are written to its standard input as they are made. A thread reads
//! its answers and passes them on a channel, which the workspace awaits; its
//! logs go to a file whose end becomes the detail of a failure. The process is
//! started on the first request and again after it stopped.

use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};

use blastr_model::protocol::{self, Request, Response};

/// How many lines of the engine's log a failure quotes.
const LOG_TAIL_LINES: usize = 8;

/// What the reader thread passes on.
#[derive(Debug)]
pub(crate) enum Incoming {
    /// An answer of the engine.
    Response(Box<Response>),
    /// The process with this number closed its output.
    Stopped(u64),
}

/// The connection to the engine process.
pub(crate) struct Engine {
    process: Option<Process>,
    /// Numbers the processes, so that the end of an old one is not taken for
    /// the end of the current one.
    started: u64,
    sender: async_channel::Sender<Incoming>,
}

/// One running engine.
struct Process {
    number: u64,
    child: Child,
    input: ChildStdin,
}

impl Drop for Process {
    fn drop(&mut self) {
        let _ = protocol::write_message(&mut self.input, &Request::Shutdown);
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

impl Engine {
    /// A connection that starts the engine on its first request, and the
    /// channel its answers arrive on.
    pub(crate) fn new() -> (Engine, async_channel::Receiver<Incoming>) {
        let (sender, receiver) = async_channel::unbounded();
        let engine = Engine {
            process: None,
            started: 0,
            sender,
        };
        (engine, receiver)
    }

    /// Sends a request, starting the engine first when it does not run.
    pub(crate) fn send(&mut self, request: &Request) -> Result<(), String> {
        if self.process.is_none() {
            self.start()?;
        }
        let process = self.process.as_mut().expect("the engine was just started");
        if let Err(error) = protocol::write_message(&mut process.input, request) {
            self.process = None;
            return Err(format!("the analysis engine could not be reached: {error}"));
        }
        Ok(())
    }

    /// Whether an engine runs.
    pub(crate) fn is_running(&self) -> bool {
        self.process.is_some()
    }

    /// Whether `number` is the process that runs now.
    pub(crate) fn is_current(&self, number: u64) -> bool {
        self.process
            .as_ref()
            .is_some_and(|process| process.number == number)
    }

    /// Stops the engine; the next request starts a new one.
    pub(crate) fn kill(&mut self) {
        self.process = None;
    }

    /// Forgets a process that stopped, returning the end of its log.
    pub(crate) fn stopped(&mut self) -> String {
        self.process = None;
        log_tail(&log_path())
    }

    fn start(&mut self) -> Result<(), String> {
        let program = engine_program()?;
        let log = log_path();
        if let Some(parent) = log.parent() {
            let _ = std::fs::create_dir_all(parent);
        }
        let errors = std::fs::File::create(&log).map_or_else(|_| Stdio::null(), Stdio::from);
        let mut command = Command::new(&program);
        command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(errors);
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt as _;
            /// Starts the engine without a console window.
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            command.creation_flags(CREATE_NO_WINDOW);
        }
        let mut child = command.spawn().map_err(|error| {
            format!(
                "could not start the analysis engine `{}`: {error}",
                program.display()
            )
        })?;
        let (Some(input), Some(mut output)) = (child.stdin.take(), child.stdout.take()) else {
            let _ = child.kill();
            return Err("the analysis engine could not be given its streams".to_owned());
        };
        self.started += 1;
        let number = self.started;
        let sender = self.sender.clone();
        std::thread::Builder::new()
            .name("blastr-engine-reader".to_owned())
            .spawn(move || {
                while let Ok(Some(response)) = protocol::read_message::<_, Response>(&mut output) {
                    if sender
                        .send_blocking(Incoming::Response(Box::new(response)))
                        .is_err()
                    {
                        return;
                    }
                }
                let _ = sender.send_blocking(Incoming::Stopped(number));
            })
            .map_err(|error| format!("could not read the engine's answers: {error}"))?;
        self.process = Some(Process {
            number,
            child,
            input,
        });
        self.send(&Request::Hello {
            protocol: protocol::PROTOCOL_VERSION,
        })
    }
}

/// Why an engine of another build cannot be used, when it cannot.
pub(crate) fn mismatch(protocol: u32, build: &str) -> Option<String> {
    let version = build.split('+').next().unwrap_or(build);
    if protocol != protocol::PROTOCOL_VERSION || version != blastr_model::VERSION {
        return Some(format!(
            "it is version {version} (protocol {protocol}) while this window is version {} \
             (protocol {}); install both programs from the same build of Blastr",
            blastr_model::VERSION,
            protocol::PROTOCOL_VERSION
        ));
    }
    None
}

/// The engine, which is installed next to the window.
fn engine_program() -> Result<PathBuf, String> {
    let program = std::env::current_exe()
        .ok()
        .and_then(|exe| {
            Some(
                exe.parent()?
                    .join(format!("blastr-engine{}", std::env::consts::EXE_SUFFIX)),
            )
        })
        .ok_or_else(|| "could not tell where Blastr is installed".to_owned())?;
    if !program.is_file() {
        return Err(format!(
            "the analysis engine `{}` is missing. Reinstall Blastr, so that both programs are \
             installed together.",
            program.display()
        ));
    }
    Ok(program)
}

/// Where the engine's log is written.
fn log_path() -> PathBuf {
    match blastr_model::paths() {
        Some(paths) => paths.cache_dir.join("engine.log"),
        None => std::env::temp_dir().join("blastr-engine.log"),
    }
}

/// The last lines of the log that say something.
fn log_tail(log: &Path) -> String {
    let text = std::fs::read_to_string(log).unwrap_or_default();
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim_end)
        .filter(|line| !line.is_empty())
        .collect();
    lines[lines.len().saturating_sub(LOG_TAIL_LINES)..].join("\n")
}
