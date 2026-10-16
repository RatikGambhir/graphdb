//! A dropdown select: a bordered control that opens a floating option list.

use gpui::{
    App, Context, Corner, EventEmitter, FocusHandle, Focusable, IntoElement, MouseButton, Render,
    SharedString, Window, anchored, deferred, div, point, prelude::*, px,
};

use crate::theme::{FONT_UI, theme};
use crate::ui::icon;

/// Emitted when the selection changes.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SelectEvent {
    Changed(usize),
}

/// The dropdown state.
pub struct Select {
    focus_handle: FocusHandle,
    options: Vec<SharedString>,
    pub selected: usize,
    open: bool,
    pub height: gpui::Pixels,
    pub font_size: gpui::Pixels,
}

impl EventEmitter<SelectEvent> for Select {}

impl Focusable for Select {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

impl Select {
    /// A select over fixed options.
    pub fn new(options: Vec<SharedString>, selected: usize, cx: &mut Context<Self>) -> Self {
        Self {
            focus_handle: cx.focus_handle(),
            options,
            selected: selected.min(options_len(&[]).max(0)),
            open: false,
            height: px(37.),
            font_size: px(12.5),
        }
        .with_selected(selected)
    }

    fn with_selected(mut self, selected: usize) -> Self {
        self.selected = selected.min(self.options.len().saturating_sub(1));
        self
    }

    /// The selected option's text.
    #[must_use]
    pub fn value(&self) -> SharedString {
        self.options.get(self.selected).cloned().unwrap_or_default()
    }

    /// Programmatic selection.
    #[allow(dead_code)]
    pub fn set_selected(&mut self, index: usize, cx: &mut Context<Self>) {
        self.selected = index.min(self.options.len().saturating_sub(1));
        cx.notify();
    }

    fn choose(&mut self, index: usize, cx: &mut Context<Self>) {
        self.open = false;
        if index != self.selected {
            self.selected = index;
            cx.emit(SelectEvent::Changed(index));
        }
        cx.notify();
    }
}

fn options_len(options: &[SharedString]) -> usize {
    options.len()
}

impl Render for Select {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme(cx);
        let open = self.open;
        let height = self.height;
        let transparent = crate::theme::transparent();
        let mut root = div()
            .id("select")
            .relative()
            .track_focus(&self.focus_handle)
            .w_full()
            .h(height)
            .flex()
            .items_center()
            .justify_between()
            .gap(px(8.))
            .px(px(10.))
            .rounded(px(4.))
            .border_1()
            .border_color(if open {
                t.mix(t.accent, t.border, 0.48)
            } else {
                t.border
            })
            .bg(t.control)
            .font_family(FONT_UI)
            .text_size(self.font_size)
            .text_color(t.ink)
            .cursor_pointer()
            .hover(move |s| s.border_color(t.mix(t.accent, t.border, 0.38)))
            .on_click(cx.listener(|this, _, _, cx| {
                this.open = !this.open;
                cx.notify();
            }))
            .child(div().truncate().child(self.value()))
            .child(icon("chevron-down", 14.).text_color(t.ink_muted));

        if open {
            let options = self.options.clone();
            let selected = self.selected;
            let mut list = div()
                .id("select-menu")
                .occlude()
                .min_w(px(180.))
                .flex()
                .flex_col()
                .p(px(4.))
                .rounded(px(6.))
                .border_1()
                .border_color(t.border)
                .bg(t.elevated)
                .shadow_lg()
                .on_mouse_down_out(cx.listener(|this, _, _, cx| {
                    this.open = false;
                    cx.notify();
                }));
            for (index, option) in options.into_iter().enumerate() {
                let is_selected = index == selected;
                list = list.child(
                    div()
                        .id(("select-option", index))
                        .flex()
                        .items_center()
                        .justify_between()
                        .gap(px(10.))
                        .px(px(9.))
                        .py(px(6.))
                        .rounded(px(4.))
                        .text_size(px(12.))
                        .text_color(if is_selected { t.accent } else { t.ink })
                        .bg(if is_selected {
                            t.mix(t.accent, transparent, 0.08)
                        } else {
                            transparent
                        })
                        .cursor_pointer()
                        .hover(move |s| s.bg(t.surface2))
                        .on_mouse_down(
                            MouseButton::Left,
                            cx.listener(move |this, _, _, cx| {
                                this.choose(index, cx);
                            }),
                        )
                        .child(div().truncate().child(option))
                        .when(is_selected, |d| {
                            d.child(icon("check", 12.).text_color(t.accent))
                        }),
                );
            }
            root = root.child(deferred(
                anchored()
                    .anchor(Corner::TopLeft)
                    .offset(point(px(0.), height + px(4.)))
                    .snap_to_window_with_margin(px(8.))
                    .child(list),
            ));
        }
        root
    }
}
