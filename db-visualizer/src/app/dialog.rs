//! The connection sheet: local host and port, or a remote URL, an optional
//! bearer token, and a request timeout. Test probes `/v1/health` without
//! saving; Connect verifies first and only then saves.

use gpui::{
    Context, Entity, EventEmitter, FocusHandle, Focusable, FontWeight, IntoElement, Render,
    SharedString, Window, div, prelude::*, px,
};

use super::query::field_box;
use super::shell::display_heading;
use crate::client::ConnectionSettings;
use crate::theme::{FONT_MONO, theme, transparent};
use crate::ui::{Button, ButtonKind, TextField, TextFieldEvent, TextFieldOptions, feedback, icon};

/// What the sheet asks of the app.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum DialogEvent {
    Close,
    Test(ConnectionSettings),
    Connect(ConnectionSettings),
    Disconnect,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Mode {
    Local,
    Remote,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum Busy {
    Test,
    Connect,
}

/// The sheet state.
pub struct ConnectionDialog {
    focus_handle: FocusHandle,
    saved: Option<ConnectionSettings>,
    connected: bool,
    mode: Mode,
    host: Entity<TextField>,
    port: Entity<TextField>,
    url: Entity<TextField>,
    token: Entity<TextField>,
    timeout: Entity<TextField>,
    clear_token: bool,
    advanced: bool,
    busy: Option<Busy>,
    feedback: Option<Result<String, String>>,
}

impl EventEmitter<DialogEvent> for ConnectionDialog {}

impl Focusable for ConnectionDialog {
    fn focus_handle(&self, _: &gpui::App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// `http://127.0.0.1:7285` → (`127.0.0.1`, `7285`) for loopback origins.
fn split_local(url: &str) -> Option<(String, String)> {
    let rest = url.strip_prefix("http://")?.trim_end_matches('/');
    if rest.contains('/') {
        return None;
    }
    let (host, port) = rest.rsplit_once(':')?;
    let loopback = matches!(host, "127.0.0.1" | "localhost" | "[::1]");
    loopback.then(|| {
        (
            if host == "localhost" {
                "127.0.0.1".to_owned()
            } else {
                host.to_owned()
            },
            port.to_owned(),
        )
    })
}

/// Adds `https://` when the scheme is missing and trims trailing slashes.
#[must_use]
pub fn normalize_remote(url: &str) -> String {
    let trimmed = url.trim().trim_end_matches('/');
    if trimmed.contains("://") {
        trimmed.to_owned()
    } else {
        format!("https://{trimmed}")
    }
}

/// `http://host:port`, bracketing IPv6 literals.
#[must_use]
pub fn local_url(host: &str, port: &str) -> String {
    let host = host.trim();
    let host = if host.eq_ignore_ascii_case("localhost") {
        "127.0.0.1"
    } else {
        host
    };
    let host = if host.contains(':') && !host.starts_with('[') {
        format!("[{host}]")
    } else {
        host.to_owned()
    };
    format!("http://{host}:{}", port.trim())
}

impl ConnectionDialog {
    /// A sheet prefilled from the saved connection.
    pub fn new(
        saved: Option<ConnectionSettings>,
        connected: bool,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let (mode, host, port, url) = match saved.as_ref() {
            Some(s) => match split_local(&s.url) {
                Some((host, port)) => (Mode::Local, host, port, String::new()),
                None => (
                    Mode::Remote,
                    "127.0.0.1".to_owned(),
                    "7285".to_owned(),
                    s.url.clone(),
                ),
            },
            None => (
                Mode::Local,
                "127.0.0.1".to_owned(),
                "7285".to_owned(),
                String::new(),
            ),
        };
        let has_token = saved.as_ref().is_some_and(|s| s.token.is_some());
        let timeout = saved.as_ref().map_or(30_000, |s| s.timeout_ms);
        let field =
            |cx: &mut Context<Self>, placeholder: &str, text: String, mono: bool, masked: bool| {
                let entity = cx.new(|cx| {
                    TextField::new(
                        TextFieldOptions::input(placeholder.to_owned())
                            .mono(mono)
                            .masked(masked),
                        text,
                        cx,
                    )
                });
                cx.subscribe(
                    &entity,
                    |this: &mut Self, _, event: &TextFieldEvent, cx| match event {
                        TextFieldEvent::Submit => this.connect(cx),
                        TextFieldEvent::Cancel => this.close(cx),
                        TextFieldEvent::Changed => {
                            this.feedback = None;
                            cx.notify();
                        }
                    },
                )
                .detach();
                entity
            };
        let host = field(cx, "127.0.0.1", host, true, false);
        let port = field(cx, "7285", port, true, false);
        let url = field(cx, "https://nexa.example.com", url, true, false);
        let token = field(
            cx,
            if has_token {
                "•••••••• (saved)"
            } else {
                "Only if the server uses --token"
            },
            String::new(),
            true,
            true,
        );
        let timeout = field(cx, "30000", timeout.to_string(), true, false);
        let first = if mode == Mode::Local {
            host.clone()
        } else {
            url.clone()
        };
        first.update(cx, |f, _| f.focus(window));
        Self {
            focus_handle: cx.focus_handle(),
            saved,
            connected,
            mode,
            host,
            port,
            url,
            token,
            timeout,
            clear_token: false,
            advanced: false,
            busy: None,
            feedback: None,
        }
    }

    fn settings(&self, cx: &gpui::App) -> Result<ConnectionSettings, String> {
        let url = match self.mode {
            Mode::Local => {
                let host = self.host.read(cx).text().trim().to_owned();
                let port = self.port.read(cx).text().trim().to_owned();
                if host.is_empty() {
                    return Err("Enter a host.".to_owned());
                }
                if port.parse::<u16>().map_or(true, |p| p == 0) {
                    return Err("Enter a port between 1 and 65535.".to_owned());
                }
                local_url(&host, &port)
            }
            Mode::Remote => {
                let raw = self.url.read(cx).text().trim().to_owned();
                if raw.is_empty() {
                    return Err("Enter the server URL.".to_owned());
                }
                normalize_remote(&raw)
            }
        };
        let typed = self.token.read(cx).text().trim().to_owned();
        // A saved token is reused only for the same endpoint, so changing the
        // host can never forward an old credential.
        let token = if !typed.is_empty() {
            Some(typed)
        } else if self.clear_token {
            None
        } else {
            self.saved
                .as_ref()
                .filter(|s| s.url == url)
                .and_then(|s| s.token.clone())
        };
        let timeout_ms = self
            .timeout
            .read(cx)
            .text()
            .trim()
            .parse::<u64>()
            .map_err(|_| "The timeout is a whole number of milliseconds.".to_owned())?
            .clamp(1_000, 600_000);
        Ok(ConnectionSettings {
            url,
            token,
            timeout_ms,
        })
    }

    fn test(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        match self.settings(cx) {
            Ok(settings) => {
                self.busy = Some(Busy::Test);
                self.feedback = None;
                cx.emit(DialogEvent::Test(settings));
            }
            Err(message) => self.feedback = Some(Err(message)),
        }
        cx.notify();
    }

    fn connect(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_some() {
            return;
        }
        match self.settings(cx) {
            Ok(settings) => {
                self.busy = Some(Busy::Connect);
                self.feedback = None;
                cx.emit(DialogEvent::Connect(settings));
            }
            Err(message) => self.feedback = Some(Err(message)),
        }
        cx.notify();
    }

    fn close(&mut self, cx: &mut Context<Self>) {
        if self.busy.is_none() {
            cx.emit(DialogEvent::Close);
        }
    }

    /// Shows the outcome of a test or a failed connect.
    pub fn report(&mut self, outcome: Result<String, String>, cx: &mut Context<Self>) {
        self.busy = None;
        self.feedback = Some(outcome);
        cx.notify();
    }

    /// Called after a successful connect, just before the sheet is dropped.
    pub fn finish(&mut self, cx: &mut Context<Self>) {
        self.busy = None;
        cx.notify();
    }

    fn label(text: &'static str, cx: &gpui::App) -> gpui::Div {
        let t = theme(cx);
        div()
            .text_size(px(11.))
            .font_weight(FontWeight::SEMIBOLD)
            .text_color(t.ink_secondary)
            .child(text)
    }
}

impl Render for ConnectionDialog {
    fn render(&mut self, _window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let t = *theme(cx);
        let busy = self.busy;
        let has_saved_token = self.saved.as_ref().is_some_and(|s| s.token.is_some());
        let mode_card = |id: &'static str,
                         which: Mode,
                         title: &'static str,
                         detail: &'static str,
                         name: &'static str,
                         this: &Self,
                         cx: &mut Context<Self>| {
            let active = this.mode == which;
            div()
                .id(id)
                .flex_1()
                .flex()
                .items_center()
                .gap(px(9.))
                .p(px(14.))
                .border_1()
                .border_color(if active { t.accent } else { t.hairline })
                .bg(if active {
                    t.mix(t.accent, t.surface1, 0.08)
                } else {
                    transparent()
                })
                .when(which == Mode::Local, |d| d.rounded_l(px(5.)))
                .when(which == Mode::Remote, |d| d.rounded_r(px(5.)))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.mode = which;
                    this.feedback = None;
                    let target = if which == Mode::Local {
                        this.host.clone()
                    } else {
                        this.url.clone()
                    };
                    target.update(cx, |f, _| f.focus(window));
                    cx.notify();
                }))
                .child(icon(name, 25.).text_color(if active { t.accent } else { t.ink_secondary }))
                .child(
                    div()
                        .flex_1()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .text_size(px(12.))
                                .font_weight(FontWeight::BOLD)
                                .text_color(if active { t.accent } else { t.ink })
                                .child(title),
                        )
                        .child(
                            div()
                                .mt(px(2.))
                                .text_size(px(10.))
                                .text_color(t.ink_muted)
                                .child(detail),
                        ),
                )
                .child(
                    div()
                        .w(px(16.))
                        .when(active, |d| d.child(icon("check", 14.).text_color(t.accent))),
                )
        };

        let mut fields = div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .px(px(26.))
            .py(px(22.))
            .bg(t.surface0)
            .border_t_1()
            .border_b_1()
            .border_color(t.hairline);
        if self.mode == Mode::Local {
            fields = fields
                .child(
                    div()
                        .flex()
                        .gap(px(10.))
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .gap(px(7.))
                                .child(Self::label("Host", cx))
                                .child(
                                    field_box(self.host.clone().into_any_element(), 0., cx)
                                        .h(px(37.)),
                                ),
                        )
                        .child(
                            div()
                                .w(px(108.))
                                .flex()
                                .flex_col()
                                .gap(px(7.))
                                .child(Self::label("Port", cx))
                                .child(
                                    field_box(self.port.clone().into_any_element(), 0., cx)
                                        .h(px(37.)),
                                ),
                        ),
                )
                .child(
                    div()
                        .text_size(px(11.))
                        .line_height(px(16.))
                        .text_color(t.ink_muted)
                        .child(
                            "`nexa serve` and `nexa start` listen on 127.0.0.1:7285 by default.",
                        ),
                );
        } else {
            fields = fields.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(7.))
                    .child(Self::label("Server URL", cx))
                    .child(field_box(self.url.clone().into_any_element(), 0., cx).h(px(37.))),
            );
        }
        fields = fields.child(
            div()
                .flex()
                .flex_col()
                .gap(px(7.))
                .child(
                    div()
                        .flex()
                        .gap(px(4.))
                        .child(Self::label("Bearer token", cx))
                        .child(
                            div()
                                .text_size(px(11.))
                                .text_color(t.ink_muted)
                                .child("(optional)"),
                        ),
                )
                .child(field_box(self.token.clone().into_any_element(), 0., cx).h(px(37.))),
        );
        if has_saved_token {
            let checked = self.clear_token;
            fields = fields.child(
                div()
                    .id("clear-token")
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .text_size(px(11.))
                    .text_color(t.ink_secondary)
                    .cursor_pointer()
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.clear_token = !this.clear_token;
                        cx.notify();
                    }))
                    .child(
                        div()
                            .size(px(14.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded(px(3.))
                            .border_1()
                            .border_color(if checked { t.accent } else { t.border })
                            .bg(if checked { t.accent } else { t.control })
                            .when(checked, |d| {
                                d.child(icon("check", 10.).text_color(t.accent_ink))
                            }),
                    )
                    .child("Remove the saved token"),
            );
        }
        let advanced = self.advanced;
        fields = fields.child(
            div()
                .id("advanced-toggle")
                .flex()
                .items_center()
                .gap(px(4.))
                .text_size(px(11.))
                .text_color(t.ink_secondary)
                .cursor_pointer()
                .hover(move |s| s.text_color(t.ink))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.advanced = !this.advanced;
                    cx.notify();
                }))
                .child(icon(
                    if advanced {
                        "chevron-down"
                    } else {
                        "chevron-right"
                    },
                    12.,
                ))
                .child("Advanced options"),
        );
        if advanced {
            fields = fields.child(
                div()
                    .p(px(12.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(t.hairline)
                    .bg(t.mix(t.surface2, transparent(), 0.52))
                    .flex()
                    .flex_col()
                    .gap(px(7.))
                    .child(Self::label("Request timeout (ms)", cx))
                    .child(field_box(self.timeout.clone().into_any_element(), 160., cx).h(px(37.))),
            );
        }
        if let Some(outcome) = &self.feedback {
            fields = fields.child(match outcome {
                Ok(message) => feedback(SharedString::from(message.clone()), true, cx),
                Err(message) => feedback(SharedString::from(message.clone()), false, cx),
            });
        }

        div()
            .id("connection-dialog")
            .track_focus(&self.focus_handle)
            .occlude()
            .w(px(560.))
            .max_h(px(720.))
            .overflow_y_scroll()
            .rounded(px(9.))
            .border_1()
            .border_color(t.border)
            .bg(t.surface1)
            .shadow_2xl()
            .text_color(t.ink)
            .child(
                div()
                    .flex()
                    .items_start()
                    .justify_between()
                    .px(px(26.))
                    .pt(px(24.))
                    .pb(px(20.))
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .child(display_heading("Connection", 28., cx))
                            .child(
                                div()
                                    .mt(px(6.))
                                    .text_size(px(12.))
                                    .text_color(t.ink_muted)
                                    .child(
                                        "Connect to NexaDB on this machine or on a remote server.",
                                    ),
                            ),
                    )
                    .child(
                        div()
                            .id("dialog-close")
                            .size(px(28.))
                            .flex()
                            .items_center()
                            .justify_center()
                            .rounded_full()
                            .text_color(t.ink_muted)
                            .cursor_pointer()
                            .hover(move |s| s.bg(t.surface2))
                            .on_click(cx.listener(|this, _, _, cx| this.close(cx)))
                            .child(icon("x", 16.)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .px(px(26.))
                    .pb(px(20.))
                    .child(mode_card(
                        "mode-local",
                        Mode::Local,
                        "Local",
                        "Host and port",
                        "monitor",
                        self,
                        cx,
                    ))
                    .child(mode_card(
                        "mode-remote",
                        Mode::Remote,
                        "Remote",
                        "URL and optional token",
                        "cloud",
                        self,
                        cx,
                    )),
            )
            .child(fields)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .gap(px(12.))
                    .px(px(26.))
                    .py(px(18.))
                    .child(if self.connected {
                        Button::new("disconnect", ButtonKind::Destructive)
                            .label("Disconnect")
                            .height(px(35.))
                            .disabled(busy.is_some())
                            .on_click(cx.listener(|_, _, _, cx| cx.emit(DialogEvent::Disconnect)))
                            .into_any_element()
                    } else {
                        div()
                            .font_family(FONT_MONO)
                            .text_size(px(10.))
                            .text_color(t.ink_muted)
                            .child("GET /v1/health")
                            .into_any_element()
                    })
                    .child(
                        div()
                            .flex()
                            .gap(px(8.))
                            .child(
                                Button::new("test-connection", ButtonKind::Outline)
                                    .label(if busy == Some(Busy::Test) {
                                        "Testing…"
                                    } else {
                                        "Test connection"
                                    })
                                    .height(px(35.))
                                    .loading(busy == Some(Busy::Test))
                                    .disabled(busy.is_some())
                                    .on_click(cx.listener(|this, _, _, cx| this.test(cx))),
                            )
                            .child(
                                Button::new("connect", ButtonKind::Primary)
                                    .label(match busy {
                                        Some(Busy::Connect) => "Connecting…",
                                        _ if self.connected => "Reconnect",
                                        _ => "Connect",
                                    })
                                    .height(px(35.))
                                    .loading(busy == Some(Busy::Connect))
                                    .disabled(busy.is_some())
                                    .on_click(cx.listener(|this, _, _, cx| this.connect(cx))),
                            ),
                    ),
            )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn urls_are_normalised() {
        assert_eq!(local_url("localhost", "7285"), "http://127.0.0.1:7285");
        assert_eq!(local_url("::1", "7285"), "http://[::1]:7285");
        assert_eq!(
            normalize_remote("db.example.com/nexa/"),
            "https://db.example.com/nexa"
        );
        assert_eq!(
            split_local("http://127.0.0.1:7285"),
            Some(("127.0.0.1".into(), "7285".into()))
        );
        assert_eq!(split_local("https://db.example.com"), None);
    }
}
