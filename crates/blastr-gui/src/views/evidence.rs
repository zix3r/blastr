//! The evidence panel: why the selected symbol is listed, the chain of
//! references that links it to a change, what limits the result, and the
//! tests that cover it. Every sentence comes from `blastr_model::wording`.

use std::ops::Range;

use blastr_model::{
    Analysis, Certainty, Change, EvidencePath, ImpactClass, ImpactItem, SymbolPath, wording,
};
use gpui_kit::component::button::Button;
use gpui_kit::component::menu::{DropdownMenu as _, PopupMenuItem};
use gpui_kit::component::{Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, Entity, HighlightStyle, IntoElement, ParentElement, Render, ScrollHandle,
    SharedString, Styled, StyledText, Subscription, Window, div, px,
};

use super::left::kind_label;
use super::shell::what_if_menu;
use super::ui;
use crate::state::outline::covering_tests;
use crate::theme::{self, Accent, TEXT_MICRO, TEXT_MONO, TEXT_SMALL, TEXT_UI};
use crate::workspace::Workspace;

/// How many covering tests are listed before "and N more".
const TESTS_SHOWN: usize = 6;

/// The evidence panel.
pub(crate) struct EvidencePanel {
    workspace: Entity<Workspace>,
    /// Which of the item's paths is shown.
    path: usize,
    all_tests: bool,
    shown: Option<SymbolPath>,
    scroll: ScrollHandle,
    _subscription: Subscription,
}

impl EvidencePanel {
    pub(crate) fn new(workspace: Entity<Workspace>, cx: &mut Context<Self>) -> EvidencePanel {
        let subscription = cx.observe(&workspace, |this, workspace, cx| {
            let symbol = workspace
                .read(cx)
                .session
                .selection()
                .map(|selection| selection.symbol.clone());
            if symbol != this.shown {
                this.shown = symbol;
                // The path from the change the graph shows comes first.
                let workspace = workspace.read(cx);
                let focus = workspace.focus_change();
                this.path = workspace
                    .selected_item()
                    .and_then(|item| {
                        item.paths
                            .iter()
                            .filter(|path| path.class == item.class)
                            .position(|path| Some(path.change as usize) == focus)
                    })
                    .unwrap_or(0);
                this.all_tests = false;
            }
            cx.notify();
        });
        EvidencePanel {
            workspace,
            path: 0,
            all_tests: false,
            shown: None,
            scroll: ScrollHandle::new(),
            _subscription: subscription,
        }
    }

    fn select(
        &self,
        symbol: SymbolPath,
        site: Option<blastr_model::Location>,
        cx: &mut Context<Self>,
    ) {
        self.workspace
            .update(cx, |workspace, cx| workspace.select(symbol, site, cx));
    }

    fn body(&self, cx: &mut Context<Self>) -> AnyElement {
        let workspace = self.workspace.read(cx);
        let session = &workspace.session;
        let Some(selection) = session.selection() else {
            return ui::empty_state(
                IconName::Network,
                Some(theme::BEHAVE),
                "Nothing selected",
                "Select a change, an affected symbol or a test to see why it is listed.",
            );
        };
        let Some(analysis) = session.analysis().cloned() else {
            return ui::empty_state(
                IconName::Loader,
                Some(theme::CHANGED),
                "Tracing impact",
                "The evidence arrives with the result.",
            );
        };
        let short = wording::short_name(&selection.symbol.path);
        let site = workspace
            .selection_location()
            .map(|(_, location)| wording::site_text(&location))
            .unwrap_or_default();
        let mut subtitle: Vec<String> = vec![kind_label(selection.symbol.kind).to_owned()];
        subtitle.extend(
            [short.module.clone(), site]
                .into_iter()
                .filter(|part| !part.is_empty()),
        );
        let item = workspace.selected_item().cloned();
        let change = workspace
            .selected_change()
            .map(|(_, change)| change.clone());
        let mut column = v_flex()
            .id("evidence")
            .size_full()
            .p_3()
            .gap_3()
            .overflow_y_scroll()
            .track_scroll(&self.scroll)
            .child(
                v_flex()
                    .gap_1()
                    .child(
                        div()
                            .truncate()
                            .font_family(theme::mono())
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_size(px(15.))
                            .child(short.name),
                    )
                    .child(
                        div()
                            .truncate()
                            .text_size(TEXT_SMALL)
                            .text_color(theme::FG_4)
                            .child(subtitle.join(" · ")),
                    ),
            );
        // A changed symbol is shown as the change, as the graph and the code
        // pane show it; that another change reaches it too is said below.
        column = match (&item, &change) {
            (_, Some(change)) => {
                let column = column.child(self.change_evidence(change, &analysis, cx));
                match &item {
                    Some(item) => column.child(also_reached(item, &analysis)),
                    None => column,
                }
            }
            (Some(item), None) => column.child(self.item_evidence(item, &analysis, cx)),
            (None, None) => column.child(
                div()
                    .text_size(TEXT_UI)
                    .text_color(theme::FG_2)
                    .child("Nothing in this comparison reaches it."),
            ),
        };
        column = column.child(limits(&analysis));
        if let Some(item) = &item {
            column = column.children(self.covering(&item.symbol, &analysis, cx));
        }
        column.into_any_element()
    }

    fn item_evidence(
        &self,
        item: &ImpactItem,
        analysis: &Analysis,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let accent = ui::class_accent(item.class);
        let paths: Vec<&EvidencePath> = item
            .paths
            .iter()
            .filter(|path| path.class == item.class)
            .collect();
        let Some(path) = paths
            .get(self.path.min(paths.len().saturating_sub(1)))
            .copied()
        else {
            return div().into_any_element();
        };
        let mut tags = wording::tag_names(&item.tags);
        if item.tags.changed {
            tags.push("edited with the change");
        }
        let mut column = v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(ui::pill_outlined(
                        wording::class_text(item.class).to_uppercase(),
                        accent,
                    ))
                    .child(soft_badge(format!(
                        "CONFIDENCE {}",
                        item.confidence.as_str().to_uppercase()
                    ))),
            )
            .when(!tags.is_empty(), |this| {
                this.child(h_flex().gap_1().children(tags.into_iter().map(ui::tag)))
            })
            .child(sentence(&wording::explanation(item, path, analysis)));
        if paths.len() > 1 {
            let picker = paths.iter().enumerate().map(|(index, entry)| {
                let selected = index == self.path.min(paths.len() - 1);
                let label = analysis
                    .diff
                    .changes
                    .get(entry.change as usize)
                    .map_or("this path".to_owned(), |change| {
                        format!("from {}", wording::short_name(&change.symbol.path).name)
                    });
                div()
                    .id(("evidence-path", index))
                    .flex_1()
                    .min_w_0()
                    .h(px(24.))
                    .flex()
                    .items_center()
                    .px_2()
                    .bg(if selected {
                        theme::BG_ACTIVE
                    } else {
                        theme::BG_INPUT
                    })
                    .text_size(px(11.))
                    .text_color(if selected { theme::FG } else { theme::FG_3 })
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.path = index;
                        cx.notify();
                    }))
                    .child(div().min_w_0().truncate().child(label))
            });
            column = column.child(
                h_flex()
                    .rounded(theme::RADIUS_MD)
                    .border_1()
                    .border_color(theme::LINE)
                    .overflow_hidden()
                    .children(picker),
            );
        }
        let mut chain = v_flex();
        let change = analysis.diff.changes.get(path.change as usize);
        if let Some(change) = change {
            let symbol = change.symbol.clone();
            chain = chain.child(
                step_row(
                    &change.symbol.path,
                    &wording::change_label(change),
                    &wording::location_text(change),
                    theme::CHANGED,
                    ui::Marker::Changed,
                    (false, !path.steps.is_empty()),
                )
                .id("evidence-change")
                .on_click(cx.listener(move |this, _, _, cx| this.select(symbol.clone(), None, cx))),
            );
        }
        let count = path.steps.len();
        for (index, step) in path.steps.iter().enumerate() {
            let text = wording::step_text(step, path, analysis, index == 0);
            let (symbol, site) = (step.from.clone(), step.site.clone());
            let marker = if step.certainty == Certainty::Candidate {
                ui::Marker::Candidate
            } else {
                ui::Marker::Dot
            };
            chain = chain.child(
                step_row(
                    &step.from.path,
                    &text,
                    &wording::site_text(&step.site),
                    accent,
                    marker,
                    (change.is_some() || index > 0, index + 1 < count),
                )
                .id(("evidence-step", index))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select(symbol.clone(), Some(site.clone()), cx)
                })),
            );
        }
        let note = if paths.len() > 1 {
            format!(
                "{} of {} paths",
                self.path.min(paths.len() - 1) + 1,
                paths.len()
            )
        } else {
            String::new()
        };
        column
            .child(section("WHY IT IS LISTED", &note))
            .child(chain)
            .into_any_element()
    }

    fn change_evidence(
        &self,
        change: &Change,
        analysis: &Analysis,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let index = analysis
            .diff
            .changes
            .iter()
            .position(|known| known.symbol == change.symbol);
        let reaches = |item: &ImpactItem| {
            index.is_some_and(|index| item.paths.iter().any(|path| path.change as usize == index))
        };
        let editing = analysis
            .items
            .iter()
            .filter(|item| {
                reaches(item) && item.class == ImpactClass::Adaptation && !item.tags.changed
            })
            .count();
        let behaving = analysis
            .items
            .iter()
            .filter(|item| reaches(item) && item.class == ImpactClass::Behaviour)
            .count();
        let tests = analysis
            .tests
            .selected
            .iter()
            .filter(|test| {
                test.symbol == change.symbol
                    || analysis
                        .items
                        .iter()
                        .any(|item| item.symbol == test.symbol && reaches(item))
            })
            .count();
        let direct: Vec<(SymbolPath, String)> = analysis
            .items
            .iter()
            .filter_map(|item| {
                let path = item
                    .paths
                    .iter()
                    .find(|path| Some(path.change as usize) == index && path.steps.len() == 1)?;
                Some((
                    item.symbol.clone(),
                    wording::reason_text(path.reason, change),
                ))
            })
            .collect();
        let rows = direct
            .iter()
            .take(4)
            .enumerate()
            .map(|(position, (symbol, reason))| {
                let target = symbol.clone();
                h_flex()
                    .id(("reach", position))
                    .h(px(24.))
                    .gap(ui::GUTTER_GAP)
                    .items_center()
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::BG_HOVER))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.select(target.clone(), None, cx)),
                    )
                    .child(ui::marker(theme::BEHAVE.solid, ui::Marker::Dot))
                    .child(
                        div()
                            .flex_none()
                            .font_family(theme::mono())
                            .text_size(px(11.5))
                            .child(wording::short_name(&symbol.path).name),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_size(px(11.5))
                            .text_color(theme::FG_3)
                            .child(reason.clone()),
                    )
            });
        v_flex()
            .gap_2()
            .child(
                h_flex()
                    .gap_2()
                    .child(ui::pill_outlined("CHANGED", theme::CHANGED))
                    .child(soft_badge(format!(
                        "KIND {}",
                        wording::change_label(change).to_uppercase()
                    ))),
            )
            .child(
                div()
                    .text_size(TEXT_UI)
                    .text_color(theme::FG_2)
                    .child(wording::change_note(change)),
            )
            .child(
                h_flex()
                    .gap_2()
                    .child(stat(editing, "need editing", theme::EDIT))
                    .child(stat(behaving, "may behave differently", theme::BEHAVE))
                    .child(stat(tests, "tests reach it", theme::TEST)),
            )
            .when(!direct.is_empty(), |this| {
                this.child(section("REACHES DIRECTLY", &direct.len().to_string()))
                    .child(v_flex().children(rows))
                    .when(direct.len() > 4, |this| {
                        this.child(
                            div()
                                .text_size(TEXT_MICRO)
                                .text_color(theme::FG_4)
                                .child(format!("and {} more", direct.len() - 4)),
                        )
                    })
            })
            .into_any_element()
    }

    fn covering(
        &self,
        symbol: &SymbolPath,
        analysis: &Analysis,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let tests = covering_tests(analysis, symbol);
        if tests.is_empty() {
            return None;
        }
        let shown = if self.all_tests {
            tests.len()
        } else {
            tests.len().min(TESTS_SHOWN)
        };
        let rows = tests.iter().take(shown).map(|index| {
            let test = &analysis.tests.selected[*index];
            let (symbol, site) = (test.symbol.clone(), test.location.clone());
            h_flex()
                .id(("covering", *index))
                .h(px(24.))
                .gap(ui::GUTTER_GAP)
                .items_center()
                .cursor_pointer()
                .hover(|style| style.bg(theme::BG_HOVER))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select(symbol.clone(), Some(site.clone()), cx)
                }))
                .child(ui::gutter(
                    Icon::new(IconName::Check)
                        .size_3()
                        .text_color(theme::TEST.solid),
                ))
                .child(
                    div()
                        .truncate()
                        .font_family(theme::mono())
                        .text_size(px(11.5))
                        .text_color(theme::FG_2)
                        .child(wording::short_name(&test.symbol.path).name),
                )
        });
        let hidden = tests.len() - shown;
        Some(
            v_flex()
                .child(section(
                    "COVERED BY",
                    &format!(
                        "{} of {} selected tests",
                        tests.len(),
                        analysis.tests.selected.len()
                    ),
                ))
                .children(rows)
                .when(hidden > 0 || self.all_tests, |this| {
                    this.child(
                        div()
                            .id("more-tests")
                            .h(px(22.))
                            .text_size(TEXT_MICRO)
                            .text_color(theme::FG_4)
                            .cursor_pointer()
                            .hover(|style| style.text_color(theme::FG))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.all_tests = !this.all_tests;
                                cx.notify();
                            }))
                            .child(if hidden > 0 {
                                format!("and {hidden} more")
                            } else {
                                "Show fewer".to_owned()
                            }),
                    )
                })
                .into_any_element(),
        )
    }

    fn footer(&self, cx: &mut Context<Self>) -> Option<AnyElement> {
        let workspace = self.workspace.read(cx);
        workspace.session.selection()?;
        workspace.session.analysis()?;
        let what_if = self.workspace.clone();
        Some(
            h_flex()
                .flex_none()
                .gap_2()
                .p_2()
                .items_center()
                .border_t_1()
                .border_color(theme::LINE)
                .child(
                    Button::new("evidence-open-editor")
                        .small()
                        .outline()
                        .label("Open in editor")
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.workspace
                                .update(cx, |workspace, cx| workspace.open_selection(cx));
                        })),
                )
                .child(
                    div()
                        .text_size(TEXT_MICRO)
                        .text_color(theme::FG_4)
                        .child("Ctrl E"),
                )
                .child(div().flex_1())
                .child(
                    Button::new("what-if")
                        .small()
                        .outline()
                        .label("What if…")
                        .dropdown_caret(true)
                        .tooltip("Trace what a change of this symbol would reach")
                        .dropdown_menu(move |mut menu, _, _| {
                            for (assume, label) in what_if_menu() {
                                let workspace = what_if.clone();
                                menu = menu.item(PopupMenuItem::new(label).on_click(
                                    move |_, _, cx| {
                                        workspace.update(cx, |workspace, cx| {
                                            workspace.begin_what_if(assume, cx)
                                        });
                                    },
                                ));
                            }
                            menu
                        }),
                )
                .into_any_element(),
        )
    }
}

impl Render for EvidencePanel {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        v_flex()
            .size_full()
            .min_w_0()
            .bg(theme::BG_PANEL)
            .child(
                h_flex()
                    .h(px(32.))
                    .flex_none()
                    .px_3()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(theme::LINE)
                    .child(
                        div()
                            .text_size(TEXT_MICRO)
                            .text_color(theme::FG_2)
                            .child("EVIDENCE"),
                    )
                    .child(div().flex_1())
                    .child(
                        ui::icon_button("copy-path", IconName::Copy, "Copy the file path")
                            .on_click(cx.listener(|this, _, _, cx| {
                                let location = this.workspace.read(cx).selection_location();
                                if let Some((_, location)) = location {
                                    let text = location.file.as_str().to_owned();
                                    this.workspace
                                        .update(cx, |workspace, cx| workspace.copy(text, cx));
                                }
                            })),
                    ),
            )
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(self.body(cx))
                    .child(ui::scrollbar("evidence", &self.scroll)),
            )
            .children(self.footer(cx))
    }
}

/// A sentence with its symbol names set in the mono face.
fn sentence(sentence: &wording::Sentence) -> AnyElement {
    let names: Vec<Range<usize>> = sentence.names.clone();
    let colour = HighlightStyle {
        color: Some(theme::FG.hsla()),
        ..HighlightStyle::default()
    };
    div()
        .text_size(TEXT_UI)
        .text_color(theme::FG_2)
        .child(
            StyledText::new(sentence.text.clone())
                .with_highlights(names.iter().map(|range| (range.clone(), colour)))
                .with_font_family_overrides(names.into_iter().map(|range| (range, theme::mono()))),
        )
        .into_any_element()
}

/// That a changed symbol is also reached, and how, under its change.
fn also_reached(item: &ImpactItem, analysis: &Analysis) -> AnyElement {
    let mut column =
        v_flex()
            .gap_1()
            .child(section("ALSO REACHED", ""))
            .child(h_flex().gap_2().child(ui::pill_outlined(
                wording::class_text(item.class).to_uppercase(),
                ui::class_accent(item.class),
            )));
    if let Some(path) = item.paths.iter().find(|path| path.class == item.class) {
        column = column.child(sentence(&wording::explanation(item, path, analysis)));
    }
    column.into_any_element()
}

/// What limits the result, from the wording of the analysis.
fn limits(analysis: &Analysis) -> AnyElement {
    let parts = wording::limit_parts(analysis);
    let line = |icon: IconName, colour: theme::Tone, text: String| {
        h_flex()
            .gap(ui::GUTTER_GAP)
            .items_start()
            .text_size(TEXT_SMALL)
            .text_color(theme::FG_3)
            .child(ui::gutter(Icon::new(icon).size_3().text_color(colour)))
            .child(div().flex_1().min_w_0().child(text))
    };
    let mut column = v_flex().gap_1().child(section("LIMITS", ""));
    if parts.is_empty() {
        column = column.child(line(
            IconName::CircleCheck,
            theme::TEST.solid,
            "Nothing limits this result.".to_owned(),
        ));
    }
    column
        .children(
            parts
                .into_iter()
                .map(|part| line(IconName::Info, theme::FG_4, part)),
        )
        .into_any_element()
}

fn soft_badge(label: String) -> AnyElement {
    div()
        .flex()
        .items_center()
        .h(ui::PILL)
        .px(ui::PILL_PAD)
        .rounded(theme::RADIUS_SM)
        .border_1()
        .border_color(theme::LINE_STRONG)
        .text_size(px(10.))
        .text_color(theme::FG_2)
        .child(label)
        .into_any_element()
}

fn stat(count: usize, label: &'static str, accent: Accent) -> AnyElement {
    v_flex()
        .flex_1()
        .min_w_0()
        .p_2()
        .rounded(theme::RADIUS_MD)
        .border_1()
        .border_color(theme::LINE)
        .bg(theme::BG_RAISED)
        .child(
            div()
                .font_family(theme::mono())
                .text_size(px(16.))
                .text_color(accent.fg)
                .child(count.to_string()),
        )
        .child(
            div()
                .text_size(TEXT_MICRO)
                .text_color(theme::FG_3)
                .child(label),
        )
        .into_any_element()
}

fn section(label: &'static str, note: &str) -> AnyElement {
    h_flex()
        .mt_2()
        .h(px(18.))
        .items_center()
        .text_size(TEXT_MICRO)
        .text_color(theme::FG_3)
        .child(label)
        .child(div().flex_1())
        .child(
            div()
                .text_color(theme::FG_4)
                .child(SharedString::from(note.to_owned())),
        )
        .into_any_element()
}

/// One link of the chain: the marker on a line that joins it to the links
/// above and below, the symbol, what the step does and where.
fn step_row(
    path: &str,
    what: &str,
    site: &str,
    accent: Accent,
    marker: ui::Marker,
    (above, below): (bool, bool),
) -> gpui_kit::Div {
    let name = wording::short_name(path).name;
    let file = site.rsplit(['/', '\\']).next().unwrap_or(site).to_owned();
    let rule = || {
        div()
            .absolute()
            .left(px(6.25))
            .w(px(1.5))
            .bg(theme::LINE_STRONG)
    };
    h_flex()
        .relative()
        .w_full()
        .min_w_0()
        .h(px(40.))
        .gap(ui::GUTTER_GAP)
        .items_start()
        .pt(px(4.))
        .cursor_pointer()
        .hover(|style| style.bg(theme::BG_HOVER))
        .when(above, |this| this.child(rule().top_0().h(px(9.))))
        .child(ui::marker(accent.solid, marker))
        .when(below, |this| this.child(rule().top(px(17.)).bottom_0()))
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .child(
                    h_flex()
                        .h(ui::ROW_LINE)
                        .gap_2()
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .truncate()
                                .font_family(theme::mono())
                                .text_size(TEXT_MONO)
                                .child(name),
                        )
                        .child(
                            div()
                                .flex_none()
                                .font_family(theme::mono())
                                .text_size(TEXT_MICRO)
                                .text_color(theme::FG_4)
                                .child(file),
                        ),
                )
                .child(
                    div()
                        .h(ui::SUB_LINE)
                        .line_height(ui::SUB_LINE)
                        .truncate()
                        .text_size(TEXT_SMALL)
                        .text_color(theme::FG_3)
                        .child(what.to_owned()),
                ),
        )
}
