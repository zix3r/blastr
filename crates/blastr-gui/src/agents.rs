//! The window's side of the bridge to AI agents.
//!
//! `blastr-mcp`, the MCP server an agent starts, connects over a per-user
//! local socket and speaks [`blastr_model::bridge`]. A thread accepts the
//! connections; each has a thread that reads its messages and one that writes
//! the window's, so that a large answer never holds up the window. The
//! messages reach the workspace on a channel, the way the engine's answers do,
//! and the workspace carries each call out as if the user had clicked.
//!
//! A call that needs a result waits until the window is settled: nothing is
//! being opened, no trust question is open and no job runs. The workspace
//! checks its waiting calls whenever it changes.

use std::io::{BufRead as _, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

use blastr_model::bridge::{
    Answer, BRIDGE_VERSION, Call, FromWindow, ShowTarget, Status, SymbolHit, ToWindow,
};
use blastr_model::protocol::{Failure, Report};
use blastr_model::{Analysis, RevSpec, SymbolPath};
use gpui_kit::Context;
use interprocess::local_socket::prelude::*;
use interprocess::local_socket::{GenericFilePath, ListenerOptions};

use crate::settings::ignore_problem;
use crate::state::session::Compare;
use crate::workspace::Workspace;

/// The answer to every call while agents are not allowed.
pub(crate) const TURNED_OFF: &str = "Agent access is turned off in Blastr's settings.";

/// How long a call for the result waits while the working tree is watched,
/// so that the engine, which looks for saves every 250 ms, has seen a file the
/// agent saved just before asking.
const SAVE_GRACE: Duration = Duration::from_millis(600);

/// The answer to a call that needs a repository when none is open.
const NO_REPOSITORY: &str = "No repository is open in Blastr; open one with open_repository.";

/// What the listener's threads pass to the window.
#[derive(Debug)]
pub(crate) enum AgentEvent {
    /// An agent connected; the window's messages to it go to `writer`.
    Connected {
        connection: u64,
        writer: mpsc::Sender<FromWindow>,
    },
    /// A message of the agent on `connection`.
    Message { connection: u64, message: ToWindow },
    /// A call that could not be read, so that its caller is not left waiting.
    Unreadable {
        connection: u64,
        id: u64,
        error: String,
    },
    /// The agent on `connection` went away.
    Closed { connection: u64 },
}

// -- the socket -------------------------------------------------------------

/// Listens for agents at `path`, off the window's thread, and returns the
/// channel their messages arrive on. When another window serves agents
/// already, or the socket cannot be made, this is logged and nothing arrives.
pub(crate) fn listen(path: PathBuf) -> async_channel::Receiver<AgentEvent> {
    let (sender, receiver) = async_channel::unbounded();
    let spawned = std::thread::Builder::new()
        .name("blastr-agents".to_owned())
        .spawn(move || match bind(&path) {
            Ok(listener) => accept(&listener, &sender),
            Err(error) => eprintln!(
                "Blastr does not serve AI agents at {}: {error}",
                path.display()
            ),
        });
    if let Err(error) = spawned {
        eprintln!("Blastr does not serve AI agents: {error}");
    }
    receiver
}

/// Claims the socket at `path`: a named pipe on Windows, an owner-only socket
/// file elsewhere, replacing one that a window which died left behind.
fn bind(path: &Path) -> std::io::Result<LocalSocketListener> {
    #[cfg(unix)]
    {
        if path.exists() {
            if LocalSocketStream::connect(path.to_fs_name::<GenericFilePath>()?).is_ok() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::AddrInUse,
                    "another Blastr window serves them",
                ));
            }
            std::fs::remove_file(path)?;
        }
    }
    let listener = ListenerOptions::new()
        .name(path.to_fs_name::<GenericFilePath>()?)
        .create_sync()?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    }
    Ok(listener)
}

/// Accepts connections until the window closes.
fn accept(listener: &LocalSocketListener, sender: &async_channel::Sender<AgentEvent>) {
    let mut count = 0;
    for stream in listener.incoming() {
        let stream = match stream {
            Ok(stream) => stream,
            Err(error) => {
                eprintln!("An AI agent could not connect: {error}");
                continue;
            }
        };
        count += 1;
        if !serve(stream, count, sender.clone()) {
            return;
        }
    }
}

/// Starts the threads of one connection; whether the window still listens.
fn serve(
    stream: LocalSocketStream,
    connection: u64,
    sender: async_channel::Sender<AgentEvent>,
) -> bool {
    let stream = Arc::new(stream);
    let (writer, messages) = mpsc::channel();
    if sender
        .send_blocking(AgentEvent::Connected { connection, writer })
        .is_err()
    {
        return false;
    }
    let output = Arc::clone(&stream);
    let wrote = std::thread::Builder::new()
        .name("blastr-agent-writer".to_owned())
        .spawn(move || write_messages(&output, &messages));
    let read = std::thread::Builder::new()
        .name("blastr-agent-reader".to_owned())
        .spawn(move || {
            read_messages(&stream, connection, &sender);
            let _ = sender.send_blocking(AgentEvent::Closed { connection });
        });
    if let Err(error) = wrote.and(read) {
        eprintln!("An AI agent could not be served: {error}");
    }
    true
}

/// Writes the window's messages, one JSON line each, until the connection
/// closes or the window forgets it.
fn write_messages(mut output: &LocalSocketStream, messages: &mpsc::Receiver<FromWindow>) {
    for message in messages {
        let mut line = match serde_json::to_string(&message) {
            Ok(line) => line,
            // A reply that cannot be written still answers its call, so the
            // agent is never left waiting.
            Err(error) => {
                let FromWindow::Reply { id, .. } = message else {
                    eprintln!("blastr: could not write a message to an agent: {error}");
                    continue;
                };
                let failure = FromWindow::Reply {
                    id,
                    result: Err(format!("Blastr could not send its answer: {error}")),
                };
                match serde_json::to_string(&failure) {
                    Ok(line) => line,
                    Err(_) => continue,
                }
            }
        };
        line.push('\n');
        if output
            .write_all(line.as_bytes())
            .and_then(|()| output.flush())
            .is_err()
        {
            return;
        }
    }
}

/// Reads the agent's messages until it closes the connection.
fn read_messages(
    input: &LocalSocketStream,
    connection: u64,
    sender: &async_channel::Sender<AgentEvent>,
) {
    for line in BufReader::new(input).lines() {
        let Ok(line) = line else {
            return;
        };
        if line.trim().is_empty() {
            continue;
        }
        let event = match serde_json::from_str::<ToWindow>(&line) {
            Ok(message) => AgentEvent::Message {
                connection,
                message,
            },
            Err(error) => match call_id(&line) {
                Some(id) => AgentEvent::Unreadable {
                    connection,
                    id,
                    error: error.to_string(),
                },
                None => {
                    eprintln!("An AI agent sent a message Blastr does not understand: {error}");
                    continue;
                }
            },
        };
        if sender.send_blocking(event).is_err() {
            return;
        }
    }
}

/// The id of a call whose content could not be read.
fn call_id(line: &str) -> Option<u64> {
    let value: serde_json::Value = serde_json::from_str(line).ok()?;
    value.get("call")?.get("id")?.as_u64()
}

// -- the connections ----------------------------------------------------------

/// One connected agent.
struct Connection {
    number: u64,
    /// The agent's name, once it said hello.
    client: Option<String>,
    writer: mpsc::Sender<FromWindow>,
}

/// A call that waits for the window to settle.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Waiting {
    connection: u64,
    id: u64,
    /// Whether it is answered with the analysis rather than the status.
    analysis: bool,
    /// Not answered before this time.
    not_before: Option<Instant>,
}

/// What tells one result the window shows from another.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Shown {
    /// The open repository.
    pub repository: Option<PathBuf>,
    /// The generation of the comparison.
    pub run: u64,
    /// The generation of the hypothetical change.
    pub what_if: u64,
    /// Whether the hypothetical change is shown.
    pub what_if_shown: bool,
    /// Which result of the comparison or the hypothetical change is shown.
    pub revision: Option<u64>,
}

/// The agents connected to the window and the calls they wait on.
#[derive(Default)]
pub(crate) struct Agents {
    connections: Vec<Connection>,
    waiting: Vec<Waiting>,
    /// What the window showed when the agents were last told it changed.
    shown: Option<Shown>,
}

impl Agents {
    /// Remembers a new connection.
    fn connected(&mut self, number: u64, writer: mpsc::Sender<FromWindow>) {
        self.connections.push(Connection {
            number,
            client: None,
            writer,
        });
    }

    /// Forgets a connection and the calls it waits on.
    fn closed(&mut self, number: u64) {
        self.connections
            .retain(|connection| connection.number != number);
        self.waiting.retain(|call| call.connection != number);
    }

    /// The names of the agents that said hello.
    pub(crate) fn clients(&self) -> Vec<&str> {
        self.connections
            .iter()
            .filter_map(|connection| connection.client.as_deref())
            .collect()
    }

    /// Sends a message to one connection.
    fn send(&self, number: u64, message: FromWindow) {
        if let Some(connection) = self
            .connections
            .iter()
            .find(|connection| connection.number == number)
        {
            let _ = connection.writer.send(message);
        }
    }

    /// Answers an agent's hello.
    fn hello(&mut self, number: u64, client: String) {
        if let Some(connection) = self
            .connections
            .iter_mut()
            .find(|connection| connection.number == number)
        {
            connection.client = Some(client);
        }
        self.send(
            number,
            FromWindow::Hello {
                version: BRIDGE_VERSION,
                blastr: blastr_model::VERSION.to_owned(),
            },
        );
    }

    /// Answers one call.
    fn reply(&self, connection: u64, id: u64, result: Result<Answer, String>) {
        self.send(connection, FromWindow::Reply { id, result });
    }

    /// Keeps a call until the window is settled.
    fn wait(&mut self, connection: u64, id: u64, analysis: bool, not_before: Option<Instant>) {
        self.waiting.push(Waiting {
            connection,
            id,
            analysis,
            not_before,
        });
    }

    /// Gives up a waiting call, answering that it was cancelled; whether it
    /// was waiting.
    fn cancel(&mut self, connection: u64, id: u64) -> bool {
        let before = self.waiting.len();
        self.waiting
            .retain(|call| (call.connection, call.id) != (connection, id));
        let cancelled = self.waiting.len() != before;
        if cancelled {
            self.reply(connection, id, Err("cancelled".to_owned()));
        }
        cancelled
    }

    /// Passes the engine's progress on to every waiting call.
    pub(crate) fn progress(&self, report: &Report) {
        for call in &self.waiting {
            let report = report.clone();
            self.send(
                call.connection,
                FromWindow::Progress {
                    id: call.id,
                    report,
                },
            );
        }
    }

    /// The window is settled: tells every agent when the result changed, and
    /// answers every waiting call.
    fn settled(&mut self, shown: Shown, status: &Status, analysis: Option<&Analysis>) {
        if self.shown.as_ref() != Some(&shown) {
            self.shown = Some(shown);
            for connection in &self.connections {
                let _ = connection.writer.send(FromWindow::Changed);
            }
        }
        let now = Instant::now();
        let (early, due) = std::mem::take(&mut self.waiting)
            .into_iter()
            .partition(|call| call.not_before.is_some_and(|time| time > now));
        self.waiting = early;
        for call in due {
            let answer = if call.analysis {
                Answer::Analysis {
                    status: status.clone(),
                    analysis: analysis.map(|analysis| Box::new(analysis.clone())),
                }
            } else {
                Answer::Status(status.clone())
            };
            self.reply(call.connection, call.id, Ok(answer));
        }
    }
}

// -- the workspace ------------------------------------------------------------

impl Workspace {
    /// Handles what the listener passed on.
    pub(crate) fn agent_event(&mut self, event: AgentEvent, cx: &mut Context<Self>) {
        match event {
            AgentEvent::Connected { connection, writer } => {
                self.agents.connected(connection, writer);
            }
            AgentEvent::Closed { connection } => self.agents.closed(connection),
            AgentEvent::Unreadable {
                connection,
                id,
                error,
            } => {
                let text = format!("Blastr could not read this call: {error}");
                self.agents.reply(connection, id, Err(text));
            }
            AgentEvent::Message {
                connection,
                message,
            } => match message {
                ToWindow::Hello { client, .. } => self.agents.hello(connection, client),
                ToWindow::Call { id, call } => self.agent_call(connection, id, call, cx),
                ToWindow::Cancel { id } => {
                    if self.agents.cancel(connection, id) {
                        self.cancel(cx);
                    }
                }
            },
        }
        // The waiting calls are checked when the workspace notifies.
        cx.notify();
    }

    /// Carries out one call, answering it now or once the window is settled.
    fn agent_call(&mut self, connection: u64, id: u64, call: Call, cx: &mut Context<Self>) {
        if !self.settings.agents {
            self.agents
                .reply(connection, id, Err(TURNED_OFF.to_owned()));
            return;
        }
        let analysis = matches!(call, Call::Analysis | Call::WhatIf { .. });
        // A save just before the call may not have been noticed yet: give the
        // engine a moment, then look again.
        let not_before =
            (matches!(call, Call::Analysis) && self.session.is_watching()).then(|| {
                cx.spawn(async move |this, cx| {
                    cx.background_executor().timer(SAVE_GRACE).await;
                    let _ = this.update(cx, |_, cx| cx.notify());
                })
                .detach();
                Instant::now() + SAVE_GRACE
            });
        match self.carry_out(call, cx) {
            Ok(Some(answer)) => self.agents.reply(connection, id, Ok(answer)),
            Ok(None) => self.agents.wait(connection, id, analysis, not_before),
            Err(text) => self.agents.reply(connection, id, Err(text)),
        }
    }

    /// Does what a call asks; `None` when its answer waits for the window to
    /// settle.
    fn carry_out(&mut self, call: Call, cx: &mut Context<Self>) -> Result<Option<Answer>, String> {
        match call {
            Call::Status => Ok(Some(Answer::Status(self.agent_status()))),
            Call::Open { path } => {
                if !path.is_absolute() {
                    return Err(format!(
                        "Give the repository as an absolute path, not `{}`.",
                        path.display()
                    ));
                }
                let open = blastr_git::discover(&path)
                    .ok()
                    .is_some_and(|root| Some(root.path()) == self.root());
                if !open || self.open_error.is_some() {
                    self.open_path(path, cx);
                }
                Ok(None)
            }
            Call::Compare { spec } => {
                if self.session.repo.is_none() {
                    return Err(NO_REPOSITORY.to_owned());
                }
                if let Some(compare) = compare_for(spec, self.session.default_compare()) {
                    self.choose(compare, cx);
                }
                Ok(None)
            }
            Call::SetOptions {
                features,
                all_features,
                target,
                ignore,
            } => {
                let root = self.root().ok_or(NO_REPOSITORY)?.to_path_buf();
                if let Some(problem) = ignore.as_deref().and_then(ignore_problem) {
                    return Err(format!("The ignore patterns cannot be used: {problem}"));
                }
                self.update_settings(cx, |settings| {
                    let repository = settings.repository_mut(&root);
                    if let Some(features) = features {
                        repository.features = features;
                    }
                    if let Some(all_features) = all_features {
                        repository.all_features = all_features;
                    }
                    if let Some(target) = target {
                        repository.target = (!target.is_empty()).then_some(target);
                    }
                    if let Some(ignore) = ignore {
                        repository.ignore = ignore;
                    }
                });
                // Before the trust question is answered nothing runs; the
                // answer runs the comparison with these options.
                if self.pending_trust.is_none() {
                    self.retry(cx);
                }
                Ok(None)
            }
            Call::Analysis => Ok(None),
            Call::FindSymbol { query, limit } => {
                if self.root().is_none() {
                    return Err(NO_REPOSITORY.to_owned());
                }
                if self.search.is_empty() {
                    return Err(
                        "Blastr knows the symbols once a comparison has a result.".to_owned()
                    );
                }
                let hits = self
                    .search
                    .search(&query, limit as usize)
                    .into_iter()
                    .filter_map(|hit| self.search.get(hit.entry))
                    .map(|entry| SymbolHit {
                        symbol: entry.symbol.clone(),
                        kind: crate::views::left::kind_label(entry.symbol.kind).to_owned(),
                        location: entry.location.clone(),
                    })
                    .collect();
                Ok(Some(Answer::Symbols { hits }))
            }
            Call::WhatIf { symbol, assume } => {
                if self.root().is_none() {
                    return Err(NO_REPOSITORY.to_owned());
                }
                if self.pending_trust.is_some() {
                    return Err(
                        "Blastr is waiting for the user to decide whether to trust this \
                         repository."
                            .to_owned(),
                    );
                }
                self.known(&symbol)?;
                self.begin_what_if_of(symbol, assume, cx);
                Ok(None)
            }
            Call::LeaveWhatIf => {
                self.leave_what_if(cx);
                Ok(Some(Answer::Status(self.agent_status())))
            }
            Call::Show { target } => {
                let symbol = match target {
                    ShowTarget::Change(index) => self
                        .session
                        .shown_changes()
                        .and_then(|diff| diff.changes.get(index as usize))
                        .map(|change| change.symbol.clone())
                        .ok_or_else(|| format!("The result shown has no change {index}."))?,
                    ShowTarget::Symbol(symbol) => {
                        self.known(&symbol)?;
                        symbol
                    }
                };
                self.select(symbol, None, cx);
                Ok(Some(Answer::Done))
            }
        }
    }

    /// Fails unless the window knows `symbol`: from its symbol search, or as
    /// an item or a change of the result shown.
    fn known(&self, symbol: &SymbolPath) -> Result<(), String> {
        let listed = self.session.analysis().is_some_and(|analysis| {
            analysis.items.iter().any(|item| &item.symbol == symbol)
                || analysis
                    .diff
                    .changes
                    .iter()
                    .any(|change| &change.symbol == symbol)
        });
        if listed || self.search.find(symbol).is_some() {
            return Ok(());
        }
        Err(format!(
            "Blastr does not know the symbol `{}`; look it up with find_symbol.",
            symbol.path
        ))
    }

    /// Whether nothing is being opened, asked or computed.
    fn agents_settled(&self) -> bool {
        !self.is_opening() && self.pending_trust.is_none() && !self.session.is_busy()
    }

    /// What the window has open and shows, for an agent.
    fn agent_status(&self) -> Status {
        let repo = self.session.repo.as_ref();
        let compare = self.session.compare.as_ref();
        let failure = self
            .open_error
            .clone()
            .or_else(|| self.session.shown().failure().map(failure_text));
        Status {
            repository: repo.map(|repo| repo.root.clone()),
            branch: repo.and_then(|repo| repo.branch.clone()),
            comparison: compare.map(Compare::labels),
            spec: compare.map(Compare::spec),
            options: repo.map(|_| self.session.options.clone()),
            ignore: repo
                .map(|repo| self.settings.repository_or_default(&repo.root).ignore)
                .unwrap_or_default(),
            what_if: self
                .session
                .what_if
                .as_ref()
                .map(|what_if| (what_if.symbol.clone(), what_if.assume)),
            busy: self.is_opening() || self.session.is_busy(),
            watching: self.session.is_watching(),
            failure,
        }
    }

    /// Answers the waiting calls once the window is settled, and tells the
    /// agents when its result changed. Runs whenever the workspace notifies.
    pub(crate) fn answer_agents(&mut self) {
        if !self.agents_settled() {
            return;
        }
        let shown = Shown {
            repository: self.root().map(Path::to_path_buf),
            run: self.session.run.generation(),
            what_if: self.session.what_if_run.generation(),
            what_if_shown: self.session.what_if.is_some(),
            revision: self.session.shown().ready().map(|outcome| outcome.revision),
        };
        let status = self.agent_status();
        let analysis = self.session.analysis().cloned();
        self.agents.settled(shown, &status, analysis.as_deref());
    }
}

/// The comparison an agent's spec asks for: the working tree, a revision, a
/// range or the index, or what a fresh repository compares when it names none.
fn compare_for(spec: RevSpec, default: Option<Compare>) -> Option<Compare> {
    if spec.rev.is_some() || spec.staged {
        Some(Compare::Typed(RevSpec {
            worktree: false,
            ..spec
        }))
    } else if spec.worktree {
        Some(Compare::WorkingTree)
    } else {
        default
    }
}

/// A failure in words, with its hint.
fn failure_text(failure: &Failure) -> String {
    match &failure.hint {
        Some(hint) => format!("{}\n{hint}", failure.message),
        None => failure.message.clone(),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    /// A set of agents with one connection, and what it receives.
    fn connected() -> (Agents, mpsc::Receiver<FromWindow>) {
        let mut agents = Agents::default();
        let (writer, received) = mpsc::channel();
        agents.connected(1, writer);
        (agents, received)
    }

    fn shown(revision: u64) -> Shown {
        Shown {
            repository: Some("/code/a".into()),
            run: 1,
            what_if: 0,
            what_if_shown: false,
            revision: Some(revision),
        }
    }

    fn drain(received: &mpsc::Receiver<FromWindow>) -> Vec<FromWindow> {
        received.try_iter().collect()
    }

    #[test]
    fn hello_is_answered_and_names_the_agent() {
        let (mut agents, received) = connected();
        assert!(agents.clients().is_empty());
        agents.hello(1, "claude-code".to_owned());
        assert_eq!(agents.clients(), vec!["claude-code"]);
        assert_eq!(
            drain(&received),
            vec![FromWindow::Hello {
                version: BRIDGE_VERSION,
                blastr: blastr_model::VERSION.to_owned(),
            }]
        );
        agents.closed(1);
        assert!(agents.clients().is_empty());
    }

    #[test]
    fn waiting_calls_hear_progress_and_are_answered_when_settled() {
        let (mut agents, received) = connected();
        agents.wait(1, 7, false, None);
        agents.wait(1, 8, true, None);
        let report = Report {
            phase: "impact".to_owned(),
            done: 1,
            total: Some(2),
        };
        agents.progress(&report);
        let status = Status {
            busy: false,
            ..Status::default()
        };
        agents.settled(shown(1), &status, None);
        assert_eq!(
            drain(&received),
            vec![
                FromWindow::Progress {
                    id: 7,
                    report: report.clone()
                },
                FromWindow::Progress { id: 8, report },
                FromWindow::Changed,
                FromWindow::Reply {
                    id: 7,
                    result: Ok(Answer::Status(status.clone()))
                },
                FromWindow::Reply {
                    id: 8,
                    result: Ok(Answer::Analysis {
                        status,
                        analysis: None
                    })
                },
            ]
        );
        agents.progress(&Report {
            phase: "impact".to_owned(),
            done: 2,
            total: None,
        });
        assert!(drain(&received).is_empty(), "nothing waits any more");
    }

    #[test]
    fn a_call_in_its_save_grace_waits_for_the_next_settle() {
        let (mut agents, received) = connected();
        let later = Instant::now() + Duration::from_secs(60);
        agents.wait(1, 9, true, Some(later));
        agents.settled(shown(1), &Status::default(), None);
        assert_eq!(drain(&received), vec![FromWindow::Changed]);
        agents.waiting[0].not_before = Some(Instant::now());
        agents.settled(shown(1), &Status::default(), None);
        assert!(matches!(
            drain(&received).as_slice(),
            [FromWindow::Reply { id: 9, .. }]
        ));
    }

    #[test]
    fn changed_is_sent_only_when_the_shown_result_changes() {
        let (mut agents, received) = connected();
        agents.settled(shown(1), &Status::default(), None);
        agents.settled(shown(1), &Status::default(), None);
        assert_eq!(drain(&received), vec![FromWindow::Changed]);
        agents.settled(shown(2), &Status::default(), None);
        assert_eq!(drain(&received), vec![FromWindow::Changed]);
    }

    #[test]
    fn a_cancelled_call_is_answered_once_and_no_longer_waits() {
        let (mut agents, received) = connected();
        agents.wait(1, 3, false, None);
        assert!(!agents.cancel(1, 4));
        assert!(agents.cancel(1, 3));
        assert!(!agents.cancel(1, 3));
        assert_eq!(
            drain(&received),
            vec![FromWindow::Reply {
                id: 3,
                result: Err("cancelled".to_owned())
            }]
        );
        agents.closed(1);
        agents.wait(1, 5, false, None);
        agents.closed(1);
        assert!(agents.waiting.is_empty());
    }

    #[test]
    fn specs_become_comparisons() {
        let worktree = RevSpec {
            worktree: true,
            ..RevSpec::default()
        };
        assert_eq!(compare_for(worktree, None), Some(Compare::WorkingTree));
        assert_eq!(
            compare_for(RevSpec::default(), Some(Compare::WorkingTree)),
            Some(Compare::WorkingTree)
        );
        let range = RevSpec {
            rev: Some("main..HEAD".to_owned()),
            worktree: true,
            ..RevSpec::default()
        };
        let Some(Compare::Typed(spec)) = compare_for(range, None) else {
            panic!("a typed comparison");
        };
        assert_eq!(spec.rev.as_deref(), Some("main..HEAD"));
        assert!(!spec.worktree);
        let staged = RevSpec {
            staged: true,
            ..RevSpec::default()
        };
        assert_eq!(
            compare_for(staged, None).map(|compare| compare.labels()),
            Some(("HEAD".to_owned(), "Index".to_owned()))
        );
    }

    #[test]
    fn an_unreadable_call_is_known_by_its_id() {
        assert_eq!(
            call_id(r#"{"call":{"id":9,"call":{"call":"something-new"}}}"#),
            Some(9)
        );
        assert_eq!(call_id("not json"), None);
    }

    #[test]
    fn an_agent_connects_says_hello_and_asks_for_the_status() {
        let path = if cfg!(windows) {
            PathBuf::from(format!(
                r"\\.\pipe\blastr-agents-test-{}",
                std::process::id()
            ))
        } else {
            std::env::temp_dir().join(format!("blastr-agents-test-{}.sock", std::process::id()))
        };
        let events = listen(path.clone());
        let name = path
            .as_path()
            .to_fs_name::<GenericFilePath>()
            .expect("a name");
        let mut stream = None;
        for _ in 0..100 {
            if let Ok(connected) = LocalSocketStream::connect(name.borrow()) {
                stream = Some(connected);
                break;
            }
            std::thread::sleep(Duration::from_millis(20));
        }
        let stream = stream.expect("the listener accepts");
        let mut agents = Agents::default();
        let AgentEvent::Connected { connection, writer } =
            events.recv_blocking().expect("a connection")
        else {
            panic!("a connection first");
        };
        agents.connected(connection, writer);

        let mut output = &stream;
        for message in [
            ToWindow::Hello {
                version: BRIDGE_VERSION,
                client: "test".to_owned(),
            },
            ToWindow::Call {
                id: 1,
                call: Call::Status,
            },
        ] {
            let line = serde_json::to_string(&message).expect("serialises") + "\n";
            output.write_all(line.as_bytes()).expect("written");
        }
        for _ in 0..2 {
            match events.recv_blocking().expect("a message") {
                AgentEvent::Message {
                    message: ToWindow::Hello { client, .. },
                    ..
                } => agents.hello(connection, client),
                AgentEvent::Message {
                    message: ToWindow::Call { id, .. },
                    ..
                } => agents.reply(connection, id, Ok(Answer::Status(Status::default()))),
                other => panic!("unexpected {other:?}"),
            }
        }

        let mut lines = BufReader::new(&stream).lines();
        let mut next = || -> FromWindow {
            let line = lines.next().expect("a line").expect("readable");
            serde_json::from_str(&line).expect("parses")
        };
        assert!(matches!(next(), FromWindow::Hello { version, .. } if version == BRIDGE_VERSION));
        assert_eq!(
            next(),
            FromWindow::Reply {
                id: 1,
                result: Ok(Answer::Status(Status::default()))
            }
        );
        assert_eq!(agents.clients(), vec!["test"]);
    }
}
