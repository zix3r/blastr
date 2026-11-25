//! The connection to the open Blastr window.
//!
//! One connection is shared by every call. It is made on the first call that
//! needs it and made again on the next call after it drops. A reader task
//! routes each reply and progress report to the call it belongs to, and turns
//! [`FromWindow::Changed`] into a broadcast that resource subscriptions follow.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use blastr_model::bridge::{Answer, BRIDGE_VERSION, Call, FromWindow, ToWindow};
use blastr_model::protocol::Report;
use interprocess::local_socket::GenericFilePath;
use interprocess::local_socket::tokio::Stream;
use interprocess::local_socket::tokio::prelude::*;
use rmcp::RoleServer;
use rmcp::model::ProgressNotificationParam;
use rmcp::service::RequestContext;
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader, ReadHalf, WriteHalf};
use tokio::sync::{broadcast, mpsc};

/// What a call answers when the window cannot be reached.
pub const NOT_RUNNING: &str =
    "Blastr is not running. Open the Blastr window (run `blastr`), then try again.";

/// What a call answers when the connection drops before the reply.
const DROPPED: &str = "The connection to the Blastr window dropped before it answered. Try \
                       again; if Blastr was closed, open it first.";

/// How long the window may take to answer the greeting.
const HELLO_TIMEOUT: Duration = Duration::from_secs(10);

/// The window as this server sees it: where it listens and the connection to
/// it, if one is open.
pub struct Window {
    /// The socket the window listens on.
    socket: PathBuf,
    /// The open connection, if any.
    link: tokio::sync::Mutex<Option<Arc<Link>>>,
    /// The id of the next call.
    next_id: AtomicU64,
    /// Fires whenever the window's result changes.
    changed: broadcast::Sender<()>,
}

/// One open connection.
struct Link {
    /// Where calls are written.
    writer: tokio::sync::Mutex<WriteHalf<Stream>>,
    /// The calls waiting for their reply, by id.
    pending: Mutex<HashMap<u64, mpsc::UnboundedSender<Event>>>,
    /// Set when the connection dropped.
    closed: AtomicBool,
}

/// What the window sends about one call.
///
/// Sent once and dropped, so the size of its variants does not matter.
#[allow(clippy::large_enum_variant)]
enum Event {
    /// The job of the call made progress.
    Progress(Report),
    /// The call is answered.
    Reply(Result<Answer, String>),
}

impl Window {
    /// A window listening on `socket`; nothing is connected yet.
    pub fn new(socket: PathBuf) -> Window {
        Window {
            socket,
            link: tokio::sync::Mutex::new(None),
            next_id: AtomicU64::new(1),
            changed: broadcast::channel(16).0,
        }
    }

    /// Follows the window's result: the receiver gets a message whenever it
    /// changes, as long as a connection is open.
    pub fn changes(&self) -> broadcast::Receiver<()> {
        self.changed.subscribe()
    }

    /// Asks the window something on behalf of the request in `context`, and
    /// waits for the answer.
    ///
    /// Progress is forwarded to the client when it sent a progress token, and
    /// cancelling the request cancels the window's job. The error is in words
    /// for the agent.
    pub async fn call(
        &self,
        context: &RequestContext<RoleServer>,
        call: Call,
    ) -> Result<Answer, String> {
        let link = self.connect(&client_name(context)).await?;
        let id = self.next_id.fetch_add(1, Ordering::Relaxed);
        let (sender, mut events) = mpsc::unbounded_channel();
        lock(&link.pending).insert(id, sender);
        // The reader marks the link closed before it lets go of the waiting
        // calls, so a call registered after that is caught here.
        if link.closed.load(Ordering::Relaxed) {
            lock(&link.pending).remove(&id);
            return Err(DROPPED.to_owned());
        }
        if let Err(error) = link.send(&ToWindow::Call { id, call }).await {
            lock(&link.pending).remove(&id);
            return Err(error);
        }

        let token = context.meta.get_progress_token();
        let mut step = 0u32;
        loop {
            tokio::select! {
                event = events.recv() => match event {
                    Some(Event::Reply(result)) => return result,
                    Some(Event::Progress(report)) => {
                        let Some(token) = &token else { continue };
                        step += 1;
                        let mut param = ProgressNotificationParam::new(token.clone(), f64::from(step));
                        param.message = Some(report.phase);
                        // A client that went away is not this call's problem.
                        let _ = context.peer.notify_progress(param).await;
                    }
                    None => return Err(DROPPED.to_owned()),
                },
                () = context.ct.cancelled() => {
                    lock(&link.pending).remove(&id);
                    let _ = link.send(&ToWindow::Cancel { id }).await;
                    return Err("Cancelled.".to_owned());
                }
            }
        }
    }

    /// The open connection, or a new one greeted with the client's name.
    async fn connect(&self, client: &str) -> Result<Arc<Link>, String> {
        let mut slot = self.link.lock().await;
        if let Some(link) = slot.as_ref()
            && !link.closed.load(Ordering::Relaxed)
        {
            return Ok(link.clone());
        }
        *slot = None;

        let name = self
            .socket
            .as_path()
            .to_fs_name::<GenericFilePath>()
            .map_err(|_| NOT_RUNNING.to_owned())?;
        let stream = Stream::connect(name)
            .await
            .map_err(|_| NOT_RUNNING.to_owned())?;
        let (reader, writer) = tokio::io::split(stream);
        let link = Arc::new(Link {
            writer: tokio::sync::Mutex::new(writer),
            pending: Mutex::new(HashMap::new()),
            closed: AtomicBool::new(false),
        });
        link.send(&ToWindow::Hello {
            version: BRIDGE_VERSION,
            client: client.to_owned(),
        })
        .await?;

        let mut reader = BufReader::new(reader);
        let mut line = String::new();
        let read = tokio::time::timeout(HELLO_TIMEOUT, reader.read_line(&mut line)).await;
        let greeting = match read {
            Ok(Ok(count)) if count > 0 => serde_json::from_str::<FromWindow>(&line).ok(),
            _ => None,
        };
        match greeting {
            Some(FromWindow::Hello { version, .. }) if version == BRIDGE_VERSION => {}
            Some(FromWindow::Hello { version, blastr }) => {
                return Err(format!(
                    "The open Blastr window ({blastr}) speaks bridge version {version}, but this \
                     blastr-mcp speaks version {BRIDGE_VERSION}. Use the blastr-mcp that comes \
                     with the running Blastr (they are installed side by side)."
                ));
            }
            _ => {
                return Err("The Blastr window did not answer the greeting. Check that \
                            \"Allow AI agents (MCP)\" is on in Blastr's settings, then try again."
                    .to_owned());
            }
        }

        tokio::spawn(read_messages(reader, link.clone(), self.changed.clone()));
        *slot = Some(link.clone());
        Ok(link)
    }
}

impl Link {
    /// Writes one message as one line; a failure closes the link.
    async fn send(&self, message: &ToWindow) -> Result<(), String> {
        let mut line = serde_json::to_string(message).map_err(|error| error.to_string())?;
        line.push('\n');
        let mut writer = self.writer.lock().await;
        if writer.write_all(line.as_bytes()).await.is_err() {
            self.closed.store(true, Ordering::Relaxed);
            return Err(DROPPED.to_owned());
        }
        Ok(())
    }
}

/// Reads the window's messages until the connection drops, routing each to
/// the call it belongs to. When it drops, every waiting call is told.
async fn read_messages(
    mut reader: BufReader<ReadHalf<Stream>>,
    link: Arc<Link>,
    changed: broadcast::Sender<()>,
) {
    let mut line = String::new();
    loop {
        line.clear();
        match reader.read_line(&mut line).await {
            Ok(0) | Err(_) => break,
            Ok(_) => {}
        }
        let Ok(message) = serde_json::from_str::<FromWindow>(&line) else {
            eprintln!("blastr-mcp: ignoring a message from the window it cannot read: {line}");
            continue;
        };
        let (id, event) = match message {
            FromWindow::Reply { id, result } => (id, Event::Reply(result)),
            FromWindow::Progress { id, report } => (id, Event::Progress(report)),
            FromWindow::Changed => {
                let _ = changed.send(());
                continue;
            }
            FromWindow::Hello { .. } => continue,
        };
        let mut pending = lock(&link.pending);
        let finished = matches!(event, Event::Reply(_));
        if let Some(sender) = pending.get(&id) {
            let _ = sender.send(event);
        }
        if finished {
            pending.remove(&id);
        }
    }
    link.closed.store(true, Ordering::Relaxed);
    lock(&link.pending).clear();
}

/// The agent's name as its client gave it at initialisation.
fn client_name(context: &RequestContext<RoleServer>) -> String {
    context
        .peer
        .peer_info()
        .map(|info| info.client_info.name.clone())
        .or_else(|| context.meta.client_info().map(|info| info.name))
        .unwrap_or_else(|| "an AI agent".to_owned())
}

/// Locks a mutex, carrying on after a panic elsewhere left it poisoned.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}
