//! The Query workspace: schema and examples rail, RQL editor, typed
//! parameters, output tabs (results and wire format) and the inspector.

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, SharedString, Window, div, prelude::*, px,
    relative,
};

use super::shell::workspace_eyebrow;
use super::{Atlas, OutputTab, Status};
use crate::rql::{self, Analysis};
use crate::theme::{FONT_MONO, theme, transparent};
use crate::ui::{
    Button, ButtonKind, divider, format_count, hint, icon, kbd, panel_error, section_heading,
};

impl Atlas {
    pub(super) fn render_query(
        &mut self,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme(cx);
        let sidebar = self.render_sidebar(cx);
        let editor = self.render_editor(cx);
        let params = self.render_params(cx);
        let output = self.render_output(window, cx);
        let inspector = self.render_inspector_pane(cx);
        div()
            .size_full()
            .flex()
            .bg(t.surface0)
            .child(sidebar)
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .h_full()
                    .flex()
                    .flex_col()
                    .child(editor)
                    .child(params)
                    .child(output),
            )
            .child(inspector)
            .into_any_element()
    }

    fn render_sidebar(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme(cx);
        let link = |id: SharedString, text: SharedString, query: String, cx: &mut Context<Self>| {
            div()
                .id(id)
                .w_full()
                .min_h(px(30.))
                .px(px(8.))
                .py(px(5.))
                .rounded(px(4.))
                .text_size(px(12.))
                .font_weight(FontWeight::MEDIUM)
                .text_color(t.ink_secondary)
                .truncate()
                .cursor_pointer()
                .hover(move |s| s.bg(t.surface2).text_color(t.ink))
                .on_click(cx.listener(move |this, _, _, cx| this.use_query(query.clone(), cx)))
                .child(text)
        };

        let mut schema_section = div().flex().flex_col().child(
            div()
                .flex()
                .items_center()
                .justify_between()
                .mb(px(13.))
                .child(section_heading("Schema", cx))
                .child(
                    Button::new("sidebar-refresh", ButtonKind::Link)
                        .label(if self.schema_loading {
                            "…"
                        } else {
                            "Refresh"
                        })
                        .font_size(px(11.))
                        .disabled(self.client.is_none() || self.schema_loading)
                        .on_click(cx.listener(|this, _, _, cx| this.refresh_schema(cx))),
                ),
        );
        if let Some(error) = &self.schema_error {
            schema_section =
                schema_section.child(panel_error(SharedString::from(error.clone()), None, cx));
        } else if let Some(schema) = &self.schema {
            // (title, [(type name, element count, is a node type)])
            type Group<'a> = (&'a str, Vec<(String, u64, bool)>);
            let groups: [Group; 2] = [
                (
                    "Nodes",
                    schema
                        .node_types()
                        .map(|t| (t.name.clone(), t.count, true))
                        .collect(),
                ),
                (
                    "Edges",
                    schema
                        .edge_types()
                        .map(|t| (t.name.clone(), t.count, false))
                        .collect(),
                ),
            ];
            for (title, entries) in groups {
                schema_section = schema_section.child(
                    div()
                        .mt(px(18.))
                        .mb(px(8.))
                        .font_family(FONT_MONO)
                        .font_weight(FontWeight::EXTRA_BOLD)
                        .text_size(px(10.))
                        .text_color(t.ink_muted)
                        .child(crate::ui::spaced(title.to_uppercase().into())),
                );
                if entries.is_empty() {
                    schema_section = schema_section.child(hint("None declared.", cx));
                }
                for (name, count, is_node) in entries {
                    let primary = if is_node {
                        rql::graph_node_type(&name)
                    } else {
                        rql::browse_edge_type(&name)
                    };
                    let secondary = if is_node {
                        rql::browse_node_type(&name)
                    } else {
                        rql::graph_edge_type(&name)
                    };
                    schema_section = schema_section.child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(2.))
                            .min_w_0()
                            .child(div().flex_1().min_w_0().child(link(
                                SharedString::from(format!("type-{title}-{name}")),
                                SharedString::from(name.clone()),
                                primary,
                                cx,
                            )))
                            .child(
                                div()
                                    .id(SharedString::from(format!("alt-{title}-{name}")))
                                    .size(px(22.))
                                    .flex()
                                    .items_center()
                                    .justify_center()
                                    .rounded(px(4.))
                                    .text_color(t.ink_muted)
                                    .cursor_pointer()
                                    .hover(move |s| s.bg(t.surface2).text_color(t.ink))
                                    .tooltip({
                                        let q = secondary.clone();
                                        move |_, cx| {
                                            crate::ui::tooltip::Tooltip::text(q.clone(), cx)
                                        }
                                    })
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        this.use_query(secondary.clone(), cx)
                                    }))
                                    .child("⋯"),
                            )
                            .child(
                                div()
                                    .min_w(px(24.))
                                    .text_right()
                                    .font_family(FONT_MONO)
                                    .text_size(px(10.))
                                    .text_color(t.ink_muted)
                                    .child(SharedString::from(format_count(count))),
                            ),
                    );
                }
            }
            if !schema.queries.is_empty() {
                schema_section = schema_section.child(
                    div()
                        .mt(px(18.))
                        .mb(px(8.))
                        .font_family(FONT_MONO)
                        .font_weight(FontWeight::EXTRA_BOLD)
                        .text_size(px(10.))
                        .text_color(t.ink_muted)
                        .child(crate::ui::spaced("STORED QUERIES".into())),
                );
                for q in &schema.queries {
                    let params: Vec<String> = q.params.iter().map(|p| p.name.clone()).collect();
                    schema_section = schema_section.child(link(
                        SharedString::from(format!("stored-{}", q.name)),
                        SharedString::from(q.signature()),
                        rql::call_query(&q.name, &params),
                        cx,
                    ));
                }
            }
            schema_section = schema_section.child(
                div()
                    .mt(px(10.))
                    .text_size(px(11.))
                    .line_height(px(16.))
                    .text_color(t.ink_muted)
                    .child("Read from the declared schema. Counts are exact; every type is declared up front."),
            );
        } else if self.schema_loading {
            schema_section = schema_section.child(hint("Reading schema…", cx));
        } else {
            schema_section = schema_section.child(hint("Not loaded yet.", cx));
        }

        let mut examples = div()
            .flex()
            .flex_col()
            .gap(px(2.))
            .child(div().mb(px(11.)).child(section_heading("Examples", cx)));
        for (index, example) in rql::EXAMPLES.iter().enumerate() {
            examples = examples.child(link(
                SharedString::from(format!("example-{index}")),
                example.title.into(),
                example.query.to_owned(),
                cx,
            ));
        }

        let mut column = div()
            .flex()
            .flex_col()
            .gap(px(22.))
            .child(schema_section)
            .child(divider(cx))
            .child(examples);
        if !self.history.is_empty() {
            let mut history = div()
                .flex()
                .flex_col()
                .gap(px(2.))
                .child(div().mb(px(11.)).child(section_heading("History", cx)));
            for (index, query) in self.history.iter().enumerate() {
                let flat: String = query.split_whitespace().collect::<Vec<_>>().join(" ");
                let flat: String = flat.chars().take(48).collect();
                history = history.child(link(
                    SharedString::from(format!("history-{index}")),
                    SharedString::from(flat),
                    query.clone(),
                    cx,
                ));
            }
            column = column.child(divider(cx)).child(history);
        }

        div()
            .id("query-sidebar")
            .flex_none()
            .w(px(210.))
            .h_full()
            .overflow_y_scroll()
            .px(px(20.))
            .py(px(24.))
            .bg(t.surface1)
            .border_r_1()
            .border_color(t.hairline)
            .child(column)
            .into_any_element()
    }

    fn render_editor(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme(cx);
        let text = self.editor.read(cx).text().to_owned();
        let line_count = self.editor.read(cx).line_count();
        let analysis = Analysis::of(&text);
        let is_write = analysis.has_write();
        let can_run = self.status.is_connected() && !self.run.running && !analysis.is_empty();
        let label = if self.run.running {
            "Running…"
        } else if is_write {
            "Run mutation"
        } else {
            "Run query"
        };
        let tooltip = if !self.status.is_connected() {
            "Connect to a RatikDB server to run queries"
        } else if is_write {
            "Writes run as one atomic transaction"
        } else {
            "Run the current query (⌘↵)"
        };

        let mut section = div()
            .flex_none()
            .h(relative(0.43))
            .min_h(px(270.))
            .flex()
            .flex_col()
            .bg(t.surface0)
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .flex_none()
                    .h(px(72.))
                    .px(px(24.))
                    .flex()
                    .items_center()
                    .justify_between()
                    .border_b_1()
                    .border_color(t.hairline)
                    .child(
                        div()
                            .flex()
                            .flex_col()
                            .gap(px(5.))
                            .child(workspace_eyebrow("WORKSPACE / RQL", cx))
                            .child(
                                div()
                                    .text_size(px(18.))
                                    .line_height(px(22.))
                                    .font_weight(FontWeight::SEMIBOLD)
                                    .child("Query editor"),
                            ),
                    )
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(12.))
                            .child(
                                div()
                                    .flex()
                                    .gap(px(2.))
                                    .child(kbd("⌘", cx))
                                    .child(kbd("↵", cx)),
                            )
                            .child(
                                div()
                                    .id("run-wrapper")
                                    .tooltip(move |_, cx| {
                                        crate::ui::tooltip::Tooltip::text(tooltip, cx)
                                    })
                                    .child(
                                        Button::new("run", ButtonKind::Primary)
                                            .label(label)
                                            .icon(if is_write { "pencil" } else { "play" })
                                            .icon_size(13.)
                                            .loading(self.run.running)
                                            .disabled(!can_run)
                                            .height(px(34.))
                                            .padding_x(px(13.))
                                            .on_click(cx.listener(|this, _, window, cx| {
                                                this.run_query(window, cx)
                                            })),
                                    ),
                            ),
                    ),
            )
            .child(div().flex_1().min_h_0().child(self.editor.clone()))
            .child(
                div()
                    .flex_none()
                    .flex()
                    .justify_between()
                    .px(px(20.))
                    .py(px(8.))
                    .border_t_1()
                    .border_color(t.hairline)
                    .font_family(FONT_MONO)
                    .text_size(px(9.))
                    .text_color(t.ink_muted)
                    .child("RQL")
                    .child(SharedString::from(format!(
                        "{line_count} {} · {} · {}",
                        if line_count == 1 { "line" } else { "lines" },
                        match analysis.statements.len() {
                            0 => "empty".to_owned(),
                            1 => "1 statement".to_owned(),
                            n => format!("{n} statements"),
                        },
                        if is_write { "Mutation" } else { "Read only" }
                    ))),
            );
        if is_write {
            section = section.child(
                div()
                    .flex_none()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .px(px(20.))
                    .py(px(10.))
                    .border_t_1()
                    .border_color(t.border)
                    .bg(t.surface2)
                    .text_size(px(12.))
                    .text_color(t.ink_secondary)
                    .child(icon("lock-keyhole-open", 13.).text_color(t.warning))
                    .child(SharedString::from(format!(
                        "{}. Writes commit only when you click Run mutation; the whole script is one atomic transaction.",
                        analysis.summary()
                    ))),
            );
        }
        if let Some(error) = &self.run.error
            && let Some(line) = self.editor.read(cx).error_line
        {
            section = section.child(
                div()
                    .flex_none()
                    .flex()
                    .items_baseline()
                    .gap(px(8.))
                    .px(px(20.))
                    .py(px(9.))
                    .border_t_1()
                    .border_color(t.mix(t.danger, t.border, 0.35))
                    .bg(t.mix(t.danger, transparent(), 0.12))
                    .text_size(px(11.))
                    .child(
                        div()
                            .font_family(FONT_MONO)
                            .text_color(t.danger)
                            .child(SharedString::from(format!("line {line}"))),
                    )
                    .child(
                        div()
                            .text_color(t.ink)
                            .child(SharedString::from(error.message.clone())),
                    ),
            );
        }
        section.into_any_element()
    }

    fn render_params(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme(cx);
        let open = self.params_open;
        let mut block = div()
            .flex_none()
            .px(px(20.))
            .py(px(10.))
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .id("params-toggle")
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .text_size(px(12.))
                    .text_color(t.ink_secondary)
                    .cursor_pointer()
                    .hover(move |s| s.text_color(t.ink))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.params_open = !this.params_open;
                        cx.notify();
                    }))
                    .child(icon(
                        if open {
                            "chevron-down"
                        } else {
                            "chevron-right"
                        },
                        13.,
                    ))
                    .child(SharedString::from(format!(
                        "Parameters ({})",
                        self.params.len()
                    ))),
            );
        if open {
            let mut rows = div()
                .id("param-rows")
                .flex()
                .flex_col()
                .gap(px(8.))
                .mt(px(8.))
                .max_h(px(200.))
                .overflow_y_scroll()
                .child(hint("Use $name in values and filters. Values are sent as typed JSON parameters, never pasted into the query text.", cx));
            for row in &self.params {
                let id = row.id;
                rows = rows.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .child(field_box(row.name.clone().into_any_element(), 160., cx))
                        .child(div().w(px(120.)).child(row.ty.clone()))
                        .child(div().flex_1().child(field_box(
                            row.value.clone().into_any_element(),
                            0.,
                            cx,
                        )))
                        .child(
                            Button::new(("remove-param", id), ButtonKind::Ghost)
                                .label("Remove")
                                .font_size(px(11.))
                                .on_click(
                                    cx.listener(move |this, _, _, cx| this.remove_param(id, cx)),
                                ),
                        ),
                );
            }
            rows = rows.child(
                div().child(
                    Button::new("add-param", ButtonKind::Outline)
                        .label("Add parameter")
                        .icon("plus")
                        .icon_size(12.)
                        .font_size(px(11.5))
                        .on_click(cx.listener(|this, _, window, cx| this.add_param(window, cx))),
                ),
            );
            block = block.child(rows);
        }
        block.into_any_element()
    }

    fn render_output(&mut self, window: &mut Window, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme(cx);
        let tab = |id: &'static str,
                   label: &'static str,
                   which: OutputTab,
                   this: &Self,
                   cx: &mut Context<Self>| {
            let active = this.output_tab == which;
            div()
                .id(id)
                .h_full()
                .px(px(3.))
                .flex()
                .items_center()
                .border_b_2()
                .border_color(if active { t.accent } else { transparent() })
                .text_size(px(12.))
                .font_weight(FontWeight::SEMIBOLD)
                .text_color(if active { t.ink } else { t.ink_secondary })
                .cursor_pointer()
                .hover(move |s| s.text_color(t.ink))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.output_tab = which;
                    cx.notify();
                }))
                .child(label)
        };
        let status = if self.run.running {
            "running…".to_owned()
        } else if let Some(result) = &self.run.result {
            format!(
                "{} · {}",
                crate::ui::format_ms(result.duration_ms),
                result.summary
            )
        } else {
            String::new()
        };
        let body = match self.output_tab {
            OutputTab::Results => self.render_results(window, cx),
            OutputTab::Wire => self.render_wire(cx),
        };
        div()
            .flex_1()
            .min_h_0()
            .flex()
            .flex_col()
            .child(
                div()
                    .flex_none()
                    .h(px(47.))
                    .px(px(20.))
                    .flex()
                    .items_center()
                    .gap(px(16.))
                    .bg(t.surface1)
                    .border_b_1()
                    .border_color(t.hairline)
                    .child(workspace_eyebrow("OUTPUT", cx).mr(px(7.)))
                    .child(tab("tab-results", "Results", OutputTab::Results, self, cx))
                    .child(tab("tab-wire", "Wire format", OutputTab::Wire, self, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_right()
                            .truncate()
                            .font_family(FONT_MONO)
                            .text_size(px(11.))
                            .text_color(t.ink_muted)
                            .child(SharedString::from(status)),
                    ),
            )
            .child(
                div()
                    .id("output-body")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .p(px(24.))
                    .bg(t.surface0)
                    .child(body),
            )
            .into_any_element()
    }

    pub(super) fn empty_query_state(&self, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme(cx);
        let connected = self.status.is_connected();
        let checking = matches!(self.status, Status::Checking);
        div()
            .max_w(px(570.))
            .mx_auto()
            .py(px(48.))
            .px(px(24.))
            .flex()
            .flex_col()
            .gap(px(8.))
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
                    .text_color(t.accent)
                    .child(icon(if connected { "arrow-up-right" } else { "plug" }, 22.).text_color(t.accent)),
            )
            .child(workspace_eyebrow("YOUR WORKSPACE", cx).mt(px(13.)))
            .child(super::shell::display_heading(
                if connected { "A question is a starting point." } else { "Start with a connection." },
                30.,
                cx,
            ))
            .child(
                div()
                    .max_w(px(410.))
                    .text_size(px(13.))
                    .line_height(px(21.))
                    .text_color(t.ink_secondary)
                    .child(if connected {
                        "Run the query above to reveal your data. Try a starting point from the left rail if you need one."
                    } else if checking {
                        "Reaching the saved RatikDB server…"
                    } else {
                        "Connect to a local or remote RatikDB server, then explore its shape with a query."
                    }),
            )
            .when(!connected && !checking, |d| {
                d.child(
                    div().mt(px(8.)).child(
                        Button::new("empty-connect", ButtonKind::Outline)
                            .label("Configure connection")
                            .icon_after("arrow-up-right")
                            .on_click(cx.listener(|this, _, window, cx| this.open_connection(window, cx))),
                    ),
                )
            })
            .into_any_element()
    }
}

/// A bordered wrapper for an inline text field.
pub fn field_box(field: AnyElement, width: f32, cx: &gpui::App) -> gpui::Div {
    let t = theme(cx);
    let d = div()
        .h(px(32.))
        .rounded(px(4.))
        .border_1()
        .border_color(t.border)
        .bg(t.control)
        .overflow_hidden()
        .flex()
        .items_center()
        .child(field);
    if width > 0. {
        d.w(px(width)).flex_none()
    } else {
        d.w_full()
    }
}
