//! The root view: the title bar, the three columns, the status bar, and the
//! dialogs and notices the workspace asks for.

use std::cell::Cell;
use std::path::PathBuf;
use std::rc::Rc;

use blastr_model::protocol::Failure;
use blastr_model::{Assume, ExecMode, Limit, wording};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::command::{Command, CommandItem, CommandState};
use gpui_kit::component::notification::Notification;
use gpui_kit::component::resizable::{h_resizable, resizable_panel, v_resizable};
use gpui_kit::component::status_bar::StatusBar;
use gpui_kit::component::{Icon, IconName, Sizable as _, TitleBar, WindowExt as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Entity, FocusHandle, IntoElement, ParentElement, Render, Styled,
    Subscription, Window, div, px,
};

use super::code::CodePane;
use super::evidence::EvidencePanel;
use super::graph::GraphView;
use super::left::LeftPane;
use super::ui;
use crate::settings::WindowPlacement;
use crate::state::loadable::{Loadable, STAGES, elapsed_text};
use crate::state::search::split_name;
use crate::theme::{self, TEXT_MICRO, TEXT_SMALL, TEXT_UI};
use crate::workspace::{Workspace, WorkspaceEvent};

gpui_kit::actions!(
    blastr,
    [
        OpenRepository,
        OpenPalette,
        Refresh,
        Back,
        Forward,
        OpenEditor,
        OpenSettings,
        Cancel,
        FocusFilter,
        ToggleCode,
        CodeZoomIn,
        CodeZoomOut,
        CodeZoomReset,
    ]
);

/// Left edge at or past which a window is minimised (Windows places it at -32000).
const MINIMISED_X: f32 = -30000.;

/// The root view of the main window.
pub(crate) struct Shell {
    pub(crate) workspace: Entity<Workspace>,
    left: Entity<LeftPane>,
    graph: Entity<GraphView>,
    code: Entity<CodePane>,
    evidence: Entity<EvidencePanel>,
    palette: Entity<CommandState>,
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    pub(crate) fn new(
        repository: Option<PathBuf>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Shell {
        let workspace = cx.new(Workspace::new);
        let left = cx.new(|cx| LeftPane::new(workspace.clone(), window, cx));
        let graph = cx.new(|cx| GraphView::new(workspace.clone(), cx));
        let code = cx.new(|cx| CodePane::new(workspace.clone(), window, cx));
        let evidence = cx.new(|cx| EvidencePanel::new(workspace.clone(), cx));
        let palette = cx.new(|cx| CommandState::new(window, cx));
        let subscriptions = vec![
            cx.observe(&workspace, |_, _, cx| cx.notify()),
            cx.observe(&code, |_, _, cx| cx.notify()),
            cx.subscribe_in(&workspace, window, Self::on_event),
            cx.observe_window_bounds(window, |this, window, cx| this.remember_bounds(window, cx)),
        ];
        let focus = cx.focus_handle();
        window.focus(&focus, cx);
        if let Some(path) = repository {
            workspace.update(cx, |workspace, cx| workspace.open_path(path, cx));
        }
        Shell {
            workspace,
            left,
            graph,
            code,
            evidence,
            palette,
            focus,
            _subscriptions: subscriptions,
        }
    }

    fn on_event(
        &mut self,
        _: &Entity<Workspace>,
        event: &WorkspaceEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match event {
            WorkspaceEvent::Notice(text) => {
                window.push_notification(Notification::info(text.clone()), cx);
            }
            WorkspaceEvent::Problem(text) => {
                window.push_notification(Notification::error(text.clone()), cx);
            }
            WorkspaceEvent::TrustNeeded => self.open_trust(window, cx),
        }
    }

    /// Keeps the window's place for the next start.
    fn remember_bounds(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        // Windows parks a minimised window at (-32000, -32000); that is not
        // a place to reopen at, and minimising changes nothing worth keeping.
        if f32::from(window.bounds().origin.x) <= MINIMISED_X {
            return;
        }
        // The bounds the window restores to, also while it is maximised.
        let (bounds, maximized) = match window.window_bounds() {
            gpui_kit::WindowBounds::Maximized(bounds) => (bounds, true),
            gpui_kit::WindowBounds::Windowed(bounds)
            | gpui_kit::WindowBounds::Fullscreen(bounds) => (bounds, false),
        };
        let placement = WindowPlacement {
            x: f32::from(bounds.origin.x),
            y: f32::from(bounds.origin.y),
            width: f32::from(bounds.size.width),
            height: f32::from(bounds.size.height),
            maximized,
        };
        self.workspace.update(cx, |workspace, cx| {
            workspace.settings.window = Some(placement);
            workspace.schedule_save(cx);
        });
    }

    /// Opens the folder picker, then the repository it returns.
    pub(crate) fn pick_folder(&mut self, cx: &mut Context<Self>) {
        let paths = cx.prompt_for_paths(gpui_kit::PathPromptOptions {
            files: false,
            directories: true,
            multiple: false,
            prompt: Some("Open a repository".into()),
        });
        let workspace = self.workspace.clone();
        cx.spawn(async move |_, cx| {
            if let Ok(Ok(Some(paths))) = paths.await
                && let Some(path) = paths.into_iter().next()
            {
                workspace.update(cx, |workspace, cx| workspace.open_path(path, cx));
            }
        })
        .detach();
    }

    // -- dialogs -----------------------------------------------------------

    /// Asks before a repository's build scripts and procedural macros run.
    ///
    /// The question cannot be dismissed with `Esc` or a click beside it: one
    /// of its two answers has to be given.
    fn open_trust(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace.clone();
        let name = workspace
            .read(cx)
            .pending_trust
            .as_ref()
            .and_then(|root| root.file_name())
            .map(|name| name.to_string_lossy().into_owned())
            .unwrap_or_default();
        window.open_dialog(cx, move |dialog, _, _| {
            let answer = |restricted: bool| {
                let workspace = workspace.clone();
                move |_: &gpui_kit::ClickEvent, window: &mut Window, cx: &mut gpui_kit::App| {
                    workspace.update(cx, |workspace, cx| workspace.answer_trust(restricted, cx));
                    window.close_dialog(cx);
                }
            };
            dialog
                .w(px(540.))
                .keyboard(false)
                .overlay_closable(false)
                .close_button(false)
                .title(
                    h_flex()
                        .gap_3()
                        .items_center()
                        .child(ui::glyph(IconName::TriangleAlert, Some(theme::EDIT)))
                        .child(
                            v_flex()
                                .child(div().text_size(px(15.)).child("Before this repository is analysed"))
                                .child(div().text_size(TEXT_SMALL).text_color(theme::FG_3).child(name.clone())),
                        ),
                )
                .child(
                    v_flex()
                        .gap_3()
                        .child(div().text_size(TEXT_UI).text_color(theme::FG_2).child(
                            "Loading this Cargo project runs its build scripts and procedural macros: \
                             ordinary programs taken from the repository. Blastr does not sandbox them.",
                        ))
                        .child(div().text_size(TEXT_SMALL).text_color(theme::FG_3).child(
                            "Open restricted skips both; code they generate is then invisible and the \
                             result says so. The answer is remembered for this repository.",
                        )),
                )
                .footer(
                    h_flex()
                        .w_full()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("open-restricted")
                                .outline()
                                .label("Open restricted")
                                .on_click(answer(true)),
                        )
                        .child(
                            Button::new("trust-continue")
                                .primary()
                                .label("Continue")
                                .on_click(answer(false)),
                        ),
                )
        });
    }

    /// Opens the symbol search.
    fn open_palette(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.workspace.read(cx).session.repo.is_none() {
            return;
        }
        let state = self.palette.clone();
        state.update(cx, |state, cx| state.set_query("", window, cx));
        let workspace = self.workspace.clone();
        let first = Rc::new(Cell::new(true));
        window.open_dialog(cx, move |dialog, window, cx| {
            if first.replace(false) {
                state.update(cx, |state, cx| state.focus(window, cx));
            }
            let query = state.read(cx).query(cx);
            let index = &workspace.read(cx).search;
            let hits: Vec<_> = index
                .search(&query, 60)
                .into_iter()
                .filter_map(|hit| index.get(hit.entry).cloned())
                .collect();
            let items = hits.iter().map(|entry| {
                let (module, name) = split_name(&entry.symbol.path);
                let (module, name) = (module.to_owned(), name.to_owned());
                let kind = entry.symbol.kind.as_str();
                let site = wording::site_text(&entry.location);
                CommandItem::new()
                    .label(entry.symbol.path.clone())
                    .child(move |_, _| {
                        h_flex()
                            .w_full()
                            .gap_2()
                            .items_center()
                            .text_size(TEXT_SMALL)
                            .child(
                                div()
                                    .w(px(56.))
                                    .text_size(TEXT_MICRO)
                                    .text_color(theme::FG_4)
                                    .child(kind),
                            )
                            .child(
                                h_flex()
                                    .flex_1()
                                    .min_w_0()
                                    .overflow_hidden()
                                    .child(div().text_color(theme::FG_3).child(module.clone()))
                                    .child(div().text_color(theme::CHANGED.fg).child(name.clone())),
                            )
                            .child(
                                div()
                                    .text_size(TEXT_MICRO)
                                    .text_color(theme::FG_4)
                                    .child(site.clone()),
                            )
                    })
            });
            let chosen = workspace.clone();
            let empty = if index.is_empty() {
                "The symbol index arrives with the first result."
            } else if query.trim().is_empty() {
                "Type part of a symbol's name."
            } else {
                "No symbol matches."
            };
            dialog.w(px(640.)).p_0().close_button(false).child(
                Command::new(&state)
                    .filterable(false)
                    .bordered(false)
                    .placeholder("Find a symbol")
                    .max_h(px(380.))
                    .items(items)
                    .empty(move |_, _, _| {
                        div()
                            .p_4()
                            .text_size(TEXT_SMALL)
                            .text_color(theme::FG_3)
                            .child(empty)
                    })
                    .on_query(|_, window, _| window.refresh())
                    .on_confirm(move |index, window, cx| {
                        if let Some(entry) = hits.get(index.row) {
                            let (symbol, site) = (entry.symbol.clone(), entry.location.clone());
                            chosen.update(cx, |workspace, cx| {
                                workspace.select(symbol, Some(site), cx)
                            });
                        }
                        window.close_dialog(cx);
                    }),
            )
        });
    }

    fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        super::settings::open(self.workspace.clone(), window, cx);
    }

    // -- the frame ---------------------------------------------------------

    fn title_bar(&self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let compare = super::commits::compare_chip(&self.workspace, window, cx);
        let workspace = self.workspace.read(cx);
        let session = &workspace.session;
        let mut chips = h_flex().min_w_0().items_center().gap(px(6.));
        if let Some(repo) = &session.repo {
            chips = chips
                .child(
                    chip("repo-chip", IconName::FolderOpen)
                        .tooltip(|window, cx| {
                            gpui_kit::component::tooltip::Tooltip::new(
                                "Open another repository  Ctrl O",
                            )
                            .build(window, cx)
                        })
                        .on_click(cx.listener(|this, _, _, cx| this.pick_folder(cx)))
                        .child(
                            div()
                                .min_w_0()
                                .truncate()
                                .text_color(theme::FG)
                                .child(repo.name.clone()),
                        )
                        .children(repo.branch.clone().map(|branch| {
                            h_flex()
                                .gap_1()
                                .child(div().text_color(theme::FG_4).child("/"))
                                .child(div().text_color(theme::FG_2).child(branch))
                        })),
                )
                .child(compare);
        }
        let ready = session.repo.is_some();
        let busy = session.is_busy();
        let refreshable = ready && session.compare.is_some() && !busy;
        let search = ui::no_drag(h_flex().id("symbol-search"))
            .flex_none()
            .w(px(300.))
            .h(px(28.))
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
                    .text_color(if ready { theme::FG_3 } else { theme::FG_4 })
                    .child("Search symbols"),
            )
            .when(ready, |search| {
                search
                    .cursor_pointer()
                    .hover(|style| style.border_color(theme::LINE_STRONG).bg(theme::BG_RAISED))
                    .on_click(cx.listener(|this, _, window, cx| this.open_palette(window, cx)))
                    .child(ui::keycap("Ctrl P", cx))
            });
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
        )
        .when(refreshable, |button| {
            button.on_click(cx.listener(|this, _, _, cx| {
                this.workspace
                    .update(cx, |workspace, cx| workspace.retry(cx));
            }))
        });
        TitleBar::new()
            .h(px(40.))
            .bg(theme::BG_BAR)
            .border_b_1()
            .border_color(theme::LINE)
            .child(
                h_flex()
                    .size_full()
                    .min_w_0()
                    .items_center()
                    .child(
                        h_flex()
                            .flex_1()
                            .min_w_0()
                            .gap(px(6.))
                            .items_center()
                            .child(ui::logo(px(18.)))
                            .child(div().flex_none().text_size(px(14.)).child("Blastr"))
                            .child(div().flex_none().w(px(10.)))
                            .child(chips),
                    )
                    .child(search)
                    .child(
                        h_flex()
                            .flex_1()
                            .justify_end()
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
                                .on_click(
                                    cx.listener(|this, _, window, cx| {
                                        this.open_settings(window, cx)
                                    }),
                                ),
                            )
                            .child(ui::toolbar_divider()),
                    ),
            )
            .into_any_element()
    }

    fn status_bar(&self, cx: &mut Context<Self>) -> AnyElement {
        let workspace = self.workspace.read(cx);
        let session = &workspace.session;
        let mut left = h_flex().min_w_0().gap_2().items_center();
        let mut right = h_flex().flex_none().gap_2().items_center();
        let icon =
            |name: IconName, colour: theme::Tone| Icon::new(name).size_3().text_color(colour);
        match session.shown() {
            Loadable::Loading(progress) => {
                left =
                    left.child(ui::spinner())
                        .child(div().text_color(theme::FG_2).child(progress.label()))
                        .children(progress.fraction().map(progress_bar))
                        .children(session.running_for().map(|elapsed| {
                            div().text_color(theme::FG_4).child(elapsed_text(elapsed))
                        }))
                        .child(
                            ui::icon_button("cancel", IconName::Close, "Cancel  Esc").on_click(
                                cx.listener(|this, _, _, cx| {
                                    this.workspace
                                        .update(cx, |workspace, cx| workspace.cancel(cx));
                                }),
                            ),
                        );
            }
            Loadable::Ready(outcome) => {
                let text = if session.refreshing.is_some() {
                    "Refreshing after a save"
                } else if session.is_watching() {
                    "Up to date · refreshes on save"
                } else {
                    "Up to date"
                };
                left = left
                    .child(icon(IconName::CircleCheck, theme::TEST.fg))
                    .child(div().text_color(theme::FG_2).child(text));
                let unresolved = outcome.analysis.limits.unresolved_total;
                if unresolved > 0 {
                    let noun = wording::count_noun(unresolved, "reference", "references");
                    left = left
                        .child(separator())
                        .child(icon(IconName::TriangleAlert, theme::EDIT.fg))
                        .child(format!("{unresolved} unresolved {noun}"));
                }
                let failed = outcome
                    .analysis
                    .diff
                    .limits
                    .iter()
                    .find_map(|limit| match limit {
                        Limit::FailedQueries { count } => Some(*count),
                        _ => None,
                    });
                if let Some(failed) = failed {
                    let noun = wording::count_noun(failed, "query", "queries");
                    left = left
                        .child(separator())
                        .child(icon(IconName::TriangleAlert, theme::EDIT.fg))
                        .child(format!("{failed} failed rust-analyzer {noun}"));
                }
                let elapsed = outcome.elapsed.as_secs_f64();
                right = right.child(if elapsed < 0.3 {
                    "From cache".to_owned()
                } else {
                    format!("Analysed in {elapsed:.1} s")
                });
            }
            Loadable::Failed(failure) => {
                left = left
                    .child(icon(IconName::CircleAlert, theme::DEL_FG))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .text_color(theme::FG_2)
                            .child(failure.message.clone()),
                    );
            }
            Loadable::Empty => {
                let text = if session.cancelled {
                    "Cancelled"
                } else if session.repo.is_some() {
                    "Choose what to compare"
                } else {
                    "Open a repository to start"
                };
                left = left.child(text);
            }
        }
        if workspace.rerun_needed {
            left = left
                .child(separator())
                .child(icon(IconName::Info, theme::EDIT.fg))
                .child(
                    div()
                        .text_color(theme::EDIT.fg)
                        .child("Settings changed since this ran"),
                )
                .child(
                    Button::new("rerun")
                        .xsmall()
                        .outline()
                        .label("Run again")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.workspace
                                .update(cx, |workspace, cx| workspace.retry(cx));
                        })),
                );
        }
        if session.params.depth != blastr_model::AnalysisParams::default().depth {
            right = right
                .child(separator())
                .child(format!("depth {}", session.params.depth));
        }
        if session.options.exec == ExecMode::Restricted {
            right = right.child(separator()).child("Restricted mode");
        }
        StatusBar::new()
            .h(px(26.))
            .bg(theme::BG_BAR)
            .text_size(TEXT_SMALL)
            .text_color(theme::FG_3)
            .left(left)
            .right(right)
            .into_any_element()
    }

    /// The banner shown while a hypothetical result replaces the comparison.
    fn what_if_banner(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let session = &self.workspace.read(cx).session;
        let what_if = session.what_if.as_ref()?;
        let name = wording::short_name(&what_if.symbol.path).name;
        Some(
            h_flex()
                .h(px(34.))
                .flex_none()
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
                        .child("WHAT IF"),
                )
                .child(
                    div()
                        .text_color(theme::FG)
                        .font_family(theme::mono())
                        .child(name),
                )
                .child(
                    div()
                        .text_color(theme::FG_3)
                        .child(what_if_label(what_if.assume).to_lowercase()),
                )
                .children(
                    what_if
                        .result
                        .progress()
                        .map(|progress| div().text_color(theme::FG_4).child(progress.label())),
                )
                .child(div().flex_1())
                .child(
                    Button::new("leave-what-if")
                        .small()
                        .outline()
                        .label("Back to the comparison")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.workspace
                                .update(cx, |workspace, cx| workspace.leave_what_if(cx));
                        })),
                )
                .into_any_element(),
        )
    }

    /// The centre: the graph over the code, the progress of the comparison
    /// on its way, or the failure.
    fn centre(&self, cx: &mut Context<Self>) -> AnyElement {
        let workspace = self.workspace.read(cx);
        let failure = workspace.session.shown().failure().cloned();
        let loading = workspace.session.shown().is_loading();
        // A refresh keeps the last result on screen and only runs a thin
        // bar along the top of it.
        let refreshing = workspace.session.refreshing.is_some() && !loading;
        let code_height = workspace.settings.panes.code;
        let view = if let Some(failure) = failure {
            failure_card(&failure, cx)
        } else if loading {
            v_flex()
                .size_full()
                .child(div().flex_1().min_h_0().child(self.progress_view(cx)))
                .child(
                    div()
                        .flex_none()
                        .h(px(code_height))
                        .border_t_1()
                        .border_color(theme::LINE)
                        .child(ui::code_skeleton()),
                )
                .into_any_element()
        } else if self.code.read(cx).maximized {
            self.code.clone().into_any_element()
        } else {
            let workspace = self.workspace.clone();
            v_resizable("centre")
                .child(resizable_panel().child(self.graph.clone()))
                .child(
                    resizable_panel()
                        .size(px(code_height))
                        .size_range(px(120.)..px(1600.))
                        .child(self.code.clone()),
                )
                .on_resize(move |state, _, cx| {
                    if let Some(height) = state.read(cx).sizes().get(1).copied() {
                        workspace.update(cx, |workspace, cx| {
                            workspace.settings.panes.code = f32::from(height);
                            workspace.schedule_save(cx);
                        });
                    }
                })
                .into_any_element()
        };
        v_flex()
            .size_full()
            .min_w_0()
            .bg(theme::BG_CANVAS)
            .children(self.what_if_banner(cx))
            .when(refreshing, |centre| {
                centre.child(
                    div()
                        .flex_none()
                        .child(ui::indeterminate_bar("refresh-bar", px(2.))),
                )
            })
            .child(div().flex_1().min_h_0().child(view))
            .into_any_element()
    }

    /// What the centre shows while the comparison is on its way: what is
    /// compared, the stages, the phase and its step, how far it is, how long
    /// it has run, and a way to stop it.
    fn progress_view(&self, cx: &mut Context<Self>) -> AnyElement {
        let session = &self.workspace.read(cx).session;
        let progress = session.shown().progress().cloned().unwrap_or_default();
        let elapsed = session.running_for().map(elapsed_text);
        let what = match (&session.what_if, &session.compare) {
            (Some(what_if), _) => {
                format!("What if {}", wording::short_name(&what_if.symbol.path).name)
            }
            (None, Some(compare)) => {
                let (base, head) = compare.labels();
                format!("Comparing {base} → {head}")
            }
            (None, None) => "Comparing".to_owned(),
        };
        let stage = progress.stage();
        let stages = STAGES.iter().enumerate().map(|(index, name)| {
            let (dot, text) = match index.cmp(&stage) {
                std::cmp::Ordering::Less => (theme::FG_3, theme::FG_3),
                std::cmp::Ordering::Equal => (theme::CHANGED.solid, theme::FG),
                std::cmp::Ordering::Greater => (theme::LINE_STRONG, theme::FG_4),
            };
            h_flex()
                .gap(px(6.))
                .items_center()
                .when(index > 0, |step| {
                    step.child(div().w(px(18.)).h(px(1.)).bg(theme::LINE_STRONG))
                })
                .child(div().size(px(6.)).rounded(px(3.)).bg(dot))
                .child(div().text_color(text).child(*name))
        });
        let bar = match progress.fraction() {
            Some(fraction) => ui::determinate_bar(fraction, px(4.)),
            None => ui::indeterminate_bar("progress-bar", px(4.)),
        };
        let count = progress
            .total
            .filter(|total| *total > 0)
            .map(|total| format!("{} of {total}", progress.done.min(total)));
        v_flex()
            .size_full()
            .items_center()
            .justify_center()
            .p_6()
            .child(
                v_flex()
                    .w_full()
                    .max_w(px(480.))
                    .gap_3()
                    .p_4()
                    .rounded(theme::RADIUS_LG)
                    .border_1()
                    .border_color(theme::LINE)
                    .bg(theme::BG_RAISED)
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .text_size(TEXT_SMALL)
                            .text_color(theme::FG_3)
                            .child(ui::spinner())
                            .child(div().min_w_0().truncate().child(what))
                            .child(div().flex_1())
                            .children(elapsed.map(|elapsed| {
                                div().flex_none().text_color(theme::FG_3).child(elapsed)
                            })),
                    )
                    .child(
                        v_flex()
                            .gap_1()
                            .child(
                                div()
                                    .text_size(px(15.))
                                    .text_color(theme::FG)
                                    .child(progress.label()),
                            )
                            .child(
                                div()
                                    .min_h(px(16.))
                                    .text_size(TEXT_SMALL)
                                    .text_color(theme::FG_3)
                                    .children(progress.detail()),
                            ),
                    )
                    .child(bar)
                    .child(
                        h_flex()
                            .gap_2()
                            .items_center()
                            .text_size(TEXT_MICRO)
                            .child(h_flex().flex_wrap().gap(px(6.)).children(stages))
                            .child(div().flex_1())
                            .children(count.map(|count| {
                                div().flex_none().text_color(theme::FG_3).child(count)
                            })),
                    )
                    .child(
                        h_flex().justify_end().child(
                            Button::new("progress-cancel")
                                .small()
                                .outline()
                                .label("Cancel")
                                .tooltip("Stop the comparison  Esc")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.workspace
                                        .update(cx, |workspace, cx| workspace.cancel(cx));
                                })),
                        ),
                    ),
            )
            .into_any_element()
    }

    fn body(&self, cx: &mut Context<Self>) -> AnyElement {
        let panes = self.workspace.read(cx).settings.panes;
        let workspace = self.workspace.clone();
        h_resizable("columns")
            .child(
                resizable_panel()
                    .size(px(panes.left))
                    .size_range(px(240.)..px(640.))
                    .child(self.left.clone()),
            )
            .child(resizable_panel().child(self.centre(cx)))
            .child(
                resizable_panel()
                    .size(px(panes.right))
                    .size_range(px(260.)..px(640.))
                    .child(self.evidence.clone()),
            )
            .on_resize(move |state, _, cx| {
                let sizes = state.read(cx).sizes().clone();
                workspace.update(cx, |workspace, cx| {
                    if let (Some(left), Some(right)) = (sizes.first(), sizes.get(2)) {
                        workspace.settings.panes.left = f32::from(*left);
                        workspace.settings.panes.right = f32::from(*right);
                    }
                    workspace.schedule_save(cx);
                });
            })
            .into_any_element()
    }
}

impl Render for Shell {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        // The shortcuts live in the root's key context, which only exists on
        // the focused element's path: the root takes the keyboard whenever
        // nothing else has it.
        if window.focused(cx).is_none() && !window.has_active_dialog(cx) {
            window.focus(&self.focus, cx);
        }
        let open = self.workspace.read(cx).session.repo.is_some();
        let content = if open {
            self.body(cx)
        } else {
            self.start_screen(cx)
        };
        v_flex()
            .key_context("Blastr")
            .track_focus(&self.focus)
            .size_full()
            .bg(theme::BG_CANVAS)
            .text_color(theme::FG)
            .text_size(TEXT_UI)
            .on_action(cx.listener(|this, _: &OpenRepository, _, cx| this.pick_folder(cx)))
            .on_action(
                cx.listener(|this, _: &OpenPalette, window, cx| this.open_palette(window, cx)),
            )
            .on_action(
                cx.listener(|this, _: &OpenSettings, window, cx| this.open_settings(window, cx)),
            )
            .on_action(cx.listener(|this, _: &Refresh, _, cx| {
                this.workspace
                    .update(cx, |workspace, cx| workspace.retry(cx));
            }))
            .on_action(cx.listener(|this, _: &Back, _, cx| {
                this.workspace
                    .update(cx, |workspace, cx| workspace.walk_history(false, cx));
            }))
            .on_action(cx.listener(|this, _: &Forward, _, cx| {
                this.workspace
                    .update(cx, |workspace, cx| workspace.walk_history(true, cx));
            }))
            .on_action(cx.listener(|this, _: &OpenEditor, _, cx| {
                this.workspace
                    .update(cx, |workspace, cx| workspace.open_selection(cx));
            }))
            .on_action(cx.listener(|this, _: &Cancel, _, cx| {
                this.workspace
                    .update(cx, |workspace, cx| workspace.cancel(cx));
            }))
            .on_action(cx.listener(|this, _: &FocusFilter, window, cx| {
                this.left
                    .update(cx, |left, cx| left.focus_filter(window, cx));
            }))
            .on_action(cx.listener(|this, _: &ToggleCode, _, cx| {
                this.code.update(cx, |code, cx| code.toggle_maximized(cx));
            }))
            .on_action(cx.listener(|this, _: &CodeZoomIn, _, cx| {
                this.code
                    .update(cx, |code, cx| code.change_zoom(Some(true), cx));
            }))
            .on_action(cx.listener(|this, _: &CodeZoomOut, _, cx| {
                this.code
                    .update(cx, |code, cx| code.change_zoom(Some(false), cx));
            }))
            .on_action(cx.listener(|this, _: &CodeZoomReset, _, cx| {
                this.code.update(cx, |code, cx| code.change_zoom(None, cx));
            }))
            .child(self.title_bar(window, cx))
            .child(div().flex_1().min_h_0().child(content))
            .child(self.status_bar(cx))
    }
}

/// A chip of the title bar.
fn chip(id: &'static str, icon: IconName) -> gpui_kit::Stateful<gpui_kit::Div> {
    ui::no_drag(h_flex().id(id))
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
        .child(Icon::new(icon).size_3().text_color(theme::FG_3))
}

fn separator() -> AnyElement {
    div()
        .w(px(1.))
        .h(px(12.))
        .bg(theme::LINE_STRONG)
        .into_any_element()
}

fn progress_bar(fraction: f32) -> AnyElement {
    div()
        .w(px(120.))
        .h(px(4.))
        .rounded(px(2.))
        .bg(theme::BG_RAISED_2)
        .child(
            div()
                .h_full()
                .w(px(120. * fraction))
                .rounded(px(2.))
                .bg(theme::CHANGED.solid),
        )
        .into_any_element()
}

/// The card a failed comparison shows in the centre.
fn failure_card(failure: &Failure, cx: &mut Context<Shell>) -> AnyElement {
    let message = failure.message.clone();
    v_flex()
        .size_full()
        .items_center()
        .justify_center()
        .p_6()
        .child(
            v_flex()
                .w_full()
                .max_w(px(560.))
                .gap_3()
                .p_4()
                .rounded(theme::RADIUS_LG)
                .border_1()
                .border_color(theme::LINE)
                .bg(theme::BG_RAISED)
                .child(
                    h_flex()
                        .gap_2()
                        .items_center()
                        .child(
                            Icon::new(IconName::TriangleAlert)
                                .size_4()
                                .text_color(theme::EDIT.fg),
                        )
                        .child(div().text_size(px(14.)).child("The comparison failed")),
                )
                .child(
                    div()
                        .id("failure-message")
                        .max_h(px(220.))
                        .overflow_y_scroll()
                        .text_size(TEXT_SMALL)
                        .text_color(theme::FG_2)
                        .child(message.clone()),
                )
                .children(failure.hint.clone().map(|hint| {
                    div()
                        .p_2()
                        .rounded(theme::RADIUS_MD)
                        .bg(theme::BG_INPUT)
                        .font_family(theme::mono())
                        .text_size(TEXT_MICRO)
                        .text_color(theme::FG_3)
                        .child(hint)
                }))
                .child(
                    h_flex()
                        .gap_2()
                        .justify_end()
                        .child(
                            Button::new("failure-copy")
                                .small()
                                .ghost()
                                .label("Copy details")
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let text = message.clone();
                                    this.workspace
                                        .update(cx, |workspace, cx| workspace.copy(text, cx));
                                })),
                        )
                        .child(
                            Button::new("failure-retry")
                                .small()
                                .primary()
                                .label("Try again")
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.workspace
                                        .update(cx, |workspace, cx| workspace.retry(cx));
                                })),
                        ),
                ),
        )
        .into_any_element()
}

/// What a hypothetical change can assume, with the words of the menu that
/// starts it.
pub(crate) fn what_if_menu() -> [(Assume, &'static str); 9] {
    [
        (Assume::Body, "If its body changes"),
        (Assume::Signature, "If its signature changes"),
        (Assume::Removed, "If it is removed"),
        (Assume::Visibility, "If it becomes private"),
        (Assume::FieldRemoved, "If a field is removed"),
        (Assume::FieldRetyped, "If a field changes its type"),
        (Assume::FieldAdded, "If a field is added"),
        (Assume::VariantAdded, "If a variant is added"),
        (Assume::VariantRemoved, "If a variant is removed"),
    ]
}

/// The words of one assumption.
pub(crate) fn what_if_label(assume: Assume) -> &'static str {
    what_if_menu()
        .into_iter()
        .find(|(known, _)| *known == assume)
        .map_or("", |(_, label)| label)
}
