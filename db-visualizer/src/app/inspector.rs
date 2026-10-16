//! The inspector: one node or edge with its properties, relationships and
//! neighbours, plus the actions that turn it into a mutation.

use gpui::{AnyElement, Context, FontWeight, IntoElement, SharedString, div, prelude::*, px};
use serde_json::Value;

use super::Atlas;
use super::shell::display_heading;
use crate::results::{ElementKind, EntityDetail, stringify};
use crate::nql::ident;
use crate::theme::{FONT_MONO, theme};
use crate::ui::{Button, ButtonKind, count_badge, hint, icon, panel_error, section_heading};

/// An NQL literal for a JSON value.
#[must_use]
pub fn nql_literal(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_owned(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::String(s) => format!("'{}'", s.replace('\'', "''")),
        Value::Array(items) => format!(
            "[{}]",
            items.iter().map(nql_literal).collect::<Vec<_>>().join(", ")
        ),
        Value::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}: {}", ident(k), nql_literal(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
    }
}

/// An `UPDATE` prefilled with the element's current properties.
#[must_use]
pub fn update_template(detail: &EntityDetail) -> String {
    let var = if detail.kind == ElementKind::Node {
        "n"
    } else {
        "e"
    };
    let assignments: Vec<String> = detail
        .properties
        .iter()
        .filter(|(_, v)| !matches!(v, Value::Array(items) if items.len() > 16))
        .map(|(k, v)| format!("  {var}.{} = {}", ident(k), nql_literal(v)))
        .collect();
    let set = if assignments.is_empty() {
        format!("  {var}.property = 'value'  -- declare the property in the schema first")
    } else {
        assignments.join(",\n")
    };
    match detail.kind {
        ElementKind::Node => format!(
            "-- Edit {} {}\nUPDATE (n:{})\nSET\n{set}\nWHERE id(n) = {}\nRETURNING n;",
            detail.label,
            detail.id,
            ident(&detail.label),
            detail.id
        ),
        ElementKind::Edge => format!(
            "-- Edit {} {}\nUPDATE (a)-[e:{}]->(b)\nSET\n{set}\nWHERE id(e) = {}\nRETURNING a, e, b;",
            detail.label,
            detail.id,
            ident(&detail.label),
            detail.id
        ),
    }
}

/// An `INSERT` that connects this node to another one.
#[must_use]
pub fn connect_template(detail: &EntityDetail, edge_type: Option<&str>) -> String {
    let edge = edge_type.map_or_else(|| "EDGE_TYPE".to_owned(), ident);
    format!(
        "-- Connect {} {} to another node: set the target id\nINSERT (a)-[:{edge}]->(b)\nFROM (a), (b)\nWHERE id(a) = {} AND id(b) = $target\nRETURNING a, b;",
        detail.label, detail.id, detail.id
    )
}

impl Atlas {
    pub(super) fn render_inspector_pane(&mut self, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme(cx);
        let content = if self.detail_loading {
            hint("Loading…", cx).into_any_element()
        } else if let Some(error) = &self.detail_error {
            panel_error(SharedString::from(error.clone()), None, cx).into_any_element()
        } else if let Some(detail) = self.detail.clone() {
            self.render_detail(&detail, cx)
        } else {
            render_inspector_empty(cx)
        };
        div()
            .id("inspector")
            .flex_none()
            .w(px(256.))
            .h_full()
            .overflow_y_scroll()
            .px(px(20.))
            .py(px(24.))
            .bg(t.surface0)
            .border_l_1()
            .border_color(t.hairline)
            .child(
                div()
                    .flex()
                    .items_center()
                    .justify_between()
                    .mb(px(13.))
                    .child(section_heading("Inspector", cx))
                    .child(
                        div()
                            .font_family(FONT_MONO)
                            .text_size(px(8.))
                            .text_color(t.ink_muted)
                            .child("DETAILS"),
                    ),
            )
            .child(content)
            .into_any_element()
    }

    fn render_detail(&mut self, detail: &EntityDetail, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme(cx);
        let id = detail.id;
        let kind = detail.kind;
        let link = |key: String,
                    text: String,
                    muted: bool,
                    target: (ElementKind, u64),
                    cx: &mut Context<Self>| {
            div()
                .id(SharedString::from(key))
                .truncate()
                .font_family(FONT_MONO)
                .text_size(px(11.5))
                .text_color(if muted { t.ink_secondary } else { t.accent })
                .underline()
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| this.inspect(target.0, target.1, cx)))
                .child(SharedString::from(text))
        };

        let update = update_template(detail);
        let edge_type = self.schema.as_ref().and_then(|s| {
            s.edge_types()
                .find(|e| e.from.contains(&detail.label))
                .map(|e| e.name.clone())
        });
        let connect = connect_template(detail, edge_type.as_deref());

        let mut actions = div().flex().flex_wrap().gap(px(6.));
        if kind == ElementKind::Node {
            actions = actions.child(
                Button::new("show-neighbourhood", ButtonKind::Outline)
                    .label("Show neighbourhood")
                    .icon("network")
                    .icon_size(13.)
                    .font_size(px(11.5))
                    .on_click(cx.listener(move |this, _, _, cx| this.expand_node(id, cx))),
            );
        }
        actions = actions.child(
            Button::new("edit-entity", ButtonKind::Outline)
                .label("Edit")
                .icon("pencil")
                .icon_size(12.)
                .font_size(px(11.5))
                .on_click(cx.listener(move |this, _, _, cx| this.use_query(update.clone(), cx))),
        );
        if kind == ElementKind::Node {
            actions = actions.child(
                Button::new("connect-entity", ButtonKind::Outline)
                    .label("Connect")
                    .icon("git-fork")
                    .icon_size(12.)
                    .font_size(px(11.5))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.use_query(connect.clone(), cx)),
                    ),
            );
        }
        actions =
            actions.child(
                Button::new("delete-entity", ButtonKind::Destructive)
                    .label("Delete")
                    .icon("trash-2")
                    .icon_size(12.)
                    .font_size(px(11.5))
                    .disabled(!self.status.is_connected())
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.delete_entity(kind, id, window, cx)
                    })),
            );

        let mut properties = div().flex().flex_col();
        if detail.properties.is_empty() {
            properties = properties.child(hint("No stored properties.", cx));
        }
        for (key, value) in &detail.properties {
            properties = properties.child(
                div()
                    .flex()
                    .gap(px(8.))
                    .py(px(9.))
                    .border_b_1()
                    .border_color(t.hairline)
                    .text_size(px(11.))
                    .child(
                        div()
                            .w(px(80.))
                            .flex_none()
                            .truncate()
                            .text_color(t.ink_muted)
                            .child(SharedString::from(key.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_color(if value.is_null() { t.ink_muted } else { t.ink })
                            .child(SharedString::from(crate::results::truncate_chars(
                                &stringify(value),
                                240,
                            ))),
                    ),
            );
        }

        let mut column = div()
            .flex()
            .flex_col()
            .gap(px(26.))
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(
                        div()
                            .font_family(FONT_MONO)
                            .font_weight(FontWeight::BOLD)
                            .text_size(px(10.))
                            .text_color(t.accent)
                            .child(crate::ui::spaced(kind.title().to_uppercase().into())),
                    )
                    .child(
                        div()
                            .text_size(px(21.))
                            .line_height(px(26.))
                            .font_weight(FontWeight::BOLD)
                            .child(SharedString::from(detail.label.clone())),
                    )
                    .child(
                        div()
                            .font_family(FONT_MONO)
                            .text_size(px(11.5))
                            .text_color(t.ink_muted)
                            .child(SharedString::from(id.to_string())),
                    )
                    .child(actions),
            )
            .child(
                div()
                    .flex()
                    .flex_col()
                    .child(div().mb(px(12.)).child(section_heading("Properties", cx)))
                    .child(properties),
            );

        if kind == ElementKind::Edge
            && let Some((source, target)) = &detail.endpoints
        {
            let mut list = div().flex().flex_col();
            for (role, node) in [("source", source), ("target", target)] {
                let mut li = div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .min_h(px(31.))
                    .border_b_1()
                    .border_color(t.hairline)
                    .child(
                        div()
                            .min_w(px(46.))
                            .text_size(px(11.))
                            .text_color(t.ink_muted)
                            .child(role),
                    );
                li = match node {
                    Some(node) => li.child(link(
                        format!("endpoint-{role}"),
                        format!("{} · {}", node.label, node.id),
                        false,
                        (ElementKind::Node, node.id),
                        cx,
                    )),
                    None => li.child(hint("unavailable", cx)),
                };
                list = list.child(li);
            }
            column = column.child(
                div()
                    .flex()
                    .flex_col()
                    .child(div().mb(px(12.)).child(section_heading("Endpoints", cx)))
                    .child(list),
            );
        }

        if kind == ElementKind::Node {
            let mut relationships = div().flex().flex_col();
            if detail.edges.is_empty() {
                relationships = relationships.child(hint("Nothing connected.", cx));
            }
            for edge in &detail.edges {
                let outgoing = edge.source == id;
                let other = if outgoing { edge.target } else { edge.source };
                relationships = relationships.child(
                    div()
                        .flex()
                        .items_center()
                        .gap(px(8.))
                        .min_h(px(31.))
                        .border_b_1()
                        .border_color(t.hairline)
                        .child(
                            icon(
                                if outgoing {
                                    "arrow-right"
                                } else {
                                    "arrow-left"
                                },
                                12.,
                            )
                            .text_color(t.ink_muted),
                        )
                        .child(link(
                            format!("rel-{}", edge.id),
                            edge.label.clone(),
                            false,
                            (ElementKind::Edge, edge.id),
                            cx,
                        ))
                        .child(link(
                            format!("rel-other-{}", edge.id),
                            other.to_string(),
                            true,
                            (ElementKind::Node, other),
                            cx,
                        )),
                );
            }
            if let Some(degree) = detail.degree
                && (detail.edges.len() as u64) < degree
            {
                relationships = relationships.child(div().mt(px(8.)).child(hint(
                    format!(
                        "Showing {} of {degree}. Open the neighbourhood to see every relationship.",
                        detail.edges.len()
                    ),
                    cx,
                )));
            }
            let degree_badge = detail.degree.unwrap_or(detail.edges.len() as u64);
            column = column.child(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .mb(px(12.))
                            .child(section_heading("Relationships", cx))
                            .child(count_badge(degree_badge.to_string(), cx)),
                    )
                    .child(relationships),
            );

            let mut neighbours = div().flex().flex_col();
            if detail.neighbours.is_empty() {
                neighbours = neighbours.child(hint("No neighbours.", cx));
            }
            for node in &detail.neighbours {
                neighbours = neighbours.child(
                    div()
                        .flex()
                        .items_center()
                        .min_h(px(31.))
                        .border_b_1()
                        .border_color(t.hairline)
                        .child(link(
                            format!("neighbour-{}", node.id),
                            format!("{} · {}", crate::results::node_caption(node), node.id),
                            false,
                            (ElementKind::Node, node.id),
                            cx,
                        )),
                );
            }
            column = column.child(
                div()
                    .flex()
                    .flex_col()
                    .child(
                        div()
                            .flex()
                            .items_center()
                            .gap(px(6.))
                            .mb(px(12.))
                            .child(section_heading("Neighbours", cx))
                            .child(count_badge(detail.neighbours.len().to_string(), cx)),
                    )
                    .child(neighbours),
            );
        }
        column.into_any_element()
    }
}

fn render_inspector_empty(cx: &gpui::App) -> AnyElement {
    let t = theme(cx);
    let guide = |title: &'static str, text: &'static str| {
        div()
            .mb(px(14.))
            .text_size(px(10.))
            .child(
                div()
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(t.ink_secondary)
                    .child(title),
            )
            .child(div().mt(px(3.)).text_color(t.ink_muted).child(text))
    };
    div()
        .pt(px(36.))
        .flex()
        .flex_col()
        .child(
            div()
                .w(px(106.))
                .h(px(112.))
                .p(px(18.))
                .mb(px(26.))
                .rounded(px(4.))
                .border_1()
                .border_color(t.border)
                .bg(t.surface1)
                .text_color(t.ink_muted)
                .child(icon("mouse-pointer-2", 22.).text_color(t.ink_muted).mb(px(12.)))
                .child(div().h(px(1.)).mt(px(9.)).bg(t.border))
                .child(div().h(px(1.)).mt(px(9.)).bg(t.border))
                .child(div().h(px(1.)).mt(px(9.)).w(px(45.)).bg(t.border)),
        )
        .child(div().max_w(px(200.)).mb(px(12.)).child(display_heading("The details are in the connections.", 23., cx)))
        .child(
            div()
                .text_size(px(12.))
                .line_height(px(21.))
                .text_color(t.ink_muted)
                .child("Select a node or edge in the graph, or click an id in the results. From here you can edit, connect or delete it."),
        )
        .child(
            div()
                .mt(px(28.))
                .pt(px(18.))
                .border_t_1()
                .border_color(t.hairline)
                .child(guide("Single click", "Inspect properties"))
                .child(guide("Double click", "Explore neighbours"))
                .child(guide("Drag & scroll", "Move around the graph")),
        )
        .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;

    #[test]
    fn literals_escape_quotes() {
        assert_eq!(nql_literal(&Value::from("O'Hara")), "'O''Hara'");
        assert_eq!(nql_literal(&serde_json::json!([1, "a"])), "[1, 'a']");
    }

    #[test]
    fn update_template_lists_current_properties() {
        let mut properties = BTreeMap::new();
        properties.insert("name".to_owned(), Value::from("Ada"));
        properties.insert("age".to_owned(), Value::from(36));
        let detail = EntityDetail {
            kind: ElementKind::Node,
            id: 7,
            label: "User".into(),
            properties,
            edges: vec![],
            neighbours: vec![],
            degree: None,
            endpoints: None,
        };
        let text = update_template(&detail);
        assert!(text.contains("UPDATE (n:User)"));
        assert!(text.contains("n.name = 'Ada'"));
        assert!(text.contains("WHERE id(n) = 7"));
    }
}
