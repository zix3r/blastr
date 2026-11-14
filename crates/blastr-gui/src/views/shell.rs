//! The shell view of the main window.

use std::path::PathBuf;

use gpui_kit::component::theme::ActiveTheme;
use gpui_kit::{Context, IntoElement, ParentElement, Render, Styled, Window, div, px};

/// The root view of the main window. A placeholder until the real interface is built.
pub(crate) struct Shell {
    repository: Option<PathBuf>,
}

impl Shell {
    /// Creates the shell for the repository root, if one was found.
    pub(crate) fn new(repository: Option<PathBuf>) -> Self {
        Self { repository }
    }
}

impl Render for Shell {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let theme = cx.theme();
        let detail = match &self.repository {
            Some(path) => path.display().to_string(),
            None => "No repository open".to_string(),
        };

        div()
            .flex()
            .flex_col()
            .gap_2()
            .size_full()
            .items_center()
            .justify_center()
            .bg(theme.background)
            .child(div().text_size(px(28.)).child("Blastr"))
            .child(div().text_color(theme.muted_foreground).child(detail))
    }
}
