//! The shared UI system: buttons, labels, badges, icons and the text field.
//! Every component reads the active [`crate::theme::Theme`] at render time.
//! It is a small component kit, so not every option is used by every screen.
#![allow(dead_code)]

pub mod select;
pub mod text_field;
pub mod tooltip;

use std::rc::Rc;
use std::time::Duration;

use gpui::{
    Animation, AnimationExt as _, App, ClickEvent, Div, ElementId, Hsla, IntoElement,
    ParentElement, Pixels, RenderOnce, SharedString, Stateful, Styled, Svg, Transformation, Window,
    div, percentage, prelude::*, px, svg,
};

pub use select::{Select, SelectEvent};
pub use text_field::{TextField, TextFieldEvent, TextFieldOptions};

use crate::theme::{FONT_MONO, FONT_UI, theme};

/// A Lucide icon from the embedded asset set, tinted by the ambient text colour.
pub fn icon(name: &'static str, size: f32) -> Svg {
    svg()
        .path(SharedString::from(format!("icons/{name}.svg")))
        .size(px(size))
        .flex_none()
        .text_color(crate::theme::icon_default())
}

/// A spinning loader icon.
pub fn spinner(id: impl Into<ElementId>, size: f32, color: Hsla) -> impl IntoElement {
    icon("loader-circle", size)
        .text_color(color)
        .with_animation(
            id,
            Animation::new(Duration::from_millis(900)).repeat(),
            |svg, delta| svg.with_transformation(Transformation::rotate(percentage(delta))),
        )
}

/// A small circular status indicator.
pub fn status_dot(color: Hsla, size: f32) -> Div {
    div().flex_none().size(px(size)).rounded_full().bg(color)
}

/// `.eyebrow`: tracked, muted, monospace micro-label.
pub fn eyebrow(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .font_family(FONT_MONO)
        .text_size(px(10.))
        .line_height(px(12.))
        .font_weight(gpui::FontWeight::BOLD)
        .text_color(t.ink_muted)
        .whitespace_nowrap()
        .child(spaced(text.into()))
}

/// Simulates CSS letter-spacing for short tracked labels by inserting hair spaces.
pub fn spaced(text: SharedString) -> SharedString {
    let mut out = String::with_capacity(text.len() * 2);
    for (i, ch) in text.chars().enumerate() {
        if i > 0 {
            out.push('\u{200A}');
        }
        out.push(ch);
    }
    out.into()
}

/// `.section-header h2` style heading.
pub fn section_heading(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .font_family(FONT_MONO)
        .text_size(px(10.))
        .line_height(px(13.))
        .font_weight(gpui::FontWeight::EXTRA_BOLD)
        .text_color(t.ink_muted)
        .whitespace_nowrap()
        .child(spaced(text.into().to_uppercase().into()))
}

/// `.hint-text`.
pub fn hint(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .text_color(t.ink_muted)
        .text_size(px(12.))
        .line_height(px(18.))
        .child(text.into())
}

/// A monospace inline code span.
pub fn code(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .font_family(FONT_MONO)
        .text_size(px(11.))
        .text_color(t.ink_secondary)
        .child(text.into())
}

/// Keyboard key cap.
pub fn kbd(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .flex_none()
        .px(px(5.))
        .py(px(1.))
        .border_1()
        .border_color(t.border)
        .rounded(px(4.))
        .font_family(FONT_MONO)
        .text_size(px(10.))
        .text_color(t.ink_muted)
        .child(text.into())
}

/// A rounded count badge.
pub fn count_badge(text: impl Into<SharedString>, cx: &App) -> Div {
    let t = theme(cx);
    div()
        .flex_none()
        .px(px(7.))
        .py(px(1.))
        .rounded(px(10.))
        .bg(t.surface2)
        .font_family(FONT_MONO)
        .text_size(px(10.))
        .text_color(t.ink_secondary)
        .child(text.into())
}

/// A `.panel-error` block.
pub fn panel_error(
    message: impl Into<SharedString>,
    detail: Option<SharedString>,
    cx: &App,
) -> Div {
    let t = theme(cx);
    div()
        .w_full()
        .px(px(14.))
        .py(px(12.))
        .rounded(px(5.))
        .bg(t.mix(t.danger, crate::theme::transparent(), 0.14))
        .border_1()
        .border_color(t.mix(t.danger, crate::theme::transparent(), 0.45))
        .flex()
        .flex_col()
        .gap(px(6.))
        .child(
            div()
                .font_weight(gpui::FontWeight::SEMIBOLD)
                .text_size(px(12.5))
                .text_color(t.ink)
                .child(message.into()),
        )
        .when_some(detail, |this, detail| {
            this.child(
                div()
                    .font_family(FONT_MONO)
                    .text_size(px(11.5))
                    .line_height(px(17.))
                    .text_color(t.ink_secondary)
                    .child(detail),
            )
        })
}

/// A feedback banner (success or error).
pub fn feedback(message: impl Into<SharedString>, success: bool, cx: &App) -> Div {
    let t = theme(cx);
    let color = if success { t.good } else { t.danger };
    div()
        .w_full()
        .px(px(10.))
        .py(px(9.))
        .rounded(px(8.))
        .text_size(px(11.))
        .line_height(px(16.))
        .text_color(color)
        .bg(t.mix(color, crate::theme::transparent(), 0.10))
        .border_1()
        .border_color(t.mix(color, crate::theme::transparent(), 0.20))
        .child(message.into())
}

/// A hairline divider.
pub fn divider(cx: &App) -> Div {
    div().w_full().h(px(1.)).bg(theme(cx).hairline)
}

/// Button variants, following the shadcn-style set of the original app.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ButtonKind {
    /// Bordered, transparent background.
    Outline,
    /// No border until hovered.
    Ghost,
    /// Copper action button.
    Primary,
    /// Accent-coloured text only.
    Link,
    /// Muted link text (secondary target in a list).
    MutedLink,
    /// Danger text.
    Destructive,
}

type ClickHandler = Rc<dyn Fn(&ClickEvent, &mut Window, &mut App)>;

/// A clickable control.
#[derive(IntoElement)]
pub struct Button {
    id: ElementId,
    kind: ButtonKind,
    label: Option<SharedString>,
    icon_before: Option<&'static str>,
    icon_after: Option<&'static str>,
    icon_size: f32,
    icon_color: Option<Hsla>,
    disabled: bool,
    loading: bool,
    active: bool,
    height: Pixels,
    padding_x: Pixels,
    font_size: Pixels,
    mono: bool,
    full_width: bool,
    justify_start: bool,
    text_color: Option<Hsla>,
    on_click: Option<ClickHandler>,
}

impl Button {
    /// A button with an id and a variant.
    pub fn new(id: impl Into<ElementId>, kind: ButtonKind) -> Self {
        Self {
            id: id.into(),
            kind,
            label: None,
            icon_before: None,
            icon_after: None,
            icon_size: 14.,
            icon_color: None,
            disabled: false,
            loading: false,
            active: false,
            height: px(30.),
            padding_x: px(11.),
            font_size: px(12.),
            mono: false,
            full_width: false,
            justify_start: false,
            text_color: None,
            on_click: None,
        }
    }

    /// Visible text.
    pub fn label(mut self, label: impl Into<SharedString>) -> Self {
        self.label = Some(label.into());
        self
    }

    /// Icon before the text.
    pub fn icon(mut self, name: &'static str) -> Self {
        self.icon_before = Some(name);
        self
    }

    /// Icon after the text.
    pub fn icon_after(mut self, name: &'static str) -> Self {
        self.icon_after = Some(name);
        self
    }

    /// Icon dimension in pixels.
    pub fn icon_size(mut self, size: f32) -> Self {
        self.icon_size = size;
        self
    }

    /// Tint the icon independently of the text.
    pub fn icon_color(mut self, color: Hsla) -> Self {
        self.icon_color = Some(color);
        self
    }

    /// Dims the button and drops the click handler.
    pub fn disabled(mut self, disabled: bool) -> Self {
        self.disabled = disabled;
        self
    }

    /// Replaces the leading icon with a spinner.
    pub fn loading(mut self, loading: bool) -> Self {
        self.loading = loading;
        self
    }

    /// Draws the pressed/selected state.
    pub fn active(mut self, active: bool) -> Self {
        self.active = active;
        self
    }

    /// Control height.
    pub fn height(mut self, height: Pixels) -> Self {
        self.height = height;
        self
    }

    /// Horizontal padding.
    pub fn padding_x(mut self, padding: Pixels) -> Self {
        self.padding_x = padding;
        self
    }

    /// Label size.
    pub fn font_size(mut self, size: Pixels) -> Self {
        self.font_size = size;
        self
    }

    /// Monospace label.
    pub fn mono(mut self, mono: bool) -> Self {
        self.mono = mono;
        self
    }

    /// Stretch to the container width.
    pub fn full_width(mut self, full: bool) -> Self {
        self.full_width = full;
        self
    }

    /// Left-align the content instead of centring it.
    pub fn justify_start(mut self, start: bool) -> Self {
        self.justify_start = start;
        self
    }

    /// Override the text colour.
    pub fn text_color(mut self, color: Hsla) -> Self {
        self.text_color = Some(color);
        self
    }

    /// Click handler; ignored while disabled.
    pub fn on_click(
        mut self,
        handler: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    ) -> Self {
        self.on_click = Some(Rc::new(handler));
        self
    }
}

impl RenderOnce for Button {
    fn render(self, _window: &mut Window, cx: &mut App) -> impl IntoElement {
        let t = *theme(cx);
        let transparent = crate::theme::transparent();
        let (bg, border, fg, hover_bg, hover_border) = match self.kind {
            ButtonKind::Outline => (
                transparent,
                t.border,
                t.ink,
                t.mix(t.surface2, t.accent, 0.84),
                t.mix(t.accent, t.border, 0.38),
            ),
            ButtonKind::Ghost => (
                transparent,
                transparent,
                t.ink_secondary,
                t.surface2,
                transparent,
            ),
            ButtonKind::Primary => (
                t.accent,
                t.accent,
                t.accent_ink,
                t.mix(t.accent, t.ink, 0.84),
                transparent,
            ),
            ButtonKind::Link => (transparent, transparent, t.accent, transparent, transparent),
            ButtonKind::MutedLink => (
                transparent,
                transparent,
                t.ink_secondary,
                transparent,
                transparent,
            ),
            ButtonKind::Destructive => (
                transparent,
                t.border,
                t.danger,
                t.mix(t.danger, transparent, 0.10),
                t.danger,
            ),
        };
        let fg = self.text_color.unwrap_or(fg);
        let is_link = matches!(self.kind, ButtonKind::Link | ButtonKind::MutedLink);
        let disabled = self.disabled || self.loading;
        let icon_color = self.icon_color.unwrap_or(fg);
        let font_family = if self.mono { FONT_MONO } else { FONT_UI };
        let on_click = self.on_click.clone();

        let mut base: Stateful<Div> = div()
            .id(self.id)
            .flex()
            .flex_none()
            .items_center()
            .gap(px(7.))
            .h(self.height)
            .rounded(px(4.))
            .border_1()
            .border_color(border)
            .bg(bg)
            .font_family(font_family)
            .text_size(self.font_size)
            .line_height(px(16.))
            .font_weight(gpui::FontWeight::SEMIBOLD)
            .text_color(fg)
            .whitespace_nowrap()
            .overflow_hidden();
        if is_link {
            base = base.px(px(0.)).h(px(18.)).border_0().rounded(px(0.));
        } else {
            base = base.px(self.padding_x);
        }
        if self.full_width {
            base = base.w_full();
        }
        base = if self.justify_start {
            base.justify_start()
        } else {
            base.justify_center()
        };
        if self.active {
            base = base
                .bg(t.mix(t.accent, transparent, 0.09))
                .border_color(t.mix(t.accent, transparent, 0.20))
                .text_color(t.accent);
        }
        if disabled {
            base = base.opacity(0.45);
        } else {
            base = base
                .cursor_pointer()
                .hover(move |style| {
                    let style = style.bg(hover_bg).border_color(hover_border);
                    if is_link { style.underline() } else { style }
                })
                .active(|style| style.opacity(0.85));
            if let Some(handler) = on_click {
                base = base.on_click(move |event, window, cx| handler(event, window, cx));
            }
        }
        if self.loading {
            base = base.child(spinner("button-spinner", self.icon_size, icon_color));
        } else if let Some(name) = self.icon_before {
            base = base.child(icon(name, self.icon_size).text_color(icon_color));
        }
        if let Some(label) = self.label {
            base = base.child(div().truncate().child(label));
        }
        if let Some(name) = self.icon_after {
            base = base.child(icon(name, self.icon_size).text_color(icon_color));
        }
        base
    }
}

/// A horizontal bar chart row set, the right form for comparing label counts.
pub fn count_bars(
    id: &'static str,
    title: impl Into<SharedString>,
    groups: &[(String, u64)],
    colors: &[Hsla],
    cx: &App,
) -> Div {
    let t = *theme(cx);
    let max = groups.iter().map(|(_, c)| *c).max().unwrap_or(1).max(1);
    let mut list = div().flex().flex_col().gap(px(4.)).w_full();
    if groups.is_empty() {
        list = list.child(hint("Nothing to show.", cx));
    }
    for (index, (label, count)) in groups.iter().enumerate() {
        #[allow(clippy::cast_precision_loss)]
        let fraction = ((*count as f32) / (max as f32)).max(0.015);
        let color = colors.get(index).copied().unwrap_or(t.ink_muted);
        list = list.child(
            div()
                .id((id, index))
                .flex()
                .items_center()
                .gap(px(8.))
                .w_full()
                .child(
                    div()
                        .w(px(130.))
                        .flex_none()
                        .truncate()
                        .text_size(px(12.))
                        .text_color(t.ink_secondary)
                        .child(SharedString::from(label.clone())),
                )
                .child(
                    div()
                        .flex_1()
                        .h(px(10.))
                        .rounded(px(2.))
                        .bg(t.surface2)
                        .overflow_hidden()
                        .child(
                            div()
                                .h_full()
                                .w(gpui::relative(fraction))
                                .rounded(px(2.))
                                .bg(color),
                        ),
                )
                .child(
                    div()
                        .flex_none()
                        .min_w(px(40.))
                        .text_right()
                        .font_family(FONT_MONO)
                        .text_size(px(11.))
                        .text_color(t.ink_secondary)
                        .child(SharedString::from(format_count(*count))),
                ),
        );
    }
    div()
        .flex()
        .flex_col()
        .gap(px(8.))
        .min_w(px(260.))
        .flex_1()
        .child(section_heading(title, cx))
        .child(list)
}

/// `1,234` style thousands separators.
#[must_use]
pub fn format_count(count: u64) -> String {
    let digits = count.to_string();
    let mut out = String::with_capacity(digits.len() + digits.len() / 3);
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}

/// `12.3 ms` formatting of a duration.
#[must_use]
pub fn format_ms(ms: f64) -> String {
    if ms < 10. {
        format!("{ms:.2} ms")
    } else if ms < 1000. {
        format!("{ms:.0} ms")
    } else {
        format!("{:.2} s", ms / 1000.)
    }
}

/// A big number tile (`.stat-tile`).
pub fn stat_tile(
    value: impl Into<SharedString>,
    caption: impl Into<SharedString>,
    last: bool,
    cx: &App,
) -> Div {
    let t = theme(cx);
    div()
        .flex()
        .flex_col()
        .gap(px(2.))
        .py(px(12.))
        .pr(px(32.))
        .when(!last, |d| d.border_r_1().border_color(t.border))
        .child(
            div()
                .font_family(FONT_MONO)
                .font_weight(gpui::FontWeight::BOLD)
                .text_size(px(36.))
                .line_height(px(40.))
                .text_color(t.ink)
                .child(value.into()),
        )
        .child(
            div()
                .text_size(px(11.))
                .text_color(t.ink_muted)
                .child(spaced(caption.into().to_uppercase().into())),
        )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn formats_counts_with_separators() {
        assert_eq!(format_count(0), "0");
        assert_eq!(format_count(999), "999");
        assert_eq!(format_count(1000), "1,000");
        assert_eq!(format_count(1234567), "1,234,567");
    }
}
