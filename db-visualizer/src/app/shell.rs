//! The application frame: header with connection controls, the navigation
//! rail, the active workspace, the status strip and the connection sheet.

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, SharedString, Window, div, prelude::*, px,
};

use super::{
    Atlas, OpenConnection, Quit, RunQuery, ShowGraph, ShowQuery, ShowSchema, Status, ToggleTheme,
    View,
};
use crate::theme::{FONT_DISPLAY, FONT_MONO, FONT_UI, theme, transparent};
use crate::ui::{icon, spaced, status_dot};

pub const RAIL_WIDTH: f32 = 82.;
pub const HEADER_HEIGHT: f32 = 64.;
pub const STATUS_HEIGHT: f32 = 28.;

impl Atlas {
    fn status_colour(&self, cx: &Context<Self>) -> gpui::Hsla {
        let t = theme(cx);
        match self.status {
            Status::Connected { .. } => t.good,
            Status::Checking => t.warning,
            Status::Failed(_) => t.danger,
            Status::Disconnected => t.ink_muted,
        }
    }

    fn status_text(&self) -> String {
        match &self.status {
            Status::Disconnected => "Not connected".to_owned(),
            Status::Checking => "Connecting…".to_owned(),
            Status::Failed(_) => "Connection failed".to_owned(),
            Status::Connected { ms, version } => {
                let host = self
                    .connection
                    .as_ref()
                    .map(|c| c.short_host())
                    .unwrap_or_default();
                format!("{host} · v{version} · {}", crate::ui::format_ms(*ms))
            }
        }
    }

    fn render_header(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme(cx);
        let connected = self.status.is_connected();
        let context = match self.view {
            View::Query => "Query workspace",
            View::Schema => "Schema catalog",
            View::Graph => "Graph explorer",
        };
        let status_colour = self.status_colour(cx);
        let summary_colour = match self.status {
            Status::Failed(_) => t.danger,
            Status::Checking => t.warning,
            Status::Connected { .. } => t.ink_secondary,
            Status::Disconnected => t.ink_muted,
        };
        let failure = if let Status::Failed(message) = &self.status {
            Some(message.clone())
        } else {
            None
        };
        div()
            .flex_none()
            .h(px(HEADER_HEIGHT))
            .flex()
            .items_center()
            .bg(t.toolbar)
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .px(px(24.))
                    .flex()
                    .items_center()
                    .gap(px(28.))
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(10.))
                            .text_size(px(18.))
                            .text_color(t.ink)
                            .whitespace_nowrap()
                            .child(icon("compass", 28.).text_color(t.accent))
                            .child(
                                div()
                                    .flex()
                                    .child(div().font_weight(FontWeight::SEMIBOLD).child("Nexa"))
                                    .child(
                                        div()
                                            .font_weight(FontWeight::NORMAL)
                                            .child("\u{00A0}Atlas"),
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .gap(px(22.))
                            .text_size(px(12.))
                            .text_color(t.ink_muted)
                            .whitespace_nowrap()
                            .child("/")
                            .child(div().text_color(t.ink_secondary).child(context)),
                    ),
            )
            .child(
                div()
                    .h_full()
                    .pr(px(22.))
                    .flex()
                    .items_center()
                    .gap(px(18.))
                    .child(
                        div()
                            .id("connection-summary")
                            .max_w(px(300.))
                            .flex()
                            .items_center()
                            .gap(px(7.))
                            .font_family(FONT_MONO)
                            .text_size(px(10.))
                            .text_color(summary_colour)
                            .child(status_dot(status_colour, 6.))
                            .child(
                                div()
                                    .truncate()
                                    .child(SharedString::from(self.status_text())),
                            )
                            .when_some(failure, |d, message| {
                                d.tooltip(move |_, cx| {
                                    crate::ui::tooltip::Tooltip::text(message.clone(), cx)
                                })
                            }),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(5.))
                            .text_size(px(10.))
                            .text_color(t.ink_muted)
                            .whitespace_nowrap()
                            .child(icon("lock-keyhole", 12.).text_color(t.ink_muted))
                            .child("Explicit writes only"),
                    )
                    .child(
                        div()
                            .id("connection-button")
                            .h(px(32.))
                            .px(px(11.))
                            .flex()
                            .items_center()
                            .gap(px(8.))
                            .rounded(px(4.))
                            .border_1()
                            .border_color(t.border)
                            .bg(t.surface0)
                            .text_size(px(12.))
                            .font_weight(if connected {
                                FontWeight::BOLD
                            } else {
                                FontWeight::SEMIBOLD
                            })
                            .text_color(if connected { t.good } else { t.ink_secondary })
                            .cursor_pointer()
                            .hover(move |s| s.border_color(t.mix(t.accent, t.border, 0.38)))
                            .on_click(
                                cx.listener(|this, _, window, cx| this.open_connection(window, cx)),
                            )
                            .child(
                                icon(if connected { "plug-zap" } else { "plug" }, 15.)
                                    .text_color(if connected { t.good } else { t.ink_muted }),
                            )
                            .child("Connection"),
                    )
                    .child(
                        div()
                            .id("theme-toggle")
                            .size(px(28.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(4.))
                            .text_color(t.ink_secondary)
                            .cursor_pointer()
                            .hover(move |s| s.bg(t.surface2))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_theme(cx)))
                            .child(icon(
                                if t.mode == crate::theme::Mode::Dark {
                                    "sun"
                                } else {
                                    "moon"
                                },
                                17.,
                            )),
                    ),
            )
            .into_any_element()
    }

    fn render_rail(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme(cx);
        let mut rail = div()
            .flex_none()
            .w(px(RAIL_WIDTH))
            .h_full()
            .flex()
            .flex_col()
            .gap(px(9.))
            .py(px(26.))
            .px(px(8.))
            .bg(t.surface1)
            .border_r_1()
            .border_color(t.border);
        for (view, label, name) in [
            (View::Query, "Query", "terminal-square"),
            (View::Schema, "Schema", "database"),
            (View::Graph, "Graph", "network"),
        ] {
            let active = self.view == view;
            rail = rail.child(
                div()
                    .id(label)
                    .relative()
                    .h(px(62.))
                    .flex()
                    .flex_col()
                    .items_center()
                    .justify_center()
                    .gap(px(7.))
                    .rounded(px(5.))
                    .border_1()
                    .border_color(if active {
                        t.mix(t.accent, transparent(), 0.20)
                    } else {
                        transparent()
                    })
                    .bg(if active {
                        t.mix(t.accent, transparent(), 0.09)
                    } else {
                        transparent()
                    })
                    .text_color(if active { t.accent } else { t.ink_secondary })
                    .cursor_pointer()
                    .when(!active, |d| {
                        d.hover(move |s| s.bg(t.surface2).text_color(t.ink))
                    })
                    .on_click(cx.listener(move |this, _, _, cx| this.set_view(view, cx)))
                    .when(active, |d| {
                        d.child(
                            div()
                                .absolute()
                                .left(px(-9.))
                                .top(px(17.))
                                .bottom(px(17.))
                                .w(px(3.))
                                .bg(t.accent),
                        )
                    })
                    .child(icon(name, 20.).text_color(if active {
                        t.accent
                    } else {
                        t.ink_secondary
                    }))
                    .child(
                        div()
                            .text_size(px(11.))
                            .font_weight(FontWeight::SEMIBOLD)
                            .child(label),
                    ),
            );
        }
        rail.into_any_element()
    }

    fn render_status(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme(cx);
        let workspace = match self.view {
            View::Query => "QUERY WORKSPACE",
            View::Schema => "SCHEMA WORKSPACE",
            View::Graph => "GRAPH WORKSPACE",
        };
        let left = match &self.status {
            Status::Connected { .. } => format!(
                "NexaDB workspace · {}",
                self.connection
                    .as_ref()
                    .map(|c| c.query_endpoint())
                    .unwrap_or_default()
            ),
            Status::Failed(message) => format!("Connection failed · {message}"),
            Status::Checking => "Connecting…".to_owned(),
            Status::Disconnected => format!("Not connected · session {}s", self.uptime()),
        };
        div()
            .flex_none()
            .h(px(STATUS_HEIGHT))
            .px(px(20.))
            .flex()
            .items_center()
            .justify_between()
            .gap(px(12.))
            .border_t_1()
            .border_color(t.border)
            .bg(t.toolbar)
            .font_family(FONT_MONO)
            .text_size(px(9.))
            .text_color(t.ink_muted)
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .min_w_0()
                    .child(status_dot(self.status_colour(cx), 6.))
                    .child(div().truncate().child(SharedString::from(left))),
            )
            .child(
                div()
                    .flex()
                    .gap(px(6.))
                    .whitespace_nowrap()
                    .child("NQL")
                    .child(div().opacity(0.5).child("/"))
                    .child(workspace),
            )
            .into_any_element()
    }

    pub(super) fn render_shell(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> impl IntoElement {
        let t = *theme(cx);
        let workspace = match self.view {
            View::Query => self.render_query(window, cx),
            View::Schema => self.render_schema(window, cx),
            View::Graph => self.render_graph(window, cx),
        };
        let header = self.render_header(cx);
        let rail = self.render_rail(cx);
        let status = self.render_status(cx);
        let dialog = self.dialog.clone();
        div()
            .id("atlas")
            .key_context("Atlas")
            .track_focus(&self.focus_handle)
            .on_action(cx.listener(|_, _: &Quit, _, cx| cx.quit()))
            .on_action(cx.listener(|this, _: &ToggleTheme, _, cx| this.toggle_theme(cx)))
            .on_action(cx.listener(|this, _: &ShowQuery, _, cx| this.set_view(View::Query, cx)))
            .on_action(cx.listener(|this, _: &ShowSchema, _, cx| this.set_view(View::Schema, cx)))
            .on_action(cx.listener(|this, _: &ShowGraph, _, cx| this.set_view(View::Graph, cx)))
            .on_action(
                cx.listener(|this, _: &OpenConnection, window, cx| {
                    this.open_connection(window, cx)
                }),
            )
            .on_action(cx.listener(|this, _: &RunQuery, window, cx| this.run_query(window, cx)))
            .relative()
            .size_full()
            .flex()
            .flex_col()
            .bg(t.window)
            .font_family(FONT_UI)
            .text_size(px(13.))
            .text_color(t.ink)
            .child(header)
            .child(
                div().flex_1().min_h_0().flex().child(rail).child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .h_full()
                        .bg(t.surface0)
                        .child(workspace),
                ),
            )
            .child(status)
            .when_some(dialog, |d, dialog| {
                d.child(
                    div()
                        .id("dialog-overlay")
                        .absolute()
                        .inset_0()
                        .occlude()
                        .flex()
                        .items_center()
                        .justify_center()
                        .p(px(24.))
                        .bg(t.overlay)
                        .child(dialog),
                )
            })
    }
}

/// A display-face heading (`Iowan Old Style`).
pub fn display_heading(text: impl Into<SharedString>, size: f32, cx: &gpui::App) -> gpui::Div {
    let t = theme(cx);
    div()
        .font_family(FONT_DISPLAY)
        .text_size(px(size))
        .line_height(px(size * 1.15))
        .text_color(t.ink)
        .child(text.into())
}

/// The `WORKSPACE / …` eyebrow.
pub fn workspace_eyebrow(text: &str, cx: &gpui::App) -> gpui::Div {
    let t = theme(cx);
    div()
        .font_family(FONT_MONO)
        .font_weight(FontWeight::BOLD)
        .text_size(px(10.))
        .text_color(t.ink_muted)
        .whitespace_nowrap()
        .child(spaced(SharedString::from(text.to_owned())))
}
