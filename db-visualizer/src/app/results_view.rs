//! The Results and Wire format tabs.

use std::cmp::Ordering;

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, SharedString, Window, div, prelude::*, px,
};
use serde_json::Value;

use super::Atlas;
use crate::graph::palette::ordered_colours;
use crate::results::{ElementKind, Execution, ResultView, element_kind, stringify};
use crate::theme::{FONT_MONO, theme, transparent};
use crate::ui::{
    Button, ButtonKind, count_bars, format_count, hint, icon, panel_error, spaced, stat_tile,
};

const MAX_ROWS: usize = 500;

fn compare(a: &Value, b: &Value) -> Ordering {
    match (a.as_f64(), b.as_f64()) {
        (Some(x), Some(y)) => x.total_cmp(&y),
        _ => match (a.is_null(), b.is_null()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            _ => natural_cmp(&stringify(a), &stringify(b)),
        },
    }
}

/// Case-insensitive comparison that orders embedded numbers numerically.
fn natural_cmp(a: &str, b: &str) -> Ordering {
    let (mut ai, mut bi) = (a.chars().peekable(), b.chars().peekable());
    loop {
        match (ai.peek().copied(), bi.peek().copied()) {
            (None, None) => return Ordering::Equal,
            (None, Some(_)) => return Ordering::Less,
            (Some(_), None) => return Ordering::Greater,
            (Some(x), Some(y)) if x.is_ascii_digit() && y.is_ascii_digit() => {
                let na: String = std::iter::from_fn(|| ai.next_if(char::is_ascii_digit)).collect();
                let nb: String = std::iter::from_fn(|| bi.next_if(char::is_ascii_digit)).collect();
                let ord = na.len().cmp(&nb.len()).then_with(|| na.cmp(&nb));
                if ord != Ordering::Equal {
                    return ord;
                }
            }
            (Some(x), Some(y)) => {
                let ord = x.to_lowercase().cmp(y.to_lowercase());
                if ord != Ordering::Equal {
                    return ord;
                }
                ai.next();
                bi.next();
            }
        }
    }
}

impl Atlas {
    pub(super) fn render_results(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        if let Some(error) = &self.run.error {
            let message = SharedString::from(error.message.clone());
            let detail = error.detail.clone().map(SharedString::from);
            return panel_error(message, detail, cx).into_any_element();
        }
        if self.run.running {
            return hint("Running…", cx).into_any_element();
        }
        let Some(execution) = self.run.result.clone() else {
            return self.empty_query_state(cx);
        };
        match execution.view {
            ResultView::Mutation => self.render_receipt(&execution, cx),
            ResultView::Text => render_text(&execution, cx),
            ResultView::Stats => self.render_stats(&execution, cx),
            ResultView::GroupCount => render_group_count(&execution, cx),
            ResultView::Rows => {
                let banner = self.graph_banner(&execution, cx);
                div()
                    .flex()
                    .flex_col()
                    .gap(px(14.))
                    .children(banner)
                    .child(self.render_table(&execution, cx))
                    .into_any_element()
            }
        }
    }

    fn graph_banner(&self, execution: &Execution, cx: &mut Context<Self>) -> Option<AnyElement> {
        let graph = execution.graph.as_ref()?;
        let t = *theme(cx);
        let text = if execution.pure_graph {
            format!(
                "{} nodes and {} edges are drawn in the Graph workspace.",
                format_count(graph.nodes.len() as u64),
                format_count(graph.edges.len() as u64)
            )
        } else {
            format!(
                "This result contains {} nodes and {} edges.",
                format_count(graph.nodes.len() as u64),
                format_count(graph.edges.len() as u64)
            )
        };
        Some(
            div()
                .flex()
                .items_center()
                .gap(px(12.))
                .px(px(12.))
                .py(px(8.))
                .rounded(px(4.))
                .border_1()
                .border_color(t.hairline)
                .bg(t.surface1)
                .child(icon("network", 15.).text_color(t.accent))
                .child(
                    div()
                        .flex_1()
                        .text_size(px(12.))
                        .text_color(t.ink_secondary)
                        .child(SharedString::from(text)),
                )
                .child(
                    Button::new("open-in-graph", ButtonKind::Outline)
                        .label(if execution.pure_graph {
                            "Show graph"
                        } else {
                            "Open in graph"
                        })
                        .font_size(px(11.))
                        .height(px(28.))
                        .on_click(cx.listener(|this, _, _, cx| this.open_result_in_graph(cx))),
                )
                .into_any_element(),
        )
    }

    fn render_table(&mut self, execution: &Execution, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme(cx);
        let response = &execution.response;
        if response.columns.is_empty() {
            return hint("Done. The statement returned no columns.", cx).into_any_element();
        }
        if response.rows.is_empty() {
            return hint("No rows matched.", cx).into_any_element();
        }
        let columns = &response.columns;
        let edge_table = columns.iter().any(|c| c == "_from");
        let numeric: Vec<bool> = (0..columns.len())
            .map(|i| {
                let mut any = false;
                for row in &response.rows {
                    match row.get(i) {
                        Some(Value::Number(_)) => any = true,
                        Some(Value::Null) | None => {}
                        _ => return false,
                    }
                }
                any
            })
            .collect();
        let widths: Vec<f32> = (0..columns.len())
            .map(|i| {
                let header = columns[i].chars().count();
                let longest = response
                    .rows
                    .iter()
                    .take(200)
                    .map(|row| row.get(i).map_or(0, |v| stringify(v).chars().count()))
                    .max()
                    .unwrap_or(0);
                #[allow(clippy::cast_precision_loss)]
                let chars = header.max(longest) as f32;
                (chars * 7.4 + 26.).clamp(70., 340.)
            })
            .collect();

        let mut order: Vec<usize> = (0..response.rows.len()).collect();
        if let Some((column, descending)) = self.table_sort {
            order.sort_by(|&a, &b| {
                let ord = compare(
                    response.rows[a].get(column).unwrap_or(&Value::Null),
                    response.rows[b].get(column).unwrap_or(&Value::Null),
                );
                if descending { ord.reverse() } else { ord }
            });
        }

        let mut header = div()
            .flex()
            .bg(t.surface1)
            .border_b_1()
            .border_color(t.hairline);
        for (i, column) in columns.iter().enumerate() {
            let sorted = self.table_sort.filter(|(c, _)| *c == i);
            let arrow = match sorted {
                Some((_, true)) => " ↓",
                Some((_, false)) => " ↑",
                None => "",
            };
            header = header.child(
                div()
                    .id(("th", i))
                    .flex_none()
                    .w(px(widths[i]))
                    .px(px(12.))
                    .py(px(7.))
                    .flex()
                    .when(numeric[i], |d| d.justify_end())
                    .font_family(FONT_MONO)
                    .font_weight(FontWeight::BOLD)
                    .text_size(px(10.))
                    .text_color(t.ink_muted)
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .cursor_pointer()
                    .hover(move |s| s.text_color(t.ink))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.table_sort = match this.table_sort {
                            Some((c, false)) if c == i => Some((i, true)),
                            _ => Some((i, false)),
                        };
                        cx.notify();
                    }))
                    .child(SharedString::from(format!(
                        "{}{arrow}",
                        column.to_uppercase()
                    ))),
            );
        }

        let mut body = div().flex().flex_col();
        for (n, &row_index) in order.iter().take(MAX_ROWS).enumerate() {
            let row = &response.rows[row_index];
            let mut tr = div()
                .id(("tr", n))
                .flex()
                .border_b_1()
                .border_color(t.hairline)
                .hover(move |s| s.bg(t.surface1));
            for (i, column) in columns.iter().enumerate() {
                let value = row.get(i).cloned().unwrap_or(Value::Null);
                let text = stringify(&value);
                let target: Option<(ElementKind, u64)> = match element_kind(&value) {
                    Some(kind) => value
                        .get("_id")
                        .and_then(Value::as_u64)
                        .map(|id| (kind, id)),
                    None if column == "_id" => value.as_u64().map(|id| {
                        (
                            if edge_table {
                                ElementKind::Edge
                            } else {
                                ElementKind::Node
                            },
                            id,
                        )
                    }),
                    None if (column == "_from" || column == "_to") => {
                        value.as_u64().map(|id| (ElementKind::Node, id))
                    }
                    None => None,
                };
                let mut td = div()
                    .id(("td", n * 1000 + i))
                    .flex_none()
                    .w(px(widths[i]))
                    .px(px(12.))
                    .py(px(9.))
                    .flex()
                    .when(numeric[i], |d| d.justify_end())
                    .text_size(px(12.))
                    .whitespace_nowrap()
                    .overflow_hidden()
                    .tooltip({
                        let text = text.clone();
                        move |_, cx| crate::ui::tooltip::Tooltip::text(text.clone(), cx)
                    });
                td = if let Some((kind, id)) = target {
                    td.child(
                        div()
                            .id(("link", n * 1000 + i))
                            .truncate()
                            .font_family(FONT_MONO)
                            .text_color(t.accent)
                            .underline()
                            .cursor_pointer()
                            .on_click(cx.listener(move |this, _, _, cx| this.inspect(kind, id, cx)))
                            .child(SharedString::from(text)),
                    )
                } else {
                    td.child(
                        div()
                            .truncate()
                            .text_color(if value.is_null() { t.ink_muted } else { t.ink })
                            .when(value.is_number(), |d| d.font_family(FONT_MONO))
                            .child(SharedString::from(text)),
                    )
                };
                tr = tr.child(td);
            }
            body = body.child(tr);
        }

        let total: f32 = widths.iter().sum();
        let mut wrapper = div().flex().flex_col().gap(px(10.)).child(
            div()
                .id("table-scroll")
                .w_full()
                .overflow_x_scroll()
                .rounded(px(4.))
                .border_1()
                .border_color(t.border)
                .child(
                    div()
                        .min_w(px(total))
                        .flex()
                        .flex_col()
                        .child(header)
                        .child(body),
                ),
        );
        if response.rows.len() > MAX_ROWS {
            wrapper = wrapper.child(hint(
                format!(
                    "Showing the first {MAX_ROWS} of {} rows. Sorting a column reorders only the rows already fetched; use ORDER BY and LIMIT to choose which rows come back.",
                    format_count(response.rows.len() as u64)
                ),
                cx,
            ));
        } else if self.table_sort.is_some() {
            wrapper = wrapper.child(hint(
                "Sorting reorders the rows already fetched. ORDER BY sorts the whole result set.",
                cx,
            ));
        }
        wrapper.into_any_element()
    }

    fn render_receipt(&mut self, execution: &Execution, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme(cx);
        let s = execution.stats;
        let mut chips = div().flex().flex_wrap().gap(px(8.));
        for (count, label) in [
            (s.nodes_created, "nodes created"),
            (s.edges_created, "edges created"),
            (s.properties_set, "properties set"),
            (s.nodes_deleted, "nodes deleted"),
            (s.edges_deleted, "edges deleted"),
            (s.schema_changes, "schema changes"),
        ] {
            if count == 0 {
                continue;
            }
            chips = chips.child(
                div()
                    .flex()
                    .items_baseline()
                    .gap(px(6.))
                    .px(px(10.))
                    .py(px(6.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.surface1)
                    .child(
                        div()
                            .font_family(FONT_MONO)
                            .font_weight(FontWeight::BOLD)
                            .text_size(px(15.))
                            .child(SharedString::from(format_count(count))),
                    )
                    .child(
                        div()
                            .text_size(px(11.))
                            .text_color(t.ink_muted)
                            .child(label),
                    ),
            );
        }
        let committed = !s.is_empty();
        let banner = div()
            .flex()
            .items_center()
            .gap(px(10.))
            .px(px(14.))
            .py(px(11.))
            .rounded(px(5.))
            .border_1()
            .border_color(t.mix(t.good, transparent(), 0.25))
            .bg(t.mix(t.good, transparent(), 0.08))
            .child(icon("check", 15.).text_color(t.good))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(2.))
                    .child(
                        div()
                            .text_size(px(13.))
                            .font_weight(FontWeight::BOLD)
                            .text_color(t.ink)
                            .child(if committed {
                                "Mutation committed"
                            } else {
                                "Transaction committed · nothing changed"
                            }),
                    )
                    .child(div().text_size(px(11.)).text_color(t.ink_secondary).child(
                        SharedString::from(format!(
                            "One atomic transaction · {} on the server · {} round trip",
                            crate::ui::format_ms(execution.server_ms),
                            crate::ui::format_ms(execution.duration_ms)
                        )),
                    )),
            );
        let mut column = div()
            .flex()
            .flex_col()
            .gap(px(16.))
            .child(banner)
            .child(chips);
        if let Some(banner) = self.graph_banner(execution, cx) {
            column = column.child(banner);
        }
        if !execution.response.columns.is_empty() {
            column = column
                .child(crate::ui::section_heading("Returning", cx))
                .child(self.render_table(execution, cx));
        }
        if self.graph.is_some() {
            column = column.child(hint(
                "The schema, the drawn graph and the inspector were refreshed after the commit.",
                cx,
            ));
        }
        column.into_any_element()
    }

    fn render_stats(&mut self, execution: &Execution, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme(cx);
        let row = execution.response.rows.first().cloned().unwrap_or_default();
        let get = |name: &str| -> u64 {
            execution
                .response
                .columns
                .iter()
                .position(|c| c == name)
                .and_then(|i| row.get(i))
                .and_then(Value::as_u64)
                .unwrap_or(0)
        };
        let tiles = div()
            .flex()
            .gap(px(32.))
            .child(stat_tile(format_count(get("nodes")), "nodes", false, cx))
            .child(stat_tile(format_count(get("edges")), "edges", false, cx))
            .child(stat_tile(
                format_count(get("node_types")),
                "node types",
                false,
                cx,
            ))
            .child(stat_tile(
                format_count(get("edge_types")),
                "edge types",
                false,
                cx,
            ))
            .child(stat_tile(
                format_count(get("stored_queries")),
                "stored queries",
                true,
                cx,
            ));
        let mut column = div().flex().flex_col().gap(px(24.)).child(tiles);
        if let Some(schema) = &self.schema {
            let mut nodes: Vec<(String, u64)> = schema
                .node_types()
                .map(|t| (t.name.clone(), t.count))
                .collect();
            let mut edges: Vec<(String, u64)> = schema
                .edge_types()
                .map(|t| (t.name.clone(), t.count))
                .collect();
            nodes.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            edges.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
            let node_colours = ordered_colours(nodes.len(), t.mode);
            let edge_colours = ordered_colours(edges.len(), t.mode);
            column = column.child(
                div()
                    .flex()
                    .gap(px(32.))
                    .child(count_bars(
                        "node-bars",
                        "Node types",
                        &nodes,
                        &node_colours,
                        cx,
                    ))
                    .child(count_bars(
                        "edge-bars",
                        "Edge types",
                        &edges,
                        &edge_colours,
                        cx,
                    )),
            );
        }
        column.into_any_element()
    }

    pub(super) fn render_wire(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme(cx);
        let source = self.editor.read(cx).text().to_owned();
        let params = self.param_values(cx).unwrap_or_default();
        let endpoint = self.connection.as_ref().map_or_else(
            || "http://127.0.0.1:7285/v1/query".to_owned(),
            |c| c.query_endpoint(),
        );
        let executed = self
            .run
            .result
            .as_ref()
            .filter(|r| r.source == source && r.params == params);
        let block = |text: String, cx: &mut Context<Self>| {
            let t = *theme(cx);
            div()
                .w_full()
                .p(px(18.))
                .rounded(px(4.))
                .border_1()
                .border_color(t.hairline)
                .bg(t.surface1)
                .font_family(FONT_MONO)
                .text_size(px(11.5))
                .line_height(px(17.))
                .text_color(t.ink_secondary)
                .child(SharedString::from(text))
        };
        let analysis = crate::nql::Analysis::of(&source);
        let mode = if analysis.has_write() {
            "one atomic write transaction"
        } else if analysis.statements.len() > 1 {
            "one request per statement, so every statement's elements can be drawn"
        } else {
            "a read"
        };
        let mut column = div().flex().flex_col().gap(px(10.)).child(
            div()
                .text_size(px(12.))
                .text_color(t.ink_muted)
                .child(SharedString::from(format!(
                    "{} POST {endpoint} · {mode}.",
                    if executed.is_some() {
                        "Executed at"
                    } else {
                        "Preview for"
                    }
                ))),
        );
        if let Some(execution) = executed {
            column = column
                .child(crate::ui::section_heading("Request", cx))
                .child(block(execution.request_json.clone(), cx))
                .child(crate::ui::section_heading("Response", cx))
                .child(block(execution.response_json.clone(), cx));
        } else {
            column = column
                .child(crate::ui::section_heading("Request", cx))
                .child(block(crate::client::wire_request(&source, &params), cx));
            if analysis.statements.len() > 1 && !analysis.has_write() {
                for (i, statement) in analysis.statements.iter().enumerate() {
                    column = column.child(div().text_size(px(11.)).text_color(t.ink_muted).child(
                        SharedString::from(format!("Statement {} · {}", i + 1, statement.keyword)),
                    ));
                }
            }
        }
        column.into_any_element()
    }
}

fn render_text(execution: &Execution, cx: &gpui::App) -> AnyElement {
    let t = theme(cx);
    let lines: Vec<String> = execution
        .response
        .rows
        .iter()
        .map(|r| r.first().map(stringify).unwrap_or_default())
        .collect();
    let column = execution
        .response
        .columns
        .first()
        .map(String::as_str)
        .unwrap_or("status");
    let colour = if column == "status" {
        t.good
    } else {
        t.ink_secondary
    };
    div()
        .flex()
        .flex_col()
        .gap(px(10.))
        .child(
            div()
                .font_family(FONT_MONO)
                .font_weight(FontWeight::BOLD)
                .text_size(px(10.))
                .text_color(t.ink_muted)
                .child(spaced(column.to_uppercase().into())),
        )
        .child(
            div()
                .p(px(18.))
                .rounded(px(4.))
                .border_1()
                .border_color(t.hairline)
                .bg(t.surface1)
                .font_family(FONT_MONO)
                .text_size(px(12.))
                .line_height(px(19.))
                .text_color(colour)
                .child(SharedString::from(lines.join("\n"))),
        )
        .into_any_element()
}

fn render_group_count(execution: &Execution, cx: &gpui::App) -> AnyElement {
    let t = theme(cx);
    let groups: Vec<(String, u64)> = execution
        .response
        .rows
        .iter()
        .map(|r| {
            (
                r.first().map(stringify).unwrap_or_default(),
                r.get(1).and_then(Value::as_u64).unwrap_or(0),
            )
        })
        .collect();
    let colours = ordered_colours(groups.len(), t.mode);
    let by = execution
        .response
        .columns
        .first()
        .cloned()
        .unwrap_or_default();
    div()
        .max_w(px(640.))
        .child(count_bars(
            "group-bars",
            format!("by {by}"),
            &groups,
            &colours,
            cx,
        ))
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn natural_order_compares_numbers_by_value() {
        assert_eq!(natural_cmp("item2", "item10"), Ordering::Less);
        assert_eq!(natural_cmp("Alice", "bob"), Ordering::Less);
        assert_eq!(compare(&Value::Null, &Value::from(1)), Ordering::Greater);
    }
}
