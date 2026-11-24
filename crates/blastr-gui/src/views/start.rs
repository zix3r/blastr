//! The start screen: open a folder or one of the recent repositories.

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::prelude::*;
use gpui_kit::{
    AnyElement, Context, ExternalPaths, IntoElement, ParentElement, SharedString, Styled, div, px,
};

use super::shell::Shell;
use super::ui;
use crate::theme::{self, TEXT_SMALL, TEXT_UI};

impl Shell {
    pub(crate) fn start_screen(&self, cx: &mut Context<Self>) -> AnyElement {
        let workspace = self.workspace.read(cx);
        let opening = workspace.is_opening();
        let error = workspace.open_error.clone();
        let recent = workspace.settings.recent.clone();

        let mut column = v_flex().w(px(680.)).gap_4().child(
            h_flex()
                .gap_4()
                .items_center()
                .child(ui::logo(px(56.)))
                .child(
                    v_flex()
                        .child(div().text_size(px(30.)).child("Blastr"))
                        .child(
                            div()
                                .text_size(px(14.))
                                .text_color(theme::FG_3)
                                .child("See what a Rust change can reach, and why."),
                        ),
                ),
        );
        if let Some(error) = error {
            column = column.child(
                h_flex()
                    .gap_2()
                    .p_3()
                    .items_start()
                    .rounded(theme::RADIUS_LG)
                    .border_1()
                    .border_color(theme::EDIT.line)
                    .bg(theme::EDIT.bg)
                    .child(
                        Icon::new(IconName::TriangleAlert)
                            .size_4()
                            .text_color(theme::EDIT.solid),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_size(TEXT_SMALL)
                            .text_color(theme::FG_2)
                            .child(error),
                    ),
            );
        }
        column = column.child(
            h_flex()
                .id("open-folder")
                .h(px(84.))
                .px_4()
                .gap_3()
                .items_center()
                .rounded(px(10.))
                .border_1()
                .border_color(theme::CHANGED.line)
                .bg(theme::BG_PANEL)
                .cursor_pointer()
                .hover(|style| {
                    style
                        .border_color(theme::CHANGED.solid)
                        .bg(theme::CHANGED.bg)
                })
                .on_click(cx.listener(|this, _, _, cx| this.pick_folder(cx)))
                .child(ui::glyph(IconName::FolderOpen, Some(theme::CHANGED)))
                .child(
                    v_flex()
                        .flex_1()
                        .child(div().text_size(px(13.5)).child(if opening {
                            "Opening the repository…"
                        } else {
                            "Open a repository"
                        }))
                        .child(
                            div()
                                .text_size(TEXT_SMALL)
                                .text_color(theme::FG_3)
                                .child("Drop a folder here, or choose one with Ctrl O."),
                        ),
                )
                .child(
                    Button::new("open-folder-button")
                        .small()
                        .primary()
                        .label("Open folder")
                        .on_click(cx.listener(|this, _, _, cx| this.pick_folder(cx))),
                ),
        );
        if !recent.is_empty() {
            let rows = recent.into_iter().map(|recent| {
                let name = recent
                    .path
                    .file_name()
                    .map(|name| name.to_string_lossy().into_owned())
                    .unwrap_or_default();
                let text = recent.path.display().to_string();
                let path = recent.path.clone();
                h_flex()
                    .id(SharedString::from(text.clone()))
                    .h(px(48.))
                    .px_2()
                    .gap_3()
                    .items_center()
                    .rounded(theme::RADIUS_MD)
                    .cursor_pointer()
                    .hover(|style| style.bg(theme::BG_HOVER))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let path = path.clone();
                        this.workspace
                            .update(cx, |workspace, cx| workspace.open_path(path, cx));
                    }))
                    .child(Icon::new(IconName::Folder).size_4().text_color(theme::FG_4))
                    .child(
                        v_flex()
                            .flex_1()
                            .min_w_0()
                            .child(div().text_size(TEXT_UI).child(name))
                            .child(
                                div()
                                    .truncate()
                                    .text_size(px(11.5))
                                    .text_color(theme::FG_3)
                                    .child(text),
                            ),
                    )
                    .children(recent.branch.map(|branch| {
                        h_flex()
                            .gap_1()
                            .items_center()
                            .text_size(px(11.5))
                            .text_color(theme::FG_3)
                            .child(
                                Icon::new(IconName::Network)
                                    .size_3()
                                    .text_color(theme::FG_4),
                            )
                            .child(branch)
                    }))
            });
            column = column.child(
                v_flex()
                    .p_1()
                    .rounded(px(10.))
                    .border_1()
                    .border_color(theme::LINE)
                    .bg(theme::BG_PANEL)
                    .children(rows),
            );
        }
        column = column.child(
            h_flex()
                .gap_2()
                .justify_center()
                .text_size(px(11.5))
                .text_color(theme::FG_4)
                .child("Everything runs locally")
                .child("·")
                .child("Ctrl O opens a folder")
                .child("·")
                .child("Ctrl , opens the settings"),
        );
        h_flex()
            .size_full()
            .justify_center()
            .items_center()
            .on_drop(cx.listener(|this, paths: &ExternalPaths, _, cx| {
                if let Some(path) = paths.paths().first().cloned() {
                    this.workspace
                        .update(cx, |workspace, cx| workspace.open_path(path, cx));
                }
            }))
            .child(column)
            .into_any_element()
    }
}
