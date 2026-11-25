//! The MCP side: tools, resources and prompts, each a thin shell around a
//! bridge call and a function of [`crate::shape`].

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use blastr_model::bridge::{Answer, Call, ShowTarget, Status};
use blastr_model::wording::short_name;
use blastr_model::{Analysis, Assume, ChangeKind, Confidence, ImpactClass, RevSpec, SymbolPath};
use rmcp::handler::server::router::tool::ToolRouter;
use rmcp::handler::server::wrapper::Parameters;
use rmcp::model::{
    CallToolResult, ContentBlock, GetPromptRequestParams, GetPromptResponse, GetPromptResult,
    Implementation, ListPromptsResult, ListResourcesResult, PaginatedRequestParams, Prompt,
    PromptArgument, PromptMessage, ReadResourceRequestParams, ReadResourceResponse,
    ReadResourceResult, Resource, ResourceContents, ResourceUpdatedNotificationParam, Role,
    ServerCapabilities, ServerConfig, SubscribeRequestParams, SubscriptionFilter,
    UnsubscribeRequestParams,
};
use rmcp::schemars::JsonSchema;
use rmcp::service::{RequestContext, SubscriptionContext};
use rmcp::{ErrorData, RoleServer, ServerHandler, tool, tool_handler, tool_router};
use serde::Deserialize;
use tokio::sync::broadcast::error::RecvError;
use tokio::task::JoinHandle;

use crate::shape::{self, ImpactFilter, Shaped};
use crate::window::{STILL_WORKING, Window};

/// The guide: what Blastr is, its concepts, workflows and tools.
const GUIDE: &str = include_str!("guide.md");

/// The short guide clients put in the agent's context at initialisation.
const INSTRUCTIONS: &str = "Blastr shows what a Rust code change can reach, and why: it compares \
two revisions of a Cargo project with rust-analyzer and lists the code that needs editing (it \
refers to something whose shape changed) and the code that may behave differently (it runs \
through changed code), each with evidence paths, plus the tests to run. It runs locally in the \
Blastr window, which must be open. Start with `status` or `get_analysis`; `list_changes` and \
`list_impact` page through the changes and the affected items. After files are saved, \
`get_analysis` returns the refreshed result; a call that answers \"still working\" is \
finished by calling `get_analysis` again. Call `guide` for details.";

/// The resource holding the guide.
const GUIDE_URI: &str = "blastr://guide";
/// The resource holding the window's status.
const STATUS_URI: &str = "blastr://status";
/// The resource holding the summary of the current analysis.
const ANALYSIS_URI: &str = "blastr://analysis";

/// How many items a page lists unless asked otherwise.
const DEFAULT_PAGE: u32 = 25;
/// The most items one page lists.
const MAX_PAGE: u32 = 100;

/// The MCP server: one per client.
#[derive(Clone)]
pub struct Server {
    /// The connection to the window.
    window: Arc<Window>,
    /// The tools.
    tool_router: ToolRouter<Server>,
    /// The task forwarding changes of the analysis to a client subscribed to
    /// `blastr://analysis` with `resources/subscribe`.
    subscription: Arc<Mutex<Option<JoinHandle<()>>>>,
}

/// Parameters of `open_repository`.
#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct OpenParams {
    /// Absolute path of the repository, or of any folder inside it.
    path: PathBuf,
}

/// Parameters of `compare`.
#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct CompareParams {
    /// What to compare: `worktree` (HEAD against the working tree, including unsaved-to-git
    /// edits on disk), `staged` (HEAD against the index), a revision such as `HEAD~1` or a commit
    /// hash (that commit against its parent), or a range such as `main..HEAD`.
    spec: String,
}

/// Parameters of `set_options`.
#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct OptionsParams {
    /// Cargo features to enable; replaces the current list. Omit to keep it.
    features: Option<Vec<String>>,
    /// Enable every Cargo feature. Omit to keep the current choice.
    all_features: Option<bool>,
    /// Target triple to analyse for, such as `x86_64-unknown-linux-gnu`; an empty string means
    /// the host. Omit to keep it.
    target: Option<String>,
    /// Glob patterns of files whose items are never listed, such as `benches/**`; replaces the
    /// current list. Omit to keep it.
    ignore: Option<Vec<String>>,
}

/// The two classes of affected items.
#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "kebab-case")]
enum ClassParam {
    /// Code that refers to something whose shape changed.
    NeedsEditing,
    /// Code that still compiles but runs through changed code.
    MayBehaveDifferently,
}

/// How certain an item's best path is.
#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "kebab-case")]
enum ConfidenceParam {
    /// Any item, including those reached through a candidate step.
    Possible,
    /// Likely or definite items.
    Likely,
    /// Only items reached through resolved steps.
    Definite,
}

/// Parameters of `list_impact`.
#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct ListParams {
    /// Only items reached from this change, by its number in `get_analysis`.
    change: Option<u32>,
    /// Only items of this class.
    class: Option<ClassParam>,
    /// Only items at least this confident.
    min_confidence: Option<ConfidenceParam>,
    /// How many matching items to skip; default 0.
    offset: Option<u32>,
    /// How many items to list; default 25, at most 100.
    limit: Option<u32>,
}

/// Parameters of `list_changes`.
#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct ChangesParams {
    /// Only changes of this kind: `body`, `signature`, `visibility`, `added`, `removed`,
    /// `renamed`, `field`, `variant`, `trait-item`, `impl`, `const-value`, `type-alias`,
    /// `macro-def`, `attribute`, `manifest` or `non-semantic`.
    kind: Option<String>,
    /// Only changes in files whose path contains this text, such as `src/lib.rs` or
    /// `crates/core/`.
    file: Option<String>,
    /// How many matching changes to skip; default 0.
    offset: Option<u32>,
    /// How many changes to list; default 25, at most 100.
    limit: Option<u32>,
}

/// Parameters of `explain_impact`.
#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct ExplainParams {
    /// The affected item or changed symbol, by its path (`crate::module::Type::method`) or a
    /// unique suffix of it (`Type::method`).
    symbol: String,
}

/// Parameters of `tests_to_run`.
#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct TestsParams {
    /// Only the tests of this change, by its number in `get_analysis`. Omit for all.
    change: Option<u32>,
}

/// Parameters of `find_symbol`.
#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct FindParams {
    /// A name or part of one, such as `total` or `Invoice::total`.
    query: String,
    /// How many symbols to list; default 25, at most 100.
    limit: Option<u32>,
}

/// The change `what_if` assumes.
#[derive(Clone, Copy, Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
#[serde(rename_all = "kebab-case")]
enum AssumeParam {
    /// The body or value changes; callers may behave differently.
    Body,
    /// The signature or type changes; users need editing.
    Signature,
    /// The item is removed.
    Removed,
    /// The item becomes private.
    Visibility,
    /// The field is removed.
    FieldRemoved,
    /// The field changes its type.
    FieldRetyped,
    /// The struct gains a field.
    FieldAdded,
    /// The enum gains a variant.
    VariantAdded,
    /// The variant is removed.
    VariantRemoved,
}

/// Parameters of `what_if`.
#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct WhatIfParams {
    /// The symbol, by its path (`crate::module::Type::method`) or a unique suffix of it.
    symbol: String,
    /// The change to assume.
    assume: AssumeParam,
}

/// Parameters of `show`.
#[derive(Deserialize, JsonSchema)]
#[schemars(crate = "rmcp::schemars")]
struct ShowParams {
    /// A symbol to select, by its path or a unique suffix of it: an affected item, a changed
    /// symbol or any symbol of the project. Give this or `change`.
    symbol: Option<String>,
    /// A change to select, by its number in `get_analysis`. Give this or `symbol`.
    change: Option<u32>,
}

impl Server {
    /// A server asking the window that listens on `socket`.
    pub fn new(socket: PathBuf) -> Server {
        Server {
            window: Arc::new(Window::new(socket)),
            tool_router: Server::tool_router(),
            subscription: Arc::new(Mutex::new(None)),
        }
    }

    /// Asks for something the window answers with its status.
    async fn status_of(
        &self,
        context: &RequestContext<RoleServer>,
        call: Call,
    ) -> Result<Status, String> {
        match self.window.call(context, call).await? {
            Answer::Status(status) => Ok(status),
            other => Err(unexpected(&other)),
        }
    }

    /// The window's status and result, waiting for a run in progress.
    async fn analysis(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<(Status, Option<Box<Analysis>>), String> {
        match self.window.call(context, Call::Analysis).await? {
            Answer::Analysis { status, analysis } => Ok((status, analysis)),
            other => Err(unexpected(&other)),
        }
    }

    /// Like [`Server::analysis`], but a missing result is an error.
    async fn current(
        &self,
        context: &RequestContext<RoleServer>,
    ) -> Result<(Status, Box<Analysis>), String> {
        let (status, analysis) = self.analysis(context).await?;
        match analysis {
            Some(analysis) => Ok((status, analysis)),
            None => Err(shape::summary(&status, None).text),
        }
    }

    /// Finds a symbol of the whole project by path or suffix, through the
    /// window's symbol search.
    async fn find_anywhere(
        &self,
        context: &RequestContext<RoleServer>,
        text: &str,
    ) -> Result<SymbolPath, String> {
        let name = text.rsplit("::").next().unwrap_or(text).trim();
        let call = Call::FindSymbol {
            query: name.to_owned(),
            limit: 200,
        };
        let hits = match self.window.call(context, call).await? {
            Answer::Symbols { hits } => hits,
            other => return Err(unexpected(&other)),
        };
        let found = matching(text, hits.iter().map(|hit| &hit.symbol));
        pick(text, found)?.ok_or_else(|| {
            format!(
                "No symbol `{text}` was found.{} `find_symbol` lists symbols by name.",
                suggestions(text, hits.iter().map(|hit| &hit.symbol))
            )
        })
    }
}

#[tool_router]
impl Server {
    /// Explains Blastr and this server: what it answers, the two classes of affected code,
    /// confidence, evidence paths, limits, the usual workflows and what each tool is for. Call
    /// it first.
    #[tool(
        name = "guide",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn guide(&self) -> CallToolResult {
        done(Shaped {
            text: GUIDE.to_owned(),
            data: serde_json::json!({ "guide": GUIDE }),
        })
    }

    /// Whether Blastr is open, and what it shows: the repository and branch, the comparison,
    /// the analysis options, a what-if if one is shown, and whether a run is in progress.
    #[tool(
        name = "status",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn status(&self, context: RequestContext<RoleServer>) -> CallToolResult {
        finish(
            self.status_of(&context, Call::Status)
                .await
                .map(|status| shape::status(&status)),
        )
    }

    /// Opens a repository in the Blastr window. If Blastr has not been told whether to trust
    /// it, the user is asked in the window and this waits for the answer.
    #[tool(
        name = "open_repository",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn open_repository(
        &self,
        Parameters(params): Parameters<OpenParams>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let call = Call::Open { path: params.path };
        finish(
            self.status_of(&context, call)
                .await
                .map(|status| shape::status(&status)),
        )
    }

    /// Chooses what the window compares, waits for the analysis and returns its summary (as
    /// `get_analysis`). Leaves a what-if if one is shown.
    #[tool(
        name = "compare",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn compare(
        &self,
        Parameters(params): Parameters<CompareParams>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let result = async {
            let spec = rev_spec(&params.spec)?;
            let before = self.status_of(&context, Call::Status).await?;
            if before.what_if.is_some() {
                self.status_of(&context, Call::LeaveWhatIf).await?;
            }
            let status = self.status_of(&context, Call::Compare { spec }).await?;
            if let Some(failure) = status.failure {
                return Err(format!("The comparison failed: {failure}"));
            }
            let (status, analysis) = self.analysis(&context).await?;
            Ok(shape::summary(&status, analysis.as_deref()))
        };
        finish(result.await)
    }

    /// Changes the open repository's analysis options, as Blastr's settings do, and runs the
    /// comparison again. Options left out keep their value.
    #[tool(
        name = "set_options",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn set_options(
        &self,
        Parameters(params): Parameters<OptionsParams>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let call = Call::SetOptions {
            features: params.features,
            all_features: params.all_features,
            target: params.target,
            ignore: params.ignore,
        };
        finish(
            self.status_of(&context, call)
                .await
                .map(|status| shape::status(&status)),
        )
    }

    /// The current result: the comparison, the numbered changes, counts per class and
    /// confidence, the tests summary, the top affected items and the limits. While a run is in
    /// progress (a comparison, or a refresh after files were saved) it waits for it, so a call
    /// made after saving sees the saved code.
    #[tool(
        name = "get_analysis",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn get_analysis(&self, context: RequestContext<RoleServer>) -> CallToolResult {
        finish(
            self.analysis(&context)
                .await
                .map(|(status, analysis)| shape::summary(&status, analysis.as_deref())),
        )
    }

    /// Affected items, best first (needs editing before may behave differently, then by
    /// confidence and distance), paged, each with its one-line reason and location. Filter by
    /// change, class or confidence.
    #[tool(
        name = "list_impact",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_impact(
        &self,
        Parameters(params): Parameters<ListParams>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let result = self.current(&context).await.map(|(status, analysis)| {
            let filter = ImpactFilter {
                change: params.change,
                class: params.class.map(|class| match class {
                    ClassParam::NeedsEditing => ImpactClass::Adaptation,
                    ClassParam::MayBehaveDifferently => ImpactClass::Behaviour,
                }),
                min_confidence: params.min_confidence.map(|level| match level {
                    ConfidenceParam::Possible => Confidence::Possible,
                    ConfidenceParam::Likely => Confidence::Likely,
                    ConfidenceParam::Definite => Confidence::Definite,
                }),
            };
            let offset = params.offset.unwrap_or(0) as usize;
            shape::impact_list(&status, &analysis, &filter, offset, page_size(params.limit))
        });
        finish(result)
    }

    /// The changes, numbered as in `get_analysis`, paged, each with its kind, symbol and
    /// file:line. Filter by kind or file.
    #[tool(
        name = "list_changes",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn list_changes(
        &self,
        Parameters(params): Parameters<ChangesParams>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let result = async {
            let kind = params.kind.as_deref().map(str::trim);
            if let Some(kind) = kind
                && !ChangeKind::ALL.iter().any(|known| known.as_str() == kind)
            {
                let known: Vec<&str> = ChangeKind::ALL.iter().map(|kind| kind.as_str()).collect();
                return Err(format!(
                    "`{kind}` is not a change kind; use one of {}.",
                    known.join(", ")
                ));
            }
            let (status, analysis) = self.current(&context).await?;
            let offset = params.offset.unwrap_or(0) as usize;
            Ok(shape::change_list(
                &status,
                &analysis,
                kind,
                params.file.as_deref(),
                offset,
                page_size(params.limit),
            ))
        };
        finish(result.await)
    }

    /// Why one affected item is listed: its class, confidence and distance, a sentence
    /// explaining it, up to three evidence paths with every step's file:line, and the selected
    /// tests that reach it. For a changed symbol: what the change is, where, what it reaches
    /// and the tests selected for it.
    #[tool(
        name = "explain_impact",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn explain_impact(
        &self,
        Parameters(params): Parameters<ExplainParams>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let result = async {
            let (status, analysis) = self.current(&context).await?;
            let text = params.symbol.trim();
            let found = matching(text, analysis.items.iter().map(|item| &item.symbol));
            if let Some(symbol) = pick(text, found)? {
                let item = analysis
                    .items
                    .iter()
                    .find(|item| item.symbol == symbol)
                    .ok_or_else(|| format!("`{text}` is not an affected item."))?;
                return Ok(shape::explain(&status, &analysis, item));
            }
            let changes = analysis.diff.changes.iter().map(|change| &change.symbol);
            if let Some(symbol) = pick(text, matching(text, changes))? {
                let indices: Vec<usize> = analysis
                    .diff
                    .changes
                    .iter()
                    .enumerate()
                    .filter(|(_, change)| change.symbol == symbol)
                    .map(|(index, _)| index)
                    .collect();
                return Ok(shape::explain_changes(&status, &analysis, &indices));
            }
            let items = analysis.items.iter().map(|item| &item.symbol);
            let changes = analysis.diff.changes.iter().map(|change| &change.symbol);
            Err(format!(
                "`{text}` is neither an affected item nor a changed symbol of this comparison.{} \
                 `find_symbol` looks a name up.",
                suggestions(text, items.chain(changes))
            ))
        };
        finish(result.await)
    }

    /// The tests worth running: `cargo test` commands per target, and the selected tests with
    /// why each was selected (it changed, or a change reaches it). Optionally for one change.
    #[tool(
        name = "tests_to_run",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn tests_to_run(
        &self,
        Parameters(params): Parameters<TestsParams>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        finish(
            self.current(&context)
                .await
                .map(|(status, analysis)| shape::tests(&status, &analysis, params.change)),
        )
    }

    /// Symbols of the open repository whose name matches, best first, with their kind and
    /// location. Use it to find the exact path to give other tools.
    #[tool(
        name = "find_symbol",
        annotations(read_only_hint = true, open_world_hint = false)
    )]
    async fn find_symbol(
        &self,
        Parameters(params): Parameters<FindParams>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let call = Call::FindSymbol {
            query: params.query.clone(),
            limit: page_size(params.limit) as u32,
        };
        let result = match self.window.call(&context, call).await {
            Ok(Answer::Symbols { hits }) => Ok(shape::symbols(&params.query, &hits)),
            Ok(other) => Err(unexpected(&other)),
            Err(error) => Err(error),
        };
        finish(result)
    }

    /// Traces what an assumed change of one symbol would reach in the working tree, without
    /// touching any file, and returns the summary (as `get_analysis`). The window shows the
    /// what-if until `compare` is called.
    #[tool(
        name = "what_if",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn what_if(
        &self,
        Parameters(params): Parameters<WhatIfParams>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let result = async {
            let symbol = self.find_anywhere(&context, params.symbol.trim()).await?;
            let call = Call::WhatIf {
                symbol,
                assume: assume(params.assume),
            };
            match self.window.call(&context, call).await? {
                Answer::Analysis { status, analysis } => {
                    Ok(shape::summary(&status, analysis.as_deref()))
                }
                other => Err(unexpected(&other)),
            }
        };
        finish(result.await)
    }

    /// Selects a change or a symbol in the Blastr window (its list entry, graph, code and
    /// evidence), to point the user at what is being discussed.
    #[tool(
        name = "show",
        annotations(
            read_only_hint = false,
            destructive_hint = false,
            idempotent_hint = true,
            open_world_hint = false
        )
    )]
    async fn show(
        &self,
        Parameters(params): Parameters<ShowParams>,
        context: RequestContext<RoleServer>,
    ) -> CallToolResult {
        let result = async {
            let (target, label) = match (params.symbol, params.change) {
                (None, Some(change)) => (ShowTarget::Change(change), format!("change #{change}")),
                (Some(text), None) => {
                    let text = text.trim();
                    let (_, analysis) = self.analysis(&context).await?;
                    let known = analysis.as_deref().map(|analysis| {
                        let items = analysis.items.iter().map(|item| &item.symbol);
                        let changes = analysis.diff.changes.iter().map(|change| &change.symbol);
                        matching(text, items.chain(changes))
                    });
                    let symbol = match pick(text, known.unwrap_or_default())? {
                        Some(symbol) => symbol,
                        None => self.find_anywhere(&context, text).await?,
                    };
                    let label = symbol.path.clone();
                    (ShowTarget::Symbol(symbol), label)
                }
                _ => return Err("Give either `symbol` or `change`, not both.".to_owned()),
            };
            match self.window.call(&context, Call::Show { target }).await? {
                Answer::Done => Ok(Shaped {
                    text: format!("Shown in the Blastr window: {label}."),
                    data: serde_json::json!({ "shown": label }),
                }),
                other => Err(unexpected(&other)),
            }
        };
        finish(result.await)
    }
}

#[tool_handler(router = self.tool_router)]
impl ServerHandler for Server {
    fn get_info(&self) -> ServerConfig {
        ServerConfig::new(
            ServerCapabilities::builder()
                .enable_prompts()
                .enable_resources()
                .enable_resources_subscribe()
                .enable_tools()
                .build(),
        )
        .with_server_info(Implementation::new("blastr", blastr_model::VERSION))
        .with_instructions(INSTRUCTIONS)
    }

    async fn list_resources(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListResourcesResult, ErrorData> {
        Ok(ListResourcesResult::with_all_items(vec![
            Resource::new(GUIDE_URI, "guide")
                .with_description("What Blastr is, its concepts, workflows and tools.")
                .with_mime_type("text/markdown"),
            Resource::new(STATUS_URI, "status")
                .with_description("What the Blastr window has open and shows.")
                .with_mime_type("application/json"),
            Resource::new(ANALYSIS_URI, "analysis")
                .with_description(
                    "Summary of the current analysis: comparison, changes, counts, tests, top \
                     items, limits. Subscribe to hear when it changes.",
                )
                .with_mime_type("application/json"),
        ]))
    }

    async fn read_resource(
        &self,
        request: ReadResourceRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<ReadResourceResponse, ErrorData> {
        let (text, mime) = match request.uri.as_str() {
            GUIDE_URI => (GUIDE.to_owned(), "text/markdown"),
            STATUS_URI => {
                let status = self
                    .status_of(&context, Call::Status)
                    .await
                    .map_err(|error| ErrorData::internal_error(error, None))?;
                (shape::status(&status).data.to_string(), "application/json")
            }
            ANALYSIS_URI => {
                let (status, analysis) = self
                    .analysis(&context)
                    .await
                    .map_err(|error| ErrorData::internal_error(error, None))?;
                let summary = shape::summary(&status, analysis.as_deref());
                (summary.data.to_string(), "application/json")
            }
            other => {
                return Err(ErrorData::resource_not_found(
                    format!("no resource {other}"),
                    None,
                ));
            }
        };
        let contents = ResourceContents::text(text, request.uri).with_mime_type(mime);
        Ok(ReadResourceResult::new(vec![contents]).into())
    }

    #[allow(deprecated)]
    async fn subscribe(
        &self,
        request: SubscribeRequestParams,
        context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        if request.uri != ANALYSIS_URI {
            return Err(ErrorData::invalid_params(
                format!("only {ANALYSIS_URI} can be subscribed to"),
                None,
            ));
        }
        // Connect now if possible, so the window's changes start arriving.
        let _ = self.window.call(&context, Call::Status).await;
        let mut changes = self.window.changes();
        let peer = context.peer.clone();
        let mut slot = self
            .subscription
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if slot.is_none() {
            *slot = Some(tokio::spawn(async move {
                while let Ok(()) | Err(RecvError::Lagged(_)) = changes.recv().await {
                    let update = ResourceUpdatedNotificationParam::new(ANALYSIS_URI);
                    if peer.notify_resource_updated(update).await.is_err() {
                        break;
                    }
                }
            }));
        }
        Ok(())
    }

    #[allow(deprecated)]
    async fn unsubscribe(
        &self,
        _request: UnsubscribeRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<(), ErrorData> {
        let task = self
            .subscription
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .take();
        if let Some(task) = task {
            task.abort();
        }
        Ok(())
    }

    fn accepted_subscription_filter(
        &self,
        requested: &SubscriptionFilter,
    ) -> Option<SubscriptionFilter> {
        Some(requested.supported_by(&self.get_info().capabilities))
    }

    async fn listen(&self, context: SubscriptionContext) -> Result<(), ErrorData> {
        let _ = self
            .window
            .call(context.request_context(), Call::Status)
            .await;
        let mut changes = self.window.changes();
        loop {
            tokio::select! {
                () = context.cancelled() => return Ok(()),
                change = changes.recv() => match change {
                    Ok(()) | Err(RecvError::Lagged(_)) => {
                        // The sink drops updates of resources the client did not ask for.
                        let _ = context.sink().notify_resource_updated(ANALYSIS_URI).await;
                    }
                    Err(RecvError::Closed) => return Ok(()),
                },
            }
        }
    }

    async fn list_prompts(
        &self,
        _request: Option<PaginatedRequestParams>,
        _context: RequestContext<RoleServer>,
    ) -> Result<ListPromptsResult, ErrorData> {
        Ok(ListPromptsResult::with_all_items(vec![
            Prompt::new(
                "review-changes",
                Some(
                    "Review the current comparison with Blastr: what needs editing, what may behave differently, which tests to run.",
                ),
                None,
            ),
            Prompt::new(
                "before-changing",
                Some("Trace with Blastr what changing a symbol would reach, before changing it."),
                Some(vec![
                    PromptArgument::new("symbol")
                        .with_description(
                            "The symbol, by its path, such as `crate::module::Type::method`.",
                        )
                        .with_required(true),
                ]),
            ),
            Prompt::new(
                "check-my-edit",
                Some(
                    "Check with Blastr what the edits in the working tree reach and which tests cover them.",
                ),
                None,
            ),
        ]))
    }

    async fn get_prompt(
        &self,
        request: GetPromptRequestParams,
        _context: RequestContext<RoleServer>,
    ) -> Result<GetPromptResponse, ErrorData> {
        let text = match request.name.as_str() {
            "review-changes" => REVIEW_CHANGES.to_owned(),
            "before-changing" => {
                let symbol = request
                    .arguments
                    .as_ref()
                    .and_then(|arguments| arguments.get("symbol"))
                    .and_then(|symbol| symbol.as_str())
                    .ok_or_else(|| ErrorData::invalid_params("`symbol` is required", None))?;
                BEFORE_CHANGING.replace("{symbol}", symbol)
            }
            "check-my-edit" => CHECK_MY_EDIT.to_owned(),
            other => {
                return Err(ErrorData::invalid_params(
                    format!("no prompt {other}"),
                    None,
                ));
            }
        };
        Ok(GetPromptResult::new(vec![PromptMessage::new_text(Role::User, text)]).into())
    }
}

/// The `review-changes` prompt.
const REVIEW_CHANGES: &str = "Review the change Blastr is comparing. Call the Blastr tool \
`get_analysis`; use `list_impact` with class `needs-editing` and `explain_impact` on the items \
that matter; then `tests_to_run`. Report what changed, what needs editing and why (with \
file:line), what may behave differently, the tests to run, and any limits that weaken the \
result.";

/// The `before-changing` prompt; `{symbol}` is replaced.
const BEFORE_CHANGING: &str = "Before changing `{symbol}`, trace what the change would reach. \
Call the Blastr tool `what_if` with symbol `{symbol}` and the assumption that matches the \
planned change (signature, body, removed, visibility, or a field or variant change). Summarise \
what would need editing, what may behave differently, and which tests cover it (`tests_to_run`), \
with the limits that matter. Afterwards call `compare` with `worktree` so the window shows the \
working tree again.";

/// The `check-my-edit` prompt.
const CHECK_MY_EDIT: &str = "Check what the edits in the working tree reach. Call the Blastr \
tool `status`; unless the working tree is already compared, call `compare` with `worktree`. Then \
call `get_analysis`, `explain_impact` for the items that need editing, and `tests_to_run`. Report \
what the edit reaches, what still needs editing, and which tests cover it.";

/// A tool result from an answer or an error in words. A call that stopped
/// waiting for the window is not an error: the agent is told to ask again.
fn finish(result: Result<Shaped, String>) -> CallToolResult {
    match result {
        Ok(shaped) => done(shaped),
        Err(error) if error.starts_with(STILL_WORKING) => done(Shaped {
            data: serde_json::json!({ "still_working": error }),
            text: error,
        }),
        Err(error) => CallToolResult::error(vec![ContentBlock::text(error)]),
    }
}

/// A successful tool result: the text, and the data as structured content.
fn done(shaped: Shaped) -> CallToolResult {
    let mut result = CallToolResult::success(vec![ContentBlock::text(shaped.text)]);
    result.structured_content = Some(shaped.data);
    result
}

/// The error for an answer that does not fit the call.
fn unexpected(answer: &Answer) -> String {
    format!("The Blastr window gave an unexpected answer: {answer:?}")
}

/// The page size asked for, within bounds.
fn page_size(limit: Option<u32>) -> usize {
    limit.unwrap_or(DEFAULT_PAGE).clamp(1, MAX_PAGE) as usize
}

/// Reads what `compare` accepts: `worktree`, `staged`, a revision or a range.
fn rev_spec(text: &str) -> Result<RevSpec, String> {
    let text = text.trim();
    let spec = match text.to_ascii_lowercase().as_str() {
        "" => return Err("Give a comparison: `worktree`, `staged`, a revision or a range.".into()),
        "worktree" | "working tree" | "working-tree" => RevSpec {
            worktree: true,
            ..RevSpec::default()
        },
        "staged" | "index" => RevSpec {
            staged: true,
            ..RevSpec::default()
        },
        _ => RevSpec {
            rev: Some(text.to_owned()),
            ..RevSpec::default()
        },
    };
    Ok(spec)
}

/// The model's assumption for the parameter.
fn assume(param: AssumeParam) -> Assume {
    match param {
        AssumeParam::Body => Assume::Body,
        AssumeParam::Signature => Assume::Signature,
        AssumeParam::Removed => Assume::Removed,
        AssumeParam::Visibility => Assume::Visibility,
        AssumeParam::FieldRemoved => Assume::FieldRemoved,
        AssumeParam::FieldRetyped => Assume::FieldRetyped,
        AssumeParam::FieldAdded => Assume::FieldAdded,
        AssumeParam::VariantAdded => Assume::VariantAdded,
        AssumeParam::VariantRemoved => Assume::VariantRemoved,
    }
}

/// The candidates `text` names: those whose path is `text`, or else those
/// whose path ends with it or whose short name is it. No duplicates.
fn matching<'a>(text: &str, candidates: impl Iterator<Item = &'a SymbolPath>) -> Vec<SymbolPath> {
    let candidates: Vec<&SymbolPath> = candidates.collect();
    let suffix = format!("::{text}");
    let exact: Vec<&SymbolPath> = candidates
        .iter()
        .copied()
        .filter(|symbol| symbol.path == text)
        .collect();
    let chosen = if exact.is_empty() {
        candidates
            .into_iter()
            .filter(|symbol| {
                symbol.path.ends_with(&suffix) || short_name(&symbol.path).name == text
            })
            .collect()
    } else {
        exact
    };
    let mut unique: Vec<SymbolPath> = Vec::new();
    for symbol in chosen {
        if !unique.contains(symbol) {
            unique.push(symbol.clone());
        }
    }
    unique
}

/// Up to five paths among `candidates` that contain the last `::` segment of
/// `text`, ignoring case, as a sentence starting with a space; empty when
/// there are none.
fn suggestions<'a>(text: &str, candidates: impl Iterator<Item = &'a SymbolPath>) -> String {
    let name = text
        .rsplit("::")
        .next()
        .unwrap_or(text)
        .trim()
        .to_lowercase();
    if name.is_empty() {
        return String::new();
    }
    let mut found: Vec<&str> = Vec::new();
    for symbol in candidates {
        if found.len() == 5 {
            break;
        }
        if symbol.path.to_lowercase().contains(&name) && !found.contains(&symbol.path.as_str()) {
            found.push(&symbol.path);
        }
    }
    if found.is_empty() {
        return String::new();
    }
    format!(" Did you mean: {}?", found.join(", "))
}

/// The one symbol of `found`, `None` when there is none, or an error listing
/// them when `text` names several paths.
fn pick(text: &str, found: Vec<SymbolPath>) -> Result<Option<SymbolPath>, String> {
    let Some(first) = found.first() else {
        return Ok(None);
    };
    if found.iter().all(|symbol| symbol.path == first.path) {
        return Ok(Some(first.clone()));
    }
    let mut message = format!("`{text}` names several symbols; give one of these paths:");
    for symbol in found.iter().take(20) {
        message.push_str(&format!("\n- {} ({})", symbol.path, symbol.kind));
    }
    if found.len() > 20 {
        message.push_str(&format!("\n- … {} more", found.len() - 20));
    }
    Err(message)
}
