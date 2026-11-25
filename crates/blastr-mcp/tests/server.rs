//! The MCP server against an MCP client and a stub window.
//!
//! The stub listens on a socket of its own and answers bridge calls with a
//! small canned analysis; the client talks to the server over an in-memory
//! pipe, as an agent would over standard input and output.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU32, Ordering};
use std::sync::{Arc, Mutex};

use blastr_mcp::{NOT_RUNNING, Server};
use blastr_model::bridge::{Answer, BRIDGE_VERSION, Call, FromWindow, Status, ToWindow};
use blastr_model::protocol::Report;
use blastr_model::{
    Analysis, AnalysisParams, Certainty, Change, ChangeDetail, ChangeKind, Comparison, Confidence,
    DiffResult, EdgeDetail, EdgeKind, EvidencePath, EvidenceStep, ImpactClass, ImpactItem,
    ImpactLimits, ImpactTags, Location, Reason, RelPath, Source, Span, SymbolKind, SymbolPath,
    TestSelection,
};
use interprocess::local_socket::tokio::prelude::*;
use interprocess::local_socket::{GenericFilePath, ListenerOptions};
use rmcp::model::{
    CallToolRequestParams, CallToolResult, ClientCapabilities, ClientConfig, Implementation,
};
use rmcp::service::RunningService;
use rmcp::{ClientHandler, RoleClient, ServiceExt};
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

/// The client, named so that the stub can check the greeting.
#[derive(Clone)]
struct Agent;

impl ClientHandler for Agent {
    fn get_info(&self) -> ClientConfig {
        ClientConfig::new(
            ClientCapabilities::default(),
            Implementation::new("test-agent", "1.0"),
        )
    }
}

/// A socket name no other test uses.
fn socket() -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let name = format!(
        "blastr-mcp-test-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    if cfg!(windows) {
        PathBuf::from(format!(r"\\.\pipe\{name}"))
    } else {
        std::env::temp_dir().join(format!("{name}.sock"))
    }
}

/// Starts a server on `socket` and connects a client to it.
async fn connect(socket: PathBuf) -> RunningService<RoleClient, Agent> {
    let (server_side, client_side) = tokio::io::duplex(1 << 16);
    tokio::spawn(async move {
        if let Ok(running) = Server::new(socket).serve(server_side).await {
            let _ = running.waiting().await;
        }
    });
    Agent.serve(client_side).await.expect("the client connects")
}

/// Calls a tool with JSON arguments.
async fn call(
    client: &RunningService<RoleClient, Agent>,
    tool: &'static str,
    arguments: Value,
) -> CallToolResult {
    let arguments = arguments.as_object().cloned().unwrap_or_default();
    client
        .call_tool(CallToolRequestParams::new(tool).with_arguments(arguments))
        .await
        .expect("the call is answered")
}

/// The text of a tool result.
fn text(result: &CallToolResult) -> String {
    result
        .content
        .iter()
        .filter_map(|block| block.as_text().map(|text| text.text.clone()))
        .collect()
}

/// A location in `src/lib.rs`.
fn at(line: u32) -> Location {
    Location {
        file: RelPath::new("src/lib.rs"),
        span: Span {
            start: 0,
            end: 1,
            start_line: line,
            end_line: line,
        },
    }
}

/// An item that calls the changed `billing::tax::vat_rate` from `line`.
fn caller(path: &str, class: ImpactClass, line: u32) -> ImpactItem {
    let symbol = SymbolPath::new(path, SymbolKind::Function);
    let step = EvidenceStep {
        from: symbol.clone(),
        to: SymbolPath::new("billing::tax::vat_rate", SymbolKind::Function),
        edge: EdgeKind::Calls,
        certainty: Certainty::Resolved,
        detail: EdgeDetail::None,
        site: at(line),
    };
    ImpactItem {
        symbol,
        location: at(line),
        class,
        confidence: Confidence::Definite,
        distance: 1,
        paths: vec![EvidencePath {
            change: 0,
            reason: Reason::SignatureChanged,
            class,
            steps: vec![step],
        }],
        path_count: 1,
        tags: ImpactTags::default(),
    }
}

/// One signature change reaching two callers.
fn analysis() -> Analysis {
    Analysis {
        diff: DiffResult {
            comparison: Comparison {
                base: Source::Tree {
                    id: "abc".into(),
                    label: "HEAD".into(),
                },
                head: Source::Worktree,
            },
            base_key: None,
            head_key: None,
            changed_files: Vec::new(),
            changes: vec![
                Change {
                    kind: ChangeKind::Signature,
                    symbol: SymbolPath::new("billing::tax::vat_rate", SymbolKind::Function),
                    old: Some(at(3)),
                    new: Some(at(3)),
                    detail: ChangeDetail::None,
                },
                Change {
                    kind: ChangeKind::Body,
                    symbol: SymbolPath::new("billing::invoice::render", SymbolKind::Function),
                    old: Some(at(30)),
                    new: Some(at(30)),
                    detail: ChangeDetail::None,
                },
            ],
            limits: Vec::new(),
        },
        items: vec![
            caller("billing::invoice::total", ImpactClass::Adaptation, 10),
            caller("billing::invoice::preview", ImpactClass::Adaptation, 20),
        ],
        tests: TestSelection::default(),
        limits: ImpactLimits {
            unresolved_total: 2,
            ..ImpactLimits::default()
        },
        params: AnalysisParams::default(),
    }
}

/// What the stub window shows.
fn status() -> Status {
    Status {
        repository: Some(PathBuf::from("/work/billing")),
        branch: Some("main".into()),
        comparison: Some(("HEAD".into(), "Working tree".into())),
        watching: true,
        ..Status::default()
    }
}

/// Listens on `socket` like the window, answering every call from the canned
/// analysis. Records the client name each greeting carries.
async fn stub_window(socket: &Path) -> Arc<Mutex<Vec<String>>> {
    let name = socket
        .to_fs_name::<GenericFilePath>()
        .expect("a socket name");
    let listener = ListenerOptions::new()
        .name(name)
        .create_tokio()
        .expect("the stub listens");
    let greeted = Arc::new(Mutex::new(Vec::new()));
    let names = greeted.clone();
    tokio::spawn(async move {
        loop {
            let Ok(stream) = listener.accept().await else {
                return;
            };
            let names = names.clone();
            tokio::spawn(async move {
                let (reader, mut writer) = tokio::io::split(stream);
                let mut lines = BufReader::new(reader).lines();
                while let Ok(Some(line)) = lines.next_line().await {
                    let replies = match serde_json::from_str::<ToWindow>(&line).expect("a message")
                    {
                        ToWindow::Hello { client, .. } => {
                            names.lock().expect("names").push(client);
                            vec![FromWindow::Hello {
                                version: BRIDGE_VERSION,
                                blastr: "test".into(),
                            }]
                        }
                        ToWindow::Call { id, call } => answer(id, call),
                        ToWindow::Cancel { .. } => Vec::new(),
                    };
                    for reply in replies {
                        let mut line = serde_json::to_string(&reply).expect("serialises");
                        line.push('\n');
                        writer.write_all(line.as_bytes()).await.expect("writes");
                    }
                }
            });
        }
    });
    greeted
}

/// The stub's messages for one call: a progress report for the analysis,
/// then the reply.
fn answer(id: u64, call: Call) -> Vec<FromWindow> {
    let result = match call {
        Call::Analysis => Answer::Analysis {
            status: status(),
            analysis: Some(Box::new(analysis())),
        },
        Call::FindSymbol { .. } => Answer::Symbols { hits: Vec::new() },
        Call::Show { .. } => Answer::Done,
        _ => Answer::Status(status()),
    };
    vec![
        FromWindow::Progress {
            id,
            report: Report {
                phase: "diff".into(),
                done: 1,
                total: None,
            },
        },
        FromWindow::Reply {
            id,
            result: Ok(result),
        },
    ]
}

#[tokio::test]
async fn lists_every_tool_and_explains_itself() {
    let client = connect(socket()).await;

    let info = client.peer_info().expect("the server introduced itself");
    let instructions = info.instructions.clone().unwrap_or_default();
    assert!(instructions.contains("Call `guide` for details."));

    let tools = client.list_all_tools().await.expect("tools are listed");
    let mut names: Vec<&str> = tools.iter().map(|tool| tool.name.as_ref()).collect();
    names.sort_unstable();
    assert_eq!(
        names,
        [
            "compare",
            "explain_impact",
            "find_symbol",
            "get_analysis",
            "guide",
            "list_changes",
            "list_impact",
            "open_repository",
            "set_options",
            "show",
            "status",
            "tests_to_run",
            "what_if",
        ]
    );
    for tool in &tools {
        assert_eq!(tool.input_schema.get("type"), Some(&json!("object")));
        assert!(
            tool.description
                .as_deref()
                .is_some_and(|text| !text.is_empty())
        );
    }
    let what_if = tools
        .iter()
        .find(|tool| tool.name == "what_if")
        .expect("what_if");
    let required = what_if.input_schema.get("required").expect("required");
    assert_eq!(required, &json!(["symbol", "assume"]));

    let guide = call(&client, "guide", json!({})).await;
    assert!(text(&guide).contains("*needs editing*"));
}

#[tokio::test]
async fn answers_from_the_window() {
    let socket = socket();
    let greeted = stub_window(&socket).await;
    let client = connect(socket).await;

    let status = call(&client, "status", json!({})).await;
    assert_eq!(status.is_error, Some(false));
    assert!(text(&status).contains("branch main"));
    assert_eq!(*greeted.lock().expect("names"), ["test-agent"]);

    let summary = call(&client, "get_analysis", json!({})).await;
    let data = summary.structured_content.clone().expect("structured");
    assert_eq!(data["counts"]["needs_editing"], 2);
    assert_eq!(data["changes"][0]["kind"], "signature");
    assert_eq!(data["limits"], json!(["2 unresolved references"]));
    assert!(text(&summary).contains("Comparison: HEAD → Working tree"));

    let page = call(&client, "list_impact", json!({ "limit": 1 })).await;
    let data = page.structured_content.clone().expect("structured");
    assert_eq!(data["total"], 2);
    assert_eq!(data["items"][0]["symbol"], "billing::invoice::total");
    assert_eq!(data["next_offset"], 1);
    let next = call(&client, "list_impact", json!({ "offset": 1, "limit": 1 })).await;
    let data = next.structured_content.clone().expect("structured");
    assert_eq!(data["items"][0]["symbol"], "billing::invoice::preview");
    assert_eq!(data["next_offset"], Value::Null);

    let explained = call(
        &client,
        "explain_impact",
        json!({ "symbol": "invoice::total" }),
    )
    .await;
    assert!(
        text(&explained).contains("calls vat_rate, whose signature changed"),
        "{}",
        text(&explained)
    );

    let changes = call(&client, "list_changes", json!({ "limit": 1 })).await;
    let data = changes.structured_content.clone().expect("structured");
    assert_eq!(data["total"], 2);
    assert_eq!(data["changes"][0]["index"], 0);
    assert_eq!(data["next_offset"], 1);
    let bodies = call(&client, "list_changes", json!({ "kind": "body" })).await;
    assert!(
        text(&bodies).contains("#1 body billing::invoice::render src/lib.rs:30"),
        "{}",
        text(&bodies)
    );
    let unknown = call(&client, "list_changes", json!({ "kind": "bodies" })).await;
    assert_eq!(unknown.is_error, Some(true));

    let changed = call(&client, "explain_impact", json!({ "symbol": "vat_rate" })).await;
    assert_eq!(changed.is_error, Some(false));
    let words = text(&changed);
    assert!(words.contains("Change #0: signature"), "{words}");
    assert!(words.contains("It reaches 2 needing editing"), "{words}");
    assert!(words.contains("No test reaches change #0."), "{words}");

    let missing = call(&client, "explain_impact", json!({ "symbol": "Total" })).await;
    assert_eq!(missing.is_error, Some(true));
    assert!(
        text(&missing).contains("Did you mean: billing::invoice::total?"),
        "{}",
        text(&missing)
    );

    let tests = call(&client, "tests_to_run", json!({ "change": 1 })).await;
    assert!(text(&tests).contains("No test reaches change #1."));

    // One connection serves every call.
    assert_eq!(greeted.lock().expect("names").len(), 1);
}

#[tokio::test]
async fn says_blastr_is_not_running_without_a_window() {
    let client = connect(socket()).await;
    let result = call(&client, "get_analysis", json!({})).await;
    assert_eq!(result.is_error, Some(true));
    assert_eq!(text(&result), NOT_RUNNING);
}
