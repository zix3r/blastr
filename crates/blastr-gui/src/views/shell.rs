//! The main window: title bar, three columns, status bar and the linking
//! between the views.
//!
//! Nothing here computes an analysis. A comparison is a command to the engine
//! thread; its progress and result come back as events and are folded into
//! [`Session`], which every view reads.

use gpui_kit::prelude::*;

use std::cell::{Cell, RefCell};
use std::path::PathBuf;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use blastr_core::wording;
use blastr_model::{Analysis, Assume, Change, ImpactItem, Location, Source, SymbolPath};
use gpui_kit::Focusable as _;
use gpui_kit::component::Disableable as _;
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::tooltip::Tooltip;
use gpui_kit::component::{
    FocusTrapElement as _, Icon, IconName, Sizable as _, TitleBar, h_flex, resizable::h_resizable,
    resizable::resizable_panel, resizable::v_resizable, v_flex,
};
use gpui_kit::{
    AnyElement, ClipboardItem, Context, Entity, FocusHandle, IntoElement, ParentElement, Pixels,
    Render, ScrollHandle, SharedString, Styled, Subscription, Task, WeakEntity, Window, div, point,
    px,
};

use super::settings::SettingsPanel;
use super::{code, diff, graph, left, list, ui};
use crate::editor::Detected;
use crate::settings::{Panes, Settings, WindowPlacement};
use crate::state::commits::CommitList;
use crate::state::loadable::{Loadable, Progress};
use crate::state::outline::{Filter, Outline};
use crate::state::search::{self, SymbolIndex};
use crate::state::selection::Selection;
use crate::state::session::{Compare, RepoInfo, Session, Stopped};
use crate::state::worker::{Command, EngineWorker, Event};
use crate::theme::{self, TEXT_MICRO, TEXT_SMALL, TEXT_UI};

gpui_kit::actions!(
    blastr,
    [
        OpenRepository,
        OpenPalette,
        Refresh,
        ShowGraph,
        ShowCode,
        ShowDiff,
        FocusFilter,
        Back,
        Forward,
        OpenEditor,
        OpenSettings,
        Dismiss,
        WhatIfBody,
        WhatIfSignature,
        WhatIfRemoved,
        WhatIfVisibility,
        WhatIfFieldRemoved,
        WhatIfFieldRetyped,
        WhatIfFieldAdded,
        WhatIfVariantAdded,
        WhatIfVariantRemoved,
    ]
);

/// What the centre shows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Tab {
    /// The impact graph.
    #[default]
    Graph,
    /// The selected symbol in its file.
    Code,
    /// The file before and after the change.
    Diff,
}

impl Tab {
    /// The tabs, left to right.
    pub(crate) const ALL: [Tab; 3] = [Tab::Graph, Tab::Code, Tab::Diff];

    /// The label of the tab.
    pub(crate) fn label(self) -> &'static str {
        match self {
            Tab::Graph => "Graph",
            Tab::Code => "Code",
            Tab::Diff => "Before / after",
        }
    }

    /// The icon of the tab.
    pub(crate) fn icon(self) -> IconName {
        match self {
            Tab::Graph => IconName::Network,
            Tab::Code => IconName::FileText,
            Tab::Diff => IconName::GalleryVerticalEnd,
        }
    }

    /// The key that shows the tab.
    pub(crate) fn key(self) -> &'static str {
        match self {
            Tab::Graph => "Ctrl 1",
            Tab::Code => "Ctrl 2",
            Tab::Diff => "Ctrl 3",
        }
    }
}

/// Which list the keyboard is working in.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Focus {
    /// The commits.
    Commits,
    /// The changes and impact blocks.
    #[default]
    Outline,
    /// The tests.
    Tests,
}

/// The key context every overlay layer declares.
///
/// The window binds its shortcuts in `!BlastrModal`: while focus is inside a
/// layer with this context, no shortcut of the window behind it matches. The
/// key context only follows the focused element, so the handlers of those
/// shortcuts also ask [`Shell::modal_open`]; together they cover the modal
/// that keeps no input of its own.
pub(crate) const MODAL_CONTEXT: &str = "BlastrModal";

/// Which overlay covers the window.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Modal {
    /// The symbol search.
    Palette,
    /// The one-time notice about build scripts.
    Trust,
    /// The settings panel.
    Settings,
}

/// The overlays that are open, as one value the window can ask about.
///
/// The order of the fields is the order the layers are drawn in, so the one
/// drawn last is the one on top: the settings panel covers the trust notice,
/// which covers the palette.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub(crate) struct Modals {
    /// The symbol palette is open.
    pub(crate) palette: bool,
    /// The trust notice is open.
    pub(crate) trust: bool,
    /// The settings panel is open.
    pub(crate) settings: bool,
}

impl Modals {
    /// Whether any overlay covers the window.
    pub(crate) fn any(self) -> bool {
        self.palette || self.trust || self.settings
    }

    /// The overlay that is on top, which is the one the eye sees.
    pub(crate) fn top(self) -> Option<Modal> {
        if self.settings {
            Some(Modal::Settings)
        } else if self.trust {
            Some(Modal::Trust)
        } else if self.palette {
            Some(Modal::Palette)
        } else {
            None
        }
    }

    /// The overlay `Esc` closes, if any.
    ///
    /// The trust notice asks a question about running the repository's own
    /// build scripts, and it has no answer that is safe to assume: `Esc`
    /// therefore leaves it open, and only the two buttons decide it. Every
    /// other overlay is closed, the one on top first.
    pub(crate) fn escape(self) -> Option<Modal> {
        match self.top() {
            Some(Modal::Trust) => self.palette.then_some(Modal::Palette),
            other => other,
        }
    }
}

/// The room the title bar gives its parts at a bar `width`.
#[derive(Debug, Clone, Copy, PartialEq)]
pub(crate) struct BarRoom {
    /// The width of the symbol search.
    pub(crate) search: Pixels,
    /// The width the chips left of the search may take: everything left of
    /// the search's half of the bar, less the padding around it.
    pub(crate) chips: Pixels,
    /// Whether there is room for the comparison chip beside the repository.
    pub(crate) comparison: bool,
}

impl BarRoom {
    /// The room at a bar `width`.
    ///
    /// The bar is the window less its window controls, which the platform
    /// draws at the right end and the window does not size.
    ///
    /// The search takes a share of the bar rather than a fixed width, so it
    /// gives way first as the window narrows; the chips get what remains on
    /// the left and ellipsise next. Narrower than a laptop window there is no
    /// room for the comparison chip, and the repository chip alone says what
    /// is open.
    pub(crate) fn at(width: f32) -> BarRoom {
        let search = (width * 0.26).clamp(240.0, 400.0);
        BarRoom {
            search: px(search),
            chips: px(((width - search) / 2.0 - 16.0).max(0.0)),
            comparison: width >= 1120.0,
        }
    }
}

/// The state of the start screen.
#[derive(Debug, Default)]
pub(crate) struct Start {
    /// A repository is being opened.
    pub(crate) opening: bool,
    /// Why the last attempt failed.
    pub(crate) error: Option<String>,
}

/// The symbol search palette.
#[derive(Debug, Default)]
pub(crate) struct Palette {
    /// The matches, best first.
    pub(crate) hits: Vec<search::Hit>,
    /// The index of the highlighted match.
    pub(crate) selected: usize,
}

/// The root view of the main window.
pub(crate) struct Shell {
    pub(crate) worker: EngineWorker,
    pub(crate) settings_path: PathBuf,
    pub(crate) settings: Settings,
    pub(crate) panes: Rc<RefCell<Panes>>,
    pub(crate) settings_dirty: Rc<Cell<bool>>,
    pub(crate) dirty_since: Option<Instant>,
    pub(crate) placement: Rc<Cell<Option<WindowPlacement>>>,

    pub(crate) session: Session,
    pub(crate) commits: CommitList,
    pub(crate) docs: super::doc::Docs,
    /// The editors found on this machine, as the editor thread reported them.
    pub(crate) editors: Vec<Detected>,
    /// Whether the search for editors has finished, so that the panel can say
    /// what it found instead of waiting forever.
    pub(crate) editors_known: bool,
    /// The settings panel while it is open.
    pub(crate) settings_panel: Option<SettingsPanel>,
    /// When a change in the panel should run the comparison again, once the
    /// typing in its fields has stopped.
    pub(crate) settings_rerun: Option<Instant>,

    pub(crate) filter: Filter,
    pub(crate) filter_input: Entity<InputState>,
    pub(crate) outline: Outline,
    pub(crate) outline_dirty: bool,
    pub(crate) tests_expanded: Vec<usize>,
    pub(crate) tests_rows: Vec<crate::state::outline::TestRow>,
    pub(crate) tests_dirty: bool,
    pub(crate) tests_cursor: Option<usize>,

    pub(crate) palette: Option<Palette>,
    pub(crate) palette_input: Entity<InputState>,
    pub(crate) search: SymbolIndex,

    pub(crate) tab: Tab,
    pub(crate) focus: Focus,
    pub(crate) evidence_path: usize,
    /// Whether the evidence panel lists every covering test.
    pub(crate) evidence_more_tests: bool,
    pub(crate) graph: graph::GraphState,
    pub(crate) code: code::CodeState,
    pub(crate) diff: diff::DiffState,

    pub(crate) start: Start,
    pub(crate) pending_trust: Option<PathBuf>,
    pub(crate) restore_compare: Option<Compare>,

    pub(crate) run_seen: u64,
    pub(crate) whatif_seen: u64,
    pub(crate) now: i64,
    pub(crate) commit_cursor: usize,
    pub(crate) outline_cursor: Option<usize>,
    pub(crate) toast: Option<(String, Instant)>,
    pub(crate) refocus: bool,
    /// Whether a failure card shows its whole message.
    pub(crate) failure_open: bool,

    pub(crate) outline_scroll: ScrollHandle,
    pub(crate) tests_scroll: ScrollHandle,
    pub(crate) commits_scroll: ScrollHandle,
    pub(crate) evidence_scroll: ScrollHandle,
    pub(crate) diff_scroll: ScrollHandle,
    pub(crate) diff_scroll_x: ScrollHandle,
    pub(crate) failure_scroll: ScrollHandle,
    pub(crate) settings_scroll: ScrollHandle,

    pub(crate) focus_handle: FocusHandle,
    /// The focus scope of the palette layer: the keyboard lives inside it
    /// while the palette is open, and `Tab` cycles within it.
    pub(crate) palette_scope: FocusHandle,
    /// The focus scope of the trust layer, for the same reasons.
    pub(crate) trust_scope: FocusHandle,
    /// The button the trust notice takes the keyboard at when it opens.
    pub(crate) trust_primary: FocusHandle,
    /// Where the keyboard was before a modal took it.
    pub(crate) focus_before_modal: Option<FocusHandle>,
    /// Whether the keyboard is to be given back after a modal closed.
    pub(crate) restore_focus: bool,
    /// Whether the trust notice has taken the keyboard yet.
    pub(crate) trust_has_keyboard: bool,
    pub(crate) _subscriptions: Vec<Subscription>,
    pub(crate) _ticker: Task<()>,
}

impl Shell {
    /// Creates the window's state. A repository path opens it immediately.
    pub(crate) fn new(
        repository: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let config_dir = crate::settings::config_dir();
        let settings_path = Settings::path_in(&config_dir);
        let settings = Settings::load(&settings_path);
        // A settings file read from the old one, or one that was damaged, is
        // written back in the current shape on the first save.
        let settings_dirty = settings.migrated || settings.damaged;
        let panes = Rc::new(RefCell::new(settings.panes));
        let focus_handle = cx.focus_handle();
        let palette_scope = cx.focus_handle();
        let trust_scope = cx.focus_handle();
        let trust_primary = cx.focus_handle();

        let filter_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter changes and impact"));
        let palette_input = cx.new(|cx| InputState::new(window, cx).placeholder("Find a symbol"));

        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(
            &filter_input,
            window,
            |this, input, event: &InputEvent, window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.filter.text = input.read(cx).value().to_string();
                    this.outline_dirty = true;
                    this.tests_dirty = true;
                    cx.notify();
                } else if matches!(event, InputEvent::PressEnter { .. }) {
                    this.confirm_focus(window, cx);
                }
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &palette_input,
            window,
            |this, input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    this.refilter_palette(input.read(cx).value().as_ref());
                    cx.notify();
                }
            },
        ));

        window.focus(&focus_handle, cx);

        let mut shell = Shell {
            worker: EngineWorker::spawn(),
            settings_path,
            settings,
            panes,
            settings_dirty: Rc::new(Cell::new(settings_dirty)),
            dirty_since: None,
            placement: Rc::new(Cell::new(None)),
            session: Session::default(),
            commits: CommitList::default(),
            docs: super::doc::Docs::default(),
            editors: Vec::new(),
            editors_known: false,
            settings_panel: None,
            settings_rerun: None,
            filter: Filter::default(),
            filter_input,
            outline: Outline::default(),
            outline_dirty: true,
            tests_expanded: Vec::new(),
            tests_rows: Vec::new(),
            tests_dirty: true,
            tests_cursor: None,
            palette: None,
            palette_input,
            search: SymbolIndex::default(),
            tab: Tab::Graph,
            focus: Focus::Outline,
            evidence_path: 0,
            evidence_more_tests: false,
            graph: graph::GraphState::default(),
            code: code::CodeState::default(),
            diff: diff::DiffState::default(),
            start: Start::default(),
            pending_trust: None,
            restore_compare: None,
            run_seen: 0,
            whatif_seen: 0,
            now: unix_now(),
            commit_cursor: 0,
            outline_cursor: None,
            toast: None,
            refocus: false,
            failure_open: false,
            outline_scroll: ScrollHandle::new(),
            tests_scroll: ScrollHandle::new(),
            commits_scroll: ScrollHandle::new(),
            evidence_scroll: ScrollHandle::new(),
            diff_scroll: ScrollHandle::new(),
            diff_scroll_x: ScrollHandle::new(),
            failure_scroll: ScrollHandle::new(),
            settings_scroll: ScrollHandle::new(),
            focus_handle,
            palette_scope,
            trust_scope,
            trust_primary,
            focus_before_modal: None,
            restore_focus: false,
            trust_has_keyboard: false,
            _subscriptions: Vec::new(),
            _ticker: Task::ready(()),
        };
        shell._subscriptions = subscriptions;
        shell._ticker = cx.spawn(async move |this: WeakEntity<Shell>, cx| {
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(120))
                    .await;
                if this.update(cx, |shell, cx| shell.tick(cx)).is_err() {
                    return;
                }
            }
        });

        // Finding the editors reads the file system, so it happens on the
        // engine thread; the panel shows what comes back.
        shell.worker.send(Command::DetectEditors);

        if let Some(path) = repository {
            shell.open_path(path, cx);
        }
        shell
    }

    // -- state transitions ------------------------------------------------

    /// Asks the engine thread to open the repository at `path`.
    pub(crate) fn open_path(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.start.opening = true;
        self.start.error = None;
        self.worker.send(Command::Open { path });
        cx.notify();
    }

    /// Opens the folder picker and then whatever repository it returns.
    pub(crate) fn pick_folder(&mut self, cx: &mut Context<Self>) {
        let receiver = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open a repository".into()),
        });
        cx.spawn(async move |this: WeakEntity<Shell>, cx| {
            let Ok(Ok(Some(paths))) = receiver.await else {
                return;
            };
            let Some(path) = paths.into_iter().next() else {
                return;
            };
            this.update(cx, |shell, cx| shell.open_path(path, cx)).ok();
        })
        .detach();
    }

    /// Asks for the next page of commits.
    pub(crate) fn request_commits(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.repo_root() else {
            return;
        };
        let Some((skip, limit)) = self.commits.next_page() else {
            return;
        };
        self.worker.send(Command::Commits {
            repo: root,
            skip,
            limit,
        });
        cx.notify();
    }

    /// Starts the default comparison of a freshly opened repository, asking
    /// about build scripts first when this repository was never opened before.
    fn finish_open(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.repo_root() else {
            return;
        };
        match self.settings.exec_mode(&root) {
            Some(_) => {
                if let Some(compare) = self.restore_compare.take() {
                    self.choose(compare, cx);
                } else if let Some(compare) = self.session.default_compare() {
                    self.choose(compare, cx);
                }
            }
            None => {
                self.pending_trust = Some(root);
                cx.notify();
            }
        }
    }

    /// Answers the trust notice.
    pub(crate) fn answer_trust(&mut self, restricted: bool, cx: &mut Context<Self>) {
        let Some(root) = self.pending_trust.take() else {
            return;
        };
        let mode = if restricted {
            blastr_model::ExecMode::Restricted
        } else {
            blastr_model::ExecMode::Full
        };
        self.settings.set_exec_mode(&root, mode);
        self.settings_dirty.set(true);
        self.session.options.exec = mode;
        if let Some(compare) = self.restore_compare.take() {
            self.choose(compare, cx);
        } else if let Some(compare) = self.session.default_compare() {
            self.choose(compare, cx);
        }
        cx.notify();
    }

    /// Starts a comparison on the engine thread.
    pub(crate) fn choose(&mut self, compare: Compare, cx: &mut Context<Self>) {
        let Some(root) = self.repo_root() else {
            return;
        };
        let spec = compare.spec();
        let ticket = self.session.choose(compare);
        self.run_seen = 0;
        self.whatif_seen = 0;
        self.evidence_path = 0;
        self.outline_cursor = None;
        self.code.reset();
        self.worker.send(Command::Analyze {
            generation: ticket.generation,
            repo: root,
            spec,
            options: self.session.options.clone(),
            params: self.session.params.clone(),
            cancel: ticket.cancel,
            progress: ticket.progress,
        });
        cx.notify();
    }

    /// Runs the current comparison again, keeping the selection.
    pub(crate) fn retry(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.repo_root() else {
            return;
        };
        let Some(spec) = self.session.compare.as_ref().map(Compare::spec) else {
            return;
        };
        let Some(ticket) = self.session.retry() else {
            return;
        };
        self.failure_open = false;
        self.run_seen = 0;
        self.worker.send(Command::Analyze {
            generation: ticket.generation,
            repo: root,
            spec,
            options: self.session.options.clone(),
            params: self.session.params.clone(),
            cancel: ticket.cancel,
            progress: ticket.progress,
        });
        cx.notify();
    }

    /// Cancels the running job, or closes what is on top. Returns whether
    /// something was closed.
    pub(crate) fn cancel_or_close(&mut self, cx: &mut Context<Self>) -> bool {
        match self.modals().escape() {
            Some(Modal::Settings) => {
                self.close_settings(cx);
                return true;
            }
            Some(Modal::Palette) => {
                self.close_palette(cx);
                return true;
            }
            Some(Modal::Trust) => return false,
            None => {}
        }
        if self.pending_trust.is_some() {
            return false;
        }
        if self.session.is_hypothetical() {
            self.leave_what_if(cx);
            return true;
        }
        self.session.cancel()
    }

    /// Starts a hypothetical change of the selected symbol.
    pub(crate) fn begin_what_if(&mut self, assume: Assume, cx: &mut Context<Self>) {
        let (Some(root), Some(selection)) = (self.repo_root(), self.session.selection().cloned())
        else {
            return;
        };
        let ticket = self.session.begin_what_if(selection.symbol.clone(), assume);
        self.whatif_seen = 0;
        self.worker.send(Command::WhatIf {
            generation: ticket.generation,
            repo: root,
            symbol: selection.symbol,
            assume,
            options: self.session.options.clone(),
            params: self.session.params.clone(),
            cancel: ticket.cancel,
            progress: ticket.progress,
        });
        cx.notify();
    }

    /// Returns from the hypothetical result to the real comparison.
    pub(crate) fn leave_what_if(&mut self, cx: &mut Context<Self>) {
        self.session.leave_what_if();
        self.evidence_path = 0;
        cx.notify();
    }

    /// Selects a symbol everywhere.
    pub(crate) fn select_symbol(
        &mut self,
        symbol: SymbolPath,
        site: Option<Location>,
        cx: &mut Context<Self>,
    ) {
        let selection = match site {
            Some(site) => Selection::at_site(symbol, site),
            None => Selection::symbol(symbol),
        };
        self.session.select(selection);
        self.evidence_path = 0;
        self.code.pending = true;
        self.outline_cursor = self.outline_position();
        self.scroll_outline_to_selection();
        cx.notify();
    }

    /// Brings the views in line with a selection the session made itself.
    fn follow_selection(&mut self) {
        self.evidence_path = 0;
        self.evidence_more_tests = false;
        self.code.pending = true;
        self.diff.hunk = 0;
        self.outline_dirty = true;
        self.outline_cursor = None;
    }

    /// Goes back in the selection history.
    pub(crate) fn back(&mut self, cx: &mut Context<Self>) {
        if self.session.history.back().is_some() {
            self.after_history(cx);
        }
    }

    /// Goes forward in the selection history.
    pub(crate) fn forward(&mut self, cx: &mut Context<Self>) {
        if self.session.history.forward().is_some() {
            self.after_history(cx);
        }
    }

    fn after_history(&mut self, cx: &mut Context<Self>) {
        self.evidence_path = 0;
        self.code.pending = true;
        self.outline_cursor = self.outline_position();
        self.scroll_outline_to_selection();
        cx.notify();
    }

    // -- folders and files -------------------------------------------------

    /// The root of the open repository.
    pub(crate) fn repo_root(&self) -> Option<PathBuf> {
        self.session.repo.as_ref().map(|repo| repo.root.clone())
    }

    /// The open repository, when there is one.
    pub(crate) fn repo(&self) -> Option<&RepoInfo> {
        self.session.repo.as_ref()
    }

    /// Opens the selected location in the configured editor.
    pub(crate) fn open_in_editor(&mut self, cx: &mut Context<Self>) {
        let (Some(root), Some(selection)) = (self.repo_root(), self.session.selection()) else {
            return;
        };
        let Some((_, location)) = self.selection_location(selection) else {
            return;
        };
        let file = root.join(location.file.as_str());
        self.open_file_in_editor(file, location.span.start_line, cx);
    }

    /// Asks the engine thread to start the chosen editor on `file`.
    ///
    /// The editor is found and started there, off the thread that draws, and a
    /// failure comes back as a message in the window instead of disappearing.
    pub(crate) fn open_file_in_editor(&mut self, file: PathBuf, line: u32, cx: &mut Context<Self>) {
        self.worker.send(Command::OpenEditor {
            file,
            line,
            column: 1,
            editor: self.settings.editor.editor,
            custom: self.settings.editor.custom.clone(),
        });
        cx.notify();
    }

    /// Copies `text` and says so briefly.
    pub(crate) fn copy(&mut self, text: impl Into<String>, cx: &mut Context<Self>) {
        let text = text.into();
        cx.write_to_clipboard(ClipboardItem::new_string(text.clone()));
        self.toast = Some((format!("Copied {text}"), Instant::now()));
        cx.notify();
    }

    // -- the selection -----------------------------------------------------

    /// The analysis the views show, when it has arrived.
    pub(crate) fn analysis(&self) -> Option<&Analysis> {
        self.session.analysis().map(|analysis| &**analysis)
    }

    /// The changes the views show.
    pub(crate) fn changes(&self) -> Option<&blastr_model::DiffResult> {
        self.session.shown_changes()
    }

    /// The item the selection names, when it is one.
    pub(crate) fn selected_item(&self) -> Option<&ImpactItem> {
        let symbol = &self.session.selection()?.symbol;
        self.analysis()?
            .items
            .iter()
            .find(|item| &item.symbol == symbol)
    }

    /// The change the selection names, when it is one.
    pub(crate) fn selected_change(&self) -> Option<&Change> {
        let symbol = &self.session.selection()?.symbol;
        self.changes()?
            .changes
            .iter()
            .find(|change| &change.symbol == symbol)
    }

    /// The source and location to read for the selection.
    pub(crate) fn selection_location(&self, selection: &Selection) -> Option<(Source, Location)> {
        let analysis = self.analysis()?;
        if let Some(site) = &selection.site {
            return Some((analysis.diff.comparison.head.clone(), site.clone()));
        }
        if let Some(item) = analysis
            .items
            .iter()
            .find(|item| item.symbol == selection.symbol)
        {
            return Some((analysis.diff.comparison.head.clone(), item.location.clone()));
        }
        if let Some(change) = analysis
            .diff
            .changes
            .iter()
            .find(|change| change.symbol == selection.symbol)
        {
            if let Some(new) = &change.new {
                return Some((analysis.diff.comparison.head.clone(), new.clone()));
            }
            if let Some(old) = &change.old {
                return Some((analysis.diff.comparison.base.clone(), old.clone()));
            }
        }
        if let Some(entry) = self.search.find(&selection.symbol) {
            return Some((
                analysis.diff.comparison.head.clone(),
                entry.location.clone(),
            ));
        }
        None
    }

    /// The row of the outline that selects the current selection.
    pub(crate) fn outline_position(&self) -> Option<usize> {
        let selection = self.session.selection()?;
        self.outline
            .position(&selection.symbol, self.changes(), self.analysis())
    }

    /// Scrolls the outline so that the selected row is visible.
    fn scroll_outline_to_selection(&mut self) {
        let Some(position) = self.outline_cursor.or_else(|| self.outline_position()) else {
            return;
        };
        let heights = left::outline_heights(&self.outline);
        let viewport = f32::from(self.outline_scroll.bounds().size.height).max(120.0);
        let offset = -f32::from(self.outline_scroll.offset().y);
        let offset = list::offset_to_show(&heights, position, offset, viewport);
        self.outline_scroll.set_offset(point(px(0.), px(-offset)));
    }

    // -- overlays ----------------------------------------------------------

    /// The overlays that are open.
    pub(crate) fn modals(&self) -> Modals {
        Modals {
            palette: self.palette.is_some(),
            trust: self.pending_trust.is_some(),
            settings: self.settings_panel.is_some(),
        }
    }

    /// Whether an overlay covers the window.
    ///
    /// This is the one question every action handler asks before it acts: an
    /// overlay owns the window, including its title bar, until it closes.
    pub(crate) fn modal_open(&self) -> bool {
        self.modals().any()
    }

    /// Whether a shortcut may act on the window.
    fn shortcut_allowed(&self) -> bool {
        !self.modal_open()
    }

    /// Gives the keyboard to the modal that is on top, and back afterwards.
    ///
    /// The palette and the settings panel focus a field of their own when they
    /// open; the trust notice has no field, so its primary button takes the
    /// keyboard here. Whatever had it is remembered and given it back when the
    /// last overlay closes.
    fn sync_modal_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let open = self.pending_trust.is_some();
        if open == self.trust_has_keyboard {
            return;
        }
        self.trust_has_keyboard = open;
        if open {
            if self.focus_before_modal.is_none() {
                self.focus_before_modal = window.focused(cx);
            }
            window.focus(&self.trust_primary, cx);
        } else {
            self.give_back_focus(window, cx);
        }
    }

    /// Gives the keyboard back to what had it when the modal opened, or to the
    /// window when that is gone or was never set.
    fn give_back_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        match self.focus_before_modal.take() {
            Some(handle) => window.focus(&handle, cx),
            None => window.focus(&self.focus_handle, cx),
        }
    }

    // -- palette -----------------------------------------------------------

    /// Opens the symbol search.
    pub(crate) fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.session.repo.is_none() || self.palette.is_some() {
            return;
        }
        self.palette = Some(Palette::default());
        self.focus_before_modal = window.focused(cx);
        self.palette_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        window.focus(&self.palette_input.read(cx).focus_handle(cx), cx);
        cx.notify();
    }

    /// Closes the palette and gives the keyboard back to the window.
    pub(crate) fn close_palette(&mut self, cx: &mut Context<Self>) {
        self.palette = None;
        self.restore_focus = true;
        cx.notify();
    }

    /// Recomputes the palette's matches.
    fn refilter_palette(&mut self, query: &str) {
        let hits = self.search.search(query, 80);
        if let Some(palette) = &mut self.palette {
            palette.hits = hits;
            palette.selected = 0;
        }
    }

    /// Moves the palette's highlight.
    fn step_palette(&mut self, forward: bool) {
        let Some(palette) = &mut self.palette else {
            return;
        };
        if palette.hits.is_empty() {
            return;
        }
        if forward {
            palette.selected = (palette.selected + 1).min(palette.hits.len() - 1);
        } else {
            palette.selected = palette.selected.saturating_sub(1);
        }
    }

    /// Selects the highlighted match and closes the palette.
    fn confirm_palette(&mut self, cx: &mut Context<Self>) {
        let Some(palette) = &self.palette else {
            return;
        };
        let Some(hit) = palette.hits.get(palette.selected) else {
            return;
        };
        let Some(entry) = self.search.get(hit.entry) else {
            return;
        };
        let symbol = entry.symbol.clone();
        let site = entry.location.clone();
        self.close_palette(cx);
        self.select_symbol(symbol, Some(site), cx);
    }

    // -- keyboard ----------------------------------------------------------

    /// Moves the keyboard's list selection.
    fn move_focus(&mut self, forward: bool, cx: &mut Context<Self>) {
        match self.focus {
            Focus::Commits => {
                let dirty = self.repo().is_some_and(|repo| repo.dirty);
                let count = self.commits.row_count(dirty);
                if count == 0 {
                    return;
                }
                self.commit_cursor = if forward {
                    (self.commit_cursor + 1).min(count - 1)
                } else {
                    self.commit_cursor.saturating_sub(1)
                };
                let heights = vec![26.0; count];
                let viewport = f32::from(self.commits_scroll.bounds().size.height).max(60.0);
                let offset = -f32::from(self.commits_scroll.offset().y);
                let offset = list::offset_to_show(&heights, self.commit_cursor, offset, viewport);
                self.commits_scroll.set_offset(point(px(0.), px(-offset)));
            }
            Focus::Outline => {
                self.outline_cursor = self.outline.step(self.outline_cursor, forward);
                self.scroll_outline_to_selection();
                if let Some(position) = self.outline_cursor
                    && let Some(row) = self.outline.rows.get(position)
                    && let Some(symbol) = row.symbol(self.changes(), self.analysis()).cloned()
                {
                    self.select_symbol(symbol, None, cx);
                }
            }
            Focus::Tests => {
                let heights = left::test_heights(&self.tests_rows);
                let count = heights.len();
                if count == 0 {
                    return;
                }
                let index = self
                    .tests_cursor
                    .map(|cursor| {
                        if forward {
                            (cursor + 1).min(count - 1)
                        } else {
                            cursor.saturating_sub(1)
                        }
                    })
                    .unwrap_or(0);
                self.tests_cursor = Some(index);
                let viewport = f32::from(self.tests_scroll.bounds().size.height).max(60.0);
                let offset = -f32::from(self.tests_scroll.offset().y);
                let offset = list::offset_to_show(&heights, index, offset, viewport);
                self.tests_scroll.set_offset(point(px(0.), px(-offset)));
            }
        }
        cx.notify();
    }

    /// Confirms the keyboard's current row.
    fn confirm_focus(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.palette.is_some() {
            self.confirm_palette(cx);
            return;
        }
        match self.focus {
            Focus::Commits => self.pick_commit(self.commit_cursor, false, cx),
            Focus::Outline => {
                if let Some(position) = self.outline_cursor.or_else(|| self.outline_position())
                    && let Some(row) = self.outline.rows.get(position)
                    && let Some(symbol) = row.symbol(self.changes(), self.analysis()).cloned()
                {
                    self.select_symbol(symbol, None, cx);
                }
            }
            Focus::Tests => self.confirm_test(cx),
        }
        window.focus(&self.focus_handle, cx);
    }

    /// Whether typing into a text field should keep the arrow keys.
    fn typing(&self, window: &Window, cx: &Context<Self>) -> bool {
        self.settings_panel.is_some()
            || self
                .filter_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
            || self
                .palette_input
                .read(cx)
                .focus_handle(cx)
                .is_focused(window)
    }

    // -- the ticker --------------------------------------------------------

    /// Drains the engine thread and the job progress, and saves what changed.
    fn tick(&mut self, cx: &mut Context<Self>) {
        let mut changed = false;
        while let Some(event) = self.worker.next_event() {
            self.handle_event(event, cx);
            changed = true;
        }

        if let Some(progress) = self.session.run.poll_progress(&mut self.run_seen) {
            let generation = self.session.run.generation();
            self.session.progress(generation, progress);
            changed = true;
        }
        if let Some(progress) = self
            .session
            .what_if_run
            .poll_progress(&mut self.whatif_seen)
        {
            let generation = self.session.what_if_run.generation();
            self.session.what_if_progress(generation, progress);
            changed = true;
        }

        if self.session.repo.is_some() && self.commits.wants_more(self.commit_cursor + 30) {
            self.request_commits(cx);
        }

        if self.outline_dirty {
            self.outline = Outline::build(
                self.changes(),
                self.analysis(),
                self.session.shown().is_loading(),
                &self.filter,
            );
            self.outline_dirty = false;
            if self.outline_cursor.is_none() {
                // A selection the session made itself, or a fresh result: put
                // the row it names in view.
                self.outline_cursor = self.outline_position();
                self.scroll_outline_to_selection();
            }
            changed = true;
        }
        if self.tests_dirty {
            self.tests_rows = match self.analysis() {
                Some(analysis) => {
                    crate::state::outline::test_rows(analysis, &self.filter, &self.tests_expanded)
                }
                None => Vec::new(),
            };
            self.tests_dirty = false;
            changed = true;
        }

        self.now = unix_now();
        if let Some(at) = self.settings_rerun
            && Instant::now() >= at
        {
            self.settings_rerun = None;
            self.rerun_analysis(cx);
            changed = true;
        }
        if let Some((_, at)) = &self.toast
            && at.elapsed() > Duration::from_secs(3)
        {
            self.toast = None;
            changed = true;
        }

        if self.settings_dirty.get() {
            let since = *self.dirty_since.get_or_insert_with(Instant::now);
            if since.elapsed() > Duration::from_millis(600) {
                self.save_settings();
                changed = true;
            }
        } else {
            self.dirty_since = None;
        }

        if changed {
            cx.notify();
        }
    }

    /// Folds one engine event into the session.
    fn handle_event(&mut self, event: Event, cx: &mut Context<Self>) {
        match event {
            Event::Opened { result } => match result {
                Ok(opened) => self.opened(*opened, cx),
                Err(message) => {
                    self.start.opening = false;
                    self.start.error = Some(message);
                }
            },
            Event::Editors { editors } => {
                self.editors = editors;
                self.editors_known = true;
            }
            Event::Notice { text } => {
                self.toast = Some((text, Instant::now()));
            }
            Event::Commits { skip, result } => match result {
                Ok(page) => self.commits.page_arrived(skip, page),
                Err(message) => self.commits.page_failed(message),
            },
            Event::Changes { generation, diff } => {
                if self.session.changes_arrived(generation, diff) {
                    self.outline_dirty = true;
                }
            }
            Event::Result {
                generation,
                analysis,
                graph,
                elapsed,
            } => {
                // The job stays alive after its result only when the working
                // tree is followed, which the settings can turn off.
                let watching = self.session.options.watch
                    && self.session.compare.as_ref().is_some_and(Compare::watches);
                if self.session.result_arrived(
                    generation,
                    analysis,
                    graph.clone(),
                    elapsed,
                    watching,
                ) {
                    self.outline_dirty = true;
                    self.tests_dirty = true;
                    self.docs.invalidate(&Source::Worktree);
                    if let Some(graph) = graph {
                        self.search = SymbolIndex::from_graph(&graph);
                    }
                    self.graph.invalidate();
                    // A result selects something when nothing is: the code view
                    // and the evidence panel must never be blank after a run.
                    self.follow_selection();
                }
            }
            Event::Failed {
                generation,
                failure,
                watching,
            } => {
                self.session.failed(generation, failure, watching);
                self.outline_dirty = true;
                self.tests_dirty = true;
            }
            Event::Refreshing { generation } => {
                self.session.refresh_started(generation);
                self.outline_dirty = true;
            }
            Event::BaseMoved { generation } => {
                if self.session.run.is_current(generation) {
                    self.restore_compare = self.session.compare.clone();
                    if let Some(root) = self.repo_root() {
                        self.worker.send(Command::Open { path: root });
                    }
                }
            }
            Event::WhatIfResult {
                generation,
                analysis,
                elapsed,
            } => {
                self.session.what_if_arrived(generation, analysis, elapsed);
                self.outline_dirty = true;
                self.tests_dirty = true;
                self.follow_selection();
            }
            Event::WhatIfFailed {
                generation,
                failure,
            } => {
                self.session.what_if_failed(generation, failure);
                self.outline_dirty = true;
            }
        }
    }

    /// A repository was opened: remember it, load its history and start.
    fn opened(&mut self, info: RepoInfo, cx: &mut Context<Self>) {
        self.start.opening = false;
        self.start.error = None;
        let root = info.root.clone();
        self.settings.opened(&root, info.branch.clone(), unix_now());
        self.settings_dirty.set(true);
        self.session.open_repository(info);
        let (options, params) = self.settings.analysis_for(Some(&root));
        self.session.options = options;
        self.session.params = params;
        self.commits.reset();
        self.commit_cursor = 0;
        self.docs.reset(root);
        self.search = SymbolIndex::default();
        self.graph.invalidate();
        self.request_commits(cx);
        self.finish_open(cx);
    }

    /// Writes the settings that changed.
    fn save_settings(&mut self) {
        self.settings.panes = *self.panes.borrow();
        if let Some(placement) = self.placement.get() {
            self.settings.window = Some(placement);
        }
        let _ = self.settings.save(&self.settings_path);
        self.settings_dirty.set(false);
        self.dirty_since = None;
    }

    // -- the render --------------------------------------------------------

    /// The title bar: what is compared on the left, the symbol search in the
    /// middle, the window's actions on the right and the platform's window
    /// controls beyond them.
    ///
    /// The search is a command bar rather than an input: clicking it, or
    /// `Ctrl P`, opens the symbol palette. It holds the middle of the bar at
    /// every window size, where the eye looks first, instead of being pushed
    /// into a corner by the chips.
    fn title_bar(&self, width: f32, cx: &mut Context<Self>) -> AnyElement {
        let room = BarRoom::at((width - window_controls_width()).max(0.0));
        TitleBar::new()
            .h(px(40.))
            .bg(theme::BG_BAR)
            .border_b_1()
            .border_color(theme::LINE)
            .child(
                div()
                    .relative()
                    .w_full()
                    .h_full()
                    .min_w_0()
                    .child(self.title_identity(room, cx))
                    // Centred on the bar itself, not in the space left between
                    // two groups of unequal width.
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .h_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .child(self.search_box(room, cx)),
                    )
                    .child(self.title_actions(cx)),
            )
            .into_any_element()
    }

    /// The mark, the wordmark and the chips that say what is compared.
    fn title_identity(&self, room: BarRoom, cx: &mut Context<Self>) -> AnyElement {
        let mut chips = h_flex().h_full().min_w_0().items_center().gap(px(6.));
        if let Some(repo) = self.repo() {
            chips = chips.child(self.repo_chip(repo, cx));
            if room.comparison
                && let Some(compare) = &self.session.compare
            {
                chips = chips.child(self.compare_chip(compare));
            }
            chips = chips.children(self.features_chip(cx));
        }
        h_flex()
            .h_full()
            .min_w_0()
            .items_center()
            .gap(px(6.))
            .max_w(room.chips)
            .child(div().flex_none().child(ui::logo(px(18.))))
            .child(
                div()
                    .flex_none()
                    .text_size(px(14.))
                    .text_color(theme::FG)
                    .child("Blastr"),
            )
            .child(div().flex_none().w(px(10.)))
            .child(chips)
            .into_any_element()
    }

    /// Refresh, open another repository and the settings, then the rule that
    /// separates them from the window controls.
    ///
    /// Each is a 28 px target with a tooltip that names it and its key: a hit
    /// size, not a label, is what makes three actions findable in a 40 px bar.
    /// While the comparison runs, the refresh target turns and says so instead
    /// of looking greyed out and broken.
    fn title_actions(&self, cx: &mut Context<Self>) -> AnyElement {
        let busy = self.session.is_busy();
        let refreshable = self.repo().is_some() && self.session.compare.is_some() && !busy;
        let refresh = ui::toolbar_button(
            "refresh",
            if busy {
                ui::spinner()
            } else {
                ui::action_icon(IconName::RefreshCw, refreshable)
            },
            if busy {
                "Running the comparison"
            } else {
                "Run the comparison again  Ctrl R"
            },
            refreshable,
        );
        let refresh = if refreshable {
            refresh.on_click(cx.listener(|this, _, _, cx| this.retry(cx)))
        } else {
            refresh
        };
        h_flex()
            .absolute()
            .top_0()
            .right_0()
            .h_full()
            .items_center()
            .gap(px(2.))
            .pr(px(8.))
            .child(refresh)
            .child(
                ui::toolbar_button(
                    "open-repository",
                    ui::action_icon(IconName::FolderOpen, true),
                    "Open a repository  Ctrl O",
                    true,
                )
                .on_click(cx.listener(|this, _, _, cx| this.pick_folder(cx))),
            )
            .child(
                ui::toolbar_button(
                    "settings",
                    ui::action_icon(IconName::Settings, true),
                    "Settings  Ctrl ,",
                    true,
                )
                .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx))),
            )
            .child(ui::toolbar_divider())
            .into_any_element()
    }

    /// The repository and the branch it has checked out.
    ///
    /// The chip says what is open, and clicking it opens the folder picker:
    /// the tooltip makes that discoverable, so the chip is an affordance
    /// rather than a hover that leads nowhere.
    fn repo_chip(&self, repo: &RepoInfo, cx: &mut Context<Self>) -> AnyElement {
        h_flex()
            // An element without an id only learns about the pointer on the
            // next redraw, which reads as a hover that lags behind it.
            .id("repo-chip")
            .flex_shrink(1.)
            .min_w_0()
            .h(ui::TOOLBAR_TARGET)
            .px_2()
            .gap_2()
            .items_center()
            .rounded(theme::RADIUS_MD)
            .border_1()
            .border_color(theme::LINE)
            .bg(theme::BG_RAISED)
            .text_size(TEXT_SMALL)
            .cursor_pointer()
            .hover(|style| style.bg(theme::BG_HOVER).border_color(theme::LINE_STRONG))
            .tooltip(|window, cx| Tooltip::new("Open another repository  Ctrl O").build(window, cx))
            .on_click(cx.listener(|this, _, _, cx| this.pick_folder(cx)))
            .child(
                Icon::new(IconName::Network)
                    .size_3()
                    .text_color(theme::FG_3),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme::FG)
                    .child(repo.name.clone()),
            )
            .children(repo.branch.clone().map(|branch| {
                h_flex()
                    .flex_none()
                    .gap_1()
                    .items_center()
                    .child(div().text_color(theme::FG_4).child("/"))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(theme::FG_2)
                            .child(branch),
                    )
            }))
            .into_any_element()
    }

    /// The two revisions the comparison reads: a status chip, not a control.
    fn compare_chip(&self, compare: &Compare) -> AnyElement {
        let (base, head) = compare.labels();
        h_flex()
            .id("compare-chip")
            .flex_shrink(1.)
            .min_w_0()
            .h(ui::TOOLBAR_TARGET)
            .px_2()
            .gap_2()
            .items_center()
            .rounded(theme::RADIUS_MD)
            .border_1()
            .border_color(theme::LINE)
            .bg(theme::BG_RAISED)
            .text_size(TEXT_SMALL)
            .child(
                Icon::new(IconName::Replace)
                    .size_3()
                    .text_color(theme::FG_3),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .text_color(theme::FG_2)
                    .child(base),
            )
            .child(div().flex_none().text_color(theme::FG_4).child("→"))
            .child(div().min_w_0().truncate().text_color(theme::FG).child(head))
            .into_any_element()
    }

    /// The features of the comparison, shown only when they differ from the
    /// default: features or a target chosen in the settings.
    ///
    /// A comparison that loads the project the way Cargo would by default has
    /// nothing to say here, so nothing is drawn.
    fn features_chip(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let options = &self.session.options;
        let mut parts: Vec<String> = Vec::new();
        if options.all_features {
            parts.push("all features".to_owned());
        } else if !options.features.is_empty() {
            parts.push(format!("features: {}", options.features.join(", ")));
        }
        if let Some(target) = &options.target {
            parts.push(format!("target: {target}"));
        }
        if parts.is_empty() {
            return None;
        }
        let label = parts.join(" · ");
        let tip = SharedString::from(
            "From Settings: this comparison does not use the default features or target.",
        );
        Some(
            h_flex()
                .id("features-chip")
                .flex_shrink(1.)
                .min_w_0()
                .h(ui::TOOLBAR_TARGET)
                .px_2()
                .gap_2()
                .items_center()
                .rounded(theme::RADIUS_MD)
                .border_1()
                .border_color(theme::LINE)
                .bg(theme::BG_RAISED)
                .text_size(TEXT_SMALL)
                .cursor_pointer()
                .hover(|style| style.bg(theme::BG_HOVER))
                .tooltip(move |window, cx| Tooltip::new(tip.clone()).build(window, cx))
                .on_click(cx.listener(|this, _, window, cx| this.open_settings(window, cx)))
                .child(Icon::new(IconName::Frame).size_3().text_color(theme::FG_3))
                .child(
                    div()
                        .min_w_0()
                        .max_w(px(280.))
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .text_color(theme::FG_2)
                        .child(label),
                )
                .into_any_element(),
        )
    }

    /// The search field of the title bar: a command bar that opens the symbol
    /// palette rather than an input of its own.
    ///
    /// Without a repository there is nothing to search, so the field says so
    /// by going quiet instead of inviting a click that does nothing.
    fn search_box(&self, room: BarRoom, cx: &mut Context<Self>) -> AnyElement {
        let ready = self.session.repo.is_some();
        let field = h_flex()
            .id("symbol-search")
            .flex_none()
            .h(px(28.))
            .w(room.search)
            .px(px(9.))
            .gap_2()
            .items_center()
            .rounded(theme::RADIUS_MD)
            .border_1()
            .border_color(theme::LINE)
            .bg(theme::BG_INPUT)
            .text_size(TEXT_SMALL)
            .child(ui::action_icon(IconName::Search, ready))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .truncate()
                    .text_color(if ready { theme::FG_3 } else { theme::FG_4 })
                    .child("Search symbols"),
            );
        if !ready {
            return field.into_any_element();
        }
        field
            .cursor_pointer()
            .hover(|style| style.border_color(theme::LINE_STRONG).bg(theme::BG_RAISED))
            .on_click(cx.listener(|this, _, window, cx| this.open_palette(window, cx)))
            .child(ui::keycap("Ctrl P", cx))
            .into_any_element()
    }

    /// The banner shown while a hypothetical result is on screen.
    fn whatif_banner(&self, cx: &mut Context<Self>) -> AnyElement {
        let symbol = self
            .session
            .what_if
            .as_ref()
            .map(|what_if| what_if.assume.as_str().to_owned())
            .unwrap_or_default();
        h_flex()
            .h(px(34.))
            .flex_shrink_0()
            .px_3()
            .gap_2()
            .items_center()
            .bg(theme::WHATIF.bg)
            .border_b_1()
            .border_color(theme::WHATIF.line)
            .text_size(TEXT_SMALL)
            .child(
                div()
                    .text_size(TEXT_MICRO)
                    .text_color(theme::WHATIF.fg)
                    .child("HYPOTHETICAL"),
            )
            .child(
                div()
                    .text_color(theme::FG_4)
                    .child("if the selected symbol changed its"),
            )
            .child(div().text_color(theme::FG).child(symbol))
            .children(
                self.session
                    .shown()
                    .progress()
                    .map(|progress| div().text_color(theme::FG_3).child(progress.label())),
            )
            .child(div().flex_1())
            .child(
                Button::new("leave-whatif")
                    .small()
                    .outline()
                    .label("Back to the comparison")
                    .on_click(cx.listener(|this, _, _, cx| this.leave_what_if(cx))),
            )
            .into_any_element()
    }

    fn body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let left_width = self.panes.borrow().left;
        let right_width = self.panes.borrow().right;
        let panes = self.panes.clone();
        let dirty = self.settings_dirty.clone();

        h_resizable("blastr-columns")
            .child(
                resizable_panel()
                    .size(px(left_width))
                    .size_range(px(240.)..px(640.))
                    .child(self.left_pane(cx)),
            )
            .child(resizable_panel().child(self.centre(cx)))
            .child(
                resizable_panel()
                    .size(px(right_width))
                    .size_range(px(260.)..px(640.))
                    .child(self.evidence_pane(cx)),
            )
            .on_resize(move |state, _, cx| {
                let sizes = state.read(cx).sizes();
                let mut current = panes.borrow_mut();
                if let Some(width) = sizes.first() {
                    current.left = f32::from(*width);
                }
                if let Some(width) = sizes.get(2) {
                    current.right = f32::from(*width);
                }
                dirty.set(true);
            })
            .into_any_element()
    }

    /// The centre: the tab strip and the graph, code or before/after view.
    /// The card a failed comparison shows in the centre pane.
    ///
    /// It is contained: a headline, one summary sentence, the hint, and the
    /// whole message behind a toggle, so that a long error never runs over
    /// the panes beside it.
    pub(crate) fn failure_card(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let Some(failure) = self.session.shown().failure().cloned() else {
            return div().into_any_element();
        };
        let open = self.failure_open;
        let details = failure.message.clone();
        let card = v_flex()
            .w_full()
            .max_w(px(520.))
            .min_w_0()
            .overflow_hidden()
            .gap_3()
            .p_4()
            .rounded(px(8.))
            .border_1()
            .border_color(theme::LINE)
            .bg(theme::BG_RAISED)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        div().flex_none().child(
                            Icon::new(IconName::TriangleAlert)
                                .size(px(15.))
                                .text_color(theme::EDIT.fg),
                        ),
                    )
                    .child(
                        div()
                            .min_w_0()
                            .text_size(px(14.))
                            .text_color(theme::FG)
                            .child("The comparison failed"),
                    ),
            )
            .child(
                div()
                    .min_w_0()
                    .text_size(TEXT_SMALL)
                    .text_color(theme::FG_2)
                    .child(ui::failure_summary(&failure.message)),
            )
            .children(failure.hint.clone().map(|hint| {
                div()
                    .min_w_0()
                    .text_size(TEXT_SMALL)
                    .text_color(theme::FG_3)
                    .child(hint)
            }))
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(
                        Button::new("failure-details")
                            .small()
                            .outline()
                            .label(if open { "Hide details" } else { "Show details" })
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.failure_open = !this.failure_open;
                                cx.notify();
                            })),
                    )
                    .child(
                        Button::new("failure-copy")
                            .small()
                            .ghost()
                            .label("Copy details")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let text = this
                                    .session
                                    .shown()
                                    .failure()
                                    .map(|failure| failure.message.clone())
                                    .unwrap_or_default();
                                cx.write_to_clipboard(ClipboardItem::new_string(text));
                                this.toast =
                                    Some(("Copied the details".to_owned(), Instant::now()));
                                cx.notify();
                            })),
                    )
                    .child(div().flex_1())
                    .child(
                        Button::new("failure-retry")
                            .small()
                            .outline()
                            .label("Retry")
                            .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                    ),
            )
            .when(open, |this| {
                this.child(
                    // The details are as long as the failure says they are:
                    // a fixed window over them, with a bar that says how much
                    // of the message is below.
                    div()
                        .relative()
                        .w_full()
                        .min_w_0()
                        .h(px(170.))
                        .rounded(px(6.))
                        .border_1()
                        .border_color(theme::LINE)
                        .bg(theme::BG_INPUT)
                        .overflow_hidden()
                        .child(
                            v_flex()
                                .id("failure-details-text")
                                .size_full()
                                .min_w_0()
                                .min_h_0()
                                .overflow_y_scroll()
                                .track_scroll(&self.failure_scroll)
                                .p_2()
                                .font_family(theme::mono())
                                .text_size(px(12.))
                                .text_color(theme::FG_2)
                                .child(div().min_w_0().child(details)),
                        )
                        .child(ui::scrollbar(
                            "failure-details",
                            &self.failure_scroll,
                            ScrollbarAxis::Vertical,
                        )),
                )
            });

        v_flex()
            .size_full()
            .min_w_0()
            .min_h_0()
            .items_center()
            .justify_center()
            .p_6()
            .bg(theme::BG_CANVAS)
            .child(card)
            .into_any_element()
    }

    fn centre(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let tabs = h_flex()
            .h(px(36.))
            .flex_shrink_0()
            .px_2()
            .gap_1()
            .items_center()
            .border_b_1()
            .border_color(theme::LINE)
            .bg(theme::BG_PANEL)
            .child(
                ui::icon_button("back", IconName::ArrowLeft, "Back  Alt ←")
                    .disabled(!self.session.history.can_go_back())
                    .on_click(cx.listener(|this, _, _, cx| this.back(cx))),
            )
            .child(
                ui::icon_button("forward", IconName::ArrowRight, "Forward  Alt →")
                    .disabled(!self.session.history.can_go_forward())
                    .on_click(cx.listener(|this, _, _, cx| this.forward(cx))),
            )
            .children(Tab::ALL.into_iter().map(|tab| {
                let selected = self.tab == tab;
                // A file with unsaved edits marks the tab that edits it.
                let modified = tab == Tab::Code && self.code.any_dirty();
                h_flex()
                    .id(SharedString::from(format!("tab-{}", tab.label())))
                    .h_full()
                    .px_2()
                    .gap_2()
                    .items_center()
                    .border_b_2()
                    .border_color(if selected {
                        theme::CHANGED.solid
                    } else {
                        theme::BG_PANEL
                    })
                    .text_size(TEXT_UI)
                    .text_color(if selected { theme::FG } else { theme::FG_3 })
                    .cursor_pointer()
                    .hover(|style| style.text_color(theme::FG))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.tab = tab;
                        cx.notify();
                    }))
                    .child(Icon::new(tab.icon()).size_3().text_color(theme::FG_4))
                    .child(tab.label())
                    .when(modified, |tab| {
                        tab.child(div().size(px(6.)).rounded_full().bg(theme::CHANGED.solid))
                    })
                    // A quiet hint, not a key cap: the strip is already busy.
                    .child(
                        div()
                            .text_size(TEXT_MICRO)
                            .text_color(theme::FG_4)
                            .child(tab.key()),
                    )
            }))
            .child(div().flex_1());

        let main = match self.tab {
            Tab::Graph => self.graph_view(cx),
            Tab::Code => self.code_full(cx),
            Tab::Diff => self.diff_pane(cx),
        };

        let view = if self.tab == Tab::Graph {
            let code_height = self.panes.borrow().code;
            let panes = self.panes.clone();
            let dirty = self.settings_dirty.clone();
            v_resizable("graph-code-split")
                .child(resizable_panel().child(main))
                .child(
                    resizable_panel()
                        .size(px(code_height))
                        .size_range(px(80.)..px(900.))
                        .child(self.code_peek(cx)),
                )
                .on_resize(move |state, _, cx| {
                    if let Some(height) = state.read(cx).sizes().get(1) {
                        panes.borrow_mut().code = f32::from(*height);
                        dirty.set(true);
                    }
                })
                .into_any_element()
        } else {
            div().size_full().child(main).into_any_element()
        };

        v_flex()
            .size_full()
            .min_w_0()
            .bg(theme::BG_CANVAS)
            .child(tabs)
            .child(div().flex_1().min_h_0().child(view))
            .into_any_element()
    }

    /// The status bar: what is happening, what limits the result, and where it
    /// came from.
    fn status_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let mut left = h_flex()
            .flex_1()
            .min_w_0()
            .gap_2()
            .items_center()
            .overflow_hidden();
        let mut right = h_flex().flex_shrink_0().gap_2().items_center();

        let analysis = self.analysis();
        match self.session.shown() {
            Loadable::Loading(progress) => {
                left = left
                    .child(
                        Icon::new(IconName::Loader)
                            .size_3()
                            .text_color(theme::CHANGED.fg),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(theme::FG_2)
                            .child(progress.label()),
                    );
                if let Some(fraction) = progress.fraction() {
                    left = left.child(progress_bar(fraction));
                } else if progress.done > 0 {
                    left = left.child(progress_text(progress));
                }
                left = left.child(
                    ui::icon_button("cancel", IconName::Close, "Cancel  Esc").on_click(
                        cx.listener(|this, _, _, cx| {
                            this.session.cancel();
                            cx.notify();
                        }),
                    ),
                );
            }
            Loadable::Ready(outcome) => {
                let refreshing = self.session.refreshing.is_some();
                let watching = self.session.is_watching();
                let cancelled = self.session.stopped == Stopped::Cancelled;
                left = left
                    .child(
                        Icon::new(if cancelled {
                            IconName::CircleX
                        } else {
                            IconName::CircleCheck
                        })
                        .size_3()
                        .text_color(if cancelled {
                            theme::FG_3
                        } else {
                            theme::TEST.fg
                        }),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .text_color(theme::FG_2)
                            .child(if refreshing {
                                "Refreshing after a save".to_owned()
                            } else if watching {
                                "Up to date · refreshes on save".to_owned()
                            } else {
                                "Up to date".to_owned()
                            }),
                    );
                if let Some(progress) = &self.session.refreshing {
                    left = left.child(
                        div()
                            .flex_shrink_0()
                            .text_color(theme::FG_3)
                            .child(progress.label()),
                    );
                }
                if let Some(analysis) = analysis {
                    let unresolved = analysis.limits.unresolved_total;
                    if unresolved > 0 {
                        left = left.child(status_separator()).child(
                            h_flex()
                                .id("status-unresolved")
                                .flex_shrink_0()
                                .gap_1()
                                .items_center()
                                .cursor_pointer()
                                .hover(|style| style.text_color(theme::FG))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    // The limits are the long form of this count.
                                    if let Some(analysis) = this.analysis()
                                        && let Some(text) = wording::limits_text(analysis)
                                    {
                                        this.toast = Some((text, Instant::now()));
                                    }
                                    cx.notify();
                                }))
                                .child(
                                    Icon::new(IconName::TriangleAlert)
                                        .size_3()
                                        .text_color(theme::EDIT.fg),
                                )
                                .child(
                                    div()
                                        .font_family(theme::mono())
                                        .text_color(theme::EDIT.fg)
                                        .child(unresolved.to_string()),
                                )
                                .child(format!(
                                    "unresolved {}",
                                    wording::count_noun(unresolved, "reference", "references")
                                )),
                        );
                    }
                    let candidates = analysis.limits.candidate_steps;
                    if candidates > 0 {
                        left = left
                            .child(status_separator())
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .font_family(theme::mono())
                                    .child(candidates.to_string()),
                            )
                            .child(format!(
                                "candidate {}",
                                wording::count_noun(candidates, "step", "steps")
                            ));
                    }
                }
                let from_cache = came_from_cache(outcome.elapsed);
                right = right.child(if from_cache {
                    div()
                        .text_color(theme::FG_3)
                        .child("From cache")
                        .into_any_element()
                } else {
                    h_flex()
                        .gap_1()
                        .child(div().text_color(theme::FG_3).child("Analysed in"))
                        .child(
                            div()
                                .font_family(theme::mono())
                                .text_color(theme::FG_2)
                                .child(format!("{:.1} s", outcome.elapsed.as_secs_f64())),
                        )
                        .into_any_element()
                });
                right = right.child(status_separator());
            }
            Loadable::Failed(failure) => {
                left = left
                    .child(
                        Icon::new(IconName::CircleAlert)
                            .size_3()
                            .text_color(theme::DEL_FG),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_color(theme::FG_2)
                            .child(failure.message.clone()),
                    )
                    .child(
                        Button::new("retry-status")
                            .small()
                            .outline()
                            .label("Try again")
                            .on_click(cx.listener(|this, _, _, cx| this.retry(cx))),
                    );
            }
            Loadable::Empty => {
                let text = if self.session.stopped == Stopped::Cancelled {
                    "Cancelled"
                } else if self.session.compare.is_some() {
                    "Ready"
                } else {
                    "Choose what to compare"
                };
                left = left.child(div().flex_shrink_0().text_color(theme::FG_3).child(text));
            }
        }

        // Only what differs from the default is worth the room: the depth
        // the analysis used, and a restricted run.
        if self.session.params.depth != ui::default_depth() {
            right = right
                .child(div().text_color(theme::FG_3).child("depth"))
                .child(
                    div()
                        .font_family(theme::mono())
                        .text_color(theme::FG_2)
                        .child(self.session.params.depth.to_string()),
                )
                .child(status_separator());
        }
        if self.session.exec() == blastr_model::ExecMode::Restricted {
            right = right.child(div().text_color(theme::FG_3).child("Restricted mode"));
        }

        h_flex()
            .h(px(26.))
            .flex_shrink_0()
            .px_2()
            .gap_2()
            .items_center()
            .bg(theme::BG_BAR)
            .border_t_1()
            .border_color(theme::LINE)
            .text_size(TEXT_SMALL)
            .text_color(theme::FG_3)
            .child(left)
            .child(right)
            .into_any_element()
    }

    /// The palette: a scrim, a query field and the matches.
    fn palette_layer(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(palette) = &self.palette else {
            return div().into_any_element();
        };
        let hits: Vec<AnyElement> = palette
            .hits
            .iter()
            .enumerate()
            .take(80)
            .map(|(index, hit)| {
                let Some(entry) = self.search.get(hit.entry) else {
                    return div().into_any_element();
                };
                let (before, name) = search::split_name(&entry.symbol.path);
                let selected = index == palette.selected;
                h_flex()
                    .id(SharedString::from(format!("palette-{index}")))
                    .h(px(34.))
                    .px_2()
                    .gap_2()
                    .items_center()
                    .rounded(px(6.))
                    .bg(if selected {
                        theme::BG_ACTIVE
                    } else {
                        theme::BG_RAISED
                    })
                    .cursor_pointer()
                    .on_click(cx.listener({
                        let symbol = entry.symbol.clone();
                        let site = entry.location.clone();
                        move |this, _, _, cx| {
                            this.close_palette(cx);
                            this.select_symbol(symbol.clone(), Some(site.clone()), cx);
                        }
                    }))
                    .child(
                        div()
                            .w(px(40.))
                            .text_size(px(10.5))
                            .text_color(theme::FG_4)
                            .child(left::kind_label(entry.symbol.kind)),
                    )
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .text_size(TEXT_SMALL)
                            .child(div().text_color(theme::FG_3).child(before.to_owned()))
                            .child(div().text_color(theme::CHANGED.fg).child(name.to_owned())),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(theme::FG_4)
                            .child(wording::site_text(&entry.location)),
                    )
                    .into_any_element()
            })
            .collect();

        let matches = if hits.is_empty() {
            div()
                .p_5()
                .w_full()
                .text_center()
                .text_size(TEXT_SMALL)
                .text_color(theme::FG_3)
                .child(if self.search.is_empty() {
                    "The symbol index is still being built."
                } else {
                    "No symbol matches."
                })
                .into_any_element()
        } else {
            v_flex()
                .id("palette-matches")
                .size_full()
                .p_1()
                .gap_0()
                .max_h(px(380.))
                .overflow_y_scroll()
                .children(hits)
                .into_any_element()
        };

        let panel = v_flex()
            .w(px(620.))
            .mt(px(84.))
            .rounded(px(10.))
            .border_1()
            .border_color(theme::LINE_STRONG)
            .bg(theme::BG_RAISED)
            .overflow_hidden()
            .child(
                h_flex()
                    .h(px(44.))
                    .px_3()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(theme::LINE)
                    .child(Icon::new(IconName::Search).size_4().text_color(theme::FG_3))
                    .child(Input::new(&self.palette_input).small().w_full()),
            )
            .child(matches)
            .child(
                h_flex()
                    .h(px(32.))
                    .px_3()
                    .gap_4()
                    .items_center()
                    .border_t_1()
                    .border_color(theme::LINE)
                    .bg(theme::BG_PANEL)
                    .text_size(px(11.5))
                    .text_color(theme::FG_3)
                    .child("↑ ↓ to move")
                    .child("Enter to select")
                    .child("Esc to close")
                    .child(div().flex_1())
                    .child(format!("{} symbols", self.search.len())),
            );

        let scrim = div()
            .absolute()
            .inset_0()
            .flex()
            .justify_center()
            .items_start()
            .bg(theme::SCRIM)
            // The palette owns the window while it is open: the pointer cannot
            // reach anything behind it, and `Tab` stays inside it.
            .key_context(MODAL_CONTEXT)
            .track_focus(&self.palette_scope)
            .focus_trap("palette", &self.palette_scope)
            .occlude()
            .capture_key_down(
                cx.listener(|this, event: &gpui_kit::KeyDownEvent, _window, cx| {
                    match event.keystroke.key.as_str() {
                        "up" => {
                            this.step_palette(false);
                            cx.stop_propagation();
                            cx.notify();
                        }
                        "down" => {
                            this.step_palette(true);
                            cx.stop_propagation();
                            cx.notify();
                        }
                        "enter" => {
                            this.confirm_palette(cx);
                            cx.stop_propagation();
                        }
                        "escape" => {
                            this.close_palette(cx);
                            cx.stop_propagation();
                        }
                        _ => {}
                    }
                }),
            )
            .child(
                div()
                    .on_mouse_down(
                        gpui_kit::MouseButton::Left,
                        cx.listener(|_, _, _, cx| cx.stop_propagation()),
                    )
                    .child(panel),
            );
        // Clicking the scrim itself closes the palette.
        div()
            .absolute()
            .inset_0()
            .occlude()
            .on_scroll(cx.listener(|_, _, _, cx| cx.stop_propagation()))
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, _, cx| this.close_palette(cx)),
            )
            .child(scrim)
            .into_any_element()
    }

    /// The one-time notice before build scripts and procedural macros run.
    fn trust_layer(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(root) = &self.pending_trust else {
            return div().into_any_element();
        };
        let name = crate::settings::repository_name(root);
        let panel = v_flex()
            .w(px(560.))
            .mt(px(120.))
            .p_4()
            .gap_3()
            .rounded(px(10.))
            .border_1()
            .border_color(theme::LINE_STRONG)
            .bg(theme::BG_RAISED)
            .child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .child(ui::glyph(IconName::TriangleAlert, Some(theme::EDIT)))
                    .child(
                        v_flex()
                            .child(
                                div()
                                    .text_size(px(15.))
                                    .text_color(theme::FG)
                                    .child("Before this repository is analysed"),
                            )
                            .child(
                                div()
                                    .text_size(TEXT_SMALL)
                                    .text_color(theme::FG_3)
                                    .child(name),
                            ),
                    ),
            )
            .child(div().text_size(TEXT_UI).text_color(theme::FG_2).child(
                "Loading this Cargo project runs its build scripts and procedural \
                         macros: ordinary programs taken from the repository. Blastr does not \
                         sandbox them, and neither mode is promised to be safe for a repository \
                         you do not trust.",
            ))
            .child(div().text_size(TEXT_SMALL).text_color(theme::FG_3).child(
                "Open restricted skips both; code they generate is then invisible and \
                         every result is marked as reduced coverage. This answer is remembered \
                         for this repository.",
            ))
            .child(
                h_flex()
                    .gap_2()
                    .justify_end()
                    .child(
                        Button::new("open-restricted")
                            .small()
                            .outline()
                            .label("Open restricted")
                            .on_click(cx.listener(|this, _, _, cx| this.answer_trust(true, cx))),
                    )
                    .child(
                        Button::new("continue")
                            .small()
                            .primary()
                            .track_focus(&self.trust_primary)
                            .label("Continue")
                            .on_click(cx.listener(|this, _, _, cx| this.answer_trust(false, cx))),
                    ),
            );

        div()
            .id("trust-layer")
            .absolute()
            .inset_0()
            .flex()
            .justify_center()
            .items_start()
            .bg(theme::SCRIM)
            // The notice covers the whole window, its title bar included, and
            // keeps the keyboard until one of its two answers is given.
            .key_context(MODAL_CONTEXT)
            .track_focus(&self.trust_scope)
            .focus_trap("trust", &self.trust_scope)
            .occlude()
            .on_scroll(cx.listener(|_, _, _, cx| cx.stop_propagation()))
            .child(panel)
            .into_any_element()
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let bounds = window.bounds();
        let placement = WindowPlacement {
            x: f32::from(bounds.origin.x),
            y: f32::from(bounds.origin.y),
            width: f32::from(bounds.size.width),
            height: f32::from(bounds.size.height),
            maximized: window.is_maximized(),
        };
        if self.placement.get() != Some(placement) {
            self.placement.set(Some(placement));
            self.settings_dirty.set(true);
        }
        // The window's shortcuts are bound in the `Blastr` key context, which
        // only exists along the focused element's path: with nothing focused
        // (at start-up, or after clicking something that takes no focus) no
        // shortcut would fire. The root takes focus whenever nothing has it.
        if self.refocus || (window.focused(cx).is_none() && !self.modal_open()) {
            self.refocus = false;
            window.focus(&self.focus_handle, cx);
        }
        if self.restore_focus {
            self.restore_focus = false;
            self.give_back_focus(window, cx);
        }
        self.sync_modal_focus(window, cx);

        let content: AnyElement = if self.session.repo.is_some() {
            self.body(cx)
        } else {
            self.start_screen(cx)
        };

        let mut root =
            v_flex()
                .key_context("Blastr")
                .size_full()
                .bg(theme::BG_CANVAS)
                .text_color(theme::FG)
                .text_size(TEXT_UI)
                .track_focus(&self.focus_handle)
                // The overlay that is open, if any, owns the keyboard: every
                // shortcut of this window asks before it acts.
                .on_action(cx.listener(|this, _: &OpenRepository, _, cx| {
                    if this.shortcut_allowed() {
                        this.pick_folder(cx);
                    }
                }))
                .on_action(cx.listener(|this, _: &OpenPalette, window, cx| {
                    if this.shortcut_allowed() {
                        this.open_palette(window, cx);
                    }
                }))
                .on_action(cx.listener(|this, _: &Refresh, _, cx| {
                    if this.shortcut_allowed() {
                        this.retry(cx);
                    }
                }))
                .on_action(cx.listener(|this, _: &ShowGraph, _, cx| {
                    if this.shortcut_allowed() {
                        this.tab = Tab::Graph;
                        cx.notify();
                    }
                }))
                .on_action(cx.listener(|this, _: &ShowCode, _, cx| {
                    if this.shortcut_allowed() {
                        this.tab = Tab::Code;
                        cx.notify();
                    }
                }))
                .on_action(cx.listener(|this, _: &ShowDiff, _, cx| {
                    if this.shortcut_allowed() {
                        this.tab = Tab::Diff;
                        cx.notify();
                    }
                }))
                .on_action(cx.listener(|this, _: &FocusFilter, window, cx| {
                    if this.shortcut_allowed() {
                        window.focus(&this.filter_input.read(cx).focus_handle(cx), cx);
                        this.focus = Focus::Outline;
                        cx.notify();
                    }
                }))
                .on_action(cx.listener(|this, _: &Back, _, cx| {
                    if this.shortcut_allowed() {
                        this.back(cx);
                    }
                }))
                .on_action(cx.listener(|this, _: &Forward, _, cx| {
                    if this.shortcut_allowed() {
                        this.forward(cx);
                    }
                }))
                .on_action(cx.listener(|this, _: &OpenEditor, _, cx| {
                    if this.shortcut_allowed() {
                        this.open_in_editor(cx);
                    }
                }))
                .on_action(cx.listener(|this, _: &OpenSettings, window, cx| {
                    if this.shortcut_allowed() {
                        this.open_settings(window, cx);
                    }
                }))
                // `Esc` is the one key an overlay leaves to the window: it closes
                // the overlay that is on top.
                .on_action(cx.listener(|this, _: &Dismiss, window, cx| {
                    if this.cancel_or_close(cx) {
                        cx.notify();
                    }
                    if !this.modal_open() {
                        window.focus(&this.focus_handle, cx);
                    }
                }))
                .on_action(
                    cx.listener(|this, _: &WhatIfBody, _, cx| this.begin_what_if(Assume::Body, cx)),
                )
                .on_action(cx.listener(|this, _: &WhatIfSignature, _, cx| {
                    this.begin_what_if(Assume::Signature, cx)
                }))
                .on_action(cx.listener(|this, _: &WhatIfRemoved, _, cx| {
                    this.begin_what_if(Assume::Removed, cx)
                }))
                .on_action(cx.listener(|this, _: &WhatIfVisibility, _, cx| {
                    this.begin_what_if(Assume::Visibility, cx)
                }))
                .on_action(cx.listener(|this, _: &WhatIfFieldRemoved, _, cx| {
                    this.begin_what_if(Assume::FieldRemoved, cx)
                }))
                .on_action(cx.listener(|this, _: &WhatIfFieldRetyped, _, cx| {
                    this.begin_what_if(Assume::FieldRetyped, cx)
                }))
                .on_action(cx.listener(|this, _: &WhatIfFieldAdded, _, cx| {
                    this.begin_what_if(Assume::FieldAdded, cx)
                }))
                .on_action(cx.listener(|this, _: &WhatIfVariantAdded, _, cx| {
                    this.begin_what_if(Assume::VariantAdded, cx)
                }))
                .on_action(cx.listener(|this, _: &WhatIfVariantRemoved, _, cx| {
                    this.begin_what_if(Assume::VariantRemoved, cx)
                }))
                .on_key_down(
                    cx.listener(|this, event: &gpui_kit::KeyDownEvent, window, cx| {
                        // A modal, or a field that is being typed into, keeps the
                        // arrow keys and `Enter` to itself.
                        if this.modal_open() || this.typing(window, cx) {
                            return;
                        }
                        match event.keystroke.key.as_str() {
                            "up" => this.move_focus(false, cx),
                            "down" => this.move_focus(true, cx),
                            "enter" => this.confirm_focus(window, cx),
                            _ => {}
                        }
                    }),
                )
                .child(self.title_bar(f32::from(bounds.size.width), cx));

        if self.session.is_hypothetical() {
            root = root.child(self.whatif_banner(cx));
        }
        root = root
            .child(div().flex_1().min_h_0().child(content))
            .child(self.status_bar(cx));
        if self.palette.is_some() {
            root = root.child(self.palette_layer(cx));
        }
        if self.pending_trust.is_some() {
            root = root.child(self.trust_layer(cx));
        }
        if self.settings_panel.is_some() {
            root = root.child(self.settings_layer(cx));
        }
        if let Some((text, _)) = &self.toast {
            root = root.child(toast_layer(text));
        }
        root
    }
}

/// The width the platform's window controls take at the right end of the
/// title bar.
///
/// The component library draws minimise, maximise and close on every desktop
/// but macOS and the web, each one as wide as the bar itself; the window draws
/// the bar and the platform draws these, so the two never overlap only if the
/// bar knows how much they take.
fn window_controls_width() -> f32 {
    if cfg!(any(target_os = "macos", target_family = "wasm")) {
        0.0
    } else {
        3.0 * f32::from(gpui_kit::component::TITLE_BAR_HEIGHT)
    }
}

/// The short-lived message above the status bar.
fn toast_layer(text: &str) -> AnyElement {
    div()
        .absolute()
        .left_0()
        .right_0()
        .bottom(px(34.))
        .flex()
        .justify_center()
        .child(
            div()
                .max_w(px(760.))
                .px_3()
                .py_2()
                .rounded(theme::RADIUS_MD)
                .border_1()
                .border_color(theme::LINE_STRONG)
                .bg(theme::BG_RAISED_2)
                .text_size(TEXT_SMALL)
                .text_color(theme::FG_2)
                .child(text.to_owned()),
        )
        .into_any_element()
}

/// Whether a comparison that took `elapsed` came from the engine's store.
///
/// The window cannot ask the engine whether it read the graphs it already had,
/// so it reads the clock: a comparison that finishes in a fraction of a second
/// did not load the project again. Anything slower is reported as the time it
/// took, which is true either way.
fn came_from_cache(elapsed: Duration) -> bool {
    elapsed < Duration::from_millis(250)
}

/// The thin rule between two status bar groups.
fn status_separator() -> AnyElement {
    div()
        .w(px(1.))
        .h(px(12.))
        .flex_shrink_0()
        .bg(theme::LINE_STRONG)
        .into_any_element()
}

/// A progress bar of known size.
fn progress_bar(fraction: f32) -> AnyElement {
    div()
        .w(px(150.))
        .h(px(4.))
        .rounded(px(2.))
        .bg(theme::BG_RAISED_2)
        .overflow_hidden()
        .child(
            div()
                .w(px(150. * fraction.clamp(0.0, 1.0)))
                .h_full()
                .rounded(px(2.))
                .bg(theme::CHANGED.solid),
        )
        .into_any_element()
}

/// The count of a phase whose total is unknown.
fn progress_text(progress: &Progress) -> AnyElement {
    div()
        .text_size(px(11.))
        .text_color(theme::FG_4)
        .child(match progress.total {
            Some(total) => format!("{}/{total}", progress.done),
            None => format!("{}", progress.done),
        })
        .into_any_element()
}

/// The current time in seconds since the Unix epoch.
pub(crate) fn unix_now() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| i64::try_from(elapsed.as_secs()).unwrap_or(i64::MAX))
        .unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_is_read_from_the_clock() {
        let now = unix_now();
        assert!(now > 1_700_000_000, "{now}");
    }

    #[test]
    fn a_quick_result_is_reported_as_cached() {
        assert!(came_from_cache(Duration::from_millis(120)));
        assert!(!came_from_cache(Duration::from_millis(1200)));
        assert!(!came_from_cache(Duration::from_secs_f64(6.67)));
    }

    #[test]
    fn each_tab_has_a_label_an_icon_and_a_key() {
        for tab in Tab::ALL {
            assert!(!tab.label().is_empty());
            assert!(tab.key().starts_with("Ctrl"));
        }
        assert_eq!(Tab::ALL.len(), 3);
    }

    #[test]
    fn the_modal_drawn_last_is_the_one_on_top() {
        assert_eq!(Modals::default().top(), None);
        assert_eq!(
            Modals {
                palette: true,
                ..Modals::default()
            }
            .top(),
            Some(Modal::Palette)
        );
        assert_eq!(
            Modals {
                palette: true,
                trust: true,
                settings: false
            }
            .top(),
            Some(Modal::Trust)
        );
        assert_eq!(
            Modals {
                palette: true,
                trust: true,
                settings: true
            }
            .top(),
            Some(Modal::Settings)
        );
    }

    #[test]
    fn escape_closes_the_palette_but_never_answers_the_trust_notice() {
        // One answer per overlay, the one on top first.
        assert_eq!(
            Modals {
                palette: true,
                trust: true,
                settings: true
            }
            .escape(),
            Some(Modal::Settings)
        );
        assert_eq!(
            Modals {
                palette: true,
                trust: true,
                settings: false
            }
            .escape(),
            Some(Modal::Palette)
        );
        // Running the repository's own build scripts is a decision with no
        // safe default: `Esc` leaves the notice and its two buttons in place.
        assert_eq!(
            Modals {
                trust: true,
                ..Modals::default()
            }
            .escape(),
            None
        );
        assert_eq!(Modals::default().escape(), None);
    }

    #[test]
    fn any_overlay_owns_the_window() {
        assert!(!Modals::default().any());
        assert!(
            Modals {
                palette: true,
                ..Modals::default()
            }
            .any()
        );
        assert!(
            Modals {
                trust: true,
                ..Modals::default()
            }
            .any()
        );
        assert!(
            Modals {
                settings: true,
                ..Modals::default()
            }
            .any()
        );
    }

    #[test]
    fn the_title_bar_shares_the_bar_between_the_search_and_the_chips() {
        let design = BarRoom::at(1280.);
        let narrow = BarRoom::at(900.);
        let wide = BarRoom::at(1920.);

        // The search grows with the bar, within bounds, and gives way first.
        assert!(f32::from(design.search) > f32::from(narrow.search));
        assert!(f32::from(wide.search) >= f32::from(design.search));
        assert!(f32::from(narrow.search) >= 240.0);
        assert!(f32::from(wide.search) <= 400.0);

        // The chips get what is left of the left half, and never run under
        // the search.
        assert!(f32::from(design.chips) > 380.0);
        assert!(f32::from(design.chips) < f32::from(design.search) * 2.0);
        assert!(f32::from(narrow.chips) < f32::from(design.chips));

        // The comparison chip is dropped before the chips have to clip.
        assert!(design.comparison);
        assert!(!narrow.comparison);
    }
}
