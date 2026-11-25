//! The one code pane: the diff of a selected change, or the read-only source
//! of any other selection with the lines of its evidence marked.

use std::ops::Range;
use std::path::{Path, PathBuf};

use blastr_model::{Location, RelPath, Source, SymbolPath, wording};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::{
    Diff, DiffDecoration, DiffFile, DiffLinePosition, DiffLineRange, DiffMode, DiffSide, DiffState,
};
use gpui_kit::component::input::{
    Editor, EditorState, GutterDecoration, GutterDecorationCollection, InputEvent, RangeDecoration,
    RangeDecorationCollection, RangeDecorationStyle,
};
use gpui_kit::component::{Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Entity, IntoElement, ParentElement, Render, SharedString, Styled,
    Subscription, Task, Window, div, point, px,
};
use similar::TextDiff;

use super::ui;
use crate::theme::{self, Accent, TEXT_MICRO, TEXT_SMALL};
use crate::workspace::Workspace;

/// What the pane shows.
enum Content {
    /// A sentence, when there is nothing to show.
    Note(&'static str, &'static str),
    /// The file is being read.
    Loading,
    /// The diff of a change.
    Diff,
    /// The source of a file.
    Source,
}

/// What the pane was asked to show; another key reads again.
#[derive(Debug, Clone, PartialEq)]
struct Key {
    analysis: usize,
    revision: u64,
    symbol: SymbolPath,
    site: Option<Location>,
}

/// The code pane.
pub(crate) struct CodePane {
    workspace: Entity<Workspace>,
    /// Whether the pane fills the centre, without the graph.
    pub(crate) maximized: bool,
    key: Option<Key>,
    content: Content,
    /// The file and line the header names and the editor opens.
    location: Option<Location>,
    title: String,
    diff: Entity<DiffState>,
    decorations: Vec<DiffDecoration>,
    editor: Entity<EditorState>,
    /// The text the editor was given, which it must keep.
    shown: SharedString,
    marks: Option<(RangeDecorationCollection, GutterDecorationCollection)>,
    read: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl CodePane {
    pub(crate) fn new(
        workspace: Entity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> CodePane {
        let diff = cx.new(|cx| DiffState::new(Vec::new(), cx));
        let editor = cx.new(|cx| source_state(window, cx));
        let subscriptions = vec![
            cx.observe_in(&workspace, window, |this, _, window, cx| {
                this.sync(window, cx)
            }),
            cx.subscribe_in(&editor, window, Self::keep_text),
        ];
        CodePane {
            workspace,
            maximized: false,
            key: None,
            content: Content::Note("Nothing selected", "Select a change or an affected item."),
            location: None,
            title: String::new(),
            diff,
            decorations: Vec::new(),
            editor,
            shown: SharedString::default(),
            marks: None,
            read: Task::ready(()),
            _subscriptions: subscriptions,
        }
    }

    /// Puts back the text the editor was given, should anything have changed
    /// it.
    ///
    /// The editor is read-only, which rejects every edit the user makes;
    /// this is the second line, so that no path the library might not
    /// guard can leave other code on screen than the file's.
    fn keep_text(
        &mut self,
        editor: &Entity<EditorState>,
        event: &InputEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if matches!(event, InputEvent::Change) && editor.read(cx).value() != self.shown {
            let text = self.shown.clone();
            editor.update(cx, |state, cx| state.set_value(text, window, cx));
        }
    }

    /// Shows or hides the graph above the pane.
    pub(crate) fn toggle_maximized(&mut self, cx: &mut Context<Self>) {
        self.maximized = !self.maximized;
        cx.notify();
    }

    /// Reads what the selection names, when it changed.
    fn sync(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let workspace = self.workspace.read(cx);
        let session = &workspace.session;
        let (Some(outcome), Some(selection)) = (session.shown().ready(), session.selection())
        else {
            self.key = None;
            self.content =
                Content::Note("Nothing selected", "Select a change or an affected item.");
            cx.notify();
            return;
        };
        let key = Key {
            analysis: std::sync::Arc::as_ptr(&outcome.analysis) as usize,
            revision: outcome.revision,
            symbol: selection.symbol.clone(),
            site: selection.site.clone(),
        };
        if self.key.as_ref() == Some(&key) {
            return;
        }
        let Some(root) = workspace.root().map(Path::to_path_buf) else {
            return;
        };
        let comparison = outcome.analysis.diff.comparison.clone();
        let name = wording::short_name(&key.symbol.path).name;
        if let Some((_, change)) = workspace.selected_change() {
            // A change: the diff of its file, scrolled to it.
            let change = change.clone();
            let file = change
                .new
                .as_ref()
                .or(change.old.as_ref())
                .map(|location| location.file.clone());
            let Some(file) = file else {
                self.content = Content::Note("No source", "This change has no place in a file.");
                self.key = Some(key);
                cx.notify();
                return;
            };
            self.location = change.new.clone().or(change.old.clone());
            self.title = name;
            let old_file = change
                .old
                .as_ref()
                .map_or(file.clone(), |location| location.file.clone());
            let texts = cx.background_executor().spawn(async move {
                let base = read_source(&root, &comparison.base, &old_file).unwrap_or_default();
                let head = read_source(&root, &comparison.head, &file).unwrap_or_default();
                (patch(&old_file, &file, &base, &head), head)
            });
            self.content = Content::Loading;
            self.read = cx.spawn_in(window, async move |this, cx| {
                let (patch, head) = texts.await;
                this.update_in(cx, |this, window, cx| {
                    let files = DiffFile::parse(&patch).unwrap_or_default();
                    let path = files
                        .first()
                        .map(|file| file.path().clone())
                        .unwrap_or_default();
                    let (side, span) = match (&change.new, &change.old) {
                        (Some(new), _) => (DiffSide::Modified, new.span),
                        (None, Some(old)) => (DiffSide::Original, old.span),
                        (None, None) => return,
                    };
                    let (start, end) = (
                        span.start_line as usize,
                        span.end_line.max(span.start_line) as usize,
                    );
                    // A hypothetical change has no diff: show its source.
                    if files.is_empty() {
                        let lines = (span.start_line, span.end_line);
                        this.show_source(
                            head,
                            theme::CHANGED,
                            lines,
                            &[],
                            span.start_line,
                            window,
                            cx,
                        );
                        return;
                    }
                    self_decorate(this, path.clone(), side, start, end);
                    this.diff.update(cx, |state, cx| {
                        state.set_files(files, cx);
                        state.scroll_to_line(
                            DiffLinePosition::new(path, side, start.saturating_sub(3).max(1)),
                            cx,
                        );
                    });
                    this.content = Content::Diff;
                    cx.notify();
                })
                .ok();
            });
        } else if let Some((source, location)) = workspace.selection_location() {
            // Anything else: its source, with the lines of its evidence marked.
            let item = workspace.selected_item();
            let accent = item.map_or(theme::CHANGED, |item| ui::class_accent(item.class));
            let mut lines: Vec<(u32, u32)> = item
                .into_iter()
                .flat_map(|item| item.paths.iter())
                .flat_map(|path| path.steps.iter())
                .filter(|step| step.from == key.symbol && step.site.file == location.file)
                .map(|step| (step.site.span.start_line, step.site.span.end_line))
                .collect();
            // The line of the path from the change the graph shows comes first.
            let focus = workspace.focus_change();
            if let Some(first) = item
                .and_then(|item| {
                    item.paths
                        .iter()
                        .find(|path| Some(path.change as usize) == focus)
                })
                .and_then(|path| path.steps.iter().find(|step| step.from == key.symbol))
            {
                lines.insert(0, (first.site.span.start_line, first.site.span.end_line));
            }
            // A clicked evidence step marks its line; a search hit, which
            // names the whole symbol, only the gutter.
            if let (Some(site), Some(item)) = (&key.site, item)
                && *site != item.location
            {
                lines.push((site.span.start_line, site.span.end_line));
            }
            let span = item.map_or(location.span, |item| item.location.span);
            let span = (span.start_line, span.end_line);
            let focus = lines.first().map_or(span.0, |line| line.0);
            self.location = Some(location.clone());
            self.title = name;
            let file = location.file.clone();
            let text = cx
                .background_executor()
                .spawn(async move { read_source(&root, &source, &file) });
            self.content = Content::Loading;
            self.read = cx.spawn_in(window, async move |this, cx| {
                let text = text.await;
                this.update_in(cx, |this, window, cx| {
                    let Some(text) = text else {
                        this.content =
                            Content::Note("Source unavailable", "The file could not be read.");
                        cx.notify();
                        return;
                    };
                    this.show_source(text, accent, span, &lines, focus, window, cx);
                })
                .ok();
            });
        } else {
            self.content = Content::Note("No source", "The symbol has no place in a file.");
        }
        self.key = Some(key);
        cx.notify();
    }

    /// Puts `text` in the editor, marks the symbol and its evidence, and
    /// scrolls to `focus`.
    #[allow(clippy::too_many_arguments)]
    fn show_source(
        &mut self,
        text: String,
        accent: Accent,
        span: (u32, u32),
        lines: &[(u32, u32)],
        focus: u32,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let starts = line_starts(&text);
        let fills: Vec<RangeDecoration> = lines
            .iter()
            .map(|(first, last)| {
                RangeDecoration::new(line_range(&starts, text.len(), *first, *last))
                    .with_style(RangeDecorationStyle::Fill)
                    .with_color(accent.solid.hsla().alpha(0.16))
            })
            .collect();
        let mut bars = vec![
            GutterDecoration::new(line_range(&starts, text.len(), span.0, span.1))
                .with_color(accent.line.hsla()),
        ];
        bars.extend(lines.iter().map(|(first, last)| {
            GutterDecoration::new(line_range(&starts, text.len(), *first, *last))
                .with_color(accent.solid.hsla())
        }));
        let marks = self.marks.take();
        self.shown = text.clone().into();
        self.editor.update(cx, |state, cx| {
            state.set_value(text, window, cx);
            let line_height = state.line_height().unwrap_or(px(18.75));
            let top = focus.saturating_sub(6) as f32;
            state.set_scroll_offset(point(px(0.), -line_height * top), cx);
        });
        self.marks = Some(match marks {
            Some((fill, bar)) => {
                fill.set(fills, cx);
                bar.set(bars, cx);
                (fill, bar)
            }
            None => self.editor.update(cx, |state, cx| {
                (
                    state.create_range_decorations_collection(fills, cx),
                    state.create_gutter_decorations_collection(bars, cx),
                )
            }),
        });
        self.content = Content::Source;
        cx.notify();
    }

    fn header(&self, cx: &mut Context<Self>) -> AnyElement {
        let site = self
            .location
            .as_ref()
            .map(wording::site_text)
            .unwrap_or_default();
        let is_diff = matches!(self.content, Content::Diff);
        let split = self.diff.read(cx).mode() == DiffMode::Split;
        h_flex()
            .h(px(32.))
            .flex_none()
            .px_2()
            .gap_2()
            .items_center()
            .border_b_1()
            .border_color(theme::LINE)
            .bg(theme::BG_PANEL)
            .text_size(TEXT_SMALL)
            .child(
                Icon::new(if is_diff {
                    IconName::Replace
                } else {
                    IconName::FileText
                })
                .size_3()
                .text_color(theme::FG_4),
            )
            .child(
                div()
                    .font_family(theme::mono())
                    .text_color(theme::FG_2)
                    .child(site),
            )
            .child(
                div()
                    .min_w_0()
                    .truncate()
                    .font_family(theme::mono())
                    .text_color(theme::FG)
                    .child(self.title.clone()),
            )
            .child(
                div()
                    .text_size(TEXT_MICRO)
                    .text_color(theme::FG_4)
                    .child(match self.content {
                        Content::Diff => "changes",
                        Content::Source => "read-only",
                        _ => "",
                    }),
            )
            .child(div().flex_1())
            .when(is_diff, |this| {
                this.child(
                    Button::new("diff-mode")
                        .xsmall()
                        .ghost()
                        .label(if split { "Unified" } else { "Side by side" })
                        .on_click(cx.listener(move |this, _, _, cx| {
                            let mode = if split {
                                DiffMode::Unified
                            } else {
                                DiffMode::Split
                            };
                            this.diff.update(cx, |state, cx| state.set_mode(mode, cx));
                        })),
                )
            })
            .child(
                Button::new("code-open-editor")
                    .xsmall()
                    .ghost()
                    .label("Open in editor")
                    .on_click(cx.listener(|this, _, _, cx| {
                        if let Some(location) = this.location.clone() {
                            this.workspace.update(cx, |workspace, cx| {
                                workspace.open_in_editor(&location, cx)
                            });
                        }
                    })),
            )
            .child(
                ui::icon_button(
                    "code-maximize",
                    if self.maximized {
                        IconName::Minimize
                    } else {
                        IconName::Maximize
                    },
                    if self.maximized {
                        "Show the graph  Ctrl M"
                    } else {
                        "Maximise the code  Ctrl M"
                    },
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_maximized(cx))),
            )
            .into_any_element()
    }
}

/// Sets the decoration of the changed symbol in the diff.
fn self_decorate(
    pane: &mut CodePane,
    path: gpui_kit::SharedString,
    side: DiffSide,
    start: usize,
    end: usize,
) {
    pane.decorations = vec![
        DiffDecoration::new(DiffLineRange::new(path, side, start, end))
            .with_background(theme::CHANGED.solid.hsla().alpha(0.07)),
    ];
}

impl Render for CodePane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.content {
            Content::Note(title, body) => ui::empty_state(IconName::FileText, None, title, body),
            Content::Loading => ui::empty_state(IconName::Loader, None, "Reading the file", ""),
            Content::Diff => Diff::new(&self.diff)
                .header_visible(false)
                .decorations(self.decorations.clone())
                .size_full()
                .into_any_element(),
            Content::Source => source_editor(&self.editor).into_any_element(),
        };
        v_flex()
            .size_full()
            .bg(theme::BG_CANVAS)
            .when(!matches!(self.content, Content::Note(..)), |this| {
                this.child(self.header(cx))
            })
            .child(div().flex_1().min_h_0().child(body))
    }
}

/// The state of the source view: Rust, unfolded, and read-only from the
/// start, before it is first drawn.
fn source_state(window: &mut Window, cx: &mut Context<EditorState>) -> EditorState {
    let mut state = EditorState::new(window, cx).language("rust").folding(false);
    state.set_readonly(true, cx);
    state
}

/// The source view: selectable, copyable and searchable, never editable.
///
/// Read-only makes the editor reject typing, IME composition, cut, paste,
/// undo and every editing command, and leaves the edit items of its menu
/// disabled; a paste is also swallowed before it reaches the editor.
fn source_editor(state: &Entity<EditorState>) -> Editor {
    Editor::new(state)
        .readonly(true)
        .bordered(false)
        .on_paste(|_, _, _| true)
        .size_full()
}

/// Reads a file of one side of the comparison.
fn read_source(root: &Path, source: &Source, file: &RelPath) -> Option<String> {
    let relative: PathBuf = file.as_str().split('/').collect();
    let bytes = match source {
        Source::Worktree => std::fs::read(root.join(relative)).ok()?,
        Source::Dir(directory) => std::fs::read(directory.join(relative)).ok()?,
        Source::Index | Source::Tree { .. } => {
            let repo = blastr_git::Repo::open(&blastr_git::discover(root).ok()?).ok()?;
            repo.read_file(source, file).ok()??
        }
    };
    String::from_utf8(bytes).ok()
}

/// The unified diff of one file, with the whole file as context so that every
/// line can be scrolled to.
fn patch(old: &RelPath, new: &RelPath, base: &str, head: &str) -> String {
    let diff = TextDiff::from_lines(base, head);
    let mut unified = diff.unified_diff();
    unified
        .context_radius(usize::MAX / 4)
        .header(&format!("a/{old}"), &format!("b/{new}"));
    let text = unified.to_string();
    if text.is_empty() {
        return String::new();
    }
    // A file the comparison only adds or removes still needs both headers.
    text
}

/// The byte offset where each line starts.
fn line_starts(text: &str) -> Vec<usize> {
    std::iter::once(0)
        .chain(text.match_indices('\n').map(|(at, _)| at + 1))
        .collect()
}

/// The bytes of the lines `first` to `last`, counted from one, without the
/// final line break.
fn line_range(starts: &[usize], length: usize, first: u32, last: u32) -> Range<usize> {
    let index = |line: u32| (line.max(1) as usize - 1).min(starts.len().saturating_sub(1));
    let start = starts.get(index(first)).copied().unwrap_or(0);
    let end = starts
        .get(index(last.max(first)) + 1)
        .map_or(length, |next| next.saturating_sub(1));
    start..end.max(start)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_line_range_covers_whole_lines() {
        let text = "one\ntwo\nthree";
        let starts = line_starts(text);
        assert_eq!(&text[line_range(&starts, text.len(), 2, 2)], "two");
        assert_eq!(&text[line_range(&starts, text.len(), 2, 3)], "two\nthree");
        assert_eq!(&text[line_range(&starts, text.len(), 9, 9)], "three");
    }

    #[test]
    fn a_patch_carries_both_headers_and_every_line() {
        let patch = patch(
            &RelPath::new("src/a.rs"),
            &RelPath::new("src/a.rs"),
            "a\nb\nc\n",
            "a\nB\nc\n",
        );
        assert!(
            patch.starts_with("--- a/src/a.rs\n+++ b/src/a.rs\n"),
            "{patch}"
        );
        assert!(patch.contains(" a\n-b\n+B\n c\n"), "{patch}");
        let files = DiffFile::parse(&patch).expect("a patch");
        assert_eq!(files.len(), 1);
    }
}
