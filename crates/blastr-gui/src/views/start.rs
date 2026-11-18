//! The start screen: what the window shows before a repository is open.

use gpui_kit::prelude::*;

use std::path::PathBuf;

use gpui_kit::component::button::{Button, ButtonVariants as _};
use gpui_kit::component::{Icon, IconName, Sizable as _, h_flex, v_flex};
use gpui_kit::{AnyElement, Context, IntoElement, ParentElement, SharedString, Styled, div, px};

use super::shell::Shell;
use crate::state::commits::relative_time;
use crate::theme::{self, TEXT_SMALL, TEXT_UI};

impl Shell {
    /// The start screen, or the wait for a repository that is being opened.
    pub(crate) fn start_screen(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let opening = self.start.opening;
        let error = self.start.error.clone();
        let recent: Vec<(PathBuf, Option<String>, i64)> = self
            .settings
            .recent
            .iter()
            .map(|recent| (recent.path.clone(), recent.branch.clone(), recent.opened))
            .collect();
        let now = self.now;

        let mut column = v_flex().w(px(720.)).max_w(px(720.)).gap_4().child(
            h_flex()
                .gap_4()
                .items_center()
                .child(super::ui::logo(px(56.)))
                .child(
                    v_flex()
                        .child(
                            div()
                                .text_size(px(30.))
                                .text_color(theme::FG)
                                .child("Blastr"),
                        )
                        .child(
                            div()
                                .text_size(px(14.))
                                .text_color(theme::FG_3)
                                .child("See what a Rust change can reach, and why."),
                        ),
                ),
        );

        if let Some(error) = &error {
            column = column.child(
                h_flex()
                    .gap_2()
                    .p_3()
                    .items_start()
                    .rounded(px(8.))
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
                            .child(error.clone()),
                    ),
            );
        }

        // The drop target is also the button: a folder can be dragged onto the
        // window or chosen with the platform picker.
        let drop = h_flex()
            .id("open-drop")
            .w_full()
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
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_center()
                    .w(px(44.))
                    .h(px(44.))
                    .rounded(px(8.))
                    .border_1()
                    .border_color(theme::CHANGED.line)
                    .child(
                        Icon::new(IconName::FolderOpen)
                            .size_5()
                            .text_color(theme::CHANGED.fg),
                    ),
            )
            .child(
                v_flex()
                    .flex_1()
                    .child(
                        div()
                            .text_size(px(13.5))
                            .text_color(theme::FG)
                            .child(if opening {
                                "Opening the repository…"
                            } else {
                                "Open a repository"
                            }),
                    )
                    .child(
                        div()
                            .text_size(TEXT_SMALL)
                            .text_color(theme::FG_3)
                            .child("Drop a folder here, or choose one with Ctrl O."),
                    ),
            )
            .child(
                Button::new("open-folder-start")
                    .small()
                    .primary()
                    .label("Open folder")
                    .on_click(cx.listener(|this, _, _, cx| this.pick_folder(cx))),
            );

        column = column.child(drop);

        if recent.is_empty() {
            column = column.child(
                div()
                    .text_size(TEXT_SMALL)
                    .text_color(theme::FG_4)
                    .child("No repository has been opened yet."),
            );
        } else {
            let mut list = v_flex()
                .rounded(px(10.))
                .border_1()
                .border_color(theme::LINE)
                .bg(theme::BG_PANEL)
                .p_1()
                .gap_0();
            for (path, branch, opened) in recent {
                let name = crate::settings::repository_name(&path);
                let path_text = path.display().to_string();
                list = list.child(
                    h_flex()
                        .id(SharedString::from(path_text.clone()))
                        .h(px(48.))
                        .px_2()
                        .gap_3()
                        .items_center()
                        .rounded(px(6.))
                        .cursor_pointer()
                        .hover(|style| style.bg(theme::BG_HOVER))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_path(path.clone(), cx);
                        }))
                        .child(Icon::new(IconName::Folder).size_4().text_color(theme::FG_4))
                        .child(
                            v_flex()
                                .flex_1()
                                .min_w_0()
                                .child(div().text_size(TEXT_UI).text_color(theme::FG).child(name))
                                .child(
                                    div()
                                        .text_size(px(11.5))
                                        .text_color(theme::FG_3)
                                        .child(path_text.clone()),
                                ),
                        )
                        .children(branch.map(|branch| {
                            h_flex()
                                .gap_1()
                                .items_center()
                                .child(
                                    Icon::new(IconName::Network)
                                        .size_3()
                                        .text_color(theme::FG_4),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.5))
                                        .text_color(theme::FG_3)
                                        .child(branch),
                                )
                        }))
                        .child(
                            div()
                                .w(px(64.))
                                .text_size(px(11.5))
                                .text_color(theme::FG_4)
                                .child(relative_time(opened, now)),
                        ),
                );
            }
            column = column.child(list);
        }

        column = column.child(Self::start_footer(cx));

        h_flex()
            .size_full()
            .justify_center()
            .items_center()
            .bg(theme::BG_CANVAS)
            .on_drop(cx.listener(|this, paths: &gpui_kit::ExternalPaths, _, cx| {
                // A file dropped on the window while an overlay is open
                // belongs to nothing: the overlay owns the window.
                if this.modal_open() {
                    return;
                }
                if let Some(path) = paths.paths().first() {
                    this.open_path(path.clone(), cx);
                }
            }))
            .child(column)
            .into_any_element()
    }

    /// The footer line of the start screen.
    pub(crate) fn start_footer(cx: &mut Context<Self>) -> AnyElement {
        let _ = cx;
        h_flex()
            .gap_2()
            .justify_center()
            .w_full()
            .text_size(px(11.5))
            .text_color(theme::FG_4)
            .child("Everything runs locally")
            .child("·")
            .child("Ctrl O opens a folder")
            .child("·")
            .child("Ctrl , opens the settings")
            .into_any_element()
    }
}
