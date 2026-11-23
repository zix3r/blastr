//! The evidence panel: why the selected symbol is listed, and what limits the
//! answer.
//!
//! The panel is the only place with room for the long form: the class, the
//! confidence, the score, every evidence path as a clickable chain of steps,
//! the limits in plain words and the tests that cover the item.

use gpui_kit::prelude::*;

use std::ops::Range;

use blastr_model::wording;
use blastr_model::{
    Analysis, Change, ChangeDetail, ChangeKind, Confidence, Delta, ImpactClass, ImpactItem,
};
use gpui_kit::component::button::Button;
use gpui_kit::component::menu::ContextMenuExt as _;
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::{Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, Context, HighlightStyle, IntoElement, ParentElement, SharedString, Styled,
    StyledText, div, px,
};

use super::shell::Shell;
use super::ui;
use crate::state::outline::covering_tests;
use crate::theme::{self, TEXT_MICRO, TEXT_MONO, TEXT_SMALL, TEXT_UI};

/// How many tests the panel lists before it offers the rest.
const TESTS_BEFORE_MORE: usize = 6;

/// How many direct targets the panel lists before it offers the rest.
const DIRECT_BEFORE_MORE: usize = 4;

/// The height of one step of an evidence chain.
const STEP_H: f32 = 40.0;
/// Where a step's marker starts, measured from the top of the step.
const MARKER_TOP: f32 = 9.0;
/// Where a step's marker ends, measured from the top of the step.
const MARKER_END: f32 = 17.0;
/// The left edge of the line that joins the markers.
const LINE_X: f32 = 6.25;
/// The width of that line. Its centre is the centre of the gutter.
const LINE_W: f32 = 1.5;

impl Shell {
    /// The right column.
    pub(crate) fn evidence_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let body = self.evidence_body(cx);
        v_flex()
            .size_full()
            .min_w_0()
            .bg(theme::BG_PANEL)
            .border_l_1()
            .border_color(theme::LINE)
            .child(
                h_flex()
                    .h(px(30.))
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
                        ui::icon_button("copy-path", IconName::Copy, "Copy path").on_click(
                            cx.listener(|this, _, _, cx| {
                                if let Some((_, location)) = this
                                    .session
                                    .selection()
                                    .and_then(|selection| this.selection_location(selection))
                                {
                                    this.copy(location.file.as_str().to_owned(), cx);
                                }
                            }),
                        ),
                    )
                    .child(
                        ui::icon_button(
                            "open-editor-top",
                            IconName::ExternalLink,
                            "Open in editor  Ctrl E",
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.open_in_editor(cx))),
                    ),
            )
            .child(div().flex_1().min_h_0().child(body))
            .into_any_element()
    }

    fn evidence_body(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.session.repo.is_none() {
            return ui::empty_state(
                IconName::FileText,
                None,
                "Nothing selected",
                "Open a repository, choose a comparison and select a symbol.",
            );
        }
        let Some(selection) = self.session.selection().cloned() else {
            if self.session.shown().failure().is_some() {
                return v_flex()
                    .size_full()
                    .min_w_0()
                    .min_h_0()
                    .p_3()
                    .child(ui::failure_line())
                    .into_any_element();
            }
            return ui::empty_state(
                IconName::Network,
                Some(theme::BEHAVE),
                "Nothing selected",
                "Select a change, an affected symbol or a test to see why it is listed.",
            );
        };
        let Some(analysis) = self.analysis() else {
            if self.session.shown().failure().is_some() {
                return v_flex()
                    .size_full()
                    .min_w_0()
                    .min_h_0()
                    .p_3()
                    .child(ui::failure_line())
                    .into_any_element();
            }
            return ui::empty_state(
                IconName::Loader,
                Some(theme::CHANGED),
                "Tracing impact",
                "Evidence arrives with the result.",
            );
        };
        let split = wording::short_name(&selection.symbol.path);
        let kind = super::left::kind_label(selection.symbol.kind);
        let crate_name = selection.symbol.crate_name().to_owned();
        let site = self
            .selection_location(&selection)
            .map(|(_, location)| wording::site_text(&location))
            .unwrap_or_default();

        let mut scroll = v_flex()
            .id("evidence")
            .size_full()
            .p_3()
            .gap_3()
            .track_scroll(&self.evidence_scroll)
            .overflow_y_scroll();
        scroll = scroll.child(
            v_flex()
                .gap_1()
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .font_family(theme::mono())
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_size(px(15.))
                        .text_color(theme::FG)
                        .child(split.name),
                )
                .child(
                    div()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_size(TEXT_SMALL)
                        .text_color(theme::FG_4)
                        .child(subtitle(kind, &split.module, &crate_name, &site)),
                ),
        );

        let item = self.selected_item().cloned();
        let change = self.selected_change().cloned();
        match (&item, &change) {
            (Some(item), _) => {
                scroll = scroll.child(self.item_evidence(item.clone(), cx));
            }
            (None, Some(change)) => {
                scroll = scroll.child(self.change_evidence(change.clone(), cx));
            }
            (None, None) => {
                scroll = scroll.child(
                    div()
                        .text_size(TEXT_UI)
                        .text_color(theme::FG_2)
                        .child("This symbol is not affected by the current comparison."),
                );
            }
        }

        scroll = scroll.child(self.limits_section(cx));

        if let Some(item) = &item {
            let tests = covering_tests(analysis, &item.symbol);
            if !tests.is_empty() {
                let shown = if self.evidence_more_tests {
                    tests.len()
                } else {
                    tests.len().min(TESTS_BEFORE_MORE)
                };
                let mut list = v_flex().gap_0();
                for index in tests.iter().take(shown) {
                    if let Some(test) = analysis.tests.selected.get(*index) {
                        let name = wording::short_name(&test.symbol.path);
                        list = list.child(
                            h_flex()
                                .h(px(24.))
                                .gap(ui::GUTTER_GAP)
                                .items_center()
                                .line_height(ui::ROW_LINE)
                                .child(ui::gutter(
                                    Icon::new(IconName::Check)
                                        .size_3()
                                        .text_color(theme::TEST.solid),
                                ))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .overflow_hidden()
                                        .whitespace_nowrap()
                                        .text_ellipsis()
                                        .font_family(theme::mono())
                                        .text_size(px(11.5))
                                        .text_color(theme::FG_2)
                                        .child(name.name),
                                ),
                        );
                    }
                }
                let hidden = tests.len().saturating_sub(shown);
                if hidden > 0 || self.evidence_more_tests {
                    let expand = !self.evidence_more_tests && hidden > 0;
                    let label = if expand {
                        format!("and {hidden} more")
                    } else {
                        "Show fewer".to_owned()
                    };
                    list = list.child(
                        div()
                            .id("evidence-more-tests")
                            .h(px(22.))
                            .flex()
                            .items_center()
                            .text_size(TEXT_MICRO)
                            .text_color(theme::FG_4)
                            .cursor_pointer()
                            .hover(|style| style.text_color(theme::FG))
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.evidence_more_tests = !this.evidence_more_tests;
                                cx.notify();
                            }))
                            .child(label),
                    );
                }
                scroll = scroll
                    .child(section_head(
                        "COVERED BY",
                        &format!(
                            "{} of {} selected tests",
                            tests.len(),
                            analysis.tests.selected.len()
                        ),
                    ))
                    .child(list);
            }
        }

        let mut footer = h_flex()
            .flex_shrink_0()
            .gap_2()
            .p_2()
            .items_center()
            .bg(theme::BG_PANEL)
            .border_t_1()
            .border_color(theme::LINE)
            .child(
                Button::new("open-editor")
                    .small()
                    .outline()
                    .label("Open in editor")
                    .on_click(cx.listener(|this, _, _, cx| this.open_in_editor(cx))),
            )
            .child(
                div()
                    .text_size(TEXT_MICRO)
                    .text_color(theme::FG_4)
                    .child("Ctrl E"),
            );
        if item.is_some() {
            footer = footer.child(div().flex_1()).child(
                Button::new("blast-radius")
                    .small()
                    .outline()
                    .label("Show blast radius")
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.begin_what_if(blastr_model::Assume::Body, cx);
                    }))
                    .context_menu(|menu, _, _| {
                        let mut menu = menu;
                        for (label, action) in what_if_actions() {
                            menu = menu.menu(label, action);
                        }
                        menu
                    }),
            );
        }

        v_flex()
            .size_full()
            .child(
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .child(scroll)
                    .child(ui::scrollbar(
                        "evidence",
                        &self.evidence_scroll,
                        ScrollbarAxis::Vertical,
                    )),
            )
            .child(footer)
            .into_any_element()
    }

    /// The badges, facts and paths of an affected item.
    fn item_evidence(&self, item: ImpactItem, cx: &mut Context<Self>) -> AnyElement {
        let accent = ui::class_accent(item.class);
        let class = wording::class_text(item.class);
        let paths: Vec<&blastr_model::EvidencePath> = item
            .paths
            .iter()
            .filter(|path| path.class == item.class)
            .collect();
        let path = paths
            .get(self.evidence_path.min(paths.len().saturating_sub(1)))
            .copied()
            .or_else(|| wording::best_path(&item));
        let Some(path) = path else {
            return div().into_any_element();
        };
        let analysis = self
            .analysis()
            .expect("the panel is only shown with a result");
        let summary = wording::path_summary(&item, analysis);

        let mut column = v_flex().gap_2();
        column = column.child(
            h_flex()
                .gap_2()
                .items_center()
                .child(badge(class, accent))
                .child(soft_badge(
                    format!("CONFIDENCE {}", item.confidence.as_str().to_uppercase()),
                    theme::FG_2,
                )),
        );
        column = column.child(
            h_flex()
                .gap_2()
                .items_center()
                .text_size(TEXT_SMALL)
                .text_color(theme::FG_3)
                .child(
                    div()
                        .w(px(44.))
                        .h(px(4.))
                        .flex_shrink_0()
                        .rounded(px(2.))
                        .bg(theme::BG_RAISED_2)
                        .child(
                            div()
                                .w(px(44. * item.score.clamp(0.0, 1.0)))
                                .h_full()
                                .rounded(px(2.))
                                .bg(accent.solid),
                        ),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .font_family(theme::mono())
                        .text_size(px(11.5))
                        .text_color(theme::FG_2)
                        .child(format!("{:.2}", item.score)),
                )
                .child("·")
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .child(summary),
                ),
        );

        let tags = wording::tag_names(&item.tags);
        let tags = if item.tags.changed {
            let mut all = tags;
            all.push("edited with the change");
            all
        } else {
            tags
        };
        if !tags.is_empty() {
            column = column.child(h_flex().gap_1().children(tags.into_iter().map(ui::tag)));
        }

        column = column.child(explanation_view(&explanation(&item, path, analysis)));

        if item.paths.len() > 1 {
            let mut picker = h_flex()
                .gap_0()
                .rounded(px(6.))
                .border_1()
                .border_color(theme::LINE)
                .overflow_hidden();
            for (index, entry) in item.paths.iter().enumerate() {
                let selected = index == self.evidence_path.min(item.paths.len() - 1);
                let label = path_tab_label(analysis, entry);
                picker = picker.child(
                    div()
                        .id(gpui_kit::ElementId::NamedInteger(
                            "ev-path".into(),
                            u64::try_from(index).unwrap_or(u64::MAX),
                        ))
                        .flex_1()
                        .min_w_0()
                        .h(px(26.))
                        .px_1()
                        .flex()
                        .items_center()
                        .justify_center()
                        .overflow_hidden()
                        .bg(if selected {
                            theme::BG_ACTIVE
                        } else {
                            theme::BG_INPUT
                        })
                        .text_size(px(11.))
                        .text_color(if selected { theme::FG } else { theme::FG_3 })
                        .text_ellipsis()
                        .cursor_pointer()
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.evidence_path = index;
                            cx.notify();
                        }))
                        .child(label),
                );
            }
            column = column.child(picker);
        }

        // The chain: the change first, then each reference until the item. The
        // line that joins the steps stops at the ends of the chain.
        let mut chain = v_flex().gap_0();
        let changes = self.changes();
        let changed = changes.and_then(|diff| diff.changes.get(path.change as usize));
        if let Some(change) = changed {
            chain = chain.child(step_row(
                &change.symbol.path,
                &wording::change_label(change),
                &wording::location_text(change),
                theme::CHANGED,
                ui::Marker::Changed,
                (false, !path.steps.is_empty()),
            ));
        }
        let steps = path.steps.len();
        for (index, step) in path.steps.iter().enumerate() {
            let first = index == 0;
            let text = wording::step_text(step, path, analysis, first);
            let site = wording::site_text(&step.site);
            let symbol = step.from.clone();
            let location = step.site.clone();
            let joined = (changed.is_some() || !first, index + 1 < steps);
            chain = chain.child(
                div()
                    .id(gpui_kit::ElementId::NamedInteger(
                        "ev-step".into(),
                        u64::try_from(index).unwrap_or(u64::MAX),
                    ))
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::BG_HOVER))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_symbol(symbol.clone(), Some(location.clone()), cx);
                    }))
                    .child(step_row(
                        &step.from.path,
                        &text,
                        &site,
                        theme::BEHAVE,
                        if step.certainty == blastr_model::Certainty::Candidate {
                            ui::Marker::Candidate
                        } else {
                            ui::Marker::Dot
                        },
                        joined,
                    )),
            );
        }
        let shown_paths = paths.len();
        let mut note = String::new();
        if shown_paths > 1 {
            let current = self.evidence_path.min(paths.len().saturating_sub(1));
            note = format!("{} of {shown_paths} paths", current + 1);
        }
        column = column
            .child(section_head("WHY IT IS LISTED", &note))
            .child(chain);
        column.into_any_element()
    }

    /// What a change is, what it reaches and how much depends on it.
    fn change_evidence(&self, change: Change, cx: &mut Context<Self>) -> AnyElement {
        let Some(analysis) = self.analysis() else {
            return div().into_any_element();
        };
        let accent = theme::CHANGED;
        let counts = change_counts(analysis, &change);
        let mut column = v_flex().gap_2();
        column = column.child(
            h_flex()
                .gap_2()
                .items_center()
                .child(badge("CHANGED", accent))
                .child(soft_badge(
                    format!("KIND {}", wording::change_label(&change).to_uppercase()),
                    theme::FG_2,
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .text_size(TEXT_SMALL)
                        .text_color(theme::FG_4)
                        .child(wording::location_text(&change)),
                ),
        );
        column = column.child(
            div()
                .text_size(TEXT_UI)
                .text_color(theme::FG_2)
                .child(change_note(&change)),
        );

        let tiles = [
            ("need editing", counts.needs_editing, theme::EDIT),
            ("may behave differently", counts.may_behave, theme::BEHAVE),
            ("tests reach it", counts.tests, theme::TEST),
        ];
        column = column.child(
            h_flex().gap_2().children(
                tiles
                    .into_iter()
                    .map(|(label, count, accent)| stat_tile(count, label, accent)),
            ),
        );

        let direct = direct_targets(analysis, &change);
        if !direct.is_empty() {
            let shown = direct.len().min(DIRECT_BEFORE_MORE);
            let mut list = v_flex().gap_0();
            for (symbol, reason) in direct.iter().take(shown) {
                let name = wording::short_name(&symbol.path).name;
                let symbol = symbol.clone();
                list = list.child(
                    h_flex()
                        .id(gpui_kit::ElementId::Name(
                            format!("reach-{}", symbol.path).into(),
                        ))
                        .h(px(24.))
                        .gap(ui::GUTTER_GAP)
                        .items_center()
                        .line_height(ui::ROW_LINE)
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::BG_HOVER))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.select_symbol(symbol.clone(), None, cx);
                        }))
                        .child(ui::marker(theme::BEHAVE.solid, ui::Marker::Dot))
                        .child(
                            div()
                                .flex_shrink_0()
                                .font_family(theme::mono())
                                .text_size(px(11.5))
                                .text_color(theme::FG)
                                .child(name),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .overflow_hidden()
                                .text_ellipsis()
                                .text_size(px(11.5))
                                .text_color(theme::FG_3)
                                .child(reason.clone()),
                        ),
                );
            }
            if direct.len() > shown {
                list = list.child(
                    div()
                        .h(px(20.))
                        .text_size(TEXT_MICRO)
                        .text_color(theme::FG_4)
                        .child(format!("and {} more", direct.len() - shown)),
                );
            }
            column = column
                .child(section_head("REACHES DIRECTLY", &direct.len().to_string()))
                .child(list);
        }
        column.into_any_element()
    }

    /// The limits of the shown result, in plain words.
    fn limits_section(&self, cx: &mut Context<Self>) -> AnyElement {
        let Some(analysis) = self.analysis() else {
            return div().into_any_element();
        };
        let parts = wording::limit_parts(analysis);
        let mut column = v_flex().gap_1().child(section_head("LIMITS", ""));
        if parts.is_empty() {
            column = column.child(
                h_flex()
                    .gap_2()
                    .items_center()
                    .text_size(TEXT_SMALL)
                    .text_color(theme::FG_3)
                    .child(
                        Icon::new(IconName::CircleCheck)
                            .size_3()
                            .text_color(theme::TEST.solid),
                    )
                    .child("Nothing limits this result."),
            );
        } else {
            for part in parts {
                column = column.child(
                    h_flex()
                        .gap(ui::GUTTER_GAP)
                        .items_start()
                        .line_height(ui::ROW_LINE)
                        .text_size(TEXT_SMALL)
                        .text_color(theme::FG_3)
                        .child(ui::gutter(
                            Icon::new(IconName::Info).size_3().text_color(theme::FG_4),
                        ))
                        .child(div().flex_1().min_w_0().child(part)),
                );
            }
        }
        let _ = cx;
        column.into_any_element()
    }
}

/// The second line of the panel's title: what the symbol is and where it is.
///
/// The crate is left out when the module already starts with it, so that the
/// line reads as one address rather than two.
fn subtitle(kind: &str, module: &str, crate_name: &str, site: &str) -> String {
    let mut parts: Vec<&str> = vec![kind];
    if !module.is_empty() {
        parts.push(module);
    }
    let module_starts_with_crate = module.split("::").next() == Some(crate_name);
    if !crate_name.is_empty() && !module_starts_with_crate {
        parts.push(crate_name);
    }
    if !site.is_empty() {
        parts.push(site);
    }
    parts.retain(|part| !part.is_empty());
    parts.join(" · ")
}

/// What counts depend on one change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
struct ChangeCounts {
    /// Items that still need editing.
    needs_editing: u32,
    /// Items that may behave differently.
    may_behave: u32,
    /// Selected tests the change reaches.
    tests: u32,
}

/// Counts the items and tests the change at `change` leads to.
fn change_counts(analysis: &Analysis, change: &Change) -> ChangeCounts {
    let Some(index) = analysis
        .diff
        .changes
        .iter()
        .position(|candidate| candidate.symbol == change.symbol)
    else {
        return ChangeCounts::default();
    };
    let index = u32::try_from(index).unwrap_or(u32::MAX);
    let mut counts = ChangeCounts::default();
    for item in &analysis.items {
        if !item.paths.iter().any(|path| path.change == index) {
            continue;
        }
        match (item.class, item.tags.changed) {
            (ImpactClass::Adaptation, false) => counts.needs_editing += 1,
            (ImpactClass::Adaptation, true) => {}
            (ImpactClass::Behaviour, _) => counts.may_behave += 1,
        }
    }
    for test in &analysis.tests.selected {
        let reached = analysis
            .items
            .iter()
            .filter(|item| item.symbol == test.symbol)
            .flat_map(|item| item.paths.iter())
            .any(|path| path.change == index);
        if reached || test.symbol == change.symbol {
            counts.tests += 1;
        }
    }
    counts
}

/// The items the change reaches in one step, with the reason.
fn direct_targets(analysis: &Analysis, change: &Change) -> Vec<(blastr_model::SymbolPath, String)> {
    let Some(index) = analysis
        .diff
        .changes
        .iter()
        .position(|candidate| candidate.symbol == change.symbol)
    else {
        return Vec::new();
    };
    let index = u32::try_from(index).unwrap_or(u32::MAX);
    let mut targets: Vec<(blastr_model::SymbolPath, String)> = Vec::new();
    for item in &analysis.items {
        let direct = item
            .paths
            .iter()
            .find(|path| path.change == index && path.steps.len() == 1);
        if let Some(path) = direct
            && !targets.iter().any(|(symbol, _)| symbol == &item.symbol)
        {
            targets.push((
                item.symbol.clone(),
                wording::reason_text(path.reason, change),
            ));
        }
    }
    targets
}

/// What a change did to the symbol, in one sentence.
pub(crate) fn change_note(change: &Change) -> String {
    let member = match &change.detail {
        ChangeDetail::Member(delta) => Some(match delta {
            Delta::Added => "a member was added",
            Delta::Removed => "a member was removed",
            Delta::Modified => "a member changed",
        }),
        ChangeDetail::DefaultBody => Some("only the default body changed"),
        ChangeDetail::Renamed { .. } => Some("it was renamed"),
        ChangeDetail::Visibility { .. } => Some("its visibility changed"),
        ChangeDetail::Retyped => Some("its declared type changed"),
        ChangeDetail::Manifest(text) => return text.clone(),
        ChangeDetail::None => None,
    };
    if let Some(member) = member {
        return capitalised(&format!("{}: {member}.", change.kind.as_str()));
    }
    match change.kind {
        ChangeKind::Body => "The body changed; the signature did not.".to_owned(),
        ChangeKind::Signature => "The signature changed; the body did not.".to_owned(),
        ChangeKind::Visibility => "How visible it is changed; nothing else did.".to_owned(),
        ChangeKind::Added => "It is new in this revision.".to_owned(),
        ChangeKind::Removed => "It is gone from this revision.".to_owned(),
        ChangeKind::Renamed => "It has a new name.".to_owned(),
        ChangeKind::NonSemantic => "Only formatting and comments changed.".to_owned(),
        ChangeKind::Field | ChangeKind::Variant | ChangeKind::TraitItem | ChangeKind::Impl => {
            "A member of it changed.".to_owned()
        }
        ChangeKind::ConstValue => "Its value changed; its type did not.".to_owned(),
        ChangeKind::TypeAlias => "What the alias names changed.".to_owned(),
        ChangeKind::MacroDef => "The expansion it produces changed.".to_owned(),
        ChangeKind::Attribute => "An attribute on it changed.".to_owned(),
        ChangeKind::Manifest => "The manifest changed.".to_owned(),
    }
}

/// `text` with its first letter in upper case.
fn capitalised(text: &str) -> String {
    let mut characters = text.chars();
    match characters.next() {
        Some(first) => first.to_uppercase().chain(characters).collect(),
        None => String::new(),
    }
}

/// The label of one path tab: where the path starts from.
fn path_tab_label(analysis: &Analysis, path: &blastr_model::EvidencePath) -> String {
    match analysis.diff.changes.get(path.change as usize) {
        Some(change) => format!("from {}", wording::short_name(&change.symbol.path).name),
        None => "this path".to_owned(),
    }
}

/// The what-if menu entries: a label and the action that starts it.
fn what_if_actions() -> Vec<(&'static str, Box<dyn gpui_kit::Action>)> {
    vec![
        (
            "If its body changes",
            Box::new(crate::views::shell::WhatIfBody),
        ),
        (
            "If its signature changes",
            Box::new(crate::views::shell::WhatIfSignature),
        ),
        (
            "If it is removed",
            Box::new(crate::views::shell::WhatIfRemoved),
        ),
        (
            "If it becomes private",
            Box::new(crate::views::shell::WhatIfVisibility),
        ),
        (
            "If a field is removed",
            Box::new(crate::views::shell::WhatIfFieldRemoved),
        ),
        (
            "If a field changes its type",
            Box::new(crate::views::shell::WhatIfFieldRetyped),
        ),
        (
            "If a field is added",
            Box::new(crate::views::shell::WhatIfFieldAdded),
        ),
        (
            "If a variant is added",
            Box::new(crate::views::shell::WhatIfVariantAdded),
        ),
        (
            "If a variant is removed",
            Box::new(crate::views::shell::WhatIfVariantRemoved),
        ),
    ]
}

/// A class badge in the evidence panel: one height and one radius with the
/// badges of every other row.
fn badge(label: &str, accent: theme::Accent) -> AnyElement {
    ui::pill_outlined(label.to_uppercase(), accent)
}

/// A badge in plain words: the same box as a class badge, in quieter colours.
fn soft_badge(label: String, colour: theme::Tone) -> AnyElement {
    div()
        .flex_none()
        .flex()
        .items_center()
        .justify_center()
        .h(ui::PILL)
        .px(ui::PILL_PAD)
        .rounded(theme::RADIUS_SM)
        .border_1()
        .border_color(theme::LINE_STRONG)
        .text_size(px(10.))
        .text_color(colour)
        .whitespace_nowrap()
        .child(label)
        .into_any_element()
}

/// One of the three counts of a change, as a tile.
fn stat_tile(count: u32, label: &str, accent: theme::Accent) -> AnyElement {
    v_flex()
        .flex_1()
        .min_w_0()
        .gap_0()
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
                .min_w_0()
                .text_size(TEXT_MICRO)
                .text_color(theme::FG_3)
                .child(label.to_owned()),
        )
        .into_any_element()
}

/// A section title with a right-hand note.
fn section_head(label: &str, note: &str) -> AnyElement {
    h_flex()
        .mt_2()
        .h(px(18.))
        .gap_2()
        .items_center()
        .text_size(TEXT_MICRO)
        .text_color(theme::FG_3)
        .child(label.to_owned())
        .child(div().flex_1())
        .child(
            div()
                .flex_shrink_0()
                .text_color(theme::FG_4)
                .child(note.to_owned()),
        )
        .into_any_element()
}

/// One step of an evidence chain.
///
/// The marker is drawn in the row's gutter, one line tall, so it is centred on
/// the first line of the step whether the step holds one line or two. The
/// connector that joins the steps runs in the same gutter, through the centre
/// of every marker, and stops at the marker's edge so that it never shows
/// through a hollow one.
///
/// `joined` says whether a line reaches the step from above and whether it
/// leaves it below; the first step of a chain is reached by none and the last
/// one leaves none.
fn step_row(
    path: &str,
    what: &str,
    site: &str,
    accent: theme::Accent,
    marker: ui::Marker,
    joined: (bool, bool),
) -> AnyElement {
    let name = wording::short_name(path);
    let mut row = h_flex()
        .relative()
        .w_full()
        .min_w_0()
        .h(px(STEP_H))
        .gap(ui::GUTTER_GAP)
        .items_start()
        .pt(px(4.));
    if joined.0 {
        row = row.child(
            div()
                .absolute()
                .left(px(LINE_X))
                .top_0()
                .h(px(MARKER_TOP))
                .w(px(LINE_W))
                .bg(theme::LINE_STRONG),
        );
    }
    row = row.child(ui::marker(accent.solid, marker));
    if joined.1 {
        row = row.child(
            div()
                .absolute()
                .left(px(LINE_X))
                .top(px(MARKER_END))
                .bottom_0()
                .w(px(LINE_W))
                .bg(theme::LINE_STRONG),
        );
    }
    row.child(
        v_flex()
            .flex_1()
            .min_w_0()
            .gap_0()
            .child(
                h_flex()
                    .w_full()
                    .h(ui::ROW_LINE)
                    .min_w_0()
                    .gap_2()
                    .items_center()
                    .line_height(ui::ROW_LINE)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .font_family(theme::mono())
                            .text_size(TEXT_MONO)
                            .text_color(theme::FG)
                            .child(name.name),
                    )
                    .child(
                        div()
                            .flex_shrink_0()
                            .font_family(theme::mono())
                            .text_size(TEXT_MICRO)
                            .text_color(theme::FG_4)
                            .child(site_file(site)),
                    ),
            )
            .child(
                div()
                    .h(ui::SUB_LINE)
                    .line_height(ui::SUB_LINE)
                    .min_w_0()
                    .overflow_hidden()
                    .text_ellipsis()
                    .text_size(TEXT_SMALL)
                    .text_color(theme::FG_3)
                    .child(what.to_owned()),
            ),
    )
    .into_any_element()
}

/// `file:line` with the folders dropped: the step names the symbol, and its
/// short site only has to say where in the file. A long workspace path would
/// otherwise take the width the symbol name needs.
fn site_file(site: &str) -> String {
    site.rsplit(['/', '\\']).next().unwrap_or(site).to_owned()
}

/// The sentence that says why an item is listed, and which parts of it name a
/// symbol.
///
/// The words are the panel's own: the core's `origin_text` names a raw path and
/// starts a sentence with a lower-case letter, so the panel composes the
/// sentence and quotes the changed symbol in its short form.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
struct Sentence {
    /// The words of the sentence.
    text: String,
    /// The byte ranges of `text` that name a symbol, drawn in mono.
    symbols: Vec<Range<usize>>,
}

impl Sentence {
    /// Adds ordinary words.
    fn plain(&mut self, words: &str) -> &mut Self {
        self.text.push_str(words);
        self
    }

    /// Adds a symbol name and marks it.
    fn symbol(&mut self, name: &str) -> &mut Self {
        let start = self.text.len();
        self.text.push_str(name);
        if !name.is_empty() {
            self.symbols.push(start..self.text.len());
        }
        self
    }

    /// Adds words and marks every place where they name `name`.
    fn naming(&mut self, words: &str, name: &str) -> &mut Self {
        let offset = self.text.len();
        if !name.is_empty() {
            for (at, _) in words.match_indices(name) {
                self.symbols.push(offset + at..offset + at + name.len());
            }
        }
        self.text.push_str(words);
        self
    }
}

/// The explanation of an item as the panel draws it: one sentence, with the
/// symbol names it quotes marked for the mono family.
fn explanation_view(sentence: &Sentence) -> AnyElement {
    let mono = theme::mono();
    let colour = HighlightStyle {
        color: Some(theme::FG.hsla()),
        ..HighlightStyle::default()
    };
    let highlights: Vec<(Range<usize>, HighlightStyle)> = sentence
        .symbols
        .iter()
        .map(|range| (range.clone(), colour))
        .collect();
    let families: Vec<(Range<usize>, SharedString)> = sentence
        .symbols
        .iter()
        .map(|range| (range.clone(), mono.clone()))
        .collect();
    div()
        .text_size(TEXT_UI)
        .text_color(theme::FG_2)
        .child(
            StyledText::new(sentence.text.clone())
                .with_highlights(highlights)
                .with_font_family_overrides(families),
        )
        .into_any_element()
}

/// What a change did, as a phrase that follows `whose`.
fn what_changed(change: &Change) -> &'static str {
    match change.kind {
        ChangeKind::Body => "body changed",
        ChangeKind::Signature => "signature changed",
        ChangeKind::Visibility => "visibility changed",
        ChangeKind::Added => "definition is new",
        ChangeKind::Removed => "definition was removed",
        ChangeKind::Renamed => "name changed",
        ChangeKind::NonSemantic => "formatting changed",
        ChangeKind::Field | ChangeKind::Variant | ChangeKind::TraitItem | ChangeKind::Impl => {
            "members changed"
        }
        ChangeKind::ConstValue => "value changed",
        ChangeKind::TypeAlias => "type changed",
        ChangeKind::MacroDef => "expansion changed",
        ChangeKind::Attribute => "attributes changed",
        ChangeKind::Manifest => "manifest changed",
    }
}

/// Rewrites the symbol name the core's reason phrase quotes in its short form,
/// so that `calls StreamDeserializer::next` never reads `calls next`.
///
/// Names too short to be told apart from ordinary words are left alone.
fn short_reason(reason: &str, change: &Change) -> (String, String) {
    let full = change.symbol.name();
    let short = wording::short_name(&change.symbol.path).name;
    if full == short || full.len() < 2 {
        return (reason.to_owned(), full.to_owned());
    }
    (reason.replace(full, &short), short)
}

/// The sentence that explains why an item is listed.
fn explanation(
    item: &ImpactItem,
    path: &blastr_model::EvidencePath,
    analysis: &Analysis,
) -> Sentence {
    let mut sentence = Sentence::default();
    let editing = item.class == ImpactClass::Adaptation;
    let closing = if editing {
        "so it needs editing."
    } else {
        "so it may behave differently."
    };
    match analysis.diff.changes.get(path.change as usize) {
        Some(change) => {
            let what = what_changed(change);
            if path.steps.len() <= 1 {
                let mut reason = wording::reason_text(path.reason, change);
                if !reason.contains("whose ") {
                    reason.push_str(&format!(", whose {what}"));
                }
                let (reason, name) = short_reason(&reason, change);
                if editing {
                    sentence.plain("It ");
                } else {
                    sentence.plain("Its own code did not change, but it ");
                }
                sentence.naming(&reason, &name).plain(", ").plain(closing);
            } else {
                let short = wording::short_name(&change.symbol.path).name;
                if editing {
                    sentence.plain("It is ");
                } else {
                    sentence.plain("Its own code did not change, but it is ");
                }
                sentence
                    .plain(&path.steps.len().to_string())
                    .plain(" steps from ")
                    .symbol(&short)
                    .plain(", whose ")
                    .plain(what)
                    .plain(", ")
                    .plain(closing);
            }
        }
        None => {
            sentence.plain(if editing {
                "It refers to something whose shape changed, so it needs editing."
            } else {
                "Its own code did not change, but something it reaches did, so it may behave differently."
            });
        }
    }
    if item.confidence == Confidence::Possible {
        sentence.plain(" One step is a candidate: at least one target could not be resolved to a single definition.");
    }
    sentence
}

#[cfg(test)]
mod tests {
    use blastr_model::{ChangeKind, ImpactTags, RelPath, Source, Span, SymbolKind, SymbolPath};

    use super::*;

    #[test]
    fn a_step_site_keeps_only_the_file_and_line() {
        assert_eq!(
            site_file("crates/blastr-gui/src/views/evidence.rs:478"),
            "evidence.rs:478"
        );
        assert_eq!(site_file("src\\de.rs:253"), "de.rs:253");
        assert_eq!(site_file("lib.rs:1"), "lib.rs:1");
    }

    #[test]
    fn the_chain_line_runs_through_the_marker_centres() {
        // The line and the marker share the centre of the gutter.
        assert_eq!(LINE_X + LINE_W / 2.0, f32::from(ui::GUTTER) / 2.0);
        // The marker sits on the first line of the step, and the two lines and
        // the padding of a step make its height exactly.
        let marker_top = 4.0 + (f32::from(ui::ROW_LINE) - 8.0) / 2.0;
        assert_eq!(MARKER_TOP, marker_top);
        assert_eq!(MARKER_END - MARKER_TOP, 8.0);
        let step_h = 4.0 + f32::from(ui::ROW_LINE) + f32::from(ui::SUB_LINE) + 4.0;
        assert_eq!(STEP_H, step_h);
        // The line leaves the marker with room to reach the next one.
        assert!(step_h - MARKER_END >= 8.0);
    }

    fn change(kind: ChangeKind, detail: ChangeDetail) -> Change {
        Change {
            kind,
            symbol: SymbolPath::new("billing::apply_discount", SymbolKind::Function),
            old: None,
            new: None,
            detail,
        }
    }

    #[test]
    fn a_body_change_says_the_signature_did_not_change() {
        assert_eq!(
            change_note(&change(ChangeKind::Body, ChangeDetail::None)),
            "The body changed; the signature did not."
        );
        assert_eq!(
            change_note(&change(ChangeKind::Signature, ChangeDetail::None)),
            "The signature changed; the body did not."
        );
    }

    #[test]
    fn a_member_change_says_what_happened_to_the_member() {
        assert_eq!(
            change_note(&change(
                ChangeKind::Field,
                ChangeDetail::Member(Delta::Added)
            )),
            "Field: a member was added."
        );
        assert_eq!(
            change_note(&change(ChangeKind::Renamed, ChangeDetail::Retyped)),
            "Renamed: its declared type changed."
        );
    }

    #[test]
    fn a_manifest_change_shows_its_own_line() {
        assert_eq!(
            change_note(&change(
                ChangeKind::Manifest,
                ChangeDetail::Manifest("feature `http` added".to_owned())
            )),
            "feature `http` added"
        );
    }

    #[test]
    fn the_subtitle_joins_what_is_known() {
        assert_eq!(
            subtitle("fn", "billing::discount", "billing", "discount.rs:41"),
            "fn · billing::discount · discount.rs:41"
        );
        assert_eq!(
            subtitle("fn", "", "billing", "a.rs:1"),
            "fn · billing · a.rs:1"
        );
    }

    fn item(class: ImpactClass, changed: bool, change: u32, steps: usize) -> ImpactItem {
        ImpactItem {
            symbol: SymbolPath::new(
                format!("billing::caller{}{steps}", if changed { "c" } else { "" }),
                SymbolKind::Function,
            ),
            location: blastr_model::Location {
                file: RelPath::new("src/lib.rs"),
                span: Span {
                    start: 0,
                    end: 1,
                    start_line: 1,
                    end_line: 1,
                },
            },
            class,
            confidence: Confidence::Likely,
            score: 0.5,
            distance: 1,
            paths: vec![blastr_model::EvidencePath {
                change,
                reason: blastr_model::Reason::CallsChanged,
                class,
                steps: (0..steps)
                    .map(|_| blastr_model::EvidenceStep {
                        from: SymbolPath::new("billing::caller", SymbolKind::Function),
                        to: SymbolPath::new("billing::apply_discount", SymbolKind::Function),
                        edge: blastr_model::EdgeKind::Calls,
                        certainty: blastr_model::Certainty::Resolved,
                        detail: blastr_model::EdgeDetail::None,
                        site: blastr_model::Location {
                            file: RelPath::new("src/lib.rs"),
                            span: Span {
                                start: 0,
                                end: 1,
                                start_line: 2,
                                end_line: 2,
                            },
                        },
                    })
                    .collect(),
                score: 0.5,
            }],
            path_count: 1,
            tags: ImpactTags {
                changed,
                ..ImpactTags::default()
            },
        }
    }

    fn analysis_with(items: Vec<ImpactItem>) -> Analysis {
        Analysis {
            diff: blastr_model::DiffResult {
                comparison: blastr_model::Comparison {
                    base: Source::Worktree,
                    head: Source::Worktree,
                },
                base_key: None,
                head_key: None,
                changed_files: Vec::new(),
                changes: vec![
                    change(ChangeKind::Signature, ChangeDetail::Retyped),
                    change(ChangeKind::Body, ChangeDetail::None),
                ],
                limits: Vec::new(),
            },
            items,
            tests: blastr_model::TestSelection::default(),
            limits: blastr_model::ImpactLimits::default(),
            params: blastr_model::AnalysisParams::default(),
        }
    }

    #[test]
    fn the_counts_of_a_change_separate_the_two_classes() {
        let analysis = analysis_with(vec![
            item(ImpactClass::Adaptation, false, 0, 1),
            item(ImpactClass::Adaptation, true, 0, 1),
            item(ImpactClass::Behaviour, false, 0, 2),
            item(ImpactClass::Behaviour, false, 1, 1),
        ]);

        let counts = change_counts(&analysis, &analysis.diff.changes[0]);

        assert_eq!(counts.needs_editing, 1);
        assert_eq!(counts.may_behave, 1);
        assert_eq!(counts.tests, 0);
    }

    #[test]
    fn the_direct_targets_of_a_change_are_one_step_away() {
        let analysis = analysis_with(vec![
            item(ImpactClass::Adaptation, false, 0, 1),
            item(ImpactClass::Behaviour, false, 0, 3),
            item(ImpactClass::Behaviour, false, 1, 1),
        ]);

        let direct = direct_targets(&analysis, &analysis.diff.changes[0]);

        assert_eq!(direct.len(), 1);
        assert_eq!(direct[0].1, "calls apply_discount");
    }

    /// The symbol names a sentence marks, as the text it shows.
    fn marked(sentence: &Sentence) -> Vec<&str> {
        sentence
            .symbols
            .iter()
            .map(|range| &sentence.text[range.clone()])
            .collect()
    }

    #[test]
    fn a_one_step_change_names_the_symbol_that_changed() {
        let analysis = analysis_with(vec![item(ImpactClass::Adaptation, false, 0, 1)]);
        let sentence = explanation(&analysis.items[0], &analysis.items[0].paths[0], &analysis);
        assert_eq!(
            sentence.text,
            "It calls apply_discount, whose signature changed, so it needs editing."
        );
        assert_eq!(marked(&sentence), vec!["apply_discount"]);
    }

    #[test]
    fn a_behaviour_item_says_its_own_code_did_not_change() {
        let analysis = analysis_with(vec![item(ImpactClass::Behaviour, false, 1, 1)]);
        let sentence = explanation(&analysis.items[0], &analysis.items[0].paths[0], &analysis);
        assert_eq!(
            sentence.text,
            "Its own code did not change, but it calls apply_discount, whose body changed, so it may behave differently."
        );
    }

    #[test]
    fn a_longer_path_says_how_many_steps_it_is() {
        let analysis = analysis_with(vec![item(ImpactClass::Behaviour, false, 0, 2)]);
        let sentence = explanation(&analysis.items[0], &analysis.items[0].paths[0], &analysis);
        assert_eq!(
            sentence.text,
            "Its own code did not change, but it is 2 steps from apply_discount, whose signature changed, so it may behave differently."
        );
        assert_eq!(marked(&sentence), vec!["apply_discount"]);
    }

    #[test]
    fn a_candidate_step_is_named_in_the_sentence() {
        let mut analysis = analysis_with(vec![item(ImpactClass::Behaviour, false, 0, 1)]);
        analysis.items[0].confidence = Confidence::Possible;
        let sentence = explanation(&analysis.items[0], &analysis.items[0].paths[0], &analysis);
        assert!(sentence.text.ends_with(
            "One step is a candidate: at least one target could not be resolved to a single definition."
        ));
    }

    #[test]
    fn the_marked_names_fall_on_character_boundaries() {
        let analysis = analysis_with(vec![item(ImpactClass::Adaptation, false, 0, 1)]);
        let sentence = explanation(&analysis.items[0], &analysis.items[0].paths[0], &analysis);
        assert!(sentence.text.is_char_boundary(sentence.symbols[0].start));
        assert!(sentence.symbols[0].end <= sentence.text.len());
    }
}
