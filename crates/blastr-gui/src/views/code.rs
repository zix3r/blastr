//! The source view: one file, editable, with line numbers and the lines the
//! selection cares about tinted behind the text, each line from the first
//! column to the last.
//!
//! Both places the source appears — the pane under the graph and the Code tab —
//! draw the same editor and keep their text, so an edit made in one is the edit
//! the other shows and the one that is saved. Nothing is folded: a hidden line
//! is a line the reader cannot check. Ctrl+S writes the file back with the line
//! breaks it was read with, and only ever inside the opened repository.

mod buffer;
mod syntax;

use std::collections::HashMap;
use std::io;
use std::ops::Range;
use std::path::PathBuf;
use std::time::Instant;

use gpui_kit::base::input::{
    Editor as CodeEditor, EditorState, GutterDecoration, GutterDecorationCollection,
    InputEditorStyle, InputEvent, Rope, RopeExt,
};
use gpui_kit::component::{Icon, IconName, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, App, AsyncApp, Context, Edges, Entity, Hsla, Subscription, Task, WeakEntity,
    Window, div, point, px,
};

use blastr_core::wording;
use blastr_model::{Location, RelPath, Source};

use super::shell::{Shell, Tab};
use super::ui;
use crate::state::selection::Selection;
use crate::theme::{self, TEXT_LINE_NUMBER, TEXT_MONO, Tone};

use buffer::{Buffer, Stamp};

// Saving the file the source view shows.
gpui_kit::actions!([SaveFile]);

/// How much of a class colour tints the lines the selected symbol covers.
const CLASS_TINT_ALPHA: f32 = 0.08;

/// How much of a class colour tints the line the evidence sits on.
const REFERENCE_TINT_ALPHA: f32 = 0.14;

/// The space the editor keeps around its text, in pixels.
const PADDING: f32 = 6.0;

/// The width of the bar the gutter shows beside a tinted line, in pixels.
const BAR_WIDTH: f32 = 2.0;

/// How many frames may ask to put a selection in place before the view is
/// left where the reader had it: an editor that never learns its height would
/// otherwise ask for frame after frame.
const PLACE_TRIES: u8 = 3;

/// `colour` at `alpha`: a tint quiet enough to leave the code readable.
fn tint(colour: Tone, alpha: f32) -> Hsla {
    let mut colour = colour.hsla();
    colour.a = alpha;
    colour
}

/// The source and path a file is shown from, which is what an editor is kept
/// for: the same path can be read from two revisions.
type Key = (Source, RelPath);

/// The lines of the file the selection wants tinted.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
struct Tints {
    /// The lines of the change the selection names.
    changed: Vec<(u32, u32)>,
    /// The lines the selected symbol covers.
    own: Option<(u32, u32)>,
    /// The line the evidence that makes the selected item affected sits on.
    reference: Option<u32>,
}

impl Tints {
    /// The ranges of lines to tint, weakest first, with the colour of each.
    ///
    /// Painting in this order leaves the reference on top of the symbol, which
    /// leaves it on top of the change.
    fn layers(&self, accent: Tone) -> Vec<((u32, u32), Hsla)> {
        let mut layers: Vec<((u32, u32), Hsla)> = self
            .changed
            .iter()
            .map(|range| (*range, theme::CHANGED_TINT.hsla()))
            .collect();
        if let Some(own) = self.own {
            layers.push((own, tint(accent, CLASS_TINT_ALPHA)));
        }
        if let Some(reference) = self.reference {
            layers.push(((reference, reference), tint(accent, REFERENCE_TINT_ALPHA)));
        }
        layers
    }

    /// The lines the gutter marks with a bar: the symbol's range and the line
    /// the evidence sits on.
    fn bar_lines(&self) -> Vec<(u32, u32)> {
        let mut lines: Vec<(u32, u32)> = self.own.into_iter().collect();
        if let Some(reference) = self.reference {
            lines.push((reference, reference));
        }
        lines
    }
}

/// The lines the view puts in place for the selection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Focus {
    /// The first line of the range the selection covers, counting from one.
    first: u32,
    /// The last line of that range, counting from one.
    last: u32,
    /// The line to put about a third of the way down the visible area.
    anchor: u32,
}

/// The lines a location puts in place for the selection made at it.
///
/// `site` is the reference of the evidence step the selection was made from,
/// when it was made by clicking one: that is the line the reader followed, so
/// that is the line the view puts in place. Every other selection shows the
/// first line of its range, which is where the symbol starts.
fn selection_focus(site: Option<&Location>, location: &Location) -> Focus {
    let first = location.span.start_line.max(1);
    let last = location.span.end_line.max(first);
    let anchor = site
        .filter(|site| site.file == location.file)
        .map_or(first, |site| site.span.start_line.max(1));
    Focus {
        first,
        last,
        anchor,
    }
}

/// The line to put at the top of the view, counting from zero, which is where
/// the view scrolls to.
///
/// `viewport` is how many lines fit in the visible part of the view and `rows`
/// how many lines the file has. The anchor goes about a third of the way down,
/// which leaves the code above it in sight and the rest below. A range that
/// fits in the view is shown whole instead: the last line holds the view back
/// far enough to be on screen, and the first line stops it from scrolling past
/// the range.
fn top_row(focus: Focus, viewport: u32, rows: u32) -> u32 {
    if viewport == 0 || rows == 0 {
        return 0;
    }
    let end_row = rows - 1;
    let first = focus.first.saturating_sub(1).min(end_row);
    let last = focus.last.saturating_sub(1).clamp(first, end_row);
    let anchor = focus.anchor.saturating_sub(1).min(end_row);
    let wanted = anchor.saturating_sub(viewport / 3);
    let wanted = if last - first < viewport {
        wanted.max((last + 1).saturating_sub(viewport)).min(first)
    } else {
        wanted
    };
    // The first and the last line can reach the place the anchor asks for,
    // and no further.
    wanted.min(rows.saturating_sub(viewport))
}

/// The row of a line, counting from one, kept inside a text of `rows` rows.
fn row_of(rows: usize, line: u32) -> usize {
    usize::try_from(line.saturating_sub(1))
        .unwrap_or(usize::MAX)
        .min(rows.saturating_sub(1))
}

/// The byte range of the lines `first` to `last`, counting from one.
fn line_range(text: &Rope, first: u32, last: u32) -> Range<usize> {
    let rows = text.lines_len();
    let start = text.line_start_offset(row_of(rows, first));
    let end = text.line_end_offset(row_of(rows, last)).max(start);
    start..end
}

/// The byte range the lines `first` to `last` cover, with the line break that
/// ends the last one.
///
/// A gutter mark covers every line of its range, and a range that stopped at
/// the end of the last line would not reach an empty one; the mark would turn
/// into a boundary between two lines instead.
fn line_span(text: &Rope, first: u32, last: u32) -> Range<usize> {
    let rows = text.lines_len();
    let start = text.line_start_offset(row_of(rows, first));
    let end = text.line_start_offset(row_of(rows, last) + 1).max(start);
    start..end
}

/// The bands to paint behind the text: the top of each, its height, and its
/// colour, weakest first.
///
/// A band is a whole line, from the left edge of the text to the right edge of
/// the editor, so it covers the empty lines inside a range as well. The bands
/// are clipped to the rows the viewport shows, which keeps a range the size of
/// the file one quad.
///
/// `viewport` is the height of the part of the editor the text scrolls in and
/// `scroll` its distance from the top of that part.
fn bands(
    layers: &[((u32, u32), Hsla)],
    line_height: f32,
    padding: f32,
    scroll: f32,
    viewport: f32,
    rows: u32,
) -> Vec<(f32, f32, Hsla)> {
    if line_height <= 0.0 || viewport <= 0.0 || rows == 0 {
        return Vec::new();
    }
    // The rows the viewport shows, as a half-open range of display rows: the
    // first whose bottom edge is still on screen to the last whose top edge is.
    let shown_first = ((-scroll) / line_height).floor().max(0.0) as u32;
    let shown_last = (((viewport - scroll) / line_height).ceil().max(0.0) as u32).min(rows);
    let mut bands = Vec::new();
    for ((first, last), colour) in layers {
        let from = first.saturating_sub(1).max(shown_first);
        let to = (*last).min(shown_last);
        if from >= to {
            continue;
        }
        bands.push((
            padding + scroll + from as f32 * line_height,
            (to - from) as f32 * line_height,
            *colour,
        ));
    }
    bands
}

/// Where the view scrolls to for the selection, in pixels.
///
/// The viewport is measured in pixels and the line in it in lines; the offset
/// is zero or negative, as the editor scrolls by moving its content up.
fn scroll_top(focus: Focus, line_height: f32, viewport: f32, rows: u32) -> f32 {
    if line_height <= 0.0 || viewport <= 0.0 || rows == 0 {
        return 0.0;
    }
    let lines = (viewport / line_height).floor().max(1.0) as u32;
    -(top_row(focus, lines, rows) as f32 * line_height)
}

/// How tall a line and the visible part of the editor are, and how many lines
/// the file has, once the editor has been laid out.
fn layout_metrics(state: &EditorState) -> Option<(f32, f32, u32)> {
    let line_height = state.line_height()?.as_f32();
    let viewport = state.text_bounds()?.size.height.as_f32();
    let rows = u32::try_from(state.text().lines_len()).unwrap_or(u32::MAX);
    (line_height > 0.0 && viewport > 0.0).then_some((line_height, viewport, rows))
}

/// The two places the source is drawn: the pane under the graph and the tab.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Pane {
    /// The short pane below the graph.
    Peek,
    /// The whole Code tab.
    Full,
}

impl Pane {
    /// The place this pane keeps what it has scrolled to.
    fn slot(self) -> usize {
        match self {
            Pane::Peek => 0,
            Pane::Full => 1,
        }
    }
}

/// What one pane has done about scrolling to the selection.
///
/// Both panes draw the same editor, which has one scroll offset, but they show
/// different amounts of it. Each places the selection once, in the height it
/// has, and leaves the reader's own scrolling alone afterwards.
#[derive(Debug, Clone, Default, PartialEq)]
struct Placement {
    /// The selection and the file this pane has already scrolled to.
    placed: Option<(Selection, Key)>,
    /// How many frames have asked for the scroll, so that an editor that never
    /// learns its height does not keep asking for frames.
    tries: u8,
}

/// A scroll a pane has asked for, kept until the frame that drew it has laid
/// the editor out: only then does the editor know how tall that pane's view is.
#[derive(Debug, Clone)]
struct Awaiting {
    /// The pane that asked.
    pane: Pane,
    /// The selection to put in place.
    selection: Selection,
    /// The file it is shown from.
    key: Key,
    /// The lines to put in place.
    focus: Focus,
}

/// One file open in the source view.
struct Open {
    /// The editor both panes draw.
    editor: Entity<EditorState>,
    /// The bar beside the lines the selection cares about, as a gutter
    /// decoration that follows edits.
    marks: GutterDecorationCollection,
    /// The lines the mark currently shows, so that a repaint of the same
    /// selection does not set it again.
    tinted: Tints,
    /// The text on disk and whether the editor has moved away from it. Absent
    /// for a revision that has no file to write.
    buffer: Option<Buffer>,
    /// The version of the file on disk when it was read or last written.
    stamp: Option<Stamp>,
    /// Whether the text may be written back at all.
    writable: bool,
    /// Keeps the change notification alive.
    _changes: Subscription,
}

/// A file that has been read, on its way into an editor.
struct Opened {
    /// Which file it is.
    key: Key,
    /// The editor, already holding the text.
    editor: Entity<EditorState>,
    /// The text as it is on disk.
    text: String,
    /// The version of the file that was read.
    stamp: Option<Stamp>,
    /// Whether the file may be written back.
    writable: bool,
}

/// The state of the source pane.
#[derive(Default)]
pub(crate) struct CodeState {
    /// The repository the open files belong to.
    root: Option<PathBuf>,
    /// The editors, by the revision and path they show.
    open: HashMap<Key, Open>,
    /// Scroll to the selected symbol in both panes before the next paint.
    pub(crate) pending: bool,
    /// What each pane has done about scrolling to the selection.
    placements: [Placement; 2],
    /// The pane whose view height the current scroll offset was computed in.
    /// A pane that is shown after the other one puts the selection in place
    /// again, at the height of its own view.
    scrolled_in: Option<Pane>,
    /// The scroll top a pane asked for, for the bands of the frame that moves.
    requested: Option<(Pane, f32)>,
    /// The placement waiting for the frame that drew its pane to be laid out.
    awaiting: Option<Awaiting>,
    /// The file being read, so another selection cancels the read it replaces.
    reading: Option<(Key, Task<()>)>,
    /// The file being checked for a change on disk.
    refresh: Option<(Key, Task<()>)>,
    /// A file that could not be read.
    failed: Option<Key>,
}

impl CodeState {
    /// Forgets the scroll request, as after another selection.
    ///
    /// The open files and their unsaved edits are kept: nothing a comparison
    /// does should cost the user an edit.
    pub(crate) fn reset(&mut self) {
        self.pending = true;
    }

    /// Takes the request for both panes to place the selection again.
    ///
    /// The panes are told by forgetting what they scrolled to: each places the
    /// selection the next time it is drawn, at the height it has then.
    fn take_request(&mut self) {
        if std::mem::take(&mut self.pending) {
            for placement in &mut self.placements {
                placement.placed = None;
            }
        }
    }

    /// Whether the pane has already scrolled to the selection in this file.
    fn placed(&self, pane: Pane, selection: &Selection, key: &Key) -> bool {
        matches!(
            &self.placements[pane.slot()].placed,
            Some((placed, file)) if placed == selection && file == key
        )
    }

    /// Whether the pane has yet to put the selection in place: either it has
    /// not scrolled to this selection, or the view was last moved for the
    /// height of the other pane.
    fn wants_place(&self, pane: Pane, selection: &Selection, key: &Key) -> bool {
        self.scrolled_in != Some(pane) || !self.placed(pane, selection, key)
    }

    /// Whether any open file has unsaved edits.
    ///
    /// The tab strip draws a dot for it; the header already says which file is
    /// modified.
    pub(crate) fn any_dirty(&self) -> bool {
        self.open
            .values()
            .any(|open| open.buffer.as_ref().is_some_and(Buffer::dirty))
    }

    /// Whether the file the key names has unsaved edits.
    fn dirty(&self, key: &Key) -> bool {
        self.open
            .get(key)
            .and_then(|open| open.buffer.as_ref())
            .is_some_and(Buffer::dirty)
    }

    /// Drops the open files when another repository is shown.
    fn claim_root(&mut self, root: Option<PathBuf>) {
        if self.root != root {
            self.root = root;
            self.open.clear();
            self.reading = None;
            self.refresh = None;
            self.failed = None;
            self.placements = [Placement::default(), Placement::default()];
            self.scrolled_in = None;
            self.requested = None;
            self.awaiting = None;
        }
    }

    /// Keeps the editors of the files met lately, and every unsaved edit.
    ///
    /// Browsing a large project visits hundreds of files; a clean one costs
    /// nothing to read again, so only the ones with unsaved edits are certain
    /// to stay. The file being shown is never one of them.
    fn forget_unused(&mut self, shown: &Key) {
        const KEPT: usize = 12;
        while self.open.len() > KEPT {
            let unused = self
                .open
                .iter()
                .find(|(key, open)| {
                    key != &shown && !open.buffer.as_ref().is_some_and(Buffer::dirty)
                })
                .map(|(key, _)| key.clone());
            let Some(unused) = unused else {
                return;
            };
            self.open.remove(&unused);
        }
    }
}

impl Shell {
    /// The full source view behind the Code tab.
    pub(crate) fn code_full(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.code_body("code-full", false, cx)
    }

    /// The short source view below the graph.
    pub(crate) fn code_peek(&mut self, cx: &mut Context<Self>) -> AnyElement {
        self.code_body("code-peek", true, cx)
    }

    /// The source view, with a header naming the file and line.
    fn code_body(&mut self, id: &'static str, peek: bool, cx: &mut Context<Self>) -> AnyElement {
        let root = self.repo_root();
        self.code.claim_root(root);

        let Some(selection) = self.session.selection().cloned() else {
            return self.code_nothing(peek, cx);
        };
        let Some((source, location)) = self.selection_location(&selection) else {
            return self.code_nothing(peek, cx);
        };

        let key: Key = (source, location.file.clone());
        let dirty = self.code.dirty(&key);
        let head = self.code_head(Some(&location), peek, dirty, cx);
        let column = v_flex()
            .size_full()
            .min_h_0()
            .bg(theme::BG_CANVAS)
            .when(peek, |this| this.border_t_1().border_color(theme::LINE))
            .child(head);

        if !self.code.open.contains_key(&key) {
            self.spawn_read(key.clone(), cx);
            let (title, body, accent) = if self.code.failed.as_ref() == Some(&key) {
                (
                    "The file could not be read",
                    "It may have been removed or renamed by the comparison.",
                    theme::EDIT,
                )
            } else {
                (
                    "Reading the file",
                    "The source appears as soon as it has been read.",
                    theme::BEHAVE,
                )
            };
            return column
                .child(div().flex_1().min_h_0().child(ui::empty_state(
                    IconName::FileText,
                    Some(accent),
                    title,
                    body,
                )))
                .into_any_element();
        }

        let open = self
            .code
            .open
            .get(&key)
            .expect("the editor was just looked for");
        let editor = open.editor.clone();
        let accent = self
            .selected_item()
            .map(|item| ui::class_accent(item.class))
            .unwrap_or(theme::CHANGED);
        let tints = self.code_tints(&location);
        let focus = selection_focus(selection.site.as_ref(), &location);
        let pane = if peek { Pane::Peek } else { Pane::Full };

        // The marks and the scroll belong to a selection, not to a frame: the
        // marks follow it as it changes, and each pane puts it in place once,
        // at the height of its own view.
        self.apply_tints(&key, &tints, accent.solid, cx);
        self.code.take_request();
        if self.code.wants_place(pane, &selection, &key) {
            if !self.code.dirty(&key) {
                self.spawn_refresh(key.clone(), cx);
            }
            self.schedule_place(pane, &selection, &key, focus, cx);
        }
        let moved_to = match self.code.requested {
            Some((asked, top)) if asked == pane => {
                self.code.requested = None;
                Some(top)
            }
            _ => None,
        };
        let bands = self.code_bands(&editor, &tints, accent.solid, moved_to, cx);

        let body = div()
            .id(id)
            .relative()
            .flex_1()
            .min_h_0()
            .overflow_hidden()
            .font_family(theme::mono())
            .text_size(TEXT_MONO)
            .line_height(theme::LINE_HEIGHT)
            .on_action(cx.listener(|this, _: &SaveFile, _, cx| this.save_code(cx)))
            // The bands sit behind the text: the editor draws no background of
            // its own, and the gutter covers its own columns, so a band shows
            // from the first column of the text to the last of the editor.
            .children(bands.into_iter().map(|(top, height, colour)| {
                div()
                    .absolute()
                    .left_0()
                    .right_0()
                    .top(px(top))
                    .h(px(height))
                    .bg(colour)
            }))
            .child(CodeEditor::new(&editor));

        // The layout the editor publishes now is the one this pane gave it,
        // which is where the placement deferred to the end of the frame reads
        // the height of the view from.
        column.child(body).into_any_element()
    }

    /// The pane when the selection names no file: a header and why it is empty.
    fn code_nothing(&mut self, peek: bool, cx: &mut Context<Self>) -> AnyElement {
        if self.session.shown().failure().is_some() {
            if !peek {
                return self.failure_card(cx);
            }
            return v_flex()
                .size_full()
                .min_h_0()
                .bg(theme::BG_CANVAS)
                .border_t_1()
                .border_color(theme::LINE)
                .child(self.code_head(None, true, false, cx))
                .child(div().flex_1().min_h_0().p_3().child(ui::failure_line()))
                .into_any_element();
        }
        let (title, body) = if self.session.shown().is_loading() {
            (
                "Waiting for the result",
                "The source appears once the comparison is done.",
            )
        } else if self.session.stopped == crate::state::session::Stopped::Cancelled {
            (
                "The comparison was cancelled",
                "Choose what to compare to start again.",
            )
        } else {
            (
                "No file to show",
                "Select a change, an affected symbol or a test.",
            )
        };
        let mut pane = v_flex().size_full().min_h_0().bg(theme::BG_CANVAS);
        if peek {
            pane = pane
                .border_t_1()
                .border_color(theme::LINE)
                .child(self.code_head(None, true, false, cx));
        }
        pane.child(div().flex_1().min_h_0().child(ui::empty_state(
            IconName::FileText,
            Some(theme::CHANGED),
            title,
            body,
        )))
        .into_any_element()
    }

    /// The lines the current selection wants tinted.
    fn code_tints(&self, location: &Location) -> Tints {
        let own = (location.span.start_line, location.span.end_line);
        Tints {
            changed: self.change_lines(),
            own: (own.0 <= own.1).then_some(own),
            reference: self.reference_line(&location.file),
        }
    }

    /// Marks the lines of the selection in the gutter.
    fn apply_tints(&mut self, key: &Key, tints: &Tints, accent: Tone, cx: &mut Context<Self>) {
        let Some(open) = self.code.open.get(key) else {
            return;
        };
        if open.tinted == *tints {
            return;
        }
        let editor = open.editor.clone();
        let state = editor.read(cx);
        let text = state.text();
        let mark: Vec<GutterDecoration> = tints
            .bar_lines()
            .into_iter()
            .map(|(first, last)| {
                GutterDecoration::new(line_span(text, first, last)).with_color(accent.hsla())
            })
            .collect();

        let open = self
            .code
            .open
            .get_mut(key)
            .expect("the editor was just looked for");
        open.marks.set(mark, cx);
        open.tinted = tints.clone();
    }

    /// Asks for the selection to be put in place once the editor has been laid
    /// out in this pane, which is where it learns the height of the view.
    ///
    /// A frame only builds the elements: the editor is laid out after it, and
    /// its layout notification is not a frame this pane can act on. So the
    /// scroll is deferred to the end of the frame, after that layout, and the
    /// frame it asks for is the one that moves the view.
    fn schedule_place(
        &mut self,
        pane: Pane,
        selection: &Selection,
        key: &Key,
        focus: Focus,
        cx: &mut Context<Self>,
    ) {
        let placement = &mut self.code.placements[pane.slot()];
        if placement.tries >= PLACE_TRIES {
            // An editor that never learnt its height is not worth another
            // frame: the selection stays where the reader left the view.
            placement.placed = Some((selection.clone(), key.clone()));
            return;
        }
        placement.tries = placement.tries.saturating_add(1);
        self.code.awaiting = Some(Awaiting {
            pane,
            selection: selection.clone(),
            key: key.clone(),
            focus,
        });
        let shell = cx.weak_entity();
        cx.defer(move |cx| {
            shell.update(cx, |shell, cx| shell.finish_place(cx)).ok();
        });
    }

    /// Puts the selection in place, after the frame that drew the pane laid the
    /// editor out at the height of that pane.
    fn finish_place(&mut self, cx: &mut Context<Self>) {
        let Some(awaiting) = self.code.awaiting.take() else {
            return;
        };
        let Awaiting {
            pane,
            selection,
            key,
            focus,
        } = awaiting;
        // Only the file the selection still names: a result that arrived in
        // between has its own scroll to place.
        let current = self
            .session
            .selection()
            .cloned()
            .and_then(|selection| self.selection_location(&selection));
        let Some((source, location)) = current else {
            return;
        };
        if (source, location.file.clone()) != key {
            return;
        }
        let Some(open) = self.code.open.get(&key) else {
            return;
        };
        let editor = open.editor.clone();
        let Some((line_height, viewport, rows)) = layout_metrics(editor.read(cx)) else {
            // The editor has no view to place anything in yet; the next draw of
            // the pane asks again.
            return;
        };

        let top = scroll_top(focus, line_height, viewport, rows);
        editor.update(cx, |state, cx| {
            let offset = line_range(state.text(), focus.anchor, focus.anchor).start;
            state.set_selected_range(offset..offset, cx);
            // Kept where the reader left it, so that the code does not jump
            // sideways as well when another symbol is selected.
            let left = state.scroll_offset().x;
            state.set_scroll_offset(point(left, px(top)), cx);
        });
        let placement = &mut self.code.placements[pane.slot()];
        placement.placed = Some((selection, key));
        placement.tries = 0;
        self.code.scrolled_in = Some(pane);
        self.code.requested = Some((pane, top));
        // The bands are drawn by this view, which has to be drawn again for
        // them to follow the text to where the editor is about to move.
        cx.notify();
    }

    /// The bands behind the text that the selection tints.
    ///
    /// `moved_to` is the scroll top a placement asked for at the end of the
    /// last frame, which the editor only applies in its layout: using it here
    /// keeps the bands under the text on the frame the view moves, rather than
    /// a frame behind it.
    fn code_bands(
        &self,
        editor: &Entity<EditorState>,
        tints: &Tints,
        accent: Tone,
        moved_to: Option<f32>,
        cx: &App,
    ) -> Vec<(f32, f32, Hsla)> {
        let state = editor.read(cx);
        let Some((line_height, viewport, rows)) = layout_metrics(state) else {
            return Vec::new();
        };
        let scroll = moved_to.unwrap_or_else(|| state.scroll_offset().y.as_f32());
        bands(
            &tints.layers(accent),
            line_height,
            PADDING,
            scroll,
            viewport,
            rows,
        )
    }

    /// Starts reading the file the key names, unless that read is under way or
    /// the file has already turned out to be unreadable.
    fn spawn_read(&mut self, key: Key, cx: &mut Context<Self>) {
        if self
            .code
            .reading
            .as_ref()
            .is_some_and(|(read, _)| *read == key)
        {
            return;
        }
        if self.code.failed.as_ref() == Some(&key) {
            return;
        }
        let Some(root) = self.repo_root() else {
            return;
        };
        let (source, file) = key.clone();
        let opened_key = key.clone();
        let read = match source {
            Source::Worktree => {
                let path = root.join(file.as_str());
                cx.background_spawn(async move { buffer::load(&path) })
            }
            // The other revisions have no file to write: their text comes from
            // the comparison, and the editor shows it without offering an edit.
            _ => Task::ready(
                self.docs
                    .get(&source, &file)
                    .map(|doc| (doc.lines.join("\n"), None)),
            ),
        };
        let opened = cx.spawn(async move |shell: WeakEntity<Shell>, cx: &mut AsyncApp| {
            let Some((text, stamp)) = read.await else {
                shell
                    .update(cx, |shell, cx| shell.code_read_failed(&opened_key, cx))
                    .ok();
                return;
            };
            let writable = source == Source::Worktree;
            let Some(editor) = open_editor(cx, &file, &text, writable) else {
                // No window was free to measure the text in; the next paint asks
                // for the file again rather than wait forever.
                shell.update(cx, |shell, _| shell.code.reading = None).ok();
                return;
            };
            shell
                .update(cx, |shell, cx| {
                    shell.code_opened(
                        Opened {
                            key: opened_key,
                            editor,
                            text,
                            stamp,
                            writable,
                        },
                        cx,
                    );
                })
                .ok();
        });
        self.code.reading = Some((key, opened));
    }

    /// Takes a file that has been read into an editor, and watches it.
    fn code_opened(&mut self, opened: Opened, cx: &mut Context<Self>) {
        let Opened {
            key,
            editor,
            text,
            stamp,
            writable,
        } = opened;
        let marks = editor.update(cx, |state, cx| {
            state.create_gutter_decorations_collection(Vec::new(), cx)
        });
        let edited = key.clone();
        let changes = cx.subscribe(
            &editor,
            move |shell: &mut Shell, editor, event: &InputEvent, cx| {
                if matches!(event, InputEvent::Change) {
                    shell.code_edited(&edited, &editor, cx);
                }
            },
        );
        // The editor learns how tall a line and its visible part are only when
        // it is laid out, which happens after the frame that draws it. That is
        // where the placement deferred to the end of the frame reads them.
        self.code.reading = None;
        self.code.failed = None;
        self.code.open.insert(
            key.clone(),
            Open {
                editor,
                marks,
                tinted: Tints::default(),
                buffer: writable.then(|| Buffer::new(&text)),
                stamp,
                writable,
                _changes: changes,
            },
        );
        self.code.forget_unused(&key);
        self.code.pending = true;
        cx.notify();
    }

    /// Says that the file could not be read.
    fn code_read_failed(&mut self, key: &Key, cx: &mut Context<Self>) {
        self.code.reading = None;
        self.code.failed = Some(key.clone());
        cx.notify();
    }

    /// Notes the editor's text after the user changed it.
    fn code_edited(&mut self, key: &Key, editor: &Entity<EditorState>, cx: &mut Context<Self>) {
        let text = editor.read(cx).text().to_string();
        let Some(open) = self.code.open.get_mut(key) else {
            return;
        };
        let Some(buffer) = open.buffer.as_mut() else {
            return;
        };
        let was = buffer.dirty();
        let is = buffer.edited(&text);
        // Only a change of the flag is worth a repaint of the window: the
        // editor has already repainted itself.
        if was != is {
            cx.notify();
        }
    }

    /// Reads the file again when it changed on disk while it was not edited.
    fn spawn_refresh(&mut self, key: Key, cx: &mut Context<Self>) {
        if self
            .code
            .refresh
            .as_ref()
            .is_some_and(|(seen, _)| *seen == key)
        {
            return;
        }
        let Some(root) = self.repo_root() else {
            return;
        };
        let (source, file) = key.clone();
        if source != Source::Worktree {
            return;
        }
        let Some(open) = self.code.open.get(&key) else {
            return;
        };
        let Some(buffer) = open.buffer.as_ref() else {
            return;
        };
        let known = (open.stamp, buffer.baseline().to_owned());
        let editor = open.editor.clone();
        let path = root.join(file.as_str());
        let checked_key = key.clone();
        let read = cx.background_spawn(async move {
            let stamp = buffer::stamp(&path);
            if stamp.is_some() && stamp == known.0 {
                return (stamp, None);
            }
            buffer::load(&path).map_or((None, None), |(text, stamp)| (stamp, Some(text)))
        });
        let checked = cx.spawn(async move |shell: WeakEntity<Shell>, cx: &mut AsyncApp| {
            let (stamp, text) = read.await;
            let Some(text) = text else {
                shell
                    .update(cx, |shell, _| {
                        shell.code.refresh = None;
                        if let Some(open) = shell.code.open.get_mut(&checked_key) {
                            open.stamp = stamp;
                        }
                    })
                    .ok();
                return;
            };
            if text != known.1 && !replace_text(cx, &editor, &text) {
                // The window was busy and did not take the text: leave the
                // buffer as it is and check again on the next selection.
                shell.update(cx, |shell, _| shell.code.refresh = None).ok();
                return;
            }
            shell
                .update(cx, |shell, cx| {
                    shell.code_reloaded(checked_key, text, stamp, cx)
                })
                .ok();
        });
        self.code.refresh = Some((key, checked));
    }

    /// Takes the text of a file that changed on disk while it was not edited.
    fn code_reloaded(
        &mut self,
        key: Key,
        text: String,
        stamp: Option<Stamp>,
        cx: &mut Context<Self>,
    ) {
        self.code.refresh = None;
        if let Some(open) = self.code.open.get_mut(&key) {
            open.stamp = stamp;
            if let Some(buffer) = open.buffer.as_mut() {
                buffer.reloaded(&text);
            }
            // The decorations were placed in the text that has just been
            // replaced; the next paint places them again.
            open.tinted = Tints::default();
        }
        self.code.pending = true;
        cx.notify();
    }

    /// Saves the file the source view shows, keeping its line breaks.
    fn save_code(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.repo_root() else {
            return;
        };
        let selection = self.session.selection().cloned();
        let located = selection
            .as_ref()
            .and_then(|selection| self.selection_location(selection));
        let Some((source, location)) = located else {
            return;
        };
        let key: Key = (source, location.file.clone());
        let Some(open) = self.code.open.get(&key) else {
            return;
        };
        if !open.writable {
            self.note("This revision has no file to save", cx);
            return;
        }
        let Some(buffer) = open.buffer.as_ref() else {
            return;
        };
        if !buffer.dirty() {
            self.note("No changes to save", cx);
            return;
        }
        let text = open.editor.read(cx).text().to_string();
        let endings = buffer.endings();
        let file = location.file.clone();
        let write_text = text.clone();
        let write =
            cx.background_spawn(async move { buffer::save(&root, &file, &write_text, endings) });
        cx.spawn(async move |shell: WeakEntity<Shell>, cx: &mut AsyncApp| {
            let result = write.await;
            shell
                .update(cx, |shell, cx| shell.code_saved(key, text, result, cx))
                .ok();
        })
        .detach();
    }

    /// Takes the outcome of a save.
    fn code_saved(
        &mut self,
        key: Key,
        text: String,
        result: io::Result<(PathBuf, Option<Stamp>)>,
        cx: &mut Context<Self>,
    ) {
        match result {
            Ok((_, stamp)) => {
                if let Some(open) = self.code.open.get_mut(&key) {
                    open.stamp = stamp;
                    // What was written is what the file holds; anything typed
                    // while the write was under way is still a change.
                    let now = open.editor.read(cx).text().to_string();
                    if let Some(buffer) = open.buffer.as_mut() {
                        buffer.saved(&text);
                        buffer.edited(&now);
                    }
                }
                // The before/after view reads the file again rather than show
                // what was there before the save.
                self.docs.invalidate(&Source::Worktree);
                self.note(format!("Saved {}", key.1.as_str()), cx);
            }
            Err(error) => self.note(format!("Could not save: {error}"), cx),
        }
    }

    /// Says something in the status bar for a moment.
    fn note(&mut self, message: impl Into<String>, cx: &mut Context<Self>) {
        self.toast = Some((message.into(), Instant::now()));
        cx.notify();
    }

    /// The header of the source view: where the code is and what acts on it.
    ///
    /// One line, one height: the directory, the file and the line, then the
    /// short name of the symbol, which ellipsises, then the actions, which do
    /// not.
    fn code_head(
        &self,
        location: Option<&Location>,
        peek: bool,
        dirty: bool,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let (directory, file, line) = match location {
            Some(location) => {
                let path = location.file.as_str();
                let (directory, file) = match path.rfind('/') {
                    Some(at) => (&path[..=at], &path[at + 1..]),
                    None => ("", path),
                };
                (
                    directory.to_owned(),
                    file.to_owned(),
                    location.span.start_line.to_string(),
                )
            }
            None => (String::new(), String::new(), String::new()),
        };
        let symbol = self
            .session
            .selection()
            .map(|selection| wording::short_name(&selection.symbol.path).name)
            .unwrap_or_default();
        let mono = theme::mono();

        let mut head = h_flex()
            .h(px(30.))
            .flex_shrink_0()
            .pl_3()
            .pr_1()
            .gap_2()
            .items_center()
            .overflow_hidden()
            .border_b_1()
            .border_color(theme::LINE_SOFT)
            .text_size(px(12.))
            .child(
                Icon::new(IconName::FileText)
                    .size_3()
                    .flex_shrink_0()
                    .text_color(theme::FG_4),
            )
            .child(
                h_flex()
                    .flex_shrink_1()
                    .min_w_0()
                    .max_w(px(380.))
                    .overflow_hidden()
                    .font_family(mono.clone())
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
                    )
                    .child(
                        div()
                            .text_color(theme::CHANGED.fg)
                            .child(format!(":{line}")),
                    ),
            );
        if dirty {
            head = head.child(
                h_flex()
                    .flex_shrink_0()
                    .gap_1()
                    .items_center()
                    .text_size(TEXT_LINE_NUMBER)
                    .text_color(theme::FG_3)
                    .child(
                        div()
                            .size(px(6.))
                            .rounded_full()
                            .bg(theme::CHANGED.solid.hsla()),
                    )
                    .child("Modified"),
            );
        }
        head = head.child(
            div()
                .flex_1()
                .min_w_0()
                .overflow_hidden()
                .font_family(mono)
                .text_size(px(11.5))
                .text_color(theme::FG_3)
                .text_ellipsis()
                .child(symbol),
        );
        if dirty {
            head = head.child(
                div()
                    .id("code-save")
                    .flex_shrink_0()
                    .h(px(22.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded(theme::RADIUS_SM)
                    .text_size(px(11.5))
                    .text_color(theme::CHANGED.fg)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::BG_HOVER))
                    .on_click(cx.listener(|this, _, _, cx| this.save_code(cx)))
                    .child("Save"),
            );
        }
        if peek && location.is_some() {
            head = head.child(
                div()
                    .id("code-full-file")
                    .flex_shrink_0()
                    .h(px(22.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded(theme::RADIUS_SM)
                    .text_size(px(11.5))
                    .text_color(theme::FG_3)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::BG_HOVER).text_color(theme::FG))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.tab = Tab::Code;
                        cx.notify();
                    }))
                    .child("Full file"),
            );
        }
        if location.is_some() {
            head = head.child(
                div()
                    .id("code-open-editor")
                    .flex_shrink_0()
                    .h(px(22.))
                    .px_2()
                    .flex()
                    .items_center()
                    .rounded(theme::RADIUS_SM)
                    .text_size(px(11.5))
                    .text_color(theme::FG_3)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::BG_HOVER).text_color(theme::FG))
                    .on_click(cx.listener(|this, _, _, cx| this.open_in_editor(cx)))
                    .child("Open in editor"),
            );
        }
        head.into_any_element()
    }

    /// The lines of the change the selection names, which are the only lines
    /// the source view tints as a change.
    fn change_lines(&self) -> Vec<(u32, u32)> {
        let mut ranges = Vec::new();
        if let Some(change) = self.selected_change() {
            if let Some(new) = &change.new {
                ranges.push((new.span.start_line, new.span.end_line));
            }
            if let Some(old) = &change.old {
                ranges.push((old.span.start_line, old.span.end_line));
            }
        }
        ranges
    }

    /// The line of the evidence that makes the selected item affected, when it
    /// sits in the file the source view shows.
    fn reference_line(&self, file: &RelPath) -> Option<u32> {
        let item = self.selected_item()?;
        let step = wording::best_path(item)?.steps.last()?;
        (step.site.file == *file).then_some(step.site.span.start_line)
    }
}

/// The editor for one file, with its colours and its scanner.
///
/// This is the one step of opening a file that needs the window: the editor
/// measures its text with it. It runs on the window's own thread, from a task
/// that is not a frame, so the window is free to be borrowed here.
fn open_editor(
    cx: &mut AsyncApp,
    file: &RelPath,
    text: &str,
    writable: bool,
) -> Option<Entity<EditorState>> {
    let handle = cx.update(|cx| cx.active_window().or_else(|| cx.windows().first().copied()))?;
    let language = if file.as_str().ends_with(".rs") {
        "rust"
    } else {
        "text"
    };
    handle
        .update(cx, |_, window, cx| {
            new_editor(window, cx, language, text, writable)
        })
        .ok()
}

/// Builds the editor, which draws itself with the colours of the theme.
///
/// A revision that has no file behind it is read-only: there is nothing a save
/// could write it to.
fn new_editor(
    window: &mut Window,
    cx: &mut App,
    language: &str,
    text: &str,
    writable: bool,
) -> Entity<EditorState> {
    let editor = cx.new(|cx| {
        EditorState::new(window, cx)
            .folding(false)
            .language(language.to_owned())
    });
    editor.update(cx, |state, cx| {
        state.set_highlighter_factory(syntax::factory(), cx);
        state.set_highlighter(language.to_owned(), cx);
        state.set_editor_style(editor_style());
        state.set_editor_paddings(Edges {
            top: px(PADDING),
            right: px(PADDING),
            bottom: px(PADDING),
            left: px(PADDING),
        });
        state.set_gutter_decorations_width(px(BAR_WIDTH), cx);
        state.set_value(text.to_owned(), window, cx);
        state.set_readonly(!writable, cx);
    });
    editor
}

/// Replaces the text of an editor with a file that changed on disk.
///
/// Returns whether the editor took the text: it needs the window to measure it,
/// and a window busy with a frame has none to lend.
fn replace_text(cx: &mut AsyncApp, editor: &Entity<EditorState>, text: &str) -> bool {
    let handle = cx.update(|cx| cx.active_window().or_else(|| cx.windows().first().copied()));
    let Some(handle) = handle else {
        return false;
    };
    let text = text.to_owned();
    handle
        .update(cx, |_, window, cx| {
            editor.update(cx, |state, cx| state.set_value(text, window, cx));
        })
        .is_ok()
}

/// The colours the source editor draws itself with, from the one dark theme.
fn editor_style() -> InputEditorStyle {
    let mut selection = theme::BEHAVE.solid.hsla();
    selection.a = 0.3;
    InputEditorStyle {
        foreground: theme::FG_2.hsla(),
        muted_foreground: theme::FG_4.hsla(),
        background: theme::BG_CANVAS.hsla(),
        border: theme::LINE.hsla(),
        selection,
        caret: theme::FG.hsla(),
        editor_gutter_background: Some(theme::BG_CANVAS.hsla()),
        ..InputEditorStyle::default()
    }
}

#[cfg(test)]
mod tests {
    use blastr_model::{Span, SymbolKind, SymbolPath};

    use super::*;

    #[test]
    fn a_fresh_pane_has_nothing_to_save_and_scrolls_again_on_reset() {
        let mut code = CodeState::default();
        assert!(!code.any_dirty(), "a pane with no file has nothing to save");

        code.reset();

        assert!(code.pending, "another selection scrolls to it");
        assert!(
            !code.any_dirty(),
            "resetting keeps what is unsaved, not a flag"
        );
    }

    #[test]
    fn each_pane_places_a_selection_once_in_its_own_view() {
        let file = RelPath::new("src/de.rs");
        let key: Key = (Source::Worktree, file.clone());
        let symbol =
            |name: &str| SymbolPath::new(format!("serde_json::de::{name}"), SymbolKind::Function);
        let next = Selection::symbol(symbol("next"));
        let mut code = CodeState::default();

        assert!(
            code.wants_place(Pane::Peek, &next, &key),
            "a new selection is placed"
        );
        code.placements[Pane::Peek.slot()].placed = Some((next.clone(), key.clone()));
        code.scrolled_in = Some(Pane::Peek);
        assert!(
            !code.wants_place(Pane::Peek, &next, &key),
            "a repaint of the same pane leaves it where it is"
        );
        assert!(
            code.wants_place(Pane::Full, &next, &key),
            "the other pane has not placed it at its own height"
        );

        // A pane shown again after the other one placed the same selection
        // puts it in place again, because the view was moved for the other
        // height.
        code.scrolled_in = Some(Pane::Full);
        assert!(code.wants_place(Pane::Peek, &next, &key));

        // Another selection, and the same lines of another revision, are new
        // places to scroll to.
        code.scrolled_in = Some(Pane::Full);
        assert!(code.wants_place(Pane::Full, &Selection::symbol(symbol("seq")), &key));
        assert!(code.wants_place(Pane::Full, &next, &(Source::Index, file.clone())));
        // Selecting the same symbol from an evidence step is a new selection,
        // so the line the reader followed is put in place.
        let site = Location {
            file,
            span: Span {
                start: 0,
                end: 1,
                start_line: 2459,
                end_line: 2459,
            },
        };
        assert!(code.wants_place(Pane::Full, &Selection::at_site(symbol("next"), site), &key));
    }

    #[test]
    fn a_request_makes_both_panes_place_the_selection_again() {
        let key: Key = (Source::Worktree, RelPath::new("src/de.rs"));
        let next = Selection::symbol(SymbolPath::new(
            "serde_json::de::next",
            SymbolKind::Function,
        ));
        let mut code = CodeState::default();
        for pane in [Pane::Peek, Pane::Full] {
            code.placements[pane.slot()].placed = Some((next.clone(), key.clone()));
        }
        code.scrolled_in = Some(Pane::Peek);
        assert!(!code.wants_place(Pane::Peek, &next, &key));

        code.pending = true;
        code.take_request();

        assert!(!code.pending, "the request is taken once");
        assert!(
            code.wants_place(Pane::Peek, &next, &key) && code.wants_place(Pane::Full, &next, &key),
            "both panes place it again"
        );
    }

    #[test]
    fn another_repository_drops_the_open_files() {
        let mut code = CodeState {
            root: Some(PathBuf::from("/one")),
            ..CodeState::default()
        };
        code.claim_root(Some(PathBuf::from("/two")));
        assert!(code.open.is_empty());
        assert_eq!(code.root, Some(PathBuf::from("/two")));

        // Asking for the repository that is already open keeps the editors.
        code.failed = Some((Source::Worktree, RelPath::new("a.rs")));
        code.claim_root(Some(PathBuf::from("/two")));
        assert!(code.failed.is_some());
    }

    #[test]
    fn a_line_range_covers_the_line_and_stays_inside_the_text() {
        let text = Rope::from("one\ntwo\nthree");
        assert_eq!(line_range(&text, 1, 1), 0..3);
        assert_eq!(line_range(&text, 2, 2), 4..7);
        assert_eq!(line_range(&text, 1, 2), 0..7);
        // A line past the end covers the last one, and nothing is reversed.
        assert_eq!(line_range(&text, 9, 9), 8..13);
        assert_eq!(line_range(&text, 0, 0), 0..3);
    }

    #[test]
    fn an_empty_file_has_no_range_to_tint() {
        let text = Rope::new();
        assert_eq!(line_range(&text, 1, 1), 0..0);
        assert_eq!(line_range(&text, 7, 9), 0..0);
    }

    #[test]
    fn a_gutter_mark_reaches_the_empty_lines_of_its_range() {
        let text = Rope::from("one\n\nthree\nfour");
        // The whole range, line break included, so that an empty line inside it
        // is marked as a line and not as a boundary between two lines.
        assert_eq!(line_span(&text, 1, 3), 0..11);
        assert_eq!(
            text.slice(line_span(&text, 1, 3)).to_string(),
            "one\n\nthree\n"
        );
        assert_eq!(line_span(&text, 2, 2), 4..5);
        // The last line has no line break of its own to include.
        assert_eq!(line_span(&text, 4, 4), 11..15);
    }

    #[test]
    fn a_band_covers_every_line_of_the_range_including_the_empty_ones() {
        // Three lines of ten pixels from the first line, one of them empty.
        let colour = tint(theme::BEHAVE.solid, CLASS_TINT_ALPHA);
        let bands = bands(&[((1, 3), colour)], 10., PADDING, 0., 400., 20);
        assert_eq!(bands.len(), 1);
        let (top, height, shade) = bands[0];
        assert_eq!(top, PADDING);
        assert_eq!(height, 30., "the empty line is tinted like the rest");
        assert_eq!(shade, colour);
    }

    #[test]
    fn a_band_follows_the_scroll_and_stays_in_view() {
        let colour = theme::CHANGED_TINT.hsla();
        // Scrolled down by two lines, so the band starts at the top of the view.
        let scrolled = bands(&[((1, 5), colour)], 10., 0., -20., 100., 50);
        assert_eq!(scrolled.len(), 1);
        assert_eq!(scrolled[0].0, 0.);
        assert_eq!(scrolled[0].1, 30.);
        // A range below the viewport paints nothing at all.
        assert!(bands(&[((40, 45), colour)], 10., PADDING, 0., 100., 50).is_empty());
        // A range larger than the viewport is one band over the rows shown.
        let whole = bands(&[((1, 50), colour)], 10., 0., 0., 100., 50);
        assert_eq!(whole.len(), 1);
        assert_eq!(whole[0].0, 0.);
        assert_eq!(whole[0].1, 100.);
    }

    #[test]
    fn bands_are_painted_weakest_first_and_kept_apart() {
        let weak = theme::CHANGED_TINT.hsla();
        let strong = tint(theme::BEHAVE.solid, REFERENCE_TINT_ALPHA);
        let painted = bands(&[((1, 2), weak), ((4, 4), strong)], 10., 0., 0., 400., 10);
        assert_eq!(painted.len(), 2);
        assert_eq!((painted[0].0, painted[0].1, painted[0].2), (0., 20., weak));
        assert_eq!(
            (painted[1].0, painted[1].1, painted[1].2),
            (30., 10., strong)
        );
        // Nothing to draw without a line height, a viewport or a file.
        assert!(bands(&[((1, 2), weak)], 0., PADDING, 0., 400., 10).is_empty());
        assert!(bands(&[((1, 2), weak)], 10., PADDING, 0., 0., 10).is_empty());
        assert!(bands(&[((1, 2), weak)], 10., PADDING, 0., 400., 0).is_empty());
    }

    #[test]
    fn the_selection_starts_a_third_of_the_way_down() {
        // The first line of a 45 line range in a 5200 line file, with 30 lines
        // in view: ten of them stay above it.
        let focus = Focus {
            first: 2451,
            last: 2495,
            anchor: 2451,
        };
        assert_eq!(top_row(focus, 30, 5200), 2440);
        // The same view in pixels, with 20 pixel lines.
        assert_eq!(scroll_top(focus, 20., 600., 5200), -48_800.);
    }

    #[test]
    fn a_range_that_fits_is_shown_whole() {
        let focus = Focus {
            first: 2451,
            last: 2495,
            anchor: 2451,
        };
        // 60 lines in view hold the 45 of the range: the last line stops the
        // view from leaving the third, and no line of the range is above it.
        let top = top_row(focus, 60, 5200);
        assert_eq!(top, 2435);
        assert!(top < 2451, "the range starts inside the view");
        assert!(top + 59 >= 2495 - 1, "its last line is visible too");
    }

    #[test]
    fn the_clicked_evidence_line_is_the_one_put_in_place() {
        // The step the selection was made from, line 2459, goes a third of the
        // way down; the range around it stays in sight.
        let focus = Focus {
            first: 2451,
            last: 2459,
            anchor: 2459,
        };
        assert_eq!(top_row(focus, 60, 5200), 2438);
    }

    #[test]
    fn the_view_stops_at_the_first_and_the_last_line() {
        let start = Focus {
            first: 1,
            last: 3,
            anchor: 1,
        };
        assert_eq!(top_row(start, 60, 5200), 0);
        // The last lines cannot go past the end of the file.
        let end = Focus {
            first: 5190,
            last: 5200,
            anchor: 5190,
        };
        assert_eq!(top_row(end, 20, 5200), 5180);
        // A range past the end keeps the last line the file has in sight.
        let past = Focus {
            first: 5199,
            last: 6000,
            anchor: 5199,
        };
        assert_eq!(top_row(past, 20, 5200), 5180);
        // A file shorter than the view does not scroll at all, and neither
        // does an empty one.
        assert_eq!(top_row(start, 60, 10), 0);
        assert_eq!(top_row(start, 60, 0), 0);
        assert_eq!(top_row(start, 0, 5200), 0);
    }

    #[test]
    fn a_selection_puts_its_first_line_in_place_unless_a_step_was_clicked() {
        let file = RelPath::new("src/de.rs");
        let span = |first: u32, last: u32| Location {
            file: file.clone(),
            span: Span {
                start: 0,
                end: 1,
                start_line: first,
                end_line: last,
            },
        };
        let item = span(2451, 2495);
        // A symbol picked from a list puts the first line of its range in place.
        assert_eq!(
            selection_focus(None, &item),
            Focus {
                first: 2451,
                last: 2495,
                anchor: 2451,
            }
        );
        // The step the selection was made from is the line the reader followed.
        assert_eq!(
            selection_focus(Some(&span(2459, 2459)), &item),
            Focus {
                first: 2451,
                last: 2495,
                anchor: 2459,
            }
        );
        // A step in another file is no reference for this one.
        let elsewhere = Location {
            file: RelPath::new("src/lib.rs"),
            span: span(2459, 2459).span,
        };
        assert_eq!(selection_focus(Some(&elsewhere), &item).anchor, 2451);
    }

    #[test]
    fn the_gutter_marks_the_range_and_the_evidence_line() {
        let tints = Tints {
            own: Some((2451, 2459)),
            reference: Some(2459),
            ..Tints::default()
        };
        assert_eq!(tints.bar_lines(), vec![(2451, 2459), (2459, 2459)]);

        let own = Tints {
            own: Some((2451, 2459)),
            ..Tints::default()
        };
        assert_eq!(own.bar_lines(), vec![(2451, 2459)]);
    }
}
