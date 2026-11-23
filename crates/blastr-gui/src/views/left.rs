//! The left column: the cause and its effects, in clearly separate blocks.
//!
//! The changes are the cause; the impact and the tests are what follows from
//! them. Each block has its own header, colour and count, and a rule between
//! blocks, so the two are never read as one list.

use gpui_kit::prelude::*;

use blastr_model::wording;
use blastr_model::{
    Change, ChangeDetail, ChangeKind, Confidence, FileStatus, ImpactClass, RelPath, SymbolKind,
    TestCommand,
};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::{Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, div, px, relative,
};

use super::shell::{Focus, Shell};
use super::ui;
use crate::state::outline::height;
use crate::state::outline::{Block, Row, TestRow};
use crate::theme::{self, Accent, RADIUS_SM, TEXT_MICRO, TEXT_SMALL};

/// The heights of the outline's rows, top to bottom.
pub(crate) fn outline_heights(outline: &crate::state::outline::Outline) -> Vec<f32> {
    outline.rows.iter().map(Row::height).collect()
}

/// The heights of the tests pane's rows, top to bottom.
pub(crate) fn test_heights(rows: &[TestRow]) -> Vec<f32> {
    rows.iter().map(TestRow::height).collect()
}

impl Shell {
    /// The whole left column: commits, the filter, the outline and the tests.
    pub(crate) fn left_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        v_flex()
            .size_full()
            .min_w_0()
            .bg(theme::BG_PANEL)
            .child(self.commits_pane(cx))
            .child(self.filter_bar(cx))
            .child(div().flex_1().min_h_0().child(self.outline_pane(cx)))
            .child(self.tests_pane(cx))
            .into_any_element()
    }

    /// The filter box and the confidence cycle.
    fn filter_bar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let confidence = self.filter.min_confidence;
        let label = match confidence {
            Confidence::Possible => "All",
            Confidence::Likely => "Likely+",
            Confidence::Definite => "Definite",
        };
        h_flex()
            .flex_shrink_0()
            .gap_2()
            .h(px(38.))
            .px_2()
            .items_center()
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .h(px(26.))
                    .px_2()
                    .gap_2()
                    .items_center()
                    .rounded(px(6.))
                    .border_1()
                    .border_color(theme::LINE)
                    .bg(theme::BG_INPUT)
                    .child(Icon::new(IconName::Search).size_3().text_color(theme::FG_3))
                    .child(
                        gpui_kit::component::input::Input::new(&self.filter_input)
                            .small()
                            .flex_1(),
                    ),
            )
            .child(
                Button::new("confidence")
                    .small()
                    .outline()
                    .label(label)
                    .tooltip("Lowest confidence listed")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.filter.min_confidence = this.filter.next_confidence();
                        this.outline_dirty = true;
                        this.tests_dirty = true;
                        cx.notify();
                    })),
            )
            .into_any_element()
    }

    /// The virtualised outline of the changes and the impact.
    fn outline_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let rows = &self.outline.rows;
        if rows.is_empty() {
            if self.session.shown().failure().is_some() {
                return div()
                    .size_full()
                    .min_w_0()
                    .bg(theme::BG_PANEL)
                    .p_3()
                    .child(ui::failure_line())
                    .into_any_element();
            }
            return div()
                .size_full()
                .bg(theme::BG_PANEL)
                .child(ui::empty_state(
                    IconName::FileText,
                    Some(theme::CHANGED),
                    "Nothing to show yet",
                    "Choose what to compare in the list above.",
                ))
                .into_any_element();
        }
        let heights = outline_heights(&self.outline);
        let viewport = f32::from(self.outline_scroll.bounds().size.height).max(120.0);
        let scroller = super::list::scroller(
            "outline",
            &self.outline_scroll,
            &heights,
            viewport,
            4,
            |index, top| self.outline_row(index, top, cx),
        )
        .on_scroll(cx.listener(|this, _, _, cx| {
            this.focus = Focus::Outline;
            cx.notify();
        }));
        div()
            .relative()
            .size_full()
            .min_h_0()
            .bg(theme::BG_PANEL)
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.focus = Focus::Outline;
                    cx.notify();
                }),
            )
            .child(scroller)
            .child(ui::scrollbar(
                "outline",
                &self.outline_scroll,
                ScrollbarAxis::Vertical,
            ))
            .into_any_element()
    }

    /// One row of the outline, drawn at exactly the height the row reports.
    fn outline_row(&self, index: usize, top: f32, cx: &mut Context<Self>) -> AnyElement {
        let Some(entry) = self.outline.rows.get(index) else {
            return div().into_any_element();
        };
        let height = entry.height();
        let selection = self.session.selection().map(|selection| &selection.symbol);
        let body: AnyElement = match entry {
            Row::Head {
                block,
                count,
                collapsed,
            } => {
                let (label, icon, accent, raised) = block_head_parts(*block);
                let block = *block;
                let collapsed = *collapsed;
                let header = div()
                    .id(SharedString::from(format!("block-{label}")))
                    .h(px(height))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.filter.toggle(block);
                        this.outline_dirty = true;
                        cx.notify();
                    }))
                    .child(ui::block_head(
                        label,
                        Some(*count),
                        icon,
                        accent,
                        raised,
                        collapsed,
                        cx,
                    ));
                header.into_any_element()
            }
            Row::ImpactGroup => ui::group_head("IMPACT", IconName::Network),
            Row::File { path, status } => self.file_row(path, *status, cx),
            Row::Change(change_index) => self.change_row(*change_index, selection, cx),
            Row::Item { index, dim } => self.item_row(*index, *dim, selection, cx),
            Row::Edited(count) => h_flex()
                .h(px(height))
                .pl(ui::LEAD_PLAIN)
                .pr(px(10.))
                .gap(ui::GUTTER_GAP)
                .items_center()
                .line_height(ui::ROW_LINE)
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
            Row::More { block, hidden } => {
                let block = *block;
                let hidden = *hidden;
                h_flex()
                    .id(SharedString::from(format!("more-{block:?}")))
                    .h(px(height))
                    .w_full()
                    .pl(ui::LEAD_PLAIN)
                    .pr(px(10.))
                    .gap(ui::GUTTER_GAP)
                    .items_center()
                    .line_height(ui::ROW_LINE)
                    .cursor_pointer()
                    .text_size(px(11.5))
                    .text_color(theme::FG_3)
                    .hover(|style| style.bg(theme::BG_HOVER).text_color(theme::FG))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.filter.show_more(block);
                        this.outline_dirty = true;
                        cx.notify();
                    }))
                    .child(ui::gutter(
                        Icon::new(IconName::ChevronDown)
                            .size_3()
                            .text_color(theme::FG_4),
                    ))
                    .child(format!("Show {hidden} more"))
                    .into_any_element()
            }
            Row::Note(text) => div()
                .h(px(height))
                .pl(ui::LEAD_PLAIN + ui::GUTTER + ui::GUTTER_GAP)
                .pr(px(10.))
                .pt(px(2.))
                .text_size(TEXT_SMALL)
                .text_color(theme::FG_4)
                .child(text.clone())
                .into_any_element(),
            Row::Skeleton => h_flex()
                .h(px(height))
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
            Row::Gap => div().h(px(height)).into_any_element(),
        };

        div()
            .absolute()
            .top(px(top))
            .left_0()
            .right_0()
            .h(px(height))
            .child(body)
            .into_any_element()
    }

    /// The file that a run of changes is in: its folder and name, and how the
    /// file itself changed.
    fn file_row(
        &self,
        path: &RelPath,
        status: Option<FileStatus>,
        cx: &Context<Self>,
    ) -> AnyElement {
        let text = path.as_str();
        let (directory, name) = match text.rfind('/') {
            Some(slash) => (&text[..slash + 1], &text[slash + 1..]),
            None => ("", text),
        };
        h_flex()
            .h(px(height::FILE))
            .pl(ui::LEAD_PLAIN)
            .pr(px(10.))
            .gap(ui::GUTTER_GAP)
            .items_center()
            .line_height(ui::ROW_LINE)
            .text_size(TEXT_MICRO)
            .child(ui::gutter(
                Icon::new(IconName::File).size_2p5().text_color(theme::FG_4),
            ))
            .child(
                h_flex()
                    .min_w_0()
                    .child(div().text_color(theme::FG_4).child(directory.to_owned()))
                    .child(div().text_color(theme::FG_2).child(name.to_owned())),
            )
            .child(div().flex_1())
            .children(status.map(|status| {
                div()
                    .flex_none()
                    .font_family(ui::mono(cx))
                    .text_size(px(10.))
                    .text_color(theme::FG_4)
                    .child(status_letter(status))
            }))
            .into_any_element()
    }

    /// One change: its short name, the module it lives in, its kind and its
    /// line.
    fn change_row(
        &self,
        change_index: usize,
        selection: Option<&blastr_model::SymbolPath>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(change) = self
            .changes()
            .and_then(|diff| diff.changes.get(change_index))
        else {
            return div().into_any_element();
        };
        let selected = selection.is_some_and(|symbol| symbol == &change.symbol);
        let accent = match change.kind {
            ChangeKind::NonSemantic => theme::NEUTRAL,
            _ => theme::CHANGED,
        };
        let (name, module) = change_parts(change);
        let badge = wording::change_row(change).label;
        let line = change
            .new
            .as_ref()
            .or(change.old.as_ref())
            .map(|location| location.span.start_line);
        let symbol = change.symbol.clone();
        let mono = ui::mono(cx);
        self.selectable_row(
            format!("change-{change_index}"),
            selected,
            accent,
            px(height::CHANGE),
            cx,
            move |this, cx| this.select_symbol(symbol.clone(), None, cx),
            h_flex()
                .w_full()
                .h(px(height::CHANGE))
                .gap(ui::GUTTER_GAP)
                .items_center()
                .line_height(ui::ROW_LINE)
                .overflow_hidden()
                .pl(ui::LEAD)
                .pr(px(10.))
                .child(ui::marker(
                    if selected {
                        accent.solid
                    } else {
                        theme::CHANGED.solid
                    },
                    ui::Marker::Changed,
                ))
                .child(
                    div()
                        .font_family(mono.clone())
                        .text_size(px(12.5))
                        .text_color(theme::FG)
                        .min_w_0()
                        .truncate()
                        .child(name),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(10.5))
                        .text_color(theme::FG_4)
                        .truncate()
                        .child(module),
                )
                // The line number is a column of its own, right-aligned, so
                // that the badges after it share their left edge.
                .child(
                    div()
                        .flex_none()
                        .w(ui::LINE_NUMBER_W)
                        .text_right()
                        .font_family(mono)
                        .text_size(px(10.5))
                        .text_color(theme::FG_4)
                        .children(line.map(|line| format!(":{line}"))),
                )
                .child(ui::pill(badge, accent))
                .into_any_element(),
        )
    }

    /// One affected item: its short name and module, the confidence that it is
    /// listed with, and the reason on a second line.
    fn item_row(
        &self,
        item_index: usize,
        dim: bool,
        selection: Option<&blastr_model::SymbolPath>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let Some(item) = self
            .analysis()
            .and_then(|analysis| analysis.items.get(item_index))
        else {
            return div().into_any_element();
        };
        let selected = selection.is_some_and(|symbol| symbol == &item.symbol);
        let accent = ui::class_accent(item.class);
        let short = wording::short_name(&item.symbol.path);
        let summary = self.analysis().map_or_else(String::new, |analysis| {
            wording::path_summary(item, analysis)
        });
        let confidence = item.confidence;
        let name_colour = if dim { theme::FG_2 } else { theme::FG };
        let summary_colour = if dim { theme::FG_4 } else { theme::FG_3 };
        let confidence_colour = if dim || confidence == Confidence::Possible {
            theme::FG_4
        } else {
            accent.fg
        };
        let symbol = item.symbol.clone();
        let mono = ui::mono(cx);
        self.selectable_row(
            format!("item-{item_index}"),
            selected,
            accent,
            px(height::ITEM),
            cx,
            move |this, cx| this.select_symbol(symbol.clone(), None, cx),
            v_flex()
                .w_full()
                .h(px(height::ITEM))
                .justify_center()
                .gap(px(2.))
                .pl(ui::LEAD)
                .pr(px(10.))
                .child(
                    h_flex()
                        .w_full()
                        .h(ui::ROW_LINE)
                        .gap(ui::GUTTER_GAP)
                        .items_center()
                        .line_height(ui::ROW_LINE)
                        .overflow_hidden()
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
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .font_family(mono)
                                        .text_size(px(12.5))
                                        .text_color(name_colour)
                                        .text_ellipsis()
                                        .child(short.name),
                                )
                                .child(
                                    div()
                                        // The module gives way long before the
                                        // name does: a narrow row drops it and
                                        // keeps the name whole.
                                        .flex_shrink(60.)
                                        .min_w(px(0.))
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_size(px(10.5))
                                        .text_color(theme::FG_4)
                                        .text_ellipsis()
                                        .child(short.module),
                                ),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(10.5))
                                .text_color(confidence_colour)
                                .child(confidence.as_str()),
                        ),
                )
                .child(
                    h_flex()
                        .w_full()
                        .h(ui::SUB_LINE)
                        .items_center()
                        .line_height(ui::SUB_LINE)
                        .child(
                            div()
                                .pl(ui::SECOND_LEAD)
                                .flex_1()
                                .min_w_0()
                                .text_size(px(11.))
                                .text_color(summary_colour)
                                .truncate()
                                .child(summary),
                        ),
                )
                .into_any_element(),
        )
    }

    /// Wraps a row's content in a clickable, selected background.
    ///
    /// The 2 px rule on the left is always laid out, so that selecting a row
    /// does not move its text; it only changes colour.
    #[allow(clippy::too_many_arguments)]
    fn selectable_row(
        &self,
        id: String,
        selected: bool,
        accent: Accent,
        height: gpui_kit::Pixels,
        cx: &mut Context<Self>,
        select: impl Fn(&mut Self, &mut Context<Self>) + 'static,
        content: AnyElement,
    ) -> AnyElement {
        div()
            .id(SharedString::from(id))
            .h(height)
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
            .on_click(cx.listener(move |this, _, _, cx| select(this, cx)))
            .child(content)
            .into_any_element()
    }

    /// The tests pane: what to run, grouped by package and target.
    fn tests_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let analysis = self.analysis();
        let Some(analysis) = analysis else {
            return div()
                .flex_shrink_0()
                .border_t_1()
                .border_color(theme::LINE_STRONG)
                .bg(theme::BG_PANEL)
                .child(
                    div()
                        .h(px(32.))
                        .px_3()
                        .text_size(TEXT_MICRO)
                        .text_color(theme::FG_2)
                        .child("TESTS TO RUN"),
                )
                .into_any_element();
        };
        let selection = &analysis.tests;
        let selected = selection.selected.len();
        let total = selection.total;
        let header = h_flex()
            .h(px(32.))
            .flex_shrink_0()
            .px_3()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(theme::LINE_SOFT)
            .child(
                Icon::new(IconName::Check)
                    .size_3()
                    .text_color(theme::TEST.solid),
            )
            .child(
                div()
                    .text_size(TEXT_MICRO)
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(theme::FG_2)
                    .child("TESTS TO RUN"),
            )
            .child(div().flex_1())
            .child(
                div()
                    .flex_none()
                    .h(px(16.))
                    .px(px(5.))
                    .rounded(RADIUS_SM)
                    .bg(theme::TEST.bg)
                    .text_size(px(11.))
                    .text_color(theme::TEST.fg)
                    .child(format!("{selected} of {total}")),
            )
            .children((selected > 0).then(|| {
                Button::new("copy-tests")
                    .small()
                    .ghost()
                    .label("Copy all")
                    .tooltip("Copy every test command")
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(analysis) = this.analysis() {
                            let text = analysis
                                .tests
                                .commands
                                .iter()
                                .map(test_command_line)
                                .collect::<Vec<_>>()
                                .join("\n");
                            this.copy(text, cx);
                        }
                    }))
            }));

        let heights = test_heights(&self.tests_rows);
        let rows = &self.tests_rows;
        let body = if rows.is_empty() {
            div()
                .p_3()
                .text_size(TEXT_SMALL)
                .text_color(theme::FG_4)
                .child("No test reaches the changed code.")
                .into_any_element()
        } else {
            let viewport = f32::from(self.tests_scroll.bounds().size.height).max(60.0);
            let scroller = super::list::scroller(
                "tests",
                &self.tests_scroll,
                &heights,
                viewport,
                4,
                |index, top| self.test_row(index, top, cx),
            )
            .on_scroll(cx.listener(|this, _, _, cx| {
                this.focus = Focus::Tests;
                cx.notify();
            }));
            div()
                .relative()
                .size_full()
                .child(scroller)
                .child(ui::scrollbar(
                    "tests",
                    &self.tests_scroll,
                    ScrollbarAxis::Vertical,
                ))
                .into_any_element()
        };

        v_flex()
            .flex_shrink_0()
            .h(relative(0.30))
            .min_h(px(140.))
            .max_h(px(420.))
            .border_t_1()
            .border_color(theme::LINE_STRONG)
            .bg(theme::BG_PANEL)
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, _, cx| {
                    this.focus = Focus::Tests;
                    cx.notify();
                }),
            )
            .child(header)
            .child(
                div()
                    .flex_shrink_0()
                    .h(px(3.))
                    .mx_3()
                    .mt(px(4.))
                    .rounded(px(2.))
                    .bg(theme::BG_RAISED_2)
                    .overflow_hidden()
                    .child(
                        div()
                            .h_full()
                            .rounded(px(2.))
                            .bg(theme::TEST.solid)
                            .w(relative(test_meter_fraction(selected, total))),
                    ),
            )
            .child(div().flex_1().min_h_0().mt(px(4.)).child(body))
            .into_any_element()
    }

    /// One row of the tests pane.
    fn test_row(&self, index: usize, top: f32, cx: &mut Context<Self>) -> AnyElement {
        let Some(row) = self.tests_rows.get(index) else {
            return div().into_any_element();
        };
        let height = row.height();
        let commands = self.analysis().map(|analysis| &analysis.tests.commands);
        let mono = ui::mono(cx);
        let body: AnyElement = match row {
            TestRow::Package(command) => {
                let Some(command) = commands.and_then(|commands| commands.get(*command)) else {
                    return div().into_any_element();
                };
                let (package, version) = crate::state::outline::package_parts(&command.package);
                h_flex()
                    .h(px(height))
                    .pl(px(10.))
                    .pr(px(10.))
                    .gap_1()
                    .items_center()
                    .line_height(ui::ROW_LINE)
                    .child(
                        div()
                            .font_family(mono)
                            .text_size(px(12.))
                            .text_color(theme::FG)
                            .truncate()
                            .child(package.to_owned()),
                    )
                    .children(version.map(|version| {
                        div()
                            .text_size(px(10.5))
                            .text_color(theme::FG_4)
                            .child(format!("@{version}"))
                    }))
                    .child(div().flex_1())
                    .into_any_element()
            }
            TestRow::Target {
                command: target,
                expanded,
            } => {
                let Some(command) = commands.and_then(|commands| commands.get(*target)) else {
                    return div().into_any_element();
                };
                let command_index = *target;
                let expanded = *expanded;
                let label = wording::target_label(command);
                let tests = command.tests;
                let total = command.total;
                let line = test_command_line(command);
                let fraction = test_meter_fraction(tests as usize, total);
                h_flex()
                    .id(SharedString::from(format!("test-target-{command_index}")))
                    .h(px(height))
                    .w_full()
                    .pl(px(22.))
                    .pr(px(8.))
                    .gap(ui::GUTTER_GAP)
                    .items_center()
                    .line_height(ui::ROW_LINE)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::BG_HOVER))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if let Some(position) = this
                            .tests_expanded
                            .iter()
                            .position(|entry| *entry == command_index)
                        {
                            this.tests_expanded.remove(position);
                        } else {
                            this.tests_expanded.push(command_index);
                        }
                        this.tests_dirty = true;
                        cx.notify();
                    }))
                    .child(ui::gutter(
                        Icon::new(if expanded {
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
                            .font_family(mono)
                            .text_size(px(12.))
                            .text_color(theme::FG_2)
                            .truncate()
                            .child(label),
                    )
                    .child(div().flex_1())
                    .child(
                        div()
                            .flex_none()
                            .text_size(px(10.5))
                            .text_color(theme::FG_3)
                            .child(format!("{tests} of {total}")),
                    )
                    .child(
                        div()
                            .flex_none()
                            .w(px(36.))
                            .h(px(3.))
                            .rounded(px(2.))
                            .bg(theme::BG_RAISED_2)
                            .overflow_hidden()
                            .child(
                                div()
                                    .h_full()
                                    .rounded(px(2.))
                                    .bg(theme::TEST.solid)
                                    .w(relative(fraction)),
                            ),
                    )
                    .child(
                        ui::icon_button(
                            SharedString::from(format!("copy-test-{command_index}")),
                            IconName::Copy,
                            "Copy test command",
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.copy(line.clone(), cx);
                        })),
                    )
                    .into_any_element()
            }
            TestRow::Command(command) => {
                let line = commands
                    .and_then(|commands| commands.get(*command))
                    .map(test_command_line)
                    .unwrap_or_default();
                div()
                    .h(px(height))
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
                                    .min_w_0()
                                    .font_family(mono)
                                    .text_size(px(11.))
                                    .text_color(theme::FG_2)
                                    .truncate()
                                    .child(line),
                            ),
                    )
                    .into_any_element()
            }
            TestRow::Test(test_index) => {
                let Some(test) = self
                    .analysis()
                    .and_then(|analysis| analysis.tests.selected.get(*test_index))
                else {
                    return div().into_any_element();
                };
                let symbol = test.symbol.clone();
                let name = wording::short_name(&test.symbol.path).name;
                let reason = wording::test_reason_text(test.reason);
                let site = test.location.clone();
                let selected = self
                    .session
                    .selection()
                    .is_some_and(|selection| selection.symbol == test.symbol);
                self.selectable_row(
                    format!("test-{test_index}"),
                    selected,
                    theme::TEST,
                    px(22.),
                    cx,
                    move |this, cx| this.select_symbol(symbol.clone(), Some(site.clone()), cx),
                    h_flex()
                        .w_full()
                        .h(px(22.))
                        .pl(px(20.))
                        .pr(px(10.))
                        .gap(ui::GUTTER_GAP)
                        .items_center()
                        .line_height(ui::ROW_LINE)
                        .child(ui::gutter(
                            Icon::new(IconName::Check)
                                .size_2p5()
                                .text_color(theme::TEST.solid),
                        ))
                        .child(
                            div()
                                .min_w_0()
                                .font_family(mono)
                                .text_size(px(11.5))
                                .text_color(theme::FG_2)
                                .truncate()
                                .child(name),
                        )
                        .child(div().flex_1())
                        .child(
                            div()
                                .flex_none()
                                .text_size(px(10.))
                                .text_color(theme::FG_4)
                                .child(reason),
                        )
                        .into_any_element(),
                )
            }
            TestRow::Note(text) => div()
                .h(px(height))
                .px_3()
                .py_1()
                .text_size(TEXT_SMALL)
                .text_color(theme::FG_4)
                .child(text.clone())
                .into_any_element(),
        };

        div()
            .absolute()
            .top(px(top))
            .left_0()
            .right_0()
            .h(px(height))
            .child(body)
            .into_any_element()
    }

    /// Confirms the keyboard's test row, when it names a test.
    pub(crate) fn confirm_test(&mut self, cx: &mut Context<Self>) {
        let Some(cursor) = self.tests_cursor else {
            return;
        };
        let Some(row) = self.tests_rows.get(cursor) else {
            return;
        };
        let Some(analysis) = self.analysis() else {
            return;
        };
        let TestRow::Test(index) = row else {
            return;
        };
        let Some(test) = analysis.tests.selected.get(*index) else {
            return;
        };
        let symbol = test.symbol.clone();
        let site = test.location.clone();
        self.select_symbol(symbol, Some(site), cx);
    }
}

/// The short name and the module of a change, with the detail that changes
/// what the name reads as.
fn change_parts(change: &Change) -> (String, String) {
    let short = wording::short_name(&change.symbol.path);
    let detail = match &change.detail {
        ChangeDetail::Renamed { from } => {
            Some(format!("was {}", wording::short_name(&from.path).name))
        }
        ChangeDetail::Visibility { from, to } => Some(format!(
            "{} → {}",
            wording::visibility_text(from),
            wording::visibility_text(to)
        )),
        _ => None,
    };
    let name = match detail {
        Some(detail) => format!("{} · {detail}", short.name),
        None => short.name,
    };
    (name, short.module)
}

/// The label, icon, colour and header style of one outline block.
fn block_head_parts(block: Block) -> (&'static str, IconName, Accent, bool) {
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

/// The letter a file status is shown by.
fn status_letter(status: FileStatus) -> &'static str {
    match status {
        FileStatus::Added => "A",
        FileStatus::Modified => "M",
        FileStatus::Deleted => "D",
        FileStatus::Renamed => "R",
    }
}

/// The command line that runs a target's selected tests.
pub(crate) fn test_command_line(command: &TestCommand) -> String {
    command.command_line()
}

/// The filled share of the tests meter for `tests` of `total`.
fn test_meter_fraction(tests: usize, total: u32) -> f32 {
    if total == 0 {
        return 0.0;
    }
    (tests as f32 / total as f32).clamp(0.0, 1.0)
}

/// The marker shape for an item's class and confidence.
pub(crate) fn marker_shape(class: ImpactClass, confidence: Confidence) -> ui::Marker {
    if confidence == Confidence::Possible {
        return ui::Marker::Candidate;
    }
    match class {
        ImpactClass::Adaptation => ui::Marker::Diamond,
        ImpactClass::Behaviour => ui::Marker::Dot,
    }
}

/// The word for a symbol kind, used in lists and the palette.
pub(crate) fn kind_label(kind: SymbolKind) -> &'static str {
    match kind {
        SymbolKind::Function | SymbolKind::Method => "fn",
        SymbolKind::Struct => "struct",
        SymbolKind::Enum => "enum",
        SymbolKind::Union => "union",
        SymbolKind::Trait => "trait",
        SymbolKind::Impl => "impl",
        SymbolKind::Field => "field",
        SymbolKind::Variant => "variant",
        SymbolKind::Const => "const",
        SymbolKind::Static => "static",
        SymbolKind::TypeAlias => "type",
        SymbolKind::Module => "mod",
        SymbolKind::Macro => "macro",
    }
}
