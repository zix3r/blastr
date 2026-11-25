//! The compare chip of the title bar and the menu of commits behind it.
//!
//! A click on a commit compares it with its parent; a shift-click compares
//! the range from the commit compared now to the one clicked.

use blastr_model::CommitInfo;
use blastr_model::protocol::Commit;
use gpui_kit::component::button::Button;
use gpui_kit::component::popover::Popover;
use gpui_kit::component::{Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{AnyElement, App, Entity, IntoElement, ParentElement, Styled, Window, div, px};

use super::ui;
use crate::state::session::Compare;
use crate::theme::{self, TEXT_MICRO, TEXT_SMALL};
use crate::workspace::Workspace;

/// The chip that says what is compared, and opens the commits.
pub(crate) fn compare_chip(
    workspace: &Entity<Workspace>,
    _: &mut Window,
    cx: &mut App,
) -> AnyElement {
    let (base, head) = workspace
        .read(cx)
        .session
        .compare
        .as_ref()
        .map_or_else(|| ("".to_owned(), "Choose".to_owned()), Compare::labels);
    let trigger = Button::new("compare-chip")
        .outline()
        .small()
        .h(ui::TOOLBAR_TARGET)
        .gap_2()
        .text_size(TEXT_SMALL)
        .tooltip("Choose what to compare")
        .child(
            Icon::new(IconName::Replace)
                .size_3()
                .text_color(theme::FG_3),
        )
        .child(div().truncate().text_color(theme::FG_2).child(base))
        .child(div().text_color(theme::FG_4).child("→"))
        .child(div().truncate().text_color(theme::FG).child(head))
        .child(
            Icon::new(IconName::ChevronDown)
                .size_3()
                .text_color(theme::FG_4),
        );
    let workspace = workspace.clone();
    // The title bar is a drag area: keep it from taking the press. On the
    // chip itself this would also hide the press from the popover, which
    // opens on the mouse-down of the element around its trigger.
    let menu = Popover::new("compare-menu")
        .trigger(trigger)
        .content(move |_, _, cx| {
            let state = workspace.read(cx);
            let current = state.session.compare.clone();
            let now = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |time| time.as_secs() as i64);
            let commits = &state.commits;
            let mut list = v_flex()
                .id("commit-list")
                .w(px(460.))
                .max_h(px(420.))
                .overflow_y_scroll();
            // Always offered: whether the tree is dirty is only known from when
            // the repository opened, and files may have been edited since.
            {
                let selected = current == Some(Compare::WorkingTree);
                let workspace = workspace.clone();
                list = list.child(
                    row("working-tree", selected, theme::CHANGED)
                        .on_click(cx.listener(move |popover, _, window, cx| {
                            workspace.update(cx, |workspace, cx| {
                                workspace.choose(Compare::WorkingTree, cx)
                            });
                            popover.dismiss(window, cx);
                        }))
                        .child(div().text_color(theme::FG).child("Working tree"))
                        .child(div().text_color(theme::FG_3).child("uncommitted changes")),
                );
            }
            for (index, commit) in commits.list.iter().enumerate() {
                let selected = in_comparison(current.as_ref(), commit);
                let workspace = workspace.clone();
                let clicked = commit.clone();
                list = list.child(
                    row(("commit", index), selected, theme::NEUTRAL)
                        .on_click(cx.listener(
                            move |popover, event: &gpui_kit::ClickEvent, window, cx| {
                                let shift = event.modifiers().shift;
                                workspace.update(cx, |workspace, cx| {
                                    let compare = click(
                                        workspace.session.compare.as_ref(),
                                        &workspace.commits.list,
                                        &clicked,
                                        shift,
                                    );
                                    workspace.choose(compare, cx);
                                });
                                popover.dismiss(window, cx);
                            },
                        ))
                        .child(
                            div()
                                .flex_none()
                                .font_family(theme::mono())
                                .text_color(theme::FG_3)
                                .child(commit.short_id.clone()),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .text_color(theme::FG)
                                .child(commit.subject.clone()),
                        )
                        .child(
                            div()
                                .flex_none()
                                .text_size(TEXT_MICRO)
                                .text_color(theme::FG_4)
                                .child(relative_time(commit.time, now)),
                        ),
                );
            }
            let footer = if let Some(failure) = &commits.failure {
                Some(failure.clone())
            } else if commits.loading {
                Some("Reading the history…".to_owned())
            } else if !commits.exhausted {
                None
            } else {
                Some(format!("{} commits", commits.list.len()))
            };
            let more = (!commits.exhausted && !commits.loading).then(|| {
                let workspace = workspace.clone();
                div()
                    .id("more-commits")
                    .px_2()
                    .py_1()
                    .text_size(TEXT_SMALL)
                    .text_color(theme::FG_3)
                    .cursor_pointer()
                    .hover(|style| style.text_color(theme::FG))
                    .on_click(move |_, _, cx| {
                        workspace.update(cx, |workspace, cx| workspace.load_commits(cx))
                    })
                    .child("Show older commits")
            });
            v_flex()
                .gap_1()
                .child(
                    div()
                        .px_2()
                        .text_size(TEXT_MICRO)
                        .text_color(theme::FG_4)
                        .child(
                            "Click to compare a commit with its parent · shift-click for a range",
                        ),
                )
                .child(list)
                .children(more)
                .children(footer.map(|text| {
                    div()
                        .px_2()
                        .text_size(TEXT_MICRO)
                        .text_color(theme::FG_4)
                        .child(text)
                }))
        });
    ui::no_drag(div().id("compare"))
        .child(menu)
        .into_any_element()
}

/// One row of the menu.
fn row(
    id: impl Into<gpui_kit::ElementId>,
    selected: bool,
    accent: theme::Accent,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    h_flex()
        .id(id)
        .h(px(28.))
        .px_2()
        .gap_2()
        .items_center()
        .rounded(theme::RADIUS_MD)
        .text_size(TEXT_SMALL)
        .cursor_pointer()
        .when(selected, |row| row.bg(theme::BG_SELECTED))
        .hover(|style| style.bg(theme::BG_HOVER))
        .child(ui::marker(accent.solid, ui::Marker::Changed))
}

/// Whether `commit` is part of what is compared.
fn in_comparison(compare: Option<&Compare>, commit: &CommitInfo) -> bool {
    match compare {
        Some(Compare::Commit(current)) => current.id == commit.id,
        Some(Compare::Range { oldest, newest, .. }) => {
            oldest.id == commit.id || newest.id == commit.id
        }
        _ => false,
    }
}

/// What a click on `clicked` compares: the commit alone, or with `shift` the
/// range between it and the commit compared now.
fn click(
    current: Option<&Compare>,
    list: &[CommitInfo],
    clicked: &CommitInfo,
    shift: bool,
) -> Compare {
    let reference = |commit: &CommitInfo| Commit {
        id: commit.id.clone(),
        short: commit.short_id.clone(),
    };
    let position = |id: &str| list.iter().position(|commit| commit.id == id);
    if shift
        && let Some(Compare::Commit(anchor)) = current
        && let (Some(from), Some(to)) = (position(&anchor.id), position(&clicked.id))
        && from != to
    {
        // The list is newest first: the larger index is the older commit.
        let (oldest, newest) = (&list[from.max(to)], &list[from.min(to)]);
        return Compare::Range {
            oldest: reference(oldest),
            newest: reference(newest),
            from_parent: oldest.parents > 0,
        };
    }
    Compare::Commit(reference(clicked))
}

/// How long ago a commit was made, in a few characters.
fn relative_time(time: i64, now: i64) -> String {
    const HOUR: i64 = 3600;
    const DAY: i64 = 24 * HOUR;
    match now.saturating_sub(time) {
        age if age < HOUR => format!("{}m ago", age / 60),
        age if age < DAY => format!("{}h ago", age / HOUR),
        age if age < 60 * DAY => format!("{}d ago", age / DAY),
        age if age < 365 * DAY => format!("{}mo ago", age / (30 * DAY)),
        age => format!("{}y ago", age / (365 * DAY)),
    }
}
