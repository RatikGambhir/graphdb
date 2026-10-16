//! The welcome splash: a valid RQL graph query types itself into an editor
//! window while a progress bar fills, then a Continue button enters the app.

use std::time::Duration;

use gpui::{
    Context, EventEmitter, FocusHandle, FontWeight, Hsla, IntoElement, Render, SharedString,
    Window, div, prelude::*, px, relative,
};

use super::ContinueSplash;
use crate::rql::{PreviewKind, SPLASH_PREVIEW};
use crate::theme::{FONT_MONO, FONT_UI, theme};
use crate::ui::{Button, ButtonKind, icon, spaced};

/// Emitted when the user continues.
pub struct SplashDone;

const MIN_DURATION_MS: u64 = 2_000;
const TICK_MS: u64 = 70;

/// The splash state.
pub struct Splash {
    focus_handle: FocusHandle,
    visible_lines: usize,
    progress: f32,
    ready: bool,
    elapsed_ms: u64,
}

impl EventEmitter<SplashDone> for Splash {}

impl Splash {
    /// Starts the animation.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let focus_handle = cx.focus_handle();
        window.focus(&focus_handle);
        cx.spawn(async move |this, cx| {
            let line_delay = MIN_DURATION_MS / (SPLASH_PREVIEW.len() as u64 + 2);
            loop {
                cx.background_executor()
                    .timer(Duration::from_millis(TICK_MS))
                    .await;
                let done = this
                    .update(cx, |this, cx| {
                        this.elapsed_ms += TICK_MS;
                        this.progress =
                            (this.progress + ((94. - this.progress) * 0.09).max(1.)).min(94.);
                        #[allow(clippy::cast_possible_truncation)]
                        let lines = (this.elapsed_ms / line_delay) as usize;
                        this.visible_lines = lines.min(SPLASH_PREVIEW.len());
                        if this.elapsed_ms >= MIN_DURATION_MS {
                            this.visible_lines = SPLASH_PREVIEW.len();
                            this.progress = 100.;
                            this.ready = true;
                        }
                        cx.notify();
                        this.ready
                    })
                    .unwrap_or(true);
                if done {
                    break;
                }
            }
        })
        .detach();
        Self {
            focus_handle,
            visible_lines: 0,
            progress: 0.,
            ready: false,
            elapsed_ms: 0,
        }
    }

    fn finish(&mut self, cx: &mut Context<Self>) {
        if self.ready {
            cx.emit(SplashDone);
        }
    }

    fn task_label(&self) -> &'static str {
        if self.progress < 34. {
            "Preparing RQL"
        } else if self.progress < 70. {
            "Restoring workspace"
        } else {
            "Finalizing visualizer"
        }
    }
}

impl Render for Splash {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme(cx);
        let syntax = t.syntax;
        let colour = |kind: PreviewKind| -> Hsla {
            match kind {
                PreviewKind::Keyword => syntax.keyword,
                PreviewKind::Entity => syntax.entity,
                PreviewKind::Label => syntax.label,
                PreviewKind::Property => syntax.property,
                PreviewKind::Direction => syntax.direction,
                PreviewKind::Operator => syntax.operator,
                PreviewKind::Literal => syntax.literal,
                PreviewKind::Number => syntax.number,
                PreviewKind::Punctuation => syntax.punctuation,
            }
        };

        let mut editor = div()
            .relative()
            .min_h(px(190.))
            .pt(px(16.))
            .pb(px(22.))
            .pr(px(24.))
            .bg(t.surface0)
            .font_family(FONT_MONO)
            .text_size(px(12.5));
        for (index, line) in SPLASH_PREVIEW.iter().enumerate() {
            let visible = index < self.visible_lines;
            let mut code = div().flex().whitespace_nowrap();
            for token in *line {
                let weight = match token.kind {
                    PreviewKind::Keyword | PreviewKind::Operator => FontWeight::BOLD,
                    PreviewKind::Punctuation => FontWeight::NORMAL,
                    _ => FontWeight::SEMIBOLD,
                };
                code = code.child(
                    div()
                        .font_weight(weight)
                        .text_color(colour(token.kind))
                        .child(SharedString::from(token.text.replace(' ', "\u{00A0}"))),
                );
            }
            editor = editor.child(
                div()
                    .flex()
                    .items_center()
                    .h(px(24.))
                    .opacity(if visible { 1. } else { 0. })
                    .child(
                        div()
                            .w(px(42.))
                            .mr(px(16.))
                            .flex_none()
                            .text_right()
                            .text_size(px(10.5))
                            .text_color(t.ink_muted)
                            .child(SharedString::from((index + 1).to_string())),
                    )
                    .child(code),
            );
        }
        if self.visible_lines < SPLASH_PREVIEW.len() {
            editor = editor.child(
                div()
                    .ml(px(58.))
                    .mt(px(1.))
                    .w(px(1.5))
                    .h(px(17.))
                    .bg(t.accent),
            );
        }

        let window_frame = div()
            .w_full()
            .rounded(px(6.))
            .border_1()
            .border_color(t.border)
            .bg(t.surface0)
            .shadow_lg()
            .overflow_hidden()
            .child(
                div()
                    .h(px(38.))
                    .flex()
                    .items_center()
                    .px(px(14.))
                    .bg(t.surface1)
                    .border_b_1()
                    .border_color(t.hairline)
                    .child(
                        div()
                            .w(px(60.))
                            .flex()
                            .gap(px(7.))
                            .child(div().size(px(11.)).rounded_full().bg(gpui::rgb(0xff5f57)))
                            .child(div().size(px(11.)).rounded_full().bg(gpui::rgb(0xfebc2e)))
                            .child(div().size(px(11.)).rounded_full().bg(gpui::rgb(0x28c840))),
                    )
                    .child(
                        div()
                            .flex_1()
                            .text_center()
                            .font_family(FONT_MONO)
                            .font_weight(FontWeight::SEMIBOLD)
                            .text_size(px(9.))
                            .text_color(t.ink_muted)
                            .child(spaced("GRAPH QUERY · RQL".into())),
                    )
                    .child(div().w(px(60.))),
            )
            .child(editor);

        let progress = div()
            .w(px(270.))
            .flex()
            .flex_col()
            .items_center()
            .gap(px(17.))
            .min_h(px(56.))
            .child(
                div().w_full().h(px(2.)).bg(t.surface2).child(
                    div()
                        .h_full()
                        .w(relative(self.progress / 100.))
                        .bg(t.accent),
                ),
            )
            .child(if self.ready {
                Button::new("splash-continue", ButtonKind::Primary)
                    .label("Continue")
                    .icon_after("arrow-right")
                    .height(px(36.))
                    .padding_x(px(18.))
                    .on_click(cx.listener(|this, _, _, cx| this.finish(cx)))
                    .into_any_element()
            } else {
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(10.5))
                    .font_weight(FontWeight::MEDIUM)
                    .text_color(t.ink_muted)
                    .child(div().size(px(5.)).rounded_full().bg(t.accent))
                    .child(self.task_label())
                    .into_any_element()
            });

        div()
            .id("splash")
            .key_context("Splash")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|this, _: &ContinueSplash, _, cx| this.finish(cx)))
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(t.window)
            .font_family(FONT_UI)
            .text_color(t.ink)
            .child(
                div()
                    .w(px(570.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .gap(px(33.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .items_center()
                            .gap(px(5.))
                            .child(icon("compass", 48.).text_color(t.accent).mb(px(13.)))
                            .child(
                                div()
                                    .flex()
                                    .items_baseline()
                                    .gap(px(9.))
                                    .text_size(px(38.))
                                    .line_height(px(40.))
                                    .child(div().font_weight(FontWeight::EXTRA_BOLD).child("Ratik"))
                                    .child(
                                        div()
                                            .font_weight(FontWeight::NORMAL)
                                            .text_color(t.ink_secondary)
                                            .child("Atlas"),
                                    ),
                            )
                            .child(
                                div()
                                    .mt(px(6.))
                                    .text_size(px(13.))
                                    .text_color(t.ink_secondary)
                                    .child("A clearer view of connected data."),
                            )
                            .child(
                                div()
                                    .mt(px(16.))
                                    .font_family(FONT_MONO)
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .text_size(px(10.))
                                    .text_color(t.ink_muted)
                                    .child(spaced("GRAPH DATA WORKBENCH · RQL".into())),
                            ),
                    )
                    .child(window_frame)
                    .child(progress),
            )
    }
}
