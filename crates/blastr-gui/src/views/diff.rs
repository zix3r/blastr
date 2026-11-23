//! Before and after: the selected file on both sides of the comparison.
//!
//! The line pairing comes from the `similar` crate. Unchanged runs are folded
//! into one quiet row with a line count, so a change in a long file is one
//! screen away from the top. The pane shows the whole file, or only the
//! changed symbol and a little of what surrounds it. Each side owns exactly
//! half of the pane and clips itself to it, and the characters that changed
//! inside a rewritten line are marked on that half. A changed pair whose lines
//! are wider than their half is shown from its first difference on, with an
//! `…` where both sides lost the characters before it, so that a change is
//! never out of sight.

use gpui_kit::prelude::*;

use std::ops::Range;
use std::sync::Arc;

use blastr_model::wording;
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::{Icon, IconName, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Context, Hsla, IntoElement, ParentElement, Pixels, SharedString, Styled,
    Window, div, px,
};
use similar::{DiffOp, TextDiff};

use super::doc::Doc;
use super::list;
use super::shell::Shell;
use super::ui;
use crate::theme::{self, TEXT_LINE_NUMBER, TEXT_MICRO, TEXT_MONO, TEXT_SMALL, Tone};

/// Unchanged runs at least this long are folded.
const FOLD_MINIMUM: usize = 8;

/// Lines kept on either side of the changed symbol in symbol scope.
const SYMBOL_CONTEXT: u32 = 3;

/// The width of the sign column of one side of a row.
const SIGN_WIDTH: f32 = 14.0;

/// The room the text of one side keeps at its right end.
const TEXT_PADDING: f32 = 12.0;

/// The advance of one character of the mono family at the size of the code, in
/// pixels, used when the text system cannot be asked: `0.6` of the size.
const FALLBACK_CHAR_WIDTH: f32 = 7.6;

/// The fewest characters a side of the pane is taken to hold.
const MIN_COLUMNS: usize = 8;

/// The characters a shifted pair keeps in view before its first difference,
/// the leading `…` included.
const DIFFERENCE_MARGIN: usize = 4;

/// The characters taken to fit a side before the pane has been laid out: the
/// width is not known then, so a line is assumed to fit and nothing moves.
const UNMEASURED_COLUMNS: usize = 200;

/// The mark that says characters were left out at the left edge of a line.
const ELLIPSIS: char = '…';

/// Folds of the rows outside the symbol scope carry keys from here up, so
/// that they cannot be confused with the first line of a real run.
const SYNTHETIC_KEY: usize = 1_000_000;

/// What the pane shows of the file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) enum Scope {
    /// The whole file.
    File,
    /// The changed symbol's lines, and a little of what surrounds them.
    #[default]
    Symbol,
}

impl Scope {
    /// The label of the scope.
    fn label(self) -> &'static str {
        match self {
            Scope::File => "File",
            Scope::Symbol => "Symbol",
        }
    }
}

/// The view state of the before/after pane.
#[derive(Debug, Default)]
pub(crate) struct DiffState {
    /// The folded runs the user opened, by their first line.
    pub(crate) expanded: Vec<usize>,
    /// Whether the pane shows the symbol or the file.
    pub(crate) scope: Scope,
    /// Index of the hunk the pane is showing.
    pub(crate) hunk: usize,
    /// Scroll to the current hunk before the next paint.
    pub(crate) pending_hunk: bool,
    cache: Option<Cache>,
}

/// The rows of the last pair of documents, with what was read for them.
#[derive(Debug)]
struct Cache {
    analysis: usize,
    key: String,
    rows: Vec<Row>,
    hunks: Vec<usize>,
    /// The width of the widest line of either side, in pixels: the two sides
    /// move sideways together by as much as this exceeds one column.
    widest: f32,
}

/// One row of the paired diff.
#[derive(Debug, Clone)]
enum Row {
    /// A line on one or both sides; `None` is the side that has no line.
    Pair {
        /// The line before, with its number.
        old: Option<(u32, String)>,
        /// The line after, with its number.
        new: Option<(u32, String)>,
        /// Whether the line differs between the two sides.
        changed: bool,
    },
    /// An unchanged run, folded.
    Fold {
        /// The first line number of the run, used as its key.
        key: usize,
        /// How many lines are hidden.
        count: usize,
    },
}

impl Row {
    /// The height of the row in pixels.
    fn height(&self) -> f32 {
        match self {
            Row::Pair { .. } => 19.0,
            Row::Fold { .. } => 24.0,
        }
    }

    /// Whether the row is one of the lines that changed.
    fn is_changed(&self) -> bool {
        matches!(self, Row::Pair { changed: true, .. })
    }

    /// Whether the row is the fold of the lines outside the symbol.
    fn is_synthetic_fold(&self) -> bool {
        matches!(self, Row::Fold { key, .. } if *key >= SYNTHETIC_KEY)
    }
}

impl Shell {
    /// The before/after pane for the selected symbol's file.
    pub(crate) fn diff_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.session.shown().failure().is_some() {
            return self.failure_card(cx);
        }
        let Some(selection) = self.session.selection().cloned() else {
            return v_flex()
                .size_full()
                .bg(theme::BG_CANVAS)
                .child(super::ui::empty_state(
                    IconName::GalleryVerticalEnd,
                    Some(theme::CHANGED),
                    "Nothing to compare",
                    "Select a change or an affected symbol to see the file before and after.",
                ))
                .into_any_element();
        };
        let Some(analysis_arc) = self.session.analysis() else {
            return v_flex()
                .size_full()
                .bg(theme::BG_CANVAS)
                .child(super::ui::empty_state(
                    IconName::Loader,
                    Some(theme::CHANGED),
                    "Waiting for the comparison",
                    "The before/after view appears with the result.",
                ))
                .into_any_element();
        };
        let Some((_, location)) = self.selection_location(&selection) else {
            return v_flex()
                .size_full()
                .bg(theme::BG_CANVAS)
                .child(super::ui::empty_state(
                    IconName::GalleryVerticalEnd,
                    None,
                    "Nothing to compare",
                    "This symbol has no location in the compared revision.",
                ))
                .into_any_element();
        };
        let comparison = analysis_arc.diff.comparison.clone();
        let path = location.file.clone();
        let base = self.docs.get(&comparison.base, &path);
        let head = self.docs.get(&comparison.head, &path);
        let base_lines = base.as_ref().map_or(0, |doc| doc.len());
        let head_lines = head.as_ref().map_or(0, |doc| doc.len());

        let scope = self.diff.scope;
        let range = (location.span.start_line, location.span.end_line);
        let window = match scope {
            Scope::File => None,
            Scope::Symbol => Some((
                range.0.saturating_sub(SYMBOL_CONTEXT),
                range.1.saturating_add(SYMBOL_CONTEXT),
            )),
        };
        let key = format!(
            "{}:{}:{}:{scope:?}:{}:{}",
            path,
            wording_source(&comparison.base),
            wording_source(&comparison.head),
            range.0,
            range.1
        );
        // One gutter for both sides, wide enough for the longer file, so that
        // the two halves start their text at the same distance from the split.
        let lines = base_lines.max(head_lines);
        let gutter = theme::gutter_width(u32::try_from(lines).unwrap_or(u32::MAX));
        // The advance of one character of the code, asked of the text system
        // once per frame so that the width of a side can be turned into a
        // number of characters.
        let char_width = mono_advance(cx);
        let cache_key = Arc::as_ptr(analysis_arc) as usize;
        if self
            .diff
            .cache
            .as_ref()
            .is_none_or(|cache| cache.analysis != cache_key || cache.key != key)
        {
            let rows = pair_rows(base.as_deref(), head.as_deref(), window);
            let hunks = hunk_starts(&rows);
            let widest = widest_side(&rows, gutter, char_width);
            self.diff.cache = Some(Cache {
                analysis: cache_key,
                key,
                rows,
                hunks,
                widest,
            });
            self.diff.hunk = 0;
            self.diff.pending_hunk = true;
            self.diff_scroll_x
                .set_offset(gpui_kit::point(px(0.), px(0.)));
        }

        let Some(cache) = &self.diff.cache else {
            return div().into_any_element();
        };
        let rows = &cache.rows;
        let hunks = &cache.hunks;
        let heights: Vec<f32> = rows.iter().map(Row::height).collect();
        let viewport = f32::from(self.diff_scroll.bounds().size.height).max(80.0);
        let (added, removed) = count_changes(rows);

        // The hunk the arrows move from; clamped so that a stale index after a
        // refresh still names a hunk.
        let hunk = self.diff.hunk.min(hunks.len().saturating_sub(1));
        if let Some(target) = hunks.get(hunk).copied() {
            let wanted = std::mem::take(&mut self.diff.pending_hunk);
            if wanted {
                let offset = list::offset_to_show(&heights, target, 0.0, viewport);
                self.diff_scroll
                    .set_offset(gpui_kit::point(px(0.), px(-offset)));
            }
        }

        let mono = theme::mono();
        let timeline = self.diff_head(&comparison, path.as_str());
        // Each side is exactly half of the pane, whatever the length of its
        // lines. A line too long for its half is read by moving both sides
        // sideways together, so that the two sides stay aligned.
        let measured = f32::from(self.diff_scroll.bounds().size.width);
        let column = measured.max(320.0) / 2.0;
        // How many characters of the code one side holds, beside its line
        // number and sign: a changed pair wider than this is read from its
        // first difference on, so that the change is in view without a
        // gesture.
        let columns = if measured > 0.0 {
            let room = (column - f32::from(gutter) - SIGN_WIDTH - TEXT_PADDING).max(char_width);
            (room / char_width).floor().max(1.0) as usize
        } else {
            UNMEASURED_COLUMNS
        };
        let shift_max = (cache.widest - column).max(0.0);
        let stored = -f32::from(self.diff_scroll_x.offset().x);
        let shift = stored.clamp(0.0, shift_max);
        if (shift - stored).abs() > 0.01 {
            self.diff_scroll_x
                .set_offset(gpui_kit::point(px(-shift), px(0.)));
        }

        let expanded = self.diff.expanded.clone();
        let body = list::scroller(
            "diff",
            &self.diff_scroll,
            &heights,
            viewport,
            8,
            |index, top| {
                let Some(row) = rows.get(index) else {
                    return div().into_any_element();
                };
                let element: AnyElement = match row {
                    Row::Pair { old, new, changed } => {
                        let old_text = old.as_ref().map_or("", |(_, text)| text.as_str());
                        let new_text = new.as_ref().map_or("", |(_, text)| text.as_str());
                        // The characters that differ inside the pair, marked on
                        // the side they belong to.
                        let spans = (*changed).then(|| change_spans(old_text, new_text));
                        let empty: &[Range<usize>] = &[];
                        let (before_spans, after_spans) = match &spans {
                            Some(spans) => (spans.before.as_slice(), spans.after.as_slice()),
                            None => (empty, empty),
                        };
                        // A pair too wide for its half is shown from its first
                        // difference on. Both sides lose the same characters,
                        // so the two lines still line up; the mark that says so
                        // is drawn where the characters were left out.
                        let skip = if *changed {
                            pair_window(old_text, new_text, columns)
                        } else {
                            0
                        };
                        h_flex()
                            .h(px(19.))
                            .w_full()
                            .flex_shrink_0()
                            .child(diff_side(Side {
                                line: old.as_ref(),
                                before: true,
                                changed: *changed,
                                spans: before_spans,
                                skip,
                                gutter,
                                shift,
                                mono: &mono,
                            }))
                            .child(diff_side(Side {
                                line: new.as_ref(),
                                before: false,
                                changed: *changed,
                                spans: after_spans,
                                skip,
                                gutter,
                                shift,
                                mono: &mono,
                            }))
                            .into_any_element()
                    }
                    Row::Fold { key, count } => {
                        let opened = expanded.contains(key);
                        let synthetic = row.is_synthetic_fold();
                        let key = *key;
                        let count = *count;
                        div()
                            .id(gpui_kit::ElementId::NamedInteger(
                                "diff-fold".into(),
                                u64::try_from(key).unwrap_or(u64::MAX),
                            ))
                            .h(px(24.))
                            .w_full()
                            .flex()
                            .items_center()
                            .justify_center()
                            .gap_2()
                            .bg(theme::BG_PANEL)
                            .border_t_1()
                            .border_b_1()
                            .border_color(theme::LINE_SOFT)
                            .text_size(px(11.5))
                            .text_color(theme::FG_3)
                            .cursor_pointer()
                            .hover(|style| style.text_color(theme::FG))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                if synthetic {
                                    // The lines were left out to keep the symbol
                                    // alone: opening them means showing the file.
                                    this.diff.scope = Scope::File;
                                } else if opened {
                                    this.diff.expanded.retain(|line| *line != key);
                                } else {
                                    this.diff.expanded.push(key);
                                }
                                cx.notify();
                            }))
                            .child(div().text_color(theme::FG_4).child("⋯"))
                            .child(div().font_family(mono.clone()).child(count.to_string()))
                            .child("unchanged lines")
                            .into_any_element()
                    }
                };
                div()
                    .absolute()
                    .top(px(top))
                    .left_0()
                    .right_0()
                    .child(element)
                    .into_any_element()
            },
        )
        .on_scroll(
            cx.listener(|this, event: &gpui_kit::ScrollEvent, window, cx| {
                this.shift_diff(event, window, cx);
            }),
        );

        // The split is marked, as in the headers, so that the two halves read
        // as one column each however long their lines are.
        let split = (measured > 0.0).then(|| {
            div()
                .absolute()
                .top_0()
                .bottom_0()
                .left(px(column))
                .w(px(1.))
                .bg(theme::LINE)
        });

        // One bar under each half, for the lines that are wider than it. Both
        // halves move by the same offset, so the two bars stand for one shift:
        // dragging either reads the tail of a line without losing the pairing.
        let shift_size = gpui_kit::size(px(cache.widest), px(1.));
        let before_bar = div()
            .absolute()
            .left_0()
            .bottom_0()
            .w(px(column))
            .h(px(ui::scroll::TRACK))
            .child(ui::scrollbar_of(
                "diff-before",
                &self.diff_scroll_x,
                ScrollbarAxis::Horizontal,
                shift_size,
            ));
        let after_bar = div()
            .absolute()
            .left(px(column))
            .right_0()
            .bottom_0()
            .h(px(ui::scroll::TRACK))
            .child(ui::scrollbar_of(
                "diff-after",
                &self.diff_scroll_x,
                ScrollbarAxis::Horizontal,
                shift_size,
            ));

        v_flex()
            .size_full()
            .bg(theme::BG_CANVAS)
            .child(timeline)
            .child(self.diff_note(&selection, added, removed, hunk, hunks.len(), cx))
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .overflow_hidden()
                    .relative()
                    .child(body)
                    .children(split)
                    .child(before_bar)
                    .child(after_bar)
                    // The vertical bar stops where the two bars under the
                    // halves begin, so the three never share a corner.
                    .child(
                        div()
                            .absolute()
                            .top_0()
                            .left_0()
                            .right_0()
                            .bottom(px(ui::scroll::TRACK))
                            .child(ui::scrollbar(
                                "diff",
                                &self.diff_scroll,
                                ScrollbarAxis::Vertical,
                            )),
                    ),
            )
            .into_any_element()
    }

    /// Moves both sides of the diff sideways, so that the tail of a line too
    /// long for its half can be read.
    ///
    /// A sideways wheel or trackpad gesture moves the text, as does the wheel
    /// with the shift key held. Both sides read the same offset from
    /// `diff_scroll_x`, so a line and its counterpart always line up.
    fn shift_diff(
        &mut self,
        event: &gpui_kit::ScrollEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let delta = event.delta.pixel_delta(window.line_height());
        let step = if delta.x == px(0.) {
            if event.modifiers.shift {
                f32::from(delta.y)
            } else {
                0.0
            }
        } else {
            f32::from(delta.x)
        };
        if step != 0.0 {
            let widest = self.diff.cache.as_ref().map_or(0.0, |cache| cache.widest);
            let column = f32::from(self.diff_scroll.bounds().size.width).max(320.0) / 2.0;
            let most = (widest - column).max(0.0);
            let now = -f32::from(self.diff_scroll_x.offset().x);
            let next = (now + step).clamp(0.0, most);
            if (next - now).abs() > f32::EPSILON {
                self.diff_scroll_x
                    .set_offset(gpui_kit::point(px(-next), px(0.)));
            }
        }
        cx.notify();
    }

    /// The two column headers: the revision on each side and the file.
    ///
    /// Each header takes exactly half of the row, over the half of the pane
    /// its side is drawn in, so that the two labels sit over their columns.
    fn diff_head(&self, comparison: &blastr_model::Comparison, path: &str) -> AnyElement {
        let (before, after) = comparison_revisions(self, comparison);
        let (directory, file) = split_path(path);
        let mono = theme::mono();
        let head = h_flex()
            .h(px(30.))
            .w_full()
            .flex_shrink_0()
            .border_b_1()
            .border_color(theme::LINE_SOFT)
            .text_size(TEXT_SMALL)
            .text_color(theme::FG_3)
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .flex_shrink_0()
                    .px_3()
                    .gap_2()
                    .items_center()
                    .overflow_hidden()
                    .child(
                        Icon::new(IconName::ChevronLeft)
                            .size_3()
                            .flex_shrink_0()
                            .text_color(theme::FG_4),
                    )
                    .child(div().flex_shrink_0().child("Before ·"))
                    .child(
                        h_flex()
                            .min_w_0()
                            .overflow_hidden()
                            .font_family(mono.clone())
                            .child(
                                div()
                                    .whitespace_nowrap()
                                    .text_color(theme::FG_4)
                                    .child(directory.clone()),
                            )
                            .child(
                                div()
                                    .whitespace_nowrap()
                                    .text_color(theme::FG_2)
                                    .child(file.clone()),
                            ),
                    )
                    .child(revision_chip(before)),
            )
            .child(
                h_flex()
                    .flex_1()
                    .min_w_0()
                    .flex_shrink_0()
                    .px_3()
                    .gap_2()
                    .items_center()
                    .overflow_hidden()
                    .border_l_1()
                    .border_color(theme::LINE)
                    .child(
                        Icon::new(IconName::ChevronRight)
                            .size_3()
                            .flex_shrink_0()
                            .text_color(theme::FG_4),
                    )
                    .child(div().flex_shrink_0().child("After ·"))
                    .child(
                        h_flex()
                            .min_w_0()
                            .overflow_hidden()
                            .font_family(mono)
                            .child(
                                div()
                                    .whitespace_nowrap()
                                    .text_color(theme::FG_4)
                                    .child(directory),
                            )
                            .child(
                                div()
                                    .whitespace_nowrap()
                                    .text_color(theme::FG_2)
                                    .child(file),
                            ),
                    )
                    .child(revision_chip(after)),
            );
        head.into_any_element()
    }

    /// The row above the diff: what changed, how much of it, and the scope.
    fn diff_note(
        &self,
        selection: &crate::state::selection::Selection,
        added: usize,
        removed: usize,
        hunk: usize,
        hunks: usize,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut row = h_flex()
            .h(px(30.))
            .flex_shrink_0()
            .px_3()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(theme::LINE_SOFT)
            .text_size(TEXT_SMALL)
            .text_color(theme::FG_3);
        if let Some(change) = self.selected_change().cloned() {
            let accent = theme::CHANGED;
            row = row
                .child(super::ui::marker(accent.solid, super::ui::Marker::Changed))
                .child(
                    div()
                        .flex_shrink_0()
                        .font_family(theme::mono())
                        .text_color(theme::FG_2)
                        .child(wording::short_name(&change.symbol.path).name),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .h(px(17.))
                        .px_1()
                        .rounded(theme::RADIUS_SM)
                        .bg(accent.bg)
                        .text_size(TEXT_MICRO)
                        .text_color(accent.fg)
                        .child(wording::change_label(&change)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_size(px(11.5))
                        .text_color(theme::FG_4)
                        .text_ellipsis()
                        .child(super::evidence::change_note(&change)),
                );
        } else {
            let name = wording::short_name(&selection.symbol.path);
            row = row
                .child(
                    div()
                        .flex_shrink_0()
                        .font_family(theme::mono())
                        .text_color(theme::FG_2)
                        .child(name.name),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_size(px(11.5))
                        .text_color(theme::FG_4)
                        .text_ellipsis()
                        .child(if name.module.is_empty() {
                            String::new()
                        } else {
                            name.module
                        }),
                );
        }
        row = row
            .child(
                div()
                    .flex_shrink_0()
                    .font_family(theme::mono())
                    .text_size(px(11.5))
                    .text_color(theme::ADD_FG)
                    .child(format!("+{added}")),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .font_family(theme::mono())
                    .text_size(px(11.5))
                    .text_color(theme::DEL_FG)
                    .child(format!("−{removed}")),
            );

        let mut toggle = h_flex()
            .flex_shrink_0()
            .rounded(theme::RADIUS_SM)
            .border_1()
            .border_color(theme::LINE)
            .overflow_hidden();
        for scope in [Scope::Symbol, Scope::File] {
            let selected = self.diff.scope == scope;
            toggle = toggle.child(
                div()
                    .id(gpui_kit::ElementId::Name(
                        format!("diff-scope-{}", scope.label()).into(),
                    ))
                    .h(px(20.))
                    .px_2()
                    .flex()
                    .items_center()
                    .bg(if selected {
                        theme::BG_RAISED_2
                    } else {
                        theme::BG_INPUT
                    })
                    .text_size(TEXT_MICRO)
                    .font_weight(if selected {
                        gpui_kit::FontWeight::SEMIBOLD
                    } else {
                        gpui_kit::FontWeight::NORMAL
                    })
                    .text_color(if selected { theme::FG } else { theme::FG_3 })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.diff.scope = scope;
                        this.diff.hunk = 0;
                        cx.notify();
                    }))
                    .child(scope.label()),
            );
        }
        let mut row = row.child(toggle);
        if hunks > 1 {
            row = row
                .child(nav_button(
                    "hunk-prev",
                    IconName::ChevronLeft,
                    "Previous hunk",
                    cx.listener({
                        let next = hunk.saturating_sub(1);
                        move |this, _, _, cx| this.step_hunk(next, cx)
                    }),
                ))
                .child(
                    div()
                        .flex_shrink_0()
                        .text_size(TEXT_MICRO)
                        .text_color(theme::FG_3)
                        .child(format!("{} of {hunks}", hunk + 1)),
                )
                .child(nav_button(
                    "hunk-next",
                    IconName::ChevronRight,
                    "Next hunk",
                    cx.listener({
                        let next = (hunk + 1).min(hunks - 1);
                        move |this, _, _, cx| this.step_hunk(next, cx)
                    }),
                ));
        }
        row.into_any_element()
    }

    /// Shows hunk `index` and scrolls to it.
    pub(crate) fn step_hunk(&mut self, index: usize, cx: &mut Context<Self>) {
        self.diff.hunk = index;
        self.diff.pending_hunk = true;
        cx.notify();
    }
}

/// A small arrow button of the hunk navigation.
fn nav_button(
    id: &'static str,
    icon: IconName,
    tooltip: &'static str,
    on_click: impl Fn(&gpui_kit::ClickEvent, &mut Window, &mut gpui_kit::App) + 'static,
) -> AnyElement {
    super::ui::icon_button(id, icon, tooltip)
        .on_click(on_click)
        .into_any_element()
}

/// The label of the revision each side shows.
fn comparison_revisions(shell: &Shell, comparison: &blastr_model::Comparison) -> (String, String) {
    if let Some(compare) = &shell.session.compare {
        return compare.labels();
    }
    (comparison.base.to_string(), comparison.head.to_string())
}

/// The chip that says which revision a side shows.
fn revision_chip(label: String) -> AnyElement {
    div()
        .flex_shrink_0()
        .h(px(17.))
        .px_1()
        .rounded(theme::RADIUS_SM)
        .border_1()
        .border_color(theme::LINE)
        .bg(theme::BG_RAISED)
        .text_size(px(10.5))
        .text_color(theme::FG_4)
        .child(label)
        .into_any_element()
}

/// The path split at its last separator.
fn split_path(path: &str) -> (String, String) {
    match path.rfind('/') {
        Some(at) => (path[..=at].to_owned(), path[at + 1..].to_owned()),
        None => (String::new(), path.to_owned()),
    }
}

/// One side of a diff row as it is drawn.
struct Side<'a> {
    /// The line and its number; `None` for the side that has no line.
    line: Option<&'a (u32, String)>,
    /// Whether this is the before side.
    before: bool,
    /// Whether the line differs from its counterpart.
    changed: bool,
    /// The characters that changed inside the line, by index.
    spans: &'a [Range<usize>],
    /// The characters left out at the left edge of the line and replaced by
    /// `…`; both sides of a pair leave out the same number, so that the two
    /// lines still line up.
    skip: usize,
    /// The width of the line-number column.
    gutter: Pixels,
    /// How far the text of the whole pane is moved sideways.
    shift: f32,
    /// The mono family.
    mono: &'a SharedString,
}

/// One side of a diff row: the line number, the sign and the line itself.
///
/// The side takes exactly half of the row and clips its text to that half, so
/// a long line can never spill into the other side. `shift` moves the text
/// sideways, by the same amount on both sides. `spans` names the characters
/// that changed inside the line, which are drawn on a stronger tint; they are
/// cut to the window, so the tint always lands on the characters the reader
/// sees.
fn diff_side(side: Side<'_>) -> AnyElement {
    let Side {
        line,
        before,
        changed,
        spans,
        skip,
        gutter,
        shift,
        mono,
    } = side;
    let (number, text) = match line {
        Some((number, text)) => (number.to_string(), text.as_str()),
        None => (String::new(), ""),
    };
    let (background, sign, colour) = match (&line, before, changed) {
        (None, _, _) => (theme::BG_PANEL, "", theme::FG_4),
        (Some(_), true, true) => (theme::DEL_BG, "−", theme::DEL_FG),
        (Some(_), false, true) => (theme::ADD_BG, "+", theme::ADD_FG),
        (Some(_), _, false) => (theme::BG_CANVAS, "", theme::FG_4),
    };
    let tint = if before { theme::DEL_FG } else { theme::ADD_FG };

    let mut text_row = h_flex()
        .absolute()
        .top_0()
        .left(px(-shift))
        .h(px(19.))
        .items_center()
        .whitespace_nowrap()
        .font_family(mono.clone())
        .text_size(TEXT_MONO)
        .text_color(if line.is_some() {
            theme::FG
        } else {
            theme::FG_4
        });
    if line.is_some() {
        for run in line_runs(text, spans, skip) {
            let piece: AnyElement = match run {
                Run::Ellipsis => div()
                    .flex_none()
                    .whitespace_nowrap()
                    .text_color(theme::FG_4)
                    .child(ELLIPSIS.to_string())
                    .into_any_element(),
                Run::Text { text, changed } => div()
                    .flex_none()
                    .whitespace_nowrap()
                    .when(changed, |this| this.bg(span_tint(tint)).rounded(px(2.)))
                    .child(text)
                    .into_any_element(),
            };
            text_row = text_row.child(piece);
        }
    }

    h_flex()
        .flex_1()
        .min_w_0()
        .flex_shrink_0()
        .h_full()
        .overflow_hidden()
        .bg(background)
        .child(
            div()
                .w(gutter)
                .flex_shrink_0()
                .pr_2()
                .text_right()
                .border_r_1()
                .border_color(theme::LINE_SOFT)
                .font_family(mono.clone())
                .text_size(TEXT_LINE_NUMBER)
                .text_color(colour)
                .child(number),
        )
        .child(
            div()
                .w(px(SIGN_WIDTH))
                .flex_shrink_0()
                .text_center()
                .font_family(mono.clone())
                .text_size(px(11.))
                .text_color(colour)
                .child(sign),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .h_full()
                .overflow_hidden()
                .relative()
                .child(text_row),
        )
        .into_any_element()
}

/// The tint behind the characters that changed inside a line.
///
/// It is the colour of the side's own sign, strong enough to be told apart
/// from the quiet tint of the whole line — which is a tenth of the way to the
/// sign's colour — and still quiet enough to read the characters over it.
fn span_tint(colour: Tone) -> Hsla {
    let mut tint = colour.hsla();
    tint.a = 0.45;
    tint
}

/// One piece of a line as it is drawn.
#[derive(Debug, Clone, PartialEq, Eq)]
enum Run {
    /// The mark that says characters were left out at the left edge.
    Ellipsis,
    /// Characters of the line, and whether they are ones that changed.
    Text {
        /// The characters of the piece.
        text: String,
        /// Whether the piece is one of the characters that changed.
        changed: bool,
    },
}

/// The runs of a line as it is drawn: the `…` that says characters were left
/// out at the left edge, then the rest of the line in as many pieces as it has
/// changed spans.
///
/// `skip` characters are dropped from the front of the line — the same number
/// on both sides of a pair, so the two lines still line up — and the spans are
/// moved to where those characters now are.
fn line_runs(text: &str, spans: &[Range<usize>], skip: usize) -> Vec<Run> {
    let total = text.chars().count();
    let skip = skip.min(total);
    let tail = characters(text, skip, total);
    let mut runs = Vec::new();
    if skip > 0 {
        runs.push(Run::Ellipsis);
    }
    for (text, changed) in segments(&tail, &shifted_spans(spans, skip)) {
        runs.push(Run::Text { text, changed });
    }
    runs
}

/// The spans of `ranges` moved past the `skip` characters dropped from the
/// front of a line; spans that fell away are left out.
fn shifted_spans(ranges: &[Range<usize>], skip: usize) -> Vec<Range<usize>> {
    ranges
        .iter()
        .filter_map(|range| {
            let start = range.start.saturating_sub(skip);
            let end = range.end.saturating_sub(skip);
            (end > start).then_some(start..end)
        })
        .collect()
}

/// The index, in characters, of the first character that differs between two
/// lines; the length of the shorter line when one begins the other.
fn first_difference(before: &str, after: &str) -> usize {
    before
        .chars()
        .zip(after.chars())
        .take_while(|(before, after)| before == after)
        .count()
}

/// How many characters are dropped from the front of both sides of a changed
/// pair, so that the change is inside a column of `columns` characters.
///
/// A pair that fits is shown whole. A wider one loses a prefix, the same on
/// both sides, and keeps its first difference at least [`DIFFERENCE_MARGIN`]
/// characters from the left edge, the `…` included: a change at the start of a
/// very long line is never cut off to show the end of it.
fn pair_window(before: &str, after: &str, columns: usize) -> usize {
    let columns = columns.max(MIN_COLUMNS);
    let longest = before.chars().count().max(after.chars().count());
    if longest <= columns {
        return 0;
    }
    // The `…` takes one of the columns the line may use.
    let needed = longest + 1 - columns;
    let keep = first_difference(before, after).saturating_sub(DIFFERENCE_MARGIN - 1);
    needed.min(keep)
}

/// The characters that differ between the two lines of a changed pair, by
/// index on each side.
#[derive(Debug, Default, PartialEq, Eq)]
struct Spans {
    /// The changed characters of the before line.
    before: Vec<Range<usize>>,
    /// The changed characters of the after line.
    after: Vec<Range<usize>>,
}

/// The ranges of characters that differ between two lines, one list for each
/// side, as a reader of a rewritten line needs them.
fn change_spans(before: &str, after: &str) -> Spans {
    let diff = TextDiff::from_chars(before, after);
    let mut spans = Spans::default();
    for op in diff.ops() {
        match *op {
            DiffOp::Equal { .. } => {}
            DiffOp::Delete {
                old_index, old_len, ..
            } => spans.before.push(old_index..old_index + old_len),
            DiffOp::Insert {
                new_index, new_len, ..
            } => spans.after.push(new_index..new_index + new_len),
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                spans.before.push(old_index..old_index + old_len);
                spans.after.push(new_index..new_index + new_len);
            }
        }
    }
    spans
}

/// `text` cut at `spans`, as the pieces to draw in order, each marked with
/// whether it is one of the characters that changed.
fn segments(text: &str, spans: &[Range<usize>]) -> Vec<(String, bool)> {
    let total = text.chars().count();
    let mut pieces = Vec::new();
    let mut at = 0;
    for span in spans {
        let start = span.start.min(total).max(at);
        let end = span.end.min(total).max(start);
        if start > at {
            pieces.push((characters(text, at, start), false));
        }
        if end > start {
            pieces.push((characters(text, start, end), true));
        }
        at = end;
    }
    if at < total {
        pieces.push((characters(text, at, total), false));
    }
    pieces
}

/// The characters of `text` from `from` to `to`, by index.
fn characters(text: &str, from: usize, to: usize) -> String {
    text.chars()
        .skip(from)
        .take(to.saturating_sub(from))
        .collect()
}

/// Pairs the lines of the two sides.
///
/// Unchanged runs are folded into one row. With a `window`, only the lines
/// inside it are drawn, and everything else becomes a fold; that is how the
/// pane shows one symbol and nothing of the rest of the file.
fn pair_rows(base: Option<&Doc>, head: Option<&Doc>, window: Option<(u32, u32)>) -> Vec<Row> {
    let (Some(base), Some(head)) = (base, head) else {
        return Vec::new();
    };
    let old_text = base.lines.join("\n");
    let new_text = head.lines.join("\n");
    let diff = TextDiff::from_lines(old_text.as_str(), new_text.as_str());
    let old_line = |index: usize| {
        diff.old_slice(index)
            .map(|line| line.trim_end_matches('\n').to_owned())
            .unwrap_or_default()
    };
    let new_line = |index: usize| {
        diff.new_slice(index)
            .map(|line| line.trim_end_matches('\n').to_owned())
            .unwrap_or_default()
    };
    let mut rows = Rows::default();
    for op in diff.ops() {
        match *op {
            DiffOp::Equal {
                old_index,
                new_index,
                len,
            } => {
                if window.is_none() && len >= FOLD_MINIMUM {
                    rows.fold(old_index + 1, len);
                    continue;
                }
                for offset in 0..len {
                    rows.pair(
                        Some((number(old_index + offset), old_line(old_index + offset))),
                        Some((number(new_index + offset), new_line(new_index + offset))),
                        false,
                        window,
                    );
                }
            }
            DiffOp::Delete {
                old_index, old_len, ..
            } => {
                for offset in 0..old_len {
                    rows.pair(
                        Some((number(old_index + offset), old_line(old_index + offset))),
                        None,
                        true,
                        window,
                    );
                }
            }
            DiffOp::Insert {
                new_index, new_len, ..
            } => {
                for offset in 0..new_len {
                    rows.pair(
                        None,
                        Some((number(new_index + offset), new_line(new_index + offset))),
                        true,
                        window,
                    );
                }
            }
            DiffOp::Replace {
                old_index,
                old_len,
                new_index,
                new_len,
            } => {
                let count = old_len.max(new_len);
                for offset in 0..count {
                    rows.pair(
                        (offset < old_len)
                            .then(|| (number(old_index + offset), old_line(old_index + offset))),
                        (offset < new_len)
                            .then(|| (number(new_index + offset), new_line(new_index + offset))),
                        true,
                        window,
                    );
                }
            }
        }
    }
    let mut rows = rows.finish();
    dedent(&mut rows);
    rows
}

/// The rows of the diff as they are built.
#[derive(Debug, Default)]
struct Rows {
    rows: Vec<Row>,
    /// Lines outside the window that the next fold will hide.
    hidden: usize,
    /// How many folds of the window have been made, to key them apart.
    synthetic: usize,
}

impl Rows {
    /// A fold of `count` lines starting at `key`.
    fn fold(&mut self, key: usize, count: usize) {
        self.flush();
        self.rows.push(Row::Fold { key, count });
    }

    /// A pair of lines, kept when it is inside the window.
    fn pair(
        &mut self,
        old: Option<(u32, String)>,
        new: Option<(u32, String)>,
        changed: bool,
        window: Option<(u32, u32)>,
    ) {
        if let Some((from, to)) = window {
            let inside = |line: &Option<(u32, String)>| {
                line.as_ref()
                    .is_some_and(|(number, _)| *number >= from && *number <= to)
            };
            if !inside(&old) && !inside(&new) {
                self.hidden += 1;
                return;
            }
            self.flush();
        }
        self.rows.push(Row::Pair { old, new, changed });
    }

    /// Turns the lines hidden so far into one row.
    fn flush(&mut self) {
        if self.hidden == 0 {
            return;
        }
        let key = SYNTHETIC_KEY + self.synthetic;
        self.synthetic += 1;
        self.rows.push(Row::Fold {
            key,
            count: self.hidden,
        });
        self.hidden = 0;
    }

    /// The rows, with a last fold for what is still hidden.
    fn finish(mut self) -> Vec<Row> {
        self.flush();
        self.rows
    }
}

/// The rows that start a run of changed lines.
fn hunk_starts(rows: &[Row]) -> Vec<usize> {
    let mut starts = Vec::new();
    let mut previous_changed = false;
    for (index, row) in rows.iter().enumerate() {
        if row.is_changed() && !previous_changed {
            starts.push(index);
        }
        previous_changed = row.is_changed();
    }
    starts
}

/// How many lines the change adds and removes.
///
/// A paired line that changed counts once on each side, so that a rewritten
/// line reads as one addition and one removal.
fn count_changes(rows: &[Row]) -> (usize, usize) {
    let mut added = 0;
    let mut removed = 0;
    for row in rows {
        if let Row::Pair { old, new, changed } = row
            && *changed
        {
            if old.is_some() {
                removed += 1;
            }
            if new.is_some() {
                added += 1;
            }
        }
    }
    (added, removed)
}

/// The number of leading whitespace characters of `text`, or `None` when the
/// line holds nothing but whitespace.
fn indent(text: &str) -> Option<usize> {
    let trimmed = text.trim_start();
    if trimmed.is_empty() {
        return None;
    }
    Some(text.len() - trimmed.len())
}

/// `text` without its first `count` characters, or without all the
/// indentation it has when it has less.
fn unindented(text: &str, count: usize) -> String {
    match indent(text) {
        Some(own) => text.get(own.min(count)..).unwrap_or("").to_owned(),
        None => String::new(),
    }
}

/// Removes the indentation that the shown lines of both sides have in common,
/// so that deeply nested code is readable in the half of the pane it gets.
///
/// Both sides lose the same amount, taken over the lines of both, so that a
/// changed line and its counterpart line up. The indentation the lines have
/// relative to each other is kept.
fn dedent(rows: &mut [Row]) {
    let common = rows
        .iter()
        .filter_map(|row| match row {
            Row::Pair { old, new, .. } => Some([old, new]),
            Row::Fold { .. } => None,
        })
        .flatten()
        .filter_map(|line| line.as_ref().and_then(|(_, text)| indent(text)))
        .min()
        .unwrap_or(0);
    if common == 0 {
        return;
    }
    for row in rows.iter_mut() {
        if let Row::Pair { old, new, .. } = row {
            for line in [old, new].into_iter().flatten() {
                line.1 = unindented(&line.1, common);
            }
        }
    }
}

/// The width of the widest line of either side, in pixels, with its line
/// number, sign and padding.
///
/// A side with a line wider than its half of the pane is read by moving both
/// sides sideways.
fn widest_side(rows: &[Row], gutter: Pixels, char_width: f32) -> f32 {
    let mut widest = 0;
    for row in rows {
        if let Row::Pair { old, new, .. } = row {
            for (_, text) in [old, new].into_iter().flatten() {
                widest = widest.max(text.chars().count());
            }
        }
    }
    f32::from(gutter) + SIGN_WIDTH + TEXT_PADDING + widest as f32 * char_width
}

/// The advance of one character of the mono family at the size of the code.
///
/// The room one side of the pane has is turned into a number of characters
/// with it, so a family of another width only changes how much of a line is
/// shown. The text system knows the family the theme picked; `0.6` of the size
/// stands in when it cannot measure.
fn mono_advance(cx: &App) -> f32 {
    let text_system = cx.text_system();
    let family = gpui_kit::font(theme::mono());
    let font_id = text_system.resolve_font(&family);
    match text_system.advance(font_id, TEXT_MONO, 'M') {
        Ok(size) => {
            let width = f32::from(size.width);
            if (1.0..=40.0).contains(&width) {
                width
            } else {
                FALLBACK_CHAR_WIDTH
            }
        }
        Err(_) => FALLBACK_CHAR_WIDTH,
    }
}

/// A line number, counting from one.
fn number(index: usize) -> u32 {
    u32::try_from(index).unwrap_or(u32::MAX).saturating_add(1)
}

/// The name of a source, for the cache key.
fn wording_source(source: &blastr_model::Source) -> String {
    source.to_string()
}

#[cfg(test)]
mod tests {
    use super::*;
    use blastr_model::{RelPath, Source};

    fn doc(lines: &[&str]) -> Doc {
        Doc {
            source: Source::Worktree,
            path: RelPath::new("src/lib.rs"),
            lines: lines.iter().map(|line| (*line).to_owned()).collect(),
        }
    }

    #[test]
    fn an_unchanged_file_has_one_fold() {
        let base = doc(&["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]);
        let head = base.clone();
        let rows = pair_rows(Some(&base), Some(&head), None);
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0], Row::Fold { count: 10, .. }));
    }

    #[test]
    fn a_changed_line_is_paired_with_its_replacement() {
        let base = doc(&["fn a() {}", "fn b() {}"]);
        let head = doc(&["fn a() { 1 }", "fn b() {}"]);
        let rows = pair_rows(Some(&base), Some(&head), None);
        assert!(rows.iter().any(|row| matches!(
            row,
            Row::Pair {
                old: Some((1, old)),
                new: Some((1, new)),
                changed: true,
            } if old == "fn a() {}" && new == "fn a() { 1 }"
        )));
    }

    #[test]
    fn a_deleted_line_has_no_after_side() {
        let base = doc(&["one", "two", "three"]);
        let head = doc(&["one", "three"]);
        let rows = pair_rows(Some(&base), Some(&head), None);
        assert!(rows.iter().any(|row| matches!(
            row,
            Row::Pair {
                old: Some((2, text)),
                new: None,
                ..
            } if text == "two"
        )));
    }

    #[test]
    fn a_missing_side_yields_no_rows() {
        assert!(pair_rows(None, Some(&doc(&["a"])), None).is_empty());
    }

    #[test]
    fn line_numbers_count_from_one() {
        assert_eq!(number(0), 1);
        assert_eq!(number(9), 10);
    }

    fn numbered(rows: &[Row]) -> Vec<(Option<u32>, Option<u32>)> {
        rows.iter()
            .filter_map(|row| match row {
                Row::Pair { old, new, .. } => Some((
                    old.as_ref().map(|(line, _)| *line),
                    new.as_ref().map(|(line, _)| *line),
                )),
                Row::Fold { .. } => None,
            })
            .collect()
    }

    #[test]
    fn the_change_note_counts_the_lines_both_ways() {
        let base = doc(&["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]);
        let inserted = doc(&["a", "b", "c", "d", "k", "e", "f", "g", "h", "i", "j"]);
        let rows = pair_rows(Some(&base), Some(&inserted), None);
        assert_eq!(count_changes(&rows), (1, 0));

        let deleted = doc(&["a", "b", "c", "e", "f", "g", "h", "i", "j"]);
        let rows = pair_rows(Some(&base), Some(&deleted), None);
        assert_eq!(count_changes(&rows), (0, 1));
    }

    fn wide_doc(text: &str) -> Doc {
        Doc {
            source: Source::Worktree,
            path: RelPath::new("src/lib.rs"),
            lines: vec![text.to_owned(), "fn b() {}".to_owned()],
        }
    }

    #[test]
    fn the_pane_starts_on_the_symbol() {
        assert_eq!(Scope::default(), Scope::Symbol);
        assert_eq!(Scope::default().label(), "Symbol");
    }

    #[test]
    fn a_rewritten_line_counts_once_on_each_side() {
        let base = doc(&["    a();", "    b();", "    c();"]);
        let head = doc(&["    a();", "    B();", "    c();"]);
        let rows = pair_rows(Some(&base), Some(&head), None);
        assert_eq!(count_changes(&rows), (1, 1));
    }

    #[test]
    fn the_shown_indentation_is_removed_and_the_rest_kept() {
        let base = doc(&["        let a = 1;", "            let b = 2;", ""]);
        let head = doc(&["        let a = 3;", "            let b = 2;", ""]);
        let rows = pair_rows(Some(&base), Some(&head), None);
        let texts: Vec<(&str, &str)> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Pair {
                    old: Some((_, old)),
                    new: Some((_, new)),
                    ..
                } => Some((old.as_str(), new.as_str())),
                _ => None,
            })
            .collect();
        assert_eq!(texts[0], ("let a = 1;", "let a = 3;"));
        assert_eq!(texts[1], ("    let b = 2;", "    let b = 2;"));
    }

    #[test]
    fn both_sides_lose_the_same_indentation() {
        let base = doc(&["        let a = 1;"]);
        let head = doc(&["            let a = 3;"]);
        let rows = pair_rows(Some(&base), Some(&head), None);
        let texts: Vec<(&str, &str)> = rows
            .iter()
            .filter_map(|row| match row {
                Row::Pair {
                    old: Some((_, old)),
                    new: Some((_, new)),
                    ..
                } => Some((old.as_str(), new.as_str())),
                _ => None,
            })
            .collect();
        // Eight spaces is what the two lines share, so the deeper line keeps
        // four and the two sides still start at the same column.
        assert_eq!(texts[0], ("let a = 1;", "    let a = 3;"));
    }

    #[test]
    fn a_side_is_as_wide_as_its_longest_line() {
        let short = doc(&["fn a() {}", "fn b() {}"]);
        let rows = pair_rows(Some(&short), Some(&short), None);
        let narrow = widest_side(&rows, px(40.), FALLBACK_CHAR_WIDTH);

        let long = wide_doc(&"x".repeat(400));
        let rows = pair_rows(Some(&long), Some(&long), None);
        let wide = widest_side(&rows, px(40.), FALLBACK_CHAR_WIDTH);
        assert!(wide > narrow, "a line of 400 characters is wider");
        assert!(wide >= 400.0 * FALLBACK_CHAR_WIDTH);
    }

    #[test]
    fn hunk_starts_name_the_first_row_of_each_run() {
        let base = doc(&["a", "b", "c", "d", "e", "f", "g", "h", "i", "j"]);
        let head = doc(&["a", "B", "c", "d", "e", "f", "g", "h", "I", "j"]);
        let rows = pair_rows(Some(&base), Some(&head), None);
        let starts = hunk_starts(&rows);
        assert_eq!(starts.len(), 2);
        let changed: Vec<usize> = rows
            .iter()
            .enumerate()
            .filter(|(_, row)| row.is_changed())
            .map(|(index, _)| index)
            .collect();
        assert_eq!(starts, changed);
    }

    #[test]
    fn symbol_scope_keeps_the_symbol_and_folds_the_rest() {
        let mut lines: Vec<String> = (1..=60).map(|line| format!("line {line}")).collect();
        lines[29] = "changed".to_owned();
        let base = Doc {
            source: Source::Worktree,
            path: RelPath::new("src/lib.rs"),
            lines: lines.clone(),
        };
        let mut other = lines;
        other[29] = "changed twice".to_owned();
        let head = Doc {
            source: Source::Worktree,
            path: RelPath::new("src/lib.rs"),
            lines: other,
        };

        let rows = pair_rows(Some(&base), Some(&head), Some((28, 32)));

        let numbers = numbered(&rows);
        assert_eq!(numbers.len(), 5, "two lines either side and the change");
        assert!(matches!(rows.first(), Some(Row::Fold { .. })));
        assert!(matches!(rows.last(), Some(Row::Fold { .. })));
        assert!(rows.iter().any(|row| row.is_changed()));
        let hidden: usize = rows
            .iter()
            .filter_map(|row| match row {
                Row::Fold { count, .. } => Some(*count),
                Row::Pair { .. } => None,
            })
            .sum();
        assert_eq!(hidden, 60 - 5);
        assert!(rows.iter().any(Row::is_synthetic_fold));
        let counted: usize = rows
            .iter()
            .filter(|row| row.is_synthetic_fold())
            .map(|row| match row {
                Row::Fold { count, .. } => *count,
                Row::Pair { .. } => 1,
            })
            .sum();
        assert_eq!(counted, 60 - 5);
    }

    #[test]
    fn a_window_around_the_whole_file_keeps_every_row() {
        let base = doc(&["a", "b", "c"]);
        let head = doc(&["a", "B", "c"]);
        let whole = pair_rows(Some(&base), Some(&head), None);
        let windowed = pair_rows(Some(&base), Some(&head), Some((1, 3)));
        assert_eq!(windowed.len(), whole.len());
        assert!(windowed.iter().all(|row| !matches!(row, Row::Fold { .. })));
    }

    #[test]
    fn a_path_is_split_at_its_last_separator() {
        assert_eq!(
            split_path("src/de.rs"),
            ("src/".to_owned(), "de.rs".to_owned())
        );
        assert_eq!(split_path("de.rs"), (String::new(), "de.rs".to_owned()));
    }

    #[test]
    fn the_characters_that_changed_are_marked_on_their_side() {
        let before = "let a = 1;";
        let after = "let a = 2;";
        let spans = change_spans(before, after);
        assert_eq!(
            segments(before, &spans.before),
            vec![
                ("let a = ".to_owned(), false),
                ("1".to_owned(), true),
                (";".to_owned(), false),
            ]
        );
        assert_eq!(
            segments(after, &spans.after),
            vec![
                ("let a = ".to_owned(), false),
                ("2".to_owned(), true),
                (";".to_owned(), false),
            ]
        );
    }

    #[test]
    fn a_line_that_only_gained_characters_marks_them_on_the_after_side() {
        let before = "Some(b' ' | b'\\n' | b'\\t' | b'\\r') => {";
        let after = "Some(b' ' | b'\\n' | b'\\t' | b'\\r' | b'\\x0c') => {";
        let spans = change_spans(before, after);
        assert!(spans.before.is_empty(), "nothing was taken away");
        let marked: String = segments(after, &spans.after)
            .into_iter()
            .filter(|(_, changed)| *changed)
            .map(|(piece, _)| piece)
            .collect();
        assert!(
            marked.contains("x0c"),
            "the added byte is marked: {marked:?}"
        );
    }

    #[test]
    fn the_pieces_of_a_line_are_the_line() {
        let before = "    let a = 1;";
        let after = "    let a = 22;";
        let spans = change_spans(before, after);
        for (text, ranges) in [
            (before, spans.before.as_slice()),
            (after, spans.after.as_slice()),
        ] {
            let joined: String = segments(text, ranges)
                .into_iter()
                .map(|(piece, _)| piece)
                .collect();
            assert_eq!(joined, text);
        }
    }

    #[test]
    fn a_span_past_the_end_of_the_line_stops_at_it() {
        let past = std::slice::from_ref(&(0..9));
        assert_eq!(segments("ab", past), vec![("ab".to_owned(), true)]);
        assert!(segments("", &[]).is_empty());
        let second = std::slice::from_ref(&(1..2));
        assert_eq!(
            segments("é1", second),
            vec![("é".to_owned(), false), ("1".to_owned(), true)]
        );
    }

    #[test]
    fn a_line_without_a_change_has_no_spans() {
        let spans = change_spans("fn a() {}", "fn a() {}");
        assert_eq!(spans, Spans::default());
    }

    /// A line as it is drawn, its runs joined and the lost front marked.
    fn drawn(text: &str, spans: &[Range<usize>], skip: usize) -> String {
        line_runs(text, spans, skip)
            .into_iter()
            .map(|run| match run {
                Run::Ellipsis => ELLIPSIS.to_string(),
                Run::Text { text, .. } => text,
            })
            .collect()
    }

    /// How many characters of `text` the runs of it left out.
    fn left_out(text: &str, runs: &[Run]) -> usize {
        let shown: usize = runs
            .iter()
            .map(|run| match run {
                Run::Ellipsis => 0,
                Run::Text { text, .. } => text.chars().count(),
            })
            .sum();
        text.chars().count() - shown
    }

    #[test]
    fn a_pair_that_fits_its_column_is_shown_whole() {
        let before = "let a = 1;";
        let after = "let a = 2;";
        assert_eq!(pair_window(before, after, 40), 0);
        let exact = "x".repeat(40);
        assert_eq!(pair_window(&exact, &exact, 40), 0, "a full column fits");

        let spans = change_spans(before, after);
        assert_eq!(drawn(before, &spans.before, 0), before);
        assert!(
            !line_runs(before, &spans.before, 0)
                .iter()
                .any(|run| matches!(run, Run::Ellipsis)),
            "nothing is left out of a line that fits"
        );
    }

    #[test]
    fn a_pair_too_wide_is_shifted_to_its_first_difference() {
        let before = format!("{}1{}", "x".repeat(60), "y".repeat(30));
        let after = format!("{}2{}", "x".repeat(60), "y".repeat(30));
        let columns = 40;
        let skip = pair_window(&before, &after, columns);
        let first = first_difference(&before, &after);
        assert_eq!(first, 60);
        assert!(skip > 0, "a line that does not fit moves");
        assert!(skip <= first, "the difference itself is not left out");
        assert!(
            1 + first - skip >= DIFFERENCE_MARGIN,
            "the difference keeps {DIFFERENCE_MARGIN} characters of room"
        );
        let longest = before.chars().count().max(after.chars().count());
        assert!(
            1 + longest - skip <= columns,
            "the window fits the column exactly"
        );
        let shown = drawn(&before, &[], skip);
        assert_eq!(shown.chars().count(), columns);
        assert!(shown.starts_with(ELLIPSIS));
        assert!(
            shown.chars().filter(|ch| *ch == '1').count() == 1,
            "the difference is in view"
        );
    }

    #[test]
    fn a_change_at_the_start_of_a_long_line_stays_in_view() {
        let before = format!("1{}", "x".repeat(100));
        let after = format!("2{}", "x".repeat(100));
        let columns = 40;
        let skip = pair_window(&before, &after, columns);
        assert_eq!(skip, 0, "nothing is dropped before a change at the start");
        assert!(first_difference(&before, &after) < columns);

        // A column narrower than the margin still yields a sane window.
        let skip = pair_window(&before, &after, 0);
        assert_eq!(skip, 0);
        let longer = format!("{}y", "x".repeat(499));
        assert!(pair_window(&"x".repeat(500), &longer, 0) > 0);
    }

    #[test]
    fn both_sides_of_a_pair_leave_out_the_same_characters() {
        let before = "    Some(b' ' | b'\\n' | b'\\t' | b'\\r') => {";
        let after = "    Some(b' ' | b'\\n' | b'\\t' | b'\\r' | b'\\x0c') => {";
        let columns = 43;
        let skip = pair_window(before, after, columns);
        assert!(skip > 0, "the pair is wider than its half");

        let spans = change_spans(before, after);
        let before_runs = line_runs(before, &spans.before, skip);
        let after_runs = line_runs(after, &spans.after, skip);
        assert_eq!(before_runs.first(), Some(&Run::Ellipsis));
        assert_eq!(after_runs.first(), Some(&Run::Ellipsis));
        assert_eq!(left_out(before, &before_runs), skip);
        assert_eq!(left_out(after, &after_runs), skip);

        // The characters that changed are inside the window, and marked.
        let marked: String = after_runs
            .iter()
            .filter_map(|run| match run {
                Run::Text {
                    text,
                    changed: true,
                } => Some(text.as_str()),
                _ => None,
            })
            .collect();
        assert!(
            marked.contains("x0c"),
            "the added bytes are in view: {marked:?}"
        );
        assert_eq!(drawn(after, &spans.after, skip), {
            let mut whole = String::from(ELLIPSIS);
            whole.extend(after.chars().skip(skip));
            whole
        });
    }

    #[test]
    fn a_shift_never_splits_a_character() {
        let before = format!("{}a{}", "→".repeat(60), "é".repeat(10));
        let after = format!("{}b{}", "→".repeat(60), "é".repeat(10));
        let columns = 30;
        let skip = pair_window(&before, &after, columns);
        assert!(skip > 0);
        let mut expected = String::from(ELLIPSIS);
        expected.extend(before.chars().skip(skip));
        assert_eq!(drawn(&before, &[], skip), expected);
        assert!(drawn(&before, &[], skip).chars().count() <= columns);
    }

    #[test]
    fn the_first_difference_is_counted_in_characters() {
        assert_eq!(first_difference("abc", "abd"), 2);
        assert_eq!(first_difference("ab", "abcd"), 2);
        assert_eq!(first_difference("", "abcd"), 0);
        assert_eq!(first_difference("é1", "é2"), 1);
        assert_eq!(first_difference("→", "→→"), 1);
    }
}
