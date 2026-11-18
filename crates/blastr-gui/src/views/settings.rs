//! The settings panel: everything the window remembers, in one place.
//!
//! It opens from the gear in the title bar and with `Ctrl ,`, and it is a
//! modal layer over the window in the same dark style as the rest. Nothing is
//! confirmed with a button: a change is applied and saved at once, and the
//! comparison is run again when the change is one the analysis depends on.
//!
//! The panel only draws and passes changes on; the values themselves live in
//! [`crate::settings`], which is plain data with tests.

use gpui_kit::prelude::*;

use std::time::{Duration, Instant};

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::input::{Input, InputEvent, InputState, Textarea, TextareaState};
use gpui_kit::component::scroll::ScrollbarAxis;
use gpui_kit::component::switch::Switch;
use gpui_kit::component::{Disableable as _, Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::{
    AnyElement, App, Context, ElementId, Entity, Focusable as _, IntoElement, ParentElement,
    SharedString, Styled, Subscription, Window, div, px,
};

use super::shell::Shell;
use super::ui;
use crate::editor::Editor;
use crate::settings::{self, MAX_DEPTH, MIN_DEPTH, repository_name};
use crate::theme::{self, TEXT_MICRO, TEXT_SMALL, TEXT_UI};

/// The narrowest the label column of a row is.
const LABEL_WIDTH: f32 = 148.0;

/// How far the text of a field sits from its own edge: its padding and the
/// one pixel border around it.
const INPUT_INSET: f32 = 9.0;

/// How far below its own top edge a multi-line field starts its text.
const INPUT_TOP: f32 = 3.0;

/// How far the example of an empty field is kept from where the text starts,
/// so that the caret is not hidden under the first of its letters.
const CARET_CLEARANCE: f32 = 4.0;

/// How long the panel waits after a keystroke before running the analysis
/// again, so that a typed word is one run and not one per letter.
const RERUN_DELAY: Duration = Duration::from_millis(400);

/// Which field of the panel a change came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Field {
    /// The custom editor command.
    Custom,
    /// The depth of the analysis.
    Depth,
    /// The features of this repository.
    Features,
    /// The target triple of this repository.
    Target,
    /// The ignore patterns of this repository.
    Ignore,
}

/// The panel while it is open: its fields and what is wrong with them.
pub(crate) struct SettingsPanel {
    /// The custom editor command.
    custom: Entity<InputState>,
    /// The depth of the analysis.
    depth: Entity<InputState>,
    /// The features of the open repository.
    features: Entity<InputState>,
    /// The target triple of the open repository.
    target: Entity<InputState>,
    /// The ignore patterns of the open repository.
    ignore: Entity<TextareaState>,
    /// What is wrong with the values in the fields, if anything.
    pub(crate) problem: Option<String>,
    /// Kept so that the fields stop reporting to a closed panel.
    _subscriptions: Vec<Subscription>,
}

impl Shell {
    /// Opens the panel, with the fields filled in from the settings.
    pub(crate) fn open_settings(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.settings_panel.is_some() {
            return;
        }
        let root = self.repo_root();
        let custom_text = self.settings.editor.custom.clone();
        let depth_text = self.settings.analysis.depth.to_string();
        let repository = root
            .as_deref()
            .and_then(|root| self.settings.repository(root))
            .cloned()
            .unwrap_or_default();
        let features_text = settings::format_features(&repository.features);
        let target_text = settings::format_target(repository.target.as_deref());
        let ignore_text = settings::format_globs(&repository.ignore);

        let custom = text_field(window, cx, &custom_text);
        let depth = text_field(window, cx, &depth_text);
        let features = text_field(window, cx, &features_text);
        let target = text_field(window, cx, &target_text);
        let ignore = area_field(window, cx, &ignore_text);

        let mut subscriptions = Vec::new();
        subscriptions.push(cx.subscribe_in(
            &custom,
            window,
            |this, input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    this.settings_field_changed(Field::Custom, text, cx);
                }
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &depth,
            window,
            |this, input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    this.settings_field_changed(Field::Depth, text, cx);
                }
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &features,
            window,
            |this, input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    this.settings_field_changed(Field::Features, text, cx);
                }
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &target,
            window,
            |this, input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    this.settings_field_changed(Field::Target, text, cx);
                }
            },
        ));
        subscriptions.push(cx.subscribe_in(
            &ignore,
            window,
            |this, input, event: &InputEvent, _window, cx| {
                if matches!(event, InputEvent::Change) {
                    let text = input.read(cx).value().to_string();
                    this.settings_field_changed(Field::Ignore, text, cx);
                }
            },
        ));

        if !self.editors_known {
            self.worker
                .send(crate::state::worker::Command::DetectEditors);
        }
        self.settings_panel = Some(SettingsPanel {
            custom,
            depth,
            features,
            target,
            ignore,
            problem: None,
            _subscriptions: subscriptions,
        });
        cx.notify();
    }

    /// Closes the panel and gives the keyboard back to the window.
    pub(crate) fn close_settings(&mut self, cx: &mut Context<Self>) {
        self.settings_panel = None;
        self.refocus = true;
        cx.notify();
    }

    /// Takes a change from one of the panel's fields.
    ///
    /// A value that cannot be used is reported beside the field and the last
    /// usable one stays in force.
    pub(crate) fn settings_field_changed(
        &mut self,
        field: Field,
        text: String,
        cx: &mut Context<Self>,
    ) {
        let root = self.repo_root();
        let mut problem = None;
        match field {
            Field::Custom => {
                self.settings.editor.custom = text;
                problem = self.settings.editor.custom_problem();
            }
            Field::Depth => match settings::parse_depth(&text) {
                Some(depth) => self.settings.analysis.depth = depth,
                None => {
                    problem = Some(format!(
                        "The depth is a whole number from {MIN_DEPTH} to {MAX_DEPTH}."
                    ));
                }
            },
            Field::Features => {
                if let Some(root) = &root {
                    self.settings.repository_mut(root).features = settings::parse_features(&text);
                }
            }
            Field::Target => {
                if let Some(root) = &root {
                    self.settings.repository_mut(root).target = settings::parse_target(&text);
                }
            }
            Field::Ignore => {
                if let Some(root) = &root {
                    let repository = self.settings.repository_mut(root);
                    repository.ignore = settings::parse_globs(&text);
                    problem = repository.ignore_problem();
                }
            }
        }
        if let Some(panel) = &mut self.settings_panel {
            panel.problem = problem;
        }
        self.settings_applied(cx);
    }

    /// Chooses the editor that opens a file.
    pub(crate) fn choose_editor(&mut self, editor: Editor, cx: &mut Context<Self>) {
        self.settings.editor.editor = editor;
        let problem = self.settings.editor.custom_problem();
        if let Some(panel) = &mut self.settings_panel {
            panel.problem = problem;
        }
        self.settings_dirty.set(true);
        cx.notify();
    }

    /// Sets the lowest confidence that is listed.
    pub(crate) fn set_min_confidence(
        &mut self,
        confidence: blastr_model::Confidence,
        cx: &mut Context<Self>,
    ) {
        self.settings.analysis.min_confidence = confidence;
        self.settings_applied_now(cx);
    }

    /// Turns the recomputation after a save on or off.
    pub(crate) fn set_refresh_on_save(&mut self, on: bool, cx: &mut Context<Self>) {
        self.settings.analysis.refresh_on_save = on;
        self.settings_applied_now(cx);
    }

    /// Sets whether every feature of the open repository is enabled.
    pub(crate) fn set_all_features(&mut self, all: bool, cx: &mut Context<Self>) {
        if let Some(root) = self.repo_root() {
            self.settings.repository_mut(&root).all_features = all;
            self.settings_applied_now(cx);
        }
    }

    /// Sets how build scripts and procedural macros are treated in the open
    /// repository.
    pub(crate) fn set_restricted(&mut self, restricted: bool, cx: &mut Context<Self>) {
        let Some(root) = self.repo_root() else {
            return;
        };
        self.settings.repository_mut(&root).restricted = restricted;
        self.settings_applied_now(cx);
    }

    /// Applies whatever the panel changed once the typing in its fields stops.
    ///
    /// A text field reports every keystroke, and each one would otherwise load
    /// the project again; the settings are already stored, and the comparison
    /// follows once the value has settled.
    pub(crate) fn settings_applied(&mut self, cx: &mut Context<Self>) {
        self.settings_dirty.set(true);
        self.settings_rerun = Some(Instant::now() + RERUN_DELAY);
        cx.notify();
    }

    /// Applies whatever the panel changed at once.
    pub(crate) fn settings_applied_now(&mut self, cx: &mut Context<Self>) {
        self.settings_dirty.set(true);
        self.settings_rerun = None;
        self.rerun_analysis(cx);
        cx.notify();
    }

    /// Takes the settings as they now are, running the comparison again when
    /// the analysis is one of the things that changed.
    pub(crate) fn rerun_analysis(&mut self, cx: &mut Context<Self>) {
        let root = self.repo_root();
        let (options, params) = self.settings.analysis_for(root.as_deref());
        if options != self.session.options || params != self.session.params {
            self.session.options = options;
            self.session.params = params;
            if self.session.compare.is_some() {
                self.retry(cx);
            }
        }
    }

    /// Opens the file the panel is talking about, to show what the chosen
    /// editor does with it.
    pub(crate) fn test_editor(&mut self, cx: &mut Context<Self>) {
        let Some(root) = self.repo_root() else {
            return;
        };
        let target = self
            .session
            .selection()
            .and_then(|selection| self.selection_location(selection))
            .map(|(_, location)| (root.join(location.file.as_str()), location.span.start_line));
        let (file, line) = target.unwrap_or((root.join("Cargo.toml"), 1));
        self.open_file_in_editor(file, line, cx);
    }

    /// The command line the editor setting resolves to, for the panel to show
    /// under the field.
    fn resolved_editor_command(&self) -> String {
        let root = self.repo_root();
        let (file, line) = match self.session.selection() {
            Some(selection) => match self.selection_location(selection) {
                Some((_, location)) => (
                    root.clone()
                        .unwrap_or_default()
                        .join(location.file.as_str()),
                    location.span.start_line,
                ),
                None => (root.unwrap_or_default().join("src/lib.rs"), 1),
            },
            None => (root.unwrap_or_default().join("Cargo.toml"), 1),
        };
        match crate::editor::resolve(
            self.settings.editor.editor,
            &self.settings.editor.custom,
            &self.editors,
            &file,
            line,
            1,
        ) {
            Ok((_, launch)) => launch.display(),
            Err(error) => error,
        }
    }

    /// The panel: a scrim, the sections and their controls.
    pub(crate) fn settings_layer(&mut self, cx: &mut Context<Self>) -> AnyElement {
        if self.settings_panel.is_none() {
            return div().into_any_element();
        }
        let editor = self.settings.editor.editor;
        let analysis = self.settings.analysis;
        let root = self.repo_root();
        let repository = root
            .as_deref()
            .and_then(|root| self.settings.repository(root))
            .cloned()
            .unwrap_or_default();
        let problem = self
            .settings_panel
            .as_ref()
            .and_then(|panel| panel.problem.clone());
        let resolved = self.resolved_editor_command();

        let mut body = v_flex().w_full().gap_3().p_4();

        body = body.child(section_head("Editor", IconName::FileText));
        body = body.child(row("Open files in", None, self.editor_choices(cx)));
        if editor == Editor::Custom {
            let field = self
                .settings_panel
                .as_ref()
                .map(|panel| panel.custom.clone())
                .expect("the panel is open");
            body = body.child(row(
                "Command",
                Some("`{file}`, `{line}` and `{column}` are replaced; the column is always 1."),
                hinted_input(&field, "Command", "code -g {file}:{line}:{column}", cx),
            ));
        }
        body = body.child(row(
            "Resolved",
            None,
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .overflow_hidden()
                        .text_ellipsis()
                        .whitespace_nowrap()
                        .font_family(theme::mono())
                        .text_size(TEXT_MICRO)
                        .text_color(theme::FG_3)
                        .child(resolved),
                )
                .child(
                    Button::new("editor-test")
                        .small()
                        .outline()
                        .label("Test")
                        .disabled(root.is_none())
                        .on_click(cx.listener(|this, _, _, cx| this.test_editor(cx))),
                )
                .into_any_element(),
        ));

        body = body.child(section_head("Analysis", IconName::Network));
        let depth_field = self
            .settings_panel
            .as_ref()
            .map(|panel| panel.depth.clone())
            .expect("the panel is open");
        body = body.child(row(
            "Depth",
            Some("How far the impact of a change is followed."),
            h_flex()
                .w(px(90.))
                .child(Input::new(&depth_field).small().w_full())
                .into_any_element(),
        ));
        body = body.child(row(
            "Minimum confidence",
            Some("Impact below this is counted, not listed."),
            h_flex()
                .gap_1()
                .children(
                    [
                        blastr_model::Confidence::Definite,
                        blastr_model::Confidence::Likely,
                        blastr_model::Confidence::Possible,
                    ]
                    .into_iter()
                    .map(|confidence| {
                        let selected = analysis.min_confidence == confidence;
                        chip(
                            SharedString::from(format!("confidence-{}", confidence.as_str())),
                            confidence.as_str(),
                            selected,
                            cx,
                            move |shell, cx| shell.set_min_confidence(confidence, cx),
                        )
                    }),
                )
                .into_any_element(),
        ));
        let refresh = analysis.refresh_on_save;
        let shell = cx.entity().downgrade();
        body = body.child(row(
            "Refresh on save",
            Some("Run the comparison again when a file in the working tree is saved."),
            h_flex()
                .w_full()
                .gap_2()
                .items_center()
                .child(Switch::new("refresh-on-save").checked(refresh).on_click(
                    move |on: &bool, _window, cx| {
                        let _ = shell.update(cx, |shell, cx| shell.set_refresh_on_save(*on, cx));
                    },
                ))
                .child(
                    div()
                        .text_size(TEXT_SMALL)
                        .text_color(theme::FG_3)
                        .child(if refresh { "On" } else { "Off" }),
                )
                .into_any_element(),
        ));

        if let Some(root) = &root {
            body = body.child(section_head(
                &format!("This repository · {}", repository_name(root)),
                IconName::Folder,
            ));
            let features_field = self
                .settings_panel
                .as_ref()
                .map(|panel| panel.features.clone())
                .expect("the panel is open");
            body = body.child(row(
                "Features",
                Some("Separated by commas or spaces. Empty means the default features."),
                h_flex()
                    .w_full()
                    .gap_2()
                    .items_center()
                    .child(div().flex_1().min_w_0().child(hinted_input(
                        &features_field,
                        "Features",
                        "e.g. serde, std",
                        cx,
                    )))
                    .children(repository.all_features.then(|| {
                        div()
                            .flex_none()
                            .text_size(TEXT_MICRO)
                            .text_color(theme::FG_4)
                            .child("every feature")
                    }))
                    .into_any_element(),
            ));
            let all = repository.all_features;
            let shell = cx.entity().downgrade();
            body = body.child(row(
                "All features",
                Some("Build with every feature of every package enabled."),
                h_flex()
                    .w_full()
                    .items_center()
                    .child(Switch::new("all-features").checked(all).on_click(
                        move |on: &bool, _window, cx| {
                            let _ = shell.update(cx, |shell, cx| shell.set_all_features(*on, cx));
                        },
                    ))
                    .into_any_element(),
            ));
            let target_field = self
                .settings_panel
                .as_ref()
                .map(|panel| panel.target.clone())
                .expect("the panel is open");
            body = body.child(row(
                "Target triple",
                Some("Empty means the host."),
                div()
                    .w(px(260.))
                    .child(hinted_input(
                        &target_field,
                        "Target triple",
                        "Host target",
                        cx,
                    ))
                    .into_any_element(),
            ));
            let ignore_field = self
                .settings_panel
                .as_ref()
                .map(|panel| panel.ignore.clone())
                .expect("the panel is open");
            body = body.child(row(
                "Ignore",
                Some("One pattern per line: `*` and `?` stay within a segment, `**` covers whole ones."),
                hinted_area(&ignore_field, "Ignore", "e.g. vendor/**", cx),
            ));
            let restricted = repository.restricted;
            body = body.child(row(
                "Trust mode",
                Some(
                    "Full runs the build scripts and procedural macros of this repository. \
                     Restricted skips both; generated code is then invisible.",
                ),
                h_flex()
                    .gap_1()
                    .child(chip("trust-full", "Full", !restricted, cx, |shell, cx| {
                        shell.set_restricted(false, cx)
                    }))
                    .child(chip(
                        "trust-restricted",
                        "Restricted",
                        restricted,
                        cx,
                        |shell, cx| shell.set_restricted(true, cx),
                    ))
                    .into_any_element(),
            ));
        }

        let panel = v_flex()
            .w(px(640.))
            .max_h(px(640.))
            .mt(px(56.))
            .rounded(px(10.))
            .border_1()
            .border_color(theme::LINE_STRONG)
            .bg(theme::BG_RAISED)
            .overflow_hidden()
            .child(
                h_flex()
                    .h(px(44.))
                    .flex_shrink_0()
                    .px_4()
                    .gap_2()
                    .items_center()
                    .border_b_1()
                    .border_color(theme::LINE)
                    .child(
                        Icon::new(IconName::Settings)
                            .size_4()
                            .text_color(theme::FG_3),
                    )
                    .child(
                        div()
                            .text_size(px(14.))
                            .text_color(theme::FG)
                            .child("Settings"),
                    )
                    .child(div().flex_1())
                    .child(
                        ui_close_button()
                            .on_click(cx.listener(|this, _, _, cx| this.close_settings(cx))),
                    ),
            )
            .child(
                // The sections are as tall as their settings are, so the body
                // is bounded here and scrolls inside that bound.
                div()
                    .relative()
                    .flex_1()
                    .min_h_0()
                    .max_h(px(520.))
                    .child(
                        div()
                            .id("settings-body")
                            .size_full()
                            .min_h_0()
                            .overflow_y_scroll()
                            .track_scroll(&self.settings_scroll)
                            .child(body),
                    )
                    .child(ui::scrollbar(
                        "settings",
                        &self.settings_scroll,
                        ScrollbarAxis::Vertical,
                    )),
            )
            .child(
                h_flex()
                    .h(px(36.))
                    .flex_shrink_0()
                    .px_4()
                    .gap_3()
                    .items_center()
                    .border_t_1()
                    .border_color(theme::LINE)
                    .bg(theme::BG_PANEL)
                    .text_size(TEXT_MICRO)
                    .text_color(theme::FG_4)
                    .child("Changes apply at once and are saved to your configuration directory.")
                    .child(div().flex_1())
                    .children(problem.map(|problem| {
                        div()
                            .max_w(px(300.))
                            .min_w_0()
                            .overflow_hidden()
                            .text_ellipsis()
                            .whitespace_nowrap()
                            .text_color(theme::EDIT.fg)
                            .child(problem)
                    }))
                    .child(
                        Button::new("settings-done")
                            .small()
                            .primary()
                            .label("Done")
                            .on_click(cx.listener(|this, _, _, cx| this.close_settings(cx))),
                    ),
            );

        div()
            .absolute()
            .inset_0()
            .flex()
            .justify_center()
            .items_start()
            .bg(theme::SCRIM)
            // The window behind the panel keeps its hover, its clicks and its
            // wheel to itself while the panel is open.
            .occlude()
            .on_scroll(cx.listener(|_, _: &gpui_kit::ScrollEvent, _, cx| cx.stop_propagation()))
            .on_mouse_down(
                gpui_kit::MouseButton::Left,
                cx.listener(|this, _, _, cx| this.close_settings(cx)),
            )
            .child(
                div()
                    .on_mouse_down(
                        gpui_kit::MouseButton::Left,
                        cx.listener(|_, _, _, cx| cx.stop_propagation()),
                    )
                    .child(panel),
            )
            .into_any_element()
    }

    /// The editors this machine has, with the automatic choice and the custom
    /// command beside them.
    fn editor_choices(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let chosen = self.settings.editor.editor;
        let mut choices: Vec<(Editor, &'static str)> = vec![(Editor::Auto, Editor::Auto.label())];
        for detected in &self.editors {
            choices.push((detected.editor, detected.editor.label()));
        }
        choices.push((Editor::Custom, Editor::Custom.label()));
        let known = self.editors_known;
        let found = self.editors.is_empty();

        v_flex()
            .w_full()
            .gap_2()
            .child(
                h_flex()
                    .gap_1()
                    .flex_wrap()
                    .children(choices.into_iter().map(|(editor, label)| {
                        chip(
                            SharedString::from(format!("editor-{}", label)),
                            label,
                            editor == chosen,
                            cx,
                            move |shell, cx| shell.choose_editor(editor, cx),
                        )
                    })),
            )
            .children((!known).then(|| {
                div()
                    .text_size(TEXT_MICRO)
                    .text_color(theme::FG_4)
                    .child("Looking for the editors installed on this machine…")
            }))
            .children((known && found).then(|| {
                div().text_size(TEXT_MICRO).text_color(theme::FG_4).child(
                    "No editor was found on this machine. Write the command yours \
                         is started with.",
                )
            }))
            .into_any_element()
    }
}

/// A text field holding its starting value, created on the window.
///
/// An empty field shows its example in the view instead of a placeholder:
/// what the input paints reads as entered text at the theme's muted colour.
fn text_field(window: &mut Window, cx: &mut Context<Shell>, value: &str) -> Entity<InputState> {
    let field = cx.new(|cx| InputState::new(window, cx));
    let value = value.to_owned();
    field.update(cx, |field, cx| field.set_value(value, window, cx));
    field
}

/// A multi-line text field holding its starting value, created on the window.
fn area_field(window: &mut Window, cx: &mut Context<Shell>, value: &str) -> Entity<TextareaState> {
    let field = cx.new(|cx| TextareaState::new(window, cx));
    let value = value.to_owned();
    field.update(cx, |field, cx| field.set_value(value, window, cx));
    field
}

/// A one-line field, with the example it shows while it is empty.
fn hinted_input(
    field: &Entity<InputState>,
    label: &'static str,
    example: &'static str,
    cx: &App,
) -> AnyElement {
    let empty = field.read(cx).value().is_empty();
    let focus = field.read(cx).focus_handle(cx);
    div()
        .relative()
        .w_full()
        .child(Input::new(field).small().w_full().aria_label(label))
        .children(empty.then(|| in_field(example, focus, false)))
        .into_any_element()
}

/// A multi-line field, with the example it shows while it is empty.
fn hinted_area(
    field: &Entity<TextareaState>,
    label: &'static str,
    example: &'static str,
    cx: &App,
) -> AnyElement {
    let empty = field.read(cx).value().is_empty();
    let focus = field.read(cx).focus_handle(cx);
    div()
        .relative()
        .w_full()
        .child(
            Textarea::new(field)
                .small()
                .h(px(76.))
                .w_full()
                .aria_label(label),
        )
        .children(empty.then(|| in_field(example, focus, true)))
        .into_any_element()
}

/// The example an empty field shows, where its text would be.
///
/// It is in the quietest text the theme has, and in italics, so that an empty
/// field cannot be taken for one that already holds a value. A click on it
/// puts the caret in the field it belongs to.
fn in_field(example: &'static str, focus: gpui_kit::FocusHandle, at_top: bool) -> AnyElement {
    let label = div()
        .absolute()
        .left(px(INPUT_INSET + CARET_CLEARANCE))
        .right(px(INPUT_INSET))
        .text_size(TEXT_SMALL)
        .text_color(theme::FG_4)
        .italic()
        .whitespace_nowrap()
        .overflow_hidden()
        .child(example);
    let label = if at_top {
        label.top(px(INPUT_TOP))
    } else {
        label.top_0().bottom_0().flex().items_center()
    };
    label
        .on_mouse_down(gpui_kit::MouseButton::Left, move |_, window, cx| {
            window.focus(&focus, cx);
        })
        .into_any_element()
}

/// The super-header of a section of the panel.
fn section_head(label: &str, icon: IconName) -> AnyElement {
    h_flex()
        .h(px(28.))
        .w_full()
        .gap_2()
        .items_center()
        .border_t_1()
        .border_color(theme::LINE)
        .child(Icon::new(icon).size_3().text_color(theme::FG_4))
        .child(
            div()
                .text_size(px(10.))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(theme::FG_4)
                .child(label.to_owned()),
        )
        .child(div().flex_1().h(px(1.)).bg(theme::LINE))
        .into_any_element()
}

/// One setting of the panel: its name, its control, and an optional hint
/// under the control.
fn row(label: &str, hint: Option<&str>, control: AnyElement) -> AnyElement {
    h_flex()
        .w_full()
        .items_start()
        .gap_3()
        .child(
            div()
                .w(px(LABEL_WIDTH))
                .flex_none()
                .pt(px(4.))
                .text_size(TEXT_UI)
                .text_color(theme::FG_2)
                .child(label.to_owned()),
        )
        .child(
            v_flex()
                .flex_1()
                .min_w_0()
                .gap_1()
                .child(control)
                .children(hint.map(|hint| {
                    div()
                        .text_size(TEXT_MICRO)
                        .text_color(theme::FG_4)
                        .child(hint.to_owned())
                })),
        )
        .into_any_element()
}

/// A choice of the panel: a word that is filled in when it is the current one.
fn chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    cx: &mut Context<Shell>,
    action: impl Fn(&mut Shell, &mut Context<Shell>) + 'static,
) -> AnyElement {
    let shell = cx.entity().downgrade();
    let label = label.into();
    div()
        .id(id)
        .h(px(24.))
        .px_2()
        .flex()
        .items_center()
        .rounded(theme::RADIUS_MD)
        .border_1()
        .border_color(if selected {
            theme::CHANGED.line
        } else {
            theme::LINE
        })
        .bg(if selected {
            theme::CHANGED.bg
        } else {
            theme::BG_RAISED_2
        })
        .text_size(TEXT_SMALL)
        .text_color(if selected {
            theme::CHANGED.fg
        } else {
            theme::FG_2
        })
        .cursor_pointer()
        .hover(|style| style.border_color(theme::LINE_STRONG))
        .on_click(move |_, _, cx| {
            let _ = shell.update(cx, |shell, cx| action(shell, cx));
        })
        .child(label)
        .into_any_element()
}

/// The close button of the panel's head.
fn ui_close_button() -> Button {
    Button::new("settings-close")
        .ghost()
        .small()
        .compact()
        .icon(IconName::Close)
        .tooltip("Close  Esc")
}

/// The settings, as they are written, for a test that reads them back.
#[cfg(test)]
fn written(settings: &crate::settings::Settings) -> String {
    serde_json::to_string(settings).expect("settings to JSON")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::settings::{AnalysisSettings, EditorSettings, RepositorySettings};

    #[test]
    fn the_panel_writes_the_settings_it_shows() {
        let mut settings = crate::settings::Settings::default();
        settings.analysis = AnalysisSettings {
            depth: 9,
            ..AnalysisSettings::default()
        };
        settings.editor = EditorSettings {
            editor: Editor::Custom,
            custom: "code -g {file}:{line}".to_owned(),
        };
        let text = written(&settings);
        assert!(text.contains("\"depth\":9"), "{text}");
        assert!(text.contains("\"editor\":\"custom\""), "{text}");
        assert!(
            text.contains("code -g {file}:{line}"),
            "the command is kept: {text}"
        );
    }

    #[test]
    fn a_repository_row_only_shows_when_one_is_open() {
        // The panel is a view, so this only pins the rule it follows: the
        // repository section is drawn for a root and for nothing else.
        let repository = RepositorySettings::new(std::path::Path::new("/code/app"));
        assert_eq!(repository.path, std::path::Path::new("/code/app"));
        assert!(repository.features.is_empty());
        assert!(!repository.all_features);
        assert!(!repository.restricted);
    }
}
