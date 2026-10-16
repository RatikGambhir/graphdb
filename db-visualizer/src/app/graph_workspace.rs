//! The Graph workspace: toolbar, canvas (or its loading, error, empty and
//! welcome states), caveats and the inspector.

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, PathBuilder, SharedString, TextRun, Window,
    canvas, div, font, point, prelude::*, px,
};

use super::shell::{display_heading, workspace_eyebrow};
use super::{Atlas, Status};
use crate::theme::{FONT_MONO, theme};
use crate::ui::{Button, ButtonKind, format_count, icon, spinner};

impl Atlas {
    pub(super) fn render_graph(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme(cx);
        let subtitle = match &self.graph {
            Some(graph) => {
                let mut text = format!(
                    "{} nodes · {} edges",
                    format_count(graph.nodes.len() as u64),
                    format_count(graph.edges.len() as u64)
                );
                if let Some(ms) = self.graph_ms {
                    text.push_str(&format!(" · {}", crate::ui::format_ms(ms)));
                }
                text
            }
            None => "A map of your nodes and relationships.".to_owned(),
        };
        let connected = self.status.is_connected();
        let toolbar = div()
            .flex_none()
            .h(px(106.))
            .px(px(32.))
            .flex()
            .items_center()
            .justify_between()
            .bg(t.surface0)
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(workspace_eyebrow("WORKSPACE / EXPLORE", cx))
                    .child(div().mt(px(7.)).mb(px(4.)).child(display_heading(
                        "Graph explorer",
                        30.,
                        cx,
                    )))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(t.ink_muted)
                            .child(SharedString::from(subtitle)),
                    ),
            )
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .when(!matches!(self.status, Status::Disconnected), |d| {
                        d.child(
                            Button::new("graph-refresh", ButtonKind::Outline)
                                .label(if self.graph_loading {
                                    "Refreshing…"
                                } else {
                                    "Refresh"
                                })
                                .icon("refresh-cw")
                                .font_size(px(11.))
                                .height(px(32.))
                                .loading(self.graph_loading)
                                .disabled(!connected)
                                .on_click(cx.listener(|this, _, _, cx| this.load_graph(cx))),
                        )
                    })
                    .child(
                        Button::new("edit-graph-query", ButtonKind::Outline)
                            .label("Edit graph query")
                            .icon("terminal-square")
                            .font_size(px(11.))
                            .height(px(32.))
                            .on_click(cx.listener(|this, _, _, cx| {
                                let source = this.graph_source.0.clone();
                                this.use_query(source, cx);
                            })),
                    ),
            );

        let stage: AnyElement = if self.graph.as_ref().is_some_and(|g| !g.is_empty()) {
            self.graph_view.clone().into_any_element()
        } else if self.graph_loading {
            state_block(
                spinner("graph-spinner", 22., t.accent).into_any_element(),
                "Syncing graph data",
                "Loading the current nodes and relationships from RatikDB…".to_owned(),
                None,
                cx,
            )
        } else if let Some(error) = self.graph_error.clone() {
            state_block(
                icon("circle-alert", 24.)
                    .text_color(t.danger)
                    .into_any_element(),
                "Couldn’t load the graph",
                error,
                Some("Retry"),
                cx,
            )
        } else if !connected {
            render_welcome(cx)
        } else {
            state_block(
                icon("network", 24.).text_color(t.accent).into_any_element(),
                "No graph data found",
                "The graph query returned no nodes. Insert some data from the Query workspace, or edit the graph query.".to_owned(),
                Some("Refresh"),
                cx,
            )
        };

        let caveat = self.graph.as_ref().filter(|g| g.dangling_edges > 0).map(|g| {
            format!(
                "Note: {} edge{} left the selection and {} not drawn; raise the node LIMIT to include their endpoints.",
                format_count(g.dangling_edges as u64),
                if g.dangling_edges == 1 { "" } else { "s" },
                if g.dangling_edges == 1 { "is" } else { "are" }
            )
        });

        let inspector = self.render_inspector_pane(cx);
        div()
            .size_full()
            .flex()
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(toolbar)
                    .child(div().flex_1().min_h_0().relative().child(stage))
                    .when_some(caveat, |d, text| {
                        d.child(
                            div()
                                .flex_none()
                                .px(px(16.))
                                .py(px(8.))
                                .border_t_1()
                                .border_color(t.hairline)
                                .text_size(px(11.5))
                                .text_color(t.ink_muted)
                                .child(SharedString::from(text)),
                        )
                    }),
            )
            .child(inspector)
            .into_any_element()
    }
}

fn state_block(
    mark: AnyElement,
    title: &'static str,
    text: String,
    action: Option<&'static str>,
    cx: &mut Context<Atlas>,
) -> AnyElement {
    let t = *theme(cx);
    div()
        .size_full()
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .p(px(42.))
        .child(
            div()
                .size(px(51.))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(6.))
                .border_1()
                .border_color(t.border)
                .bg(t.surface1)
                .child(mark),
        )
        .child(div().mt(px(6.)).child(display_heading(title, 26., cx)))
        .child(
            div()
                .max_w(px(430.))
                .text_center()
                .text_size(px(13.))
                .line_height(px(20.))
                .text_color(t.ink_secondary)
                .child(SharedString::from(text)),
        )
        .when_some(action, |d, label| {
            d.child(
                div().mt(px(8.)).child(
                    Button::new("graph-state-action", ButtonKind::Primary)
                        .label(label)
                        .on_click(cx.listener(|this, _, _, cx| this.load_graph(cx))),
                ),
            )
        })
        .into_any_element()
}

/// The disconnected onboarding page: copy, a decorative network drawing and
/// three steps. The drawing explains; it is not a database result.
fn render_welcome(cx: &mut Context<Atlas>) -> AnyElement {
    let t = *theme(cx);
    let caption = |left: &'static str, right: &'static str, top: bool| {
        div()
            .flex()
            .justify_between()
            .font_family(FONT_MONO)
            .text_size(px(8.))
            .text_color(t.ink_muted)
            .when(top, |d| d.pb(px(12.)).border_b_1().border_color(t.border))
            .when(!top, |d| {
                d.pt(px(12.))
                    .border_t_1()
                    .border_color(t.border)
                    .text_size(px(9.))
            })
            .child(left)
            .child(right)
    };
    let map = canvas(
        |_, _, _| {},
        move |bounds, (), window, cx| {
            // A 440×310 drawing scaled into the available box.
            let scale =
                (f32::from(bounds.size.width) / 440.).min(f32::from(bounds.size.height) / 310.);
            let ox = f32::from(bounds.left()) + (f32::from(bounds.size.width) - 440. * scale) / 2.;
            let oy = f32::from(bounds.top()) + (f32::from(bounds.size.height) - 310. * scale) / 2.;
            let p = |x: f32, y: f32| point(px(ox + x * scale), px(oy + y * scale));
            let circle = |cx_: f32, cy_: f32, r: f32, stroke: Option<f32>| {
                let mut b = match stroke {
                    Some(w) => PathBuilder::stroke(px(w)),
                    None => PathBuilder::fill(),
                };
                let r = r * scale;
                let c = p(cx_, cy_);
                b.move_to(point(c.x + px(r), c.y));
                b.arc_to(
                    point(px(r), px(r)),
                    px(0.),
                    false,
                    false,
                    point(c.x - px(r), c.y),
                );
                b.arc_to(
                    point(px(r), px(r)),
                    px(0.),
                    false,
                    false,
                    point(c.x + px(r), c.y),
                );
                b.close();
                b.build().ok()
            };
            // Orbits and axes, dashed.
            for r in [116., 73.] {
                let mut b = PathBuilder::stroke(px(1.)).dash_array(&[px(3.), px(5.)]);
                let c = p(220., 155.);
                let rr = r * scale;
                b.move_to(point(c.x + px(rr), c.y));
                b.arc_to(
                    point(px(rr), px(rr)),
                    px(0.),
                    false,
                    false,
                    point(c.x - px(rr), c.y),
                );
                b.arc_to(
                    point(px(rr), px(rr)),
                    px(0.),
                    false,
                    false,
                    point(c.x + px(rr), c.y),
                );
                if let Ok(path) = b.build() {
                    window.paint_path(path, t.border);
                }
            }
            for (a, b_) in [((20., 155.), (420., 155.)), ((220., 20.), (220., 290.))] {
                let mut b = PathBuilder::stroke(px(1.)).dash_array(&[px(3.), px(5.)]);
                b.move_to(p(a.0, a.1));
                b.line_to(p(b_.0, b_.1));
                if let Ok(path) = b.build() {
                    window.paint_path(path, t.border);
                }
            }
            let lines = [
                ((84., 90.), (220., 155.)),
                ((220., 155.), (335., 63.)),
                ((220., 155.), (337., 246.)),
                ((220., 155.), (100., 254.)),
                ((220., 155.), (365., 156.)),
                ((84., 90.), (100., 254.)),
                ((335., 63.), (365., 156.)),
                ((365., 156.), (337., 246.)),
            ];
            for (a, b_) in lines {
                let mut b = PathBuilder::stroke(px(1.));
                b.move_to(p(a.0, a.1));
                b.line_to(p(b_.0, b_.1));
                if let Ok(path) = b.build() {
                    window.paint_path(path, t.ink_muted);
                }
            }
            for (x, y, r) in [
                (84., 90., 12.),
                (335., 63., 10.),
                (337., 246., 14.),
                (100., 254., 9.),
                (365., 156., 7.),
            ] {
                if let Some(path) = circle(x, y, r, None) {
                    window.paint_path(path, t.surface0);
                }
                if let Some(path) = circle(x, y, r, Some(1.5)) {
                    window.paint_path(path, t.ink_secondary);
                }
            }
            if let Some(path) = circle(220., 155., 32., None) {
                window.paint_path(path, t.surface0);
            }
            {
                let mut b = PathBuilder::stroke(px(1.)).dash_array(&[px(2.), px(4.)]);
                let c = p(220., 155.);
                let rr = 32. * scale;
                b.move_to(point(c.x + px(rr), c.y));
                b.arc_to(
                    point(px(rr), px(rr)),
                    px(0.),
                    false,
                    false,
                    point(c.x - px(rr), c.y),
                );
                b.arc_to(
                    point(px(rr), px(rr)),
                    px(0.),
                    false,
                    false,
                    point(c.x + px(rr), c.y),
                );
                if let Ok(path) = b.build() {
                    window.paint_path(path, t.accent);
                }
            }
            if let Some(path) = circle(220., 155., 20., None) {
                window.paint_path(path, t.accent);
            }
            for (a, b_) in [((213., 155.), (227., 155.)), ((220., 148.), (220., 162.))] {
                let mut b = PathBuilder::stroke(px(1.5));
                b.move_to(p(a.0, a.1));
                b.line_to(p(b_.0, b_.1));
                if let Ok(path) = b.build() {
                    window.paint_path(path, t.accent_ink);
                }
            }
            for (x, y, label) in [
                (66., 64., "NODE"),
                (266., 106., "RELATIONSHIP"),
                (193., 204., "CONNECTION"),
            ] {
                let text = SharedString::from(label);
                let run = TextRun {
                    len: text.len(),
                    font: font(FONT_MONO),
                    color: t.ink_muted,
                    background_color: None,
                    underline: None,
                    strikethrough: None,
                };
                let line =
                    window
                        .text_system()
                        .shape_line(text, px(8. * scale.max(0.9)), &[run], None);
                line.paint(p(x, y - 8.), px(10.), window, cx).ok();
            }
        },
    )
    .w_full()
    .h(px(300.));

    let step =
        |name: &'static str, eyebrow: &'static str, title: &'static str, text: &'static str| {
            div()
                .flex_1()
                .flex()
                .gap(px(12.))
                .child(icon(name, 17.).mt(px(2.)).text_color(t.ink_muted))
                .child(
                    div()
                        .flex()
                        .flex_col()
                        .child(
                            div()
                                .mb(px(10.))
                                .font_family(FONT_MONO)
                                .text_size(px(8.))
                                .text_color(t.ink_muted)
                                .child(eyebrow),
                        )
                        .child(
                            div()
                                .text_size(px(11.))
                                .font_weight(FontWeight::SEMIBOLD)
                                .child(title),
                        )
                        .child(
                            div()
                                .mt(px(5.))
                                .text_size(px(11.))
                                .line_height(px(17.))
                                .text_color(t.ink_muted)
                                .child(text),
                        ),
                )
        };

    div()
        .id("graph-welcome")
        .size_full()
        .overflow_y_scroll()
        .bg(t.surface0)
        .child(
            div()
                .min_h_full()
                .flex()
                .flex_col()
                .justify_center()
                .px(px(56.))
                .py(px(48.))
                .gap(px(40.))
                .child(
                    div()
                        .flex()
                        .gap(px(20.))
                        .items_center()
                        .child(
                            div()
                                .flex_1()
                                .flex()
                                .flex_col()
                                .child(div().font_family(FONT_MONO).text_size(px(9.)).text_color(t.accent).child(crate::ui::spaced("A NEW PERSPECTIVE ON YOUR DATA".into())))
                                .child(div().my(px(20.)).child(display_heading("Every connection\ntells a story.", 46., cx)))
                                .child(
                                    div()
                                        .max_w(px(350.))
                                        .mb(px(26.))
                                        .text_size(px(13.))
                                        .line_height(px(23.))
                                        .text_color(t.ink_secondary)
                                        .child("Bring your RatikDB graph into focus. Follow relationships, ask better questions, change what you find, and see how everything fits together."),
                                )
                                .child(
                                    div().child(
                                        Button::new("welcome-connect", ButtonKind::Primary)
                                            .label("Connect Now")
                                            .icon_after("arrow-right")
                                            .height(px(38.))
                                            .padding_x(px(16.))
                                            .on_click(cx.listener(|this, _, window, cx| this.open_connection(window, cx))),
                                    ),
                                )
                                .child(div().mt(px(12.)).text_size(px(10.)).text_color(t.ink_muted).child("Local or remote · Reads are automatic, writes are explicit")),
                        )
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .child(caption("ATLAS / CONNECTED DATA", "FIG. 01", true))
                                .child(map)
                                .child(caption("Discover the shape of your graph.", "", false)),
                        ),
                )
                .child(
                    div()
                        .flex()
                        .gap(px(24.))
                        .pt(px(24.))
                        .border_t_1()
                        .border_color(t.border)
                        .child(step("database", "01 / CONNECT", "Start with your server", "One connection to your RatikDB data."))
                        .child(step("network", "02 / EXPLORE", "Follow the relationships", "Pan, zoom, and expand your network."))
                        .child(step("terminal-square", "03 / CHANGE", "Ask, then act", "Use RQL to find what matters and to write it.")),
                ),
        )
        .into_any_element()
}
