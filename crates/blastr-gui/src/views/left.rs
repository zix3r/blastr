//! The left column: the changes and their impact, or the tests to run, each a
//! virtual list over the rows the workspace builds.

use std::rc::Rc;

use blastr_model::{
    ChangeDetail, ChangeKind, Confidence, FileStatus, ImpactClass, SymbolKind, wording,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState};
use gpui_kit::component::tab::{Tab, TabBar};
use gpui_kit::component::{Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::component::{VirtualListScrollHandle, v_virtual_list};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Entity, FocusHandle, IntoElement, KeyDownEvent, ParentElement, Render,
    ScrollStrategy, SharedString, Styled, Subscription, Window, div, px, relative, size,
};

use super::ui;
use crate::state::outline::{Block, Row, TestRow, height, package_parts};
use crate::theme::{self, Accent, RADIUS_SM, TEXT_MICRO, TEXT_SMALL};
use crate::workspace::Workspace;

/// The left column.
pub(crate) struct LeftPane {
    workspace: Entity<Workspace>,
    filter: Entity<InputState>,
    tab: usize,
    outline_scroll: VirtualListScrollHandle,
    tests_scroll: VirtualListScrollHandle,
    focus: FocusHandle,
    /// The selection the list last scrolled to.
    shown: Option<blastr_model::SymbolPath>,
    _subscriptions: Vec<Subscription>,
}

impl LeftPane {
    pub(crate) fn new(
        workspace: Entity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> LeftPane {
        let filter =
            cx.new(|cx| InputState::new(window, cx).placeholder("Filter changes and impact"));
        let subscriptions = vec![
            cx.subscribe(&filter, |this, input, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    this.workspace
                        .update(cx, |workspace, cx| workspace.set_filter_text(text, cx));
                }
            }),
            cx.observe(&workspace, |this, _, cx| {
                this.follow_selection(cx);
                cx.notify();
            }),
        ];
        LeftPane {
            workspace,
            filter,
            tab: 0,
            outline_scroll: VirtualListScrollHandle::new(),
            tests_scroll: VirtualListScrollHandle::new(),
            focus: cx.focus_handle(),
            shown: None,
            _subscriptions: subscriptions,
        }
    }

    /// Gives the keyboard to the filter field.
    pub(crate) fn focus_filter(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.tab = 0;
        self.filter.update(cx, |input, cx| input.focus(window, cx));
        cx.notify();
    }

    /// Scrolls the row of a new selection into view.
    fn follow_selection(&mut self, cx: &mut Context<Self>) {
        let workspace = self.workspace.read(cx);
        let Some(symbol) = workspace
            .session
            .selection()
            .map(|selection| selection.symbol.clone())
        else {
            return;
        };
        if self.shown.as_ref() == Some(&symbol) {
            return;
        }
        let analysis = workspace.session.analysis().map(|analysis| &**analysis);
        if let Some(row) =
            workspace
                .outline
                .position(&symbol, workspace.session.shown_changes(), analysis)
        {
            self.outline_scroll
                .scroll_to_item(row, ScrollStrategy::Center);
            self.shown = Some(symbol);
        }
    }

    /// Moves the selection to the next or the previous row.
    fn step(&mut self, forward: bool, cx: &mut Context<Self>) {
        let workspace = self.workspace.read(cx);
        let session = &workspace.session;
        let analysis = session.analysis().map(|analysis| &**analysis);
        let from = session.selection().and_then(|selection| {
            workspace
                .outline
                .position(&selection.symbol, session.shown_changes(), analysis)
        });
        let Some(next) = workspace.outline.step(from, forward) else {
            return;
        };
        let Some(symbol) = workspace.outline.rows[next]
            .symbol(session.shown_changes(), analysis)
            .cloned()
        else {
            return;
        };
        self.workspace
            .update(cx, |workspace, cx| workspace.select(symbol, None, cx));
    }

    fn toolbar(&self, cx: &mut Context<Self>) -> AnyElement {
        let current = self.workspace.read(cx).filter.min_confidence;
        let choice = |confidence: Confidence, label: &'static str| {
            let selected = current == confidence;
            div()
                .id(label)
                .px_2()
                .h(px(22.))
                .flex()
                .items_center()
                .rounded(RADIUS_SM)
                .text_size(TEXT_MICRO)
                .text_color(if selected { theme::FG } else { theme::FG_3 })
                .when(selected, |this| this.bg(theme::BG_ACTIVE))
                .cursor_pointer()
                .hover(|style| style.text_color(theme::FG))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.workspace.update(cx, |workspace, cx| {
                        workspace.set_min_confidence(confidence, cx)
                    });
                }))
                .child(label)
        };
        v_flex()
            .flex_none()
            .gap_2()
            .p_2()
            .border_b_1()
            .border_color(theme::LINE)
            .child(
                Input::new(&self.filter)
                    .small()
                    .cleanable(true)
                    .prefix(Icon::new(IconName::Search).size_3().text_color(theme::FG_3)),
            )
            .child(
                h_flex()
                    .gap_1()
                    .items_center()
                    .child(
                        div()
                            .text_size(TEXT_MICRO)
                            .text_color(theme::FG_4)
                            .mr_1()
                            .child("Show"),
                    )
                    .child(choice(Confidence::Possible, "Every item"))
                    .child(choice(Confidence::Likely, "Likely and definite"))
                    .child(choice(Confidence::Definite, "Definite")),
            )
            .into_any_element()
    }

    fn outline_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let workspace = self.workspace.read(cx);
        let Some(row) = workspace.outline.rows.get(index) else {
            return div().into_any_element();
        };
        let session = &workspace.session;
        let selection = session.selection().map(|selection| &selection.symbol);
        match row {
            Row::Head {
                block,
                count,
                collapsed,
            } => {
                let (label, icon, accent, raised) = block_parts(*block);
                let block = *block;
                div()
                    .id(SharedString::from(format!("block-{label}")))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.workspace
                            .update(cx, |workspace, cx| workspace.toggle_block(block, cx));
                    }))
                    .child(ui::block_head(
                        label, *count, icon, accent, raised, *collapsed, cx,
                    ))
                    .into_any_element()
            }
            Row::ImpactGroup => ui::group_head("IMPACT", IconName::Network),
            Row::File { path, status } => {
                let text = path.as_str();
                let (directory, name) = text
                    .rsplit_once('/')
                    .map_or(("", text), |(dir, name)| (dir, name));
                h_flex()
                    .h(px(height::FILE))
                    .pl(ui::LEAD_PLAIN)
                    .pr(px(10.))
                    .gap(ui::GUTTER_GAP)
                    .items_center()
                    .text_size(TEXT_MICRO)
                    .child(ui::gutter(
                        Icon::new(IconName::File).size_2p5().text_color(theme::FG_4),
                    ))
                    .child(
                        h_flex()
                            .min_w_0()
                            .truncate()
                            .when(!directory.is_empty(), |this| {
                                this.child(
                                    div().text_color(theme::FG_4).child(format!("{directory}/")),
                                )
                            })
                            .child(div().text_color(theme::FG_2).child(name.to_owned())),
                    )
                    .child(div().flex_1())
                    .children(status.map(|status| {
                        div()
                            .font_family(theme::mono())
                            .text_size(px(10.))
                            .text_color(theme::FG_4)
                            .child(match status {
                                FileStatus::Added => "A",
                                FileStatus::Modified => "M",
                                FileStatus::Deleted => "D",
                                FileStatus::Renamed => "R",
                            })
                    }))
                    .into_any_element()
            }
            Row::Change(change_index) => {
                let Some(change) = session
                    .shown_changes()
                    .and_then(|diff| diff.changes.get(*change_index))
                else {
                    return div().into_any_element();
                };
                let selected = selection == Some(&change.symbol);
                let short = wording::short_name(&change.symbol.path);
                let name = match &change.detail {
                    ChangeDetail::Renamed { from } => {
                        format!(
                            "{} · was {}",
                            short.name,
                            wording::short_name(&from.path).name
                        )
                    }
                    ChangeDetail::Visibility { from, to } => format!(
                        "{} · {} → {}",
                        short.name,
                        wording::visibility_text(from),
                        wording::visibility_text(to)
                    ),
                    _ => short.name,
                };
                let line = change
                    .new
                    .as_ref()
                    .or(change.old.as_ref())
                    .map(|location| location.span.start_line);
                let accent = if change.kind == ChangeKind::NonSemantic {
                    theme::NEUTRAL
                } else {
                    theme::CHANGED
                };
                let symbol = change.symbol.clone();
                selectable(("change", *change_index), selected, accent, height::CHANGE)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let symbol = symbol.clone();
                        this.workspace
                            .update(cx, |workspace, cx| workspace.select(symbol, None, cx));
                    }))
                    .child(
                        h_flex()
                            .size_full()
                            .gap(ui::GUTTER_GAP)
                            .items_center()
                            .pl(ui::LEAD)
                            .pr(px(10.))
                            .child(ui::marker(accent.solid, ui::Marker::Changed))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(theme::mono())
                                    .text_size(px(12.5))
                                    .child(name),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_size(px(10.5))
                                    .text_color(theme::FG_4)
                                    .child(short.module),
                            )
                            .child(
                                div()
                                    .flex_none()
                                    .w(ui::LINE_NUMBER_W)
                                    .text_right()
                                    .font_family(theme::mono())
                                    .text_size(px(10.5))
                                    .text_color(theme::FG_4)
                                    .children(line.map(|line| format!(":{line}"))),
                            )
                            .child(ui::pill(wording::change_label(change), accent)),
                    )
                    .into_any_element()
            }
            Row::Item {
                index: item_index,
                dim,
            } => {
                let Some(analysis) = session.analysis() else {
                    return div().into_any_element();
                };
                let Some(item) = analysis.items.get(*item_index) else {
                    return div().into_any_element();
                };
                let selected = selection == Some(&item.symbol);
                let accent = ui::class_accent(item.class);
                let short = wording::short_name(&item.symbol.path);
                let summary = wording::path_summary(item, analysis);
                let confidence = item.confidence;
                let symbol = item.symbol.clone();
                let dim = *dim;
                selectable(("item", *item_index), selected, accent, height::ITEM)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let symbol = symbol.clone();
                        this.workspace
                            .update(cx, |workspace, cx| workspace.select(symbol, None, cx));
                    }))
                    .child(
                        v_flex()
                            .size_full()
                            .justify_center()
                            .gap(px(2.))
                            .pl(ui::LEAD)
                            .pr(px(10.))
                            .child(
                                h_flex()
                                    .h(ui::ROW_LINE)
                                    .gap(ui::GUTTER_GAP)
                                    .items_center()
                                    .child(ui::marker(
                                        accent.solid,
                                        marker_shape(item.class, confidence),
                                    ))
                                    .child(
                                        h_flex()
                                            .flex_1()
                                            .min_w_0()
                                            .gap(px(6.))
                                            .items_center()
                                            .overflow_hidden()
                                            .child(
                                                div()
                                                    .flex_shrink(1.)
                                                    .min_w_0()
                                                    .truncate()
                                                    .font_family(theme::mono())
                                                    .text_size(px(12.5))
                                                    .text_color(if dim {
                                                        theme::FG_2
                                                    } else {
                                                        theme::FG
                                                    })
                                                    .child(short.name),
                                            )
                                            .child(
                                                div()
                                                    .flex_shrink(60.)
                                                    .min_w_0()
                                                    .truncate()
                                                    .text_size(px(10.5))
                                                    .text_color(theme::FG_4)
                                                    .child(short.module),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .flex_none()
                                            .text_size(px(10.5))
                                            .text_color(
                                                if dim || confidence == Confidence::Possible {
                                                    theme::FG_4
                                                } else {
                                                    accent.fg
                                                },
                                            )
                                            .child(confidence.as_str()),
                                    ),
                            )
                            .child(
                                div()
                                    .h(ui::SUB_LINE)
                                    .line_height(ui::SUB_LINE)
                                    .pl(ui::SECOND_LEAD)
                                    .truncate()
                                    .text_size(px(11.))
                                    .text_color(if dim { theme::FG_4 } else { theme::FG_3 })
                                    .child(summary),
                            ),
                    )
                    .into_any_element()
            }
            Row::Edited(count) => h_flex()
                .h(px(height::EDITED))
                .pl(ui::LEAD_PLAIN)
                .pr(px(10.))
                .gap(ui::GUTTER_GAP)
                .items_center()
                .text_size(px(10.5))
                .text_color(theme::FG_3)
                .child(ui::gutter(
                    Icon::new(IconName::Check)
                        .size_2p5()
                        .text_color(theme::FG_4),
                ))
                .child(format!("Edited with the change · {count}"))
                .child(div().flex_1().h(px(1.)).bg(theme::LINE_SOFT))
                .into_any_element(),
            Row::Note(text) => div()
                .pl(ui::LEAD_PLAIN + ui::GUTTER + ui::GUTTER_GAP)
                .pr(px(10.))
                .pt(px(2.))
                .text_size(TEXT_SMALL)
                .text_color(theme::FG_4)
                .child(text.clone())
                .into_any_element(),
            Row::Skeleton => h_flex()
                .h(px(height::SKELETON))
                .px_3()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .w(px(90.))
                        .h(px(8.))
                        .rounded(px(4.))
                        .bg(theme::BG_RAISED_2),
                )
                .child(
                    div()
                        .w(px(40.))
                        .h(px(8.))
                        .rounded(px(4.))
                        .bg(theme::BG_RAISED_2),
                )
                .into_any_element(),
            Row::Gap => div().into_any_element(),
        }
    }

    fn test_row(&self, index: usize, cx: &mut Context<Self>) -> AnyElement {
        let workspace = self.workspace.read(cx);
        let Some(analysis) = workspace.session.analysis() else {
            return div().into_any_element();
        };
        let tests = &analysis.tests;
        let Some(row) = workspace.tests.get(index) else {
            return div().into_any_element();
        };
        match row {
            TestRow::Package(command) => {
                let (package, version) = package_parts(&tests.commands[*command].package);
                h_flex()
                    .h_full()
                    .px(px(10.))
                    .gap_1()
                    .items_center()
                    .child(
                        div()
                            .truncate()
                            .font_family(theme::mono())
                            .text_size(px(12.))
                            .child(package.to_owned()),
                    )
                    .children(version.map(|version| {
                        div()
                            .text_size(px(10.5))
                            .text_color(theme::FG_4)
                            .child(format!("@{version}"))
                    }))
                    .into_any_element()
            }
            TestRow::Target {
                command: target,
                expanded,
            } => {
                let command = &tests.commands[*target];
                let target = *target;
                let line = command.command_line();
                let fraction = if command.total == 0 {
                    0.0
                } else {
                    command.tests as f32 / command.total as f32
                };
                h_flex()
                    .id(("test-target", target))
                    .h_full()
                    .pl(px(22.))
                    .pr(px(8.))
                    .gap(ui::GUTTER_GAP)
                    .items_center()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::BG_HOVER))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.workspace
                            .update(cx, |workspace, cx| workspace.toggle_test_target(target, cx));
                    }))
                    .child(ui::gutter(
                        Icon::new(if *expanded {
                            IconName::ChevronDown
                        } else {
                            IconName::ChevronRight
                        })
                        .size_3()
                        .text_color(theme::FG_4),
                    ))
                    .child(
                        div()
                            .min_w_0()
                            .truncate()
                            .font_family(theme::mono())
                            .text_size(px(12.))
                            .text_color(theme::FG_2)
                            .child(wording::target_label(command)),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme::FG_3)
                            .child(format!("{} of {}", command.tests, command.total)),
                    )
                    .child(
                        div()
                            .w(px(36.))
                            .h(px(3.))
                            .rounded(px(2.))
                            .bg(theme::BG_RAISED_2)
                            .child(
                                div()
                                    .h_full()
                                    .rounded(px(2.))
                                    .bg(theme::TEST.solid)
                                    .w(relative(fraction)),
                            ),
                    )
                    .child(
                        ui::icon_button(("copy-test", target), IconName::Copy, "Copy the command")
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let line = line.clone();
                                this.workspace
                                    .update(cx, |workspace, cx| workspace.copy(line, cx));
                            })),
                    )
                    .into_any_element()
            }
            TestRow::Command(command) => div()
                .h_full()
                .pl(px(31.))
                .pr(px(10.))
                .py(px(2.))
                .child(
                    div()
                        .h_full()
                        .px_2()
                        .flex()
                        .items_center()
                        .rounded(RADIUS_SM)
                        .border_1()
                        .border_color(theme::LINE)
                        .bg(theme::BG_INPUT)
                        .child(
                            div()
                                .truncate()
                                .font_family(theme::mono())
                                .text_size(px(11.))
                                .text_color(theme::FG_2)
                                .child(tests.commands[*command].command_line()),
                        ),
                )
                .into_any_element(),
            TestRow::Test(test_index) => {
                let test = &tests.selected[*test_index];
                let selected = workspace
                    .session
                    .selection()
                    .is_some_and(|selection| selection.symbol == test.symbol);
                let (symbol, site) = (test.symbol.clone(), test.location.clone());
                selectable(("test", *test_index), selected, theme::TEST, 22.0)
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let (symbol, site) = (symbol.clone(), site.clone());
                        this.workspace
                            .update(cx, |workspace, cx| workspace.select(symbol, Some(site), cx));
                    }))
                    .child(
                        h_flex()
                            .size_full()
                            .pl(px(20.))
                            .pr(px(10.))
                            .gap(ui::GUTTER_GAP)
                            .items_center()
                            .child(ui::gutter(
                                Icon::new(IconName::Check)
                                    .size_2p5()
                                    .text_color(theme::TEST.solid),
                            ))
                            .child(
                                div()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(theme::mono())
                                    .text_size(px(11.5))
                                    .text_color(theme::FG_2)
                                    .child(wording::short_name(&test.symbol.path).name),
                            )
                            .child(div().flex_1())
                            .child(
                                div()
                                    .text_size(px(10.))
                                    .text_color(theme::FG_4)
                                    .child(wording::test_reason_text(test.reason)),
                            ),
                    )
                    .into_any_element()
            }
            TestRow::Note(text) => div()
                .px_3()
                .py_1()
                .text_size(TEXT_SMALL)
                .text_color(theme::FG_4)
                .child(text.clone())
                .into_any_element(),
        }
    }

    fn tests_header(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let workspace = self.workspace.read(cx);
        let analysis = workspace.session.analysis()?;
        (!analysis.tests.commands.is_empty()).then(|| {
            h_flex()
                .flex_none()
                .h(px(32.))
                .px_3()
                .gap_2()
                .items_center()
                .border_b_1()
                .border_color(theme::LINE_SOFT)
                .text_size(TEXT_SMALL)
                .text_color(theme::FG_3)
                .child(format!(
                    "{} of {} tests reach the change",
                    analysis.tests.selected.len(),
                    analysis.tests.total
                ))
                .child(div().flex_1())
                .child(
                    Button::new("copy-tests")
                        .xsmall()
                        .ghost()
                        .label("Copy all")
                        .on_click(cx.listener(|this, _, _, cx| {
                            let workspace = this.workspace.read(cx);
                            let Some(analysis) = workspace.session.analysis() else {
                                return;
                            };
                            let text = analysis
                                .tests
                                .commands
                                .iter()
                                .map(|command| command.command_line())
                                .collect::<Vec<_>>()
                                .join("\n");
                            this.workspace
                                .update(cx, |workspace, cx| workspace.copy(text, cx));
                        })),
                )
                .into_any_element()
        })
    }
}

impl Render for LeftPane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let workspace = self.workspace.read(cx);
        let rows: Rc<Vec<_>> = Rc::new(
            workspace
                .outline
                .rows
                .iter()
                .map(|row| size(px(100.), px(row.height())))
                .collect(),
        );
        let test_rows: Rc<Vec<_>> = Rc::new(
            workspace
                .tests
                .iter()
                .map(|row| size(px(100.), px(row.height())))
                .collect(),
        );
        let list = if self.tab == 0 {
            v_virtual_list(cx.entity(), "outline", rows, |this, range, _, cx| {
                range.map(|index| this.outline_row(index, cx)).collect()
            })
            .track_scroll(&self.outline_scroll)
            .into_any_element()
        } else {
            v_virtual_list(cx.entity(), "tests", test_rows, |this, range, _, cx| {
                range.map(|index| this.test_row(index, cx)).collect()
            })
            .track_scroll(&self.tests_scroll)
            .into_any_element()
        };
        let scroll = if self.tab == 0 {
            &self.outline_scroll
        } else {
            &self.tests_scroll
        };
        v_flex()
            .size_full()
            .min_w_0()
            .bg(theme::BG_PANEL)
            .track_focus(&self.focus)
            .on_key_down(cx.listener(|this, event: &KeyDownEvent, _, cx| {
                match event.keystroke.key.as_str() {
                    "down" => this.step(true, cx),
                    "up" => this.step(false, cx),
                    _ => {}
                }
            }))
            .child(
                // Two equal halves across the whole column, so the second
                // list is as easy to find as the first.
                TabBar::new("left-tabs")
                    .w_full()
                    .underline()
                    .small()
                    .selected_index(self.tab)
                    .on_click(cx.listener(|this, index: &usize, _, cx| {
                        this.tab = *index;
                        cx.notify();
                    }))
                    .child(Tab::new().flex_1().label("Changes and impact"))
                    .child(Tab::new().flex_1().label("Tests to run")),
            )
            .when(self.tab == 0, |this| this.child(self.toolbar(cx)))
            .when(self.tab == 1, |this| this.children(self.tests_header(cx)))
            .child(
                div()
                    .id("left-list")
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .on_click(cx.listener(|this, _, window, cx| window.focus(&this.focus, cx)))
                    .child(list)
                    .child(ui::scrollbar("left", scroll)),
            )
    }
}

/// A row that selects something, with a rule on its left edge when selected.
fn selectable(
    id: impl Into<gpui_kit::ElementId>,
    selected: bool,
    accent: Accent,
    height: f32,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    div()
        .id(id)
        .h(px(height))
        .w_full()
        .cursor_pointer()
        .border_l_2()
        .border_color(if selected {
            accent.solid
        } else {
            theme::BG_PANEL
        })
        .bg(if selected {
            theme::BG_SELECTED
        } else {
            theme::BG_PANEL
        })
        .hover(|style| style.bg(theme::BG_HOVER))
}

/// The label, icon, colour and weight of a block header.
fn block_parts(block: Block) -> (&'static str, IconName, Accent, bool) {
    match block {
        Block::Changes => ("CHANGES", IconName::Square, theme::CHANGED, true),
        Block::NeedsEditing => ("NEEDS EDITING", IconName::FileText, theme::EDIT, false),
        Block::MayBehave => (
            "MAY BEHAVE DIFFERENTLY",
            IconName::Network,
            theme::BEHAVE,
            false,
        ),
    }
}

/// The marker of an impact item: its class, or a dashed ring when only a
/// candidate step reaches it.
pub(crate) fn marker_shape(class: ImpactClass, confidence: Confidence) -> ui::Marker {
    match (class, confidence) {
        (_, Confidence::Possible) => ui::Marker::Candidate,
        (ImpactClass::Adaptation, _) => ui::Marker::Diamond,
        (ImpactClass::Behaviour, _) => ui::Marker::Dot,
    }
}

/// The short word for a kind of symbol.
pub(crate) fn kind_label(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function | SymbolKind::Method => "fn",
        SymbolKind::TypeAlias => "type",
        SymbolKind::Module => "mod",
        other => other.as_str(),
    }
}
