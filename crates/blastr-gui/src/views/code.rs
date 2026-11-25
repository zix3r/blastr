//! The one code pane: the diff of a selected change, or the source of any
//! other selection with the lines of its evidence marked, both in the same
//! diff view and both limited to the symbol and its evidence.

use std::path::{Path, PathBuf};

use blastr_model::{Location, RelPath, Source, SymbolPath, wording};
use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::diff::{
    Diff, DiffDecoration, DiffFile, DiffLinePosition, DiffLineRange, DiffMode, DiffSide, DiffState,
};
use gpui_kit::component::{Disableable as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Entity, IntoElement, ParentElement, Render, Styled, Subscription, Task,
    Window, div, px,
};
use similar::{ChangeTag, TextDiff};

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
    /// Source without changes, in the same diff view.
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
    /// Whether the user wants a diff side by side; source is always unified.
    split: bool,
    read: Task<()>,
    _subscriptions: Vec<Subscription>,
}

impl CodePane {
    pub(crate) fn new(
        workspace: Entity<Workspace>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> CodePane {
        // The patch holds only the lines worth showing: fold none of them.
        let diff = cx.new(|cx| DiffState::new(Vec::new(), cx).with_context_lines(None));
        let subscriptions = vec![cx.observe_in(&workspace, window, |this, _, window, cx| {
            this.sync(window, cx)
        })];
        theme::set_code_zoom(workspace.read(cx).settings.code_zoom, cx);
        CodePane {
            workspace,
            maximized: false,
            key: None,
            content: Content::Note("Nothing selected", "Select a change or an affected item."),
            location: None,
            title: String::new(),
            diff,
            decorations: Vec::new(),
            split: false,
            read: Task::ready(()),
            _subscriptions: subscriptions,
        }
    }

    /// The text size in percent.
    fn zoom(&self, cx: &Context<Self>) -> u16 {
        theme::code_zoom(self.workspace.read(cx).settings.code_zoom)
    }

    /// Makes the text one step bigger or smaller, or back to 100% with
    /// `None`, and remembers it.
    pub(crate) fn change_zoom(&mut self, bigger: Option<bool>, cx: &mut Context<Self>) {
        let zoom = match bigger {
            Some(bigger) => theme::step_code_zoom(self.zoom(cx), bigger),
            None => 100,
        };
        self.workspace.update(cx, |workspace, cx| {
            workspace.update_settings(cx, |settings| settings.code_zoom = zoom)
        });
        theme::set_code_zoom(zoom, cx);
        cx.notify();
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
            let (side, span) = match (&change.new, &change.old) {
                (Some(new), _) => (DiffSide::Modified, new.span),
                (None, Some(old)) => (DiffSide::Original, old.span),
                (None, None) => return,
            };
            let span = (span.start_line, span.end_line);
            let texts = cx.background_executor().spawn(async move {
                let base = read_source(&root, &comparison.base, &old_file).unwrap_or_default();
                let head = read_source(&root, &comparison.head, &file).unwrap_or_default();
                // A hypothetical change has no diff: its source, as context.
                let changed = base != head;
                let patch = patch(&old_file, &file, &base, &head, side, &[span]);
                (patch, changed)
            });
            self.content = Content::Loading;
            self.read = cx.spawn_in(window, async move |this, cx| {
                let (patch, changed) = texts.await;
                this.update_in(cx, |this, _, cx| {
                    this.show(&patch, side, theme::CHANGED, span, &[], span.0, changed, cx);
                })
                .ok();
            });
        } else if let Some((source, location)) = workspace.selection_location() {
            // Anything else: its source, with the lines of its evidence
            // marked. The item's own code is unchanged, so it is always shown
            // as context from the selection's side, never as a diff, even when
            // other lines of its file changed.
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
            // names the whole symbol, only the symbol.
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
            let mut shown = vec![span];
            shown.extend(&lines);
            let patch = cx.background_executor().spawn(async move {
                let text = read_source(&root, &source, &file)?;
                Some(self::patch(
                    &file,
                    &file,
                    &text,
                    &text,
                    DiffSide::Modified,
                    &shown,
                ))
            });
            self.content = Content::Loading;
            self.read = cx.spawn_in(window, async move |this, cx| {
                let patch = patch.await.unwrap_or_default();
                this.update_in(cx, |this, _, cx| {
                    let side = DiffSide::Modified;
                    this.show(&patch, side, accent, span, &lines, focus, false, cx);
                })
                .ok();
            });
        } else {
            self.content = Content::Note("No source", "The symbol has no place in a file.");
        }
        self.key = Some(key);
        cx.notify();
    }

    /// Puts `patch` in the diff view, marks the symbol's `span` and its
    /// evidence `lines` on `side`, and scrolls to `focus`; a patch without
    /// changes shows as source.
    #[allow(clippy::too_many_arguments)]
    fn show(
        &mut self,
        patch: &str,
        side: DiffSide,
        accent: Accent,
        span: (u32, u32),
        lines: &[(u32, u32)],
        focus: u32,
        changed: bool,
        cx: &mut Context<Self>,
    ) {
        let Some(file) = DiffFile::parse(patch)
            .unwrap_or_default()
            .into_iter()
            .next()
        else {
            self.content = Content::Note("Source unavailable", "The file could not be read.");
            cx.notify();
            return;
        };
        let path = file.path().clone();
        let mode = if changed && self.split {
            DiffMode::Split
        } else {
            DiffMode::Unified
        };
        let range = |(first, last): (u32, u32), alpha: f32| {
            DiffDecoration::new(DiffLineRange::new(
                path.clone(),
                side,
                first as usize,
                last.max(first) as usize,
            ))
            .with_background(accent.solid.hsla().alpha(alpha))
        };
        // Where marks overlap, the last one shows: the evidence over the symbol.
        self.decorations = std::iter::once(range(span, 0.07))
            .chain(lines.iter().map(|line| range(*line, 0.16)))
            .collect();
        self.diff.update(cx, |state, cx| {
            state.set_mode(mode, cx);
            state.set_files([file], cx);
            // The patch shows the context above the focus, so this line is in it.
            let line = (focus as usize).saturating_sub(CONTEXT).max(1);
            state.scroll_to_line(DiffLinePosition::new(path, side, line), cx);
        });
        self.content = if changed {
            Content::Diff
        } else {
            Content::Source
        };
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
                            this.split = !split;
                            let mode = if split {
                                DiffMode::Unified
                            } else {
                                DiffMode::Split
                            };
                            this.diff.update(cx, |state, cx| state.set_mode(mode, cx));
                        })),
                )
            })
            .child(self.zoom_controls(cx))
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

impl CodePane {
    /// Smaller, the size (click for 100%) and bigger.
    fn zoom_controls(&self, cx: &mut Context<Self>) -> AnyElement {
        let zoom = self.zoom(cx);
        h_flex()
            .items_center()
            .child(
                ui::icon_button("code-zoom-out", IconName::Minus, "Smaller text  Ctrl -")
                    .disabled(zoom == theme::CODE_ZOOMS[0])
                    .on_click(cx.listener(|this, _, _, cx| this.change_zoom(Some(false), cx))),
            )
            .child(
                Button::new("code-zoom-reset")
                    .xsmall()
                    .ghost()
                    .label(format!("{zoom}%"))
                    .tooltip("Text size; click for 100%  Ctrl 0")
                    .on_click(cx.listener(|this, _, _, cx| this.change_zoom(None, cx))),
            )
            .child(
                ui::icon_button("code-zoom-in", IconName::Plus, "Bigger text  Ctrl +")
                    .disabled(zoom == theme::CODE_ZOOMS[theme::CODE_ZOOMS.len() - 1])
                    .on_click(cx.listener(|this, _, _, cx| this.change_zoom(Some(true), cx))),
            )
            .into_any_element()
    }
}

impl Render for CodePane {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let body = match &self.content {
            Content::Note(title, body) => ui::empty_state(IconName::FileText, None, title, body),
            Content::Loading => ui::empty_state(IconName::Loader, None, "Reading the file", ""),
            Content::Diff | Content::Source => Diff::new(&self.diff)
                .header_visible(false)
                .decorations(self.decorations.clone())
                .size_full()
                .into_any_element(),
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

/// The lines of context shown around a symbol and its evidence.
const CONTEXT: usize = 3;

/// The unified diff of one file, limited to the lines `areas` names on
/// `side` (first and last, counted from one) and [`CONTEXT`] lines around
/// each, with their real line numbers.
///
/// The lines of the other side that sit between shown lines, such as those
/// a change removed, are shown too. Identical texts give a patch of context
/// only; nothing to show gives no hunk.
fn patch(
    old: &RelPath,
    new: &RelPath,
    base: &str,
    head: &str,
    side: DiffSide,
    areas: &[(u32, u32)],
) -> String {
    let diff = TextDiff::from_lines(base, head);
    let rows: Vec<_> = diff.iter_all_changes().collect();
    let wanted = |line: usize| {
        areas.iter().any(|&(first, last)| {
            let (first, last) = (first.max(1) as usize, last.max(first) as usize);
            line + CONTEXT >= first && line <= last + CONTEXT
        })
    };
    let line_on_side = |row: usize| match side {
        DiffSide::Original => rows[row].old_index(),
        DiffSide::Modified => rows[row].new_index(),
    };
    // A row with a line on `side` is shown when that line is wanted; another
    // when a neighbouring row that has one is shown.
    let mut shown: Vec<Option<bool>> = (0..rows.len())
        .map(|row| line_on_side(row).map(|index| wanted(index + 1)))
        .collect();
    let mut before = vec![false; rows.len()];
    let mut last = false;
    for (row, value) in shown.iter().enumerate() {
        before[row] = last;
        last = value.unwrap_or(last);
    }
    let mut after = false;
    for row in (0..rows.len()).rev() {
        let value = shown[row].unwrap_or(before[row] || after);
        after = shown[row].unwrap_or(after);
        shown[row] = Some(value);
    }
    let mut text = format!("--- a/{old}\n+++ b/{new}\n");
    let (mut old_line, mut new_line) = (0, 0);
    let mut row = 0;
    while row < rows.len() {
        if shown[row] != Some(true) {
            old_line += usize::from(rows[row].old_index().is_some());
            new_line += usize::from(rows[row].new_index().is_some());
            row += 1;
            continue;
        }
        let end = (row..rows.len())
            .find(|&row| shown[row] != Some(true))
            .unwrap_or(rows.len());
        let hunk = &rows[row..end];
        let olds = hunk.iter().filter(|row| row.old_index().is_some()).count();
        let news = hunk.iter().filter(|row| row.new_index().is_some()).count();
        let start = |line: usize, count: usize| if count == 0 { line } else { line + 1 };
        text.push_str(&format!(
            "@@ -{},{olds} +{},{news} @@\n",
            start(old_line, olds),
            start(new_line, news)
        ));
        for change in hunk {
            let sign = match change.tag() {
                ChangeTag::Equal => ' ',
                ChangeTag::Delete => '-',
                ChangeTag::Insert => '+',
            };
            let value = change.value();
            text.push(sign);
            text.push_str(value.strip_suffix('\n').unwrap_or(value));
            text.push('\n');
            if !value.ends_with('\n') {
                text.push_str("\\ No newline at end of file\n");
            }
        }
        old_line += olds;
        new_line += news;
        row = end;
    }
    text
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Lines `line 1` to `line {count}`, each ending in a line break.
    fn numbered(count: usize) -> String {
        (1..=count).map(|line| format!("line {line}\n")).collect()
    }

    #[test]
    fn a_changed_symbol_shows_only_its_area_and_its_changes() {
        let path = RelPath::new("src/a.rs");
        let base = numbered(40);
        let head = base
            .replace("line 2\n", "line two\n")
            .replace("line 20\n", "line twenty\n");
        // The symbol is lines 19 to 21; the change on line 2 is far away.
        let patch = patch(&path, &path, &base, &head, DiffSide::Modified, &[(19, 21)]);
        assert!(
            patch.starts_with("--- a/src/a.rs\n+++ b/src/a.rs\n@@ -16,9 +16,9 @@\n"),
            "{patch}"
        );
        assert!(patch.contains("-line 20\n+line twenty\n"), "{patch}");
        assert!(!patch.contains("line two"), "{patch}");
        assert!(!patch.contains("line 15\n") && !patch.contains("line 25\n"));
        let files = DiffFile::parse(&patch).expect("a patch");
        assert_eq!(files.len(), 1);
        assert_eq!((files[0].additions(), files[0].deletions()), (1, 1));
        assert_eq!(files[0].language().as_str(), "rust");
    }

    #[test]
    fn an_item_shows_its_lines_and_every_evidence_line_as_context() {
        let path = RelPath::new("src/a.rs");
        let text = numbered(40);
        // The item is lines 10 to 12; evidence on line 11 and, outside it,
        // on line 30.
        let areas = [(10, 12), (11, 11), (30, 30)];
        let patch = patch(&path, &path, &text, &text, DiffSide::Modified, &areas);
        let hunks: Vec<&str> = patch
            .lines()
            .filter(|line| line.starts_with("@@"))
            .collect();
        // Real line numbers, not from one: 7 to 15 and 27 to 33.
        assert_eq!(hunks, ["@@ -7,9 +7,9 @@", "@@ -27,7 +27,7 @@"], "{patch}");
        let body: Vec<&str> = patch
            .lines()
            .skip(2)
            .filter(|line| !line.starts_with("@@"))
            .collect();
        assert!(body.iter().all(|line| line.starts_with(' ')), "{patch}");
        let shown: Vec<usize> = body
            .iter()
            .map(|line| line.trim_start_matches(" line ").parse().unwrap())
            .collect();
        let expected: Vec<usize> = (7..=15).chain(27..=33).collect();
        assert_eq!(shown, expected);
        let files = DiffFile::parse(&patch).expect("a patch");
        assert!(!files[0].has_changes());
    }

    #[test]
    fn a_last_line_without_a_break_is_marked() {
        let path = RelPath::new("a.rs");
        let patch = patch(&path, &path, "a\nb", "a\nb", DiffSide::Modified, &[(2, 2)]);
        assert!(
            patch.ends_with("@@ -1,2 +1,2 @@\n a\n b\n\\ No newline at end of file\n"),
            "{patch}"
        );
        DiffFile::parse(&patch).expect("a patch");
    }
}
