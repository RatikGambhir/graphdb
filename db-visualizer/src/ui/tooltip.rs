//! A small tooltip view.

use gpui::{AnyView, App, Context, IntoElement, Render, SharedString, Window, div, prelude::*, px};

use crate::theme::theme;

/// A tooltip that shows one block of text.
pub struct Tooltip {
    text: SharedString,
}

impl Tooltip {
    /// Builds the tooltip view for `.tooltip(...)`.
    pub fn text(text: impl Into<SharedString>, cx: &mut App) -> AnyView {
        let text = text.into();
        cx.new(|_| Self { text }).into()
    }
}

impl Render for Tooltip {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme(cx);
        div()
            .max_w(px(360.))
            .px(px(9.))
            .py(px(6.))
            .rounded(px(4.))
            .border_1()
            .border_color(t.border)
            .bg(t.elevated)
            .shadow_md()
            .text_size(px(11.))
            .line_height(px(16.))
            .text_color(t.ink_secondary)
            .child(self.text.clone())
    }
}
