//! The commit list: what is compared, chosen from the history of the branch.

use gpui_kit::prelude::*;

use gpui_kit::component::button::Button;
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::{Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::{AnyElement, Context, IntoElement, ParentElement, Styled, div, px};

use super::list;
use super::shell::{Focus, Shell};
use super::ui;
use crate::state::commits::{CommitRow, relative_time};
use crate::state::session::Compare;
use crate::theme::{self, TEXT_MICRO, TEXT_SMALL};

/// The height of one row of the commit list.
const COMMIT_ROW: f32 = 26.0;
/// The width of the rail that holds the history dots.
const RAIL: f32 = 30.0;
/// The x of the line that joins the dots, at the centre of a dot of [`DOT`].
const RAIL_X: f32 = 17.0;
/// The size of a dot on the rail.
const DOT: f32 = 8.0;

impl Shell {
    /// Whether the working tree has changes worth showing.
    fn dirty(&self) -> bool {
        self.session.repo.as_ref().is_some_and(|repo| repo.dirty)
    }

    /// The commits pane: its header and a virtualised list.
    pub(crate) fn commits_pane(&self, cx: &mut Context<Self>) -> AnyElement {
        let branch = self
            .session
            .repo
            .as_ref()
            .and_then(|repo| repo.branch.clone());
        let dirty = self.dirty();
        let count = self.commits.row_count(dirty);
        let selected = self
            .session
            .compare
            .as_ref()
            .and_then(|compare| self.commits.selected(compare));

        let header = h_flex()
            .h(px(30.))
            .flex_shrink_0()
            .pl(px(10.))
            .pr(px(8.))
            .gap_2()
            .items_center()
            .child(
                Icon::new(IconName::SquareTerminal)
                    .size_3()
                    .text_color(theme::FG_3),
            )
            .child(
                div()
                    .text_size(TEXT_MICRO)
                    .text_color(theme::FG_2)
                    .child("COMMITS"),
            )
            .child(div().flex_1())
            .children(branch.map(|branch| {
                div()
                    .flex()
                    .items_center()
                    .gap_1()
                    .h(px(17.))
                    .px_1()
                    .rounded(px(4.))
                    .bg(theme::BG_RAISED_2)
                    .child(
                        Icon::new(IconName::Network)
                            .size_2p5()
                            .text_color(theme::FG_3),
                    )
                    .child(
                        div()
                            .text_size(px(10.5))
                            .text_color(theme::FG_2)
                            .child(branch),
                    )
            }));

        let heights = vec![COMMIT_ROW; count];
        let viewport = f32::from(self.commits_scroll.bounds().size.height).max(60.0);

        let body = if let Some(failure) = &self.commits.failure {
            v_flex()
                .size_full()
                .gap_2()
                .p_3()
                .child(
                    div()
                        .text_size(TEXT_SMALL)
                        .text_color(theme::EDIT.fg)
                        .child("The history could not be read."),
                )
                .child(
                    div()
                        .text_size(TEXT_SMALL)
                        .text_color(theme::FG_3)
                        .child(failure.clone()),
                )
                .child(
                    Button::new("retry-commits")
                        .small()
                        .outline()
                        .label("Try again")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.request_commits(cx);
                        })),
                )
                .into_any_element()
        } else if count == 0 {
            div()
                .p_3()
                .text_size(TEXT_SMALL)
                .text_color(theme::FG_3)
                .child(if self.commits.loading {
                    "Reading the history…"
                } else {
                    "This repository has no commits yet."
                })
                .into_any_element()
        } else {
            let scroller = list::scroller(
                "commits",
                &self.commits_scroll,
                &heights,
                viewport,
                4,
                |index, top| self.commit_row(index, top, dirty, selected, cx),
            )
            .on_scroll(cx.listener(|_, _: &gpui_kit::ScrollEvent, _, cx| cx.notify()));
            div()
                .relative()
                .size_full()
                .child(scroller)
                .child(ui::scrollbar(
                    "commits",
                    &self.commits_scroll,
                    ScrollbarAxis::Vertical,
                ))
                .into_any_element()
        };

        v_flex()
            .w_full()
            .h(px(self.panes.borrow().commits))
            .flex_shrink_0()
            .bg(theme::BG_PANEL)
            .border_b_1()
            .border_color(theme::LINE)
            .child(header)
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    /// One row of the commit list.
    fn commit_row(
        &self,
        index: usize,
        top: f32,
        dirty: bool,
        selected: Option<(usize, usize)>,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let row = self.commits.row(index, dirty);
        let is_selected = match row {
            Some(CommitRow::WorkingTree) => {
                matches!(self.session.compare, Some(Compare::WorkingTree))
            }
            Some(CommitRow::Commit(commit)) => {
                selected.is_some_and(|(first, last)| commit >= first && commit <= last)
            }
            None => false,
        };
        let mut content = h_flex()
            .relative()
            .h(px(COMMIT_ROW))
            .w_full()
            .pr(px(10.))
            .gap_2()
            .items_center()
            .line_height(ui::ROW_LINE);

        // The rail: a hairline with a dot, so the list reads as a history. The
        // line and the dot share one centre, which is the row's middle.
        let working_tree = matches!(row, Some(CommitRow::WorkingTree));
        content = content.child(
            div()
                .relative()
                .w(px(RAIL))
                .h_full()
                .flex_shrink_0()
                .child(
                    div()
                        .absolute()
                        .left(px(RAIL_X - 0.5))
                        .top_0()
                        .bottom_0()
                        .w(px(1.))
                        .bg(theme::LINE_STRONG),
                )
                .child(
                    div()
                        .absolute()
                        .left(px(RAIL_X - DOT / 2.0))
                        .top(px(COMMIT_ROW / 2.0 - DOT / 2.0))
                        .w(px(DOT))
                        .h(px(DOT))
                        .rounded(px(DOT / 2.0))
                        .border_1()
                        .border_color(if working_tree || is_selected {
                            theme::CHANGED.solid
                        } else {
                            theme::FG_4
                        })
                        .when(working_tree || is_selected, |this| {
                            this.bg(theme::CHANGED.solid)
                        }),
                ),
        );

        match row {
            Some(CommitRow::WorkingTree) => {
                content = content
                    .child(
                        div()
                            .flex_none()
                            .text_size(TEXT_SMALL)
                            .text_color(theme::FG)
                            .child("Working tree"),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(TEXT_MICRO)
                            .text_color(theme::FG_3)
                            .truncate()
                            .child("uncommitted changes"),
                    );
            }
            Some(CommitRow::Commit(position)) => {
                let Some(commit) = self.commits.commits.get(position) else {
                    return div().into_any_element();
                };
                content = content
                    .child(
                        div()
                            .flex_none()
                            .font_family(ui::mono(cx))
                            .text_size(px(11.5))
                            .text_color(theme::FG_4)
                            .child(commit.short_id.clone()),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(TEXT_SMALL)
                            .text_color(theme::FG_2)
                            .truncate()
                            .child(commit.subject.clone()),
                    )
                    .child(
                        div()
                            .flex_none()
                            .text_size(TEXT_MICRO)
                            .text_color(theme::FG_4)
                            .child(relative_time(commit.time, self.now)),
                    );
            }
            None => return div().into_any_element(),
        }

        if is_selected {
            // The accent of the chosen comparison, drawn over the rail.
            content = content.child(
                div()
                    .absolute()
                    .left_0()
                    .top_0()
                    .bottom_0()
                    .w(px(2.))
                    .bg(theme::CHANGED.solid),
            );
        }

        div()
            .absolute()
            .top(px(top))
            .left_0()
            .right_0()
            .child(
                div()
                    .id(gpui_kit::ElementId::NamedInteger(
                        "commit-row".into(),
                        u64::try_from(index).unwrap_or(u64::MAX),
                    ))
                    .w_full()
                    .cursor_pointer()
                    // The background and the hover belong to the element that
                    // carries the id: gpui only tells a view that the pointer
                    // entered or left an element whose state it can key, so a
                    // hover style on an id-less box only showed up on the next
                    // redraw, which the wheel happened to cause.
                    .bg(if is_selected {
                        theme::BG_SELECTED
                    } else {
                        theme::BG_PANEL
                    })
                    .hover(|style| style.bg(theme::BG_HOVER))
                    .on_click(
                        cx.listener(move |this, event: &gpui_kit::ClickEvent, _, cx| {
                            this.pick_commit(index, event.modifiers().shift, cx);
                        }),
                    )
                    .child(content),
            )
            .into_any_element()
    }

    /// Chooses what clicking the row at `index` compares.
    pub(crate) fn pick_commit(&mut self, index: usize, shift: bool, cx: &mut Context<Self>) {
        self.commit_cursor = index;
        self.focus = Focus::Commits;
        let dirty = self.dirty();
        let Some(row) = self.commits.row(index, dirty) else {
            return;
        };
        if let Some(compare) = self.commits.click(row, shift) {
            self.choose(compare, cx);
        }
    }
}
