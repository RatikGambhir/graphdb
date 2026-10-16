//! The Schema workspace: a catalog of declared node and edge types, vector
//! indexes and stored queries, each expandable to its properties.

use gpui::{
    AnyElement, Context, FontWeight, IntoElement, SharedString, Window, div, prelude::*, px,
};

use super::shell::{display_heading, workspace_eyebrow};
use super::{Atlas, SchemaCategory};
use crate::nql;
use crate::schema::{Schema, TypeInfo};
use crate::theme::{FONT_MONO, theme, transparent};
use crate::ui::{Button, ButtonKind, format_count, hint, icon, spinner};

impl Atlas {
    pub(super) fn render_schema(
        &mut self,
        _window: &mut Window,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let t = *theme(cx);
        let search = self.schema_search.read(cx).text().to_owned();
        let category = self.schema_category;
        let schema = self.schema.clone().unwrap_or_default();
        let counts = [
            (
                SchemaCategory::Nodes,
                "Nodes",
                "circle-dot",
                schema.node_types().count(),
            ),
            (
                SchemaCategory::Edges,
                "Edges",
                "git-fork",
                schema.edge_types().count(),
            ),
            (
                SchemaCategory::Vectors,
                "Vectors",
                "zap",
                schema.vector_indexes().len(),
            ),
            (
                SchemaCategory::Queries,
                "Queries",
                "braces",
                schema.queries.len(),
            ),
        ];
        let visible_keys = visible_keys(&schema, category, &search);
        let all_expanded = !visible_keys.is_empty()
            && visible_keys
                .iter()
                .all(|k| self.schema_expanded.contains(k));

        let mut tabs = div().h_full().flex().gap(px(16.));
        for (which, label, name, count) in counts {
            let active = which == category;
            tabs = tabs.child(
                div()
                    .id(label)
                    .h_full()
                    .px(px(2.))
                    .flex()
                    .items_center()
                    .gap(px(6.))
                    .border_b_2()
                    .border_color(if active { t.accent } else { transparent() })
                    .text_size(px(12.))
                    .font_weight(FontWeight::SEMIBOLD)
                    .text_color(if active { t.ink } else { t.ink_muted })
                    .cursor_pointer()
                    .hover(move |s| s.text_color(t.ink))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.schema_category = which;
                        this.schema_search.update(cx, |f, cx| f.set_text("", cx));
                        cx.notify();
                    }))
                    .child(icon(name, 15.).text_color(if active { t.accent } else { t.ink_muted }))
                    .child(label)
                    .child(
                        div()
                            .font_family(FONT_MONO)
                            .text_size(px(10.))
                            .text_color(t.ink_muted)
                            .child(SharedString::from(count.to_string())),
                    ),
            );
        }

        let toolbar = div()
            .flex_none()
            .h(px(57.))
            .px(px(24.))
            .flex()
            .items_center()
            .gap(px(16.))
            .bg(t.surface1)
            .border_b_1()
            .border_color(t.hairline)
            .child(
                div()
                    .w(px(280.))
                    .h(px(34.))
                    .flex()
                    .items_center()
                    .gap(px(4.))
                    .pl(px(10.))
                    .rounded(px(4.))
                    .border_1()
                    .border_color(t.border)
                    .bg(t.control)
                    .text_color(t.ink_muted)
                    .child(icon("search", 15.).text_color(t.ink_muted))
                    .child(div().flex_1().min_w_0().child(self.schema_search.clone()))
                    .when(!search.is_empty(), |d| {
                        d.child(
                            div()
                                .id("clear-search")
                                .size(px(22.))
                                .mr(px(4.))
                                .flex()
                                .items_center()
                                .justify_center()
                                .rounded_full()
                                .cursor_pointer()
                                .hover(move |s| s.bg(t.surface2))
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.schema_search.update(cx, |f, cx| f.set_text("", cx));
                                }))
                                .child(icon("x", 12.)),
                        )
                    }),
            )
            .child(tabs)
            .child(div().flex_1())
            .child(
                div()
                    .flex()
                    .items_center()
                    .gap(px(8.))
                    .child(
                        Button::new("expand-all", ButtonKind::Outline)
                            .label(if all_expanded { "Collapse" } else { "Expand" })
                            .icon("chevrons-up-down")
                            .font_size(px(11.))
                            .disabled(visible_keys.is_empty())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                for key in &visible_keys {
                                    if all_expanded {
                                        this.schema_expanded.remove(key);
                                    } else {
                                        this.schema_expanded.insert(key.clone());
                                    }
                                }
                                cx.notify();
                            })),
                    )
                    .child(div().w(px(1.)).h(px(25.)).bg(t.hairline))
                    .child(
                        Button::new("schema-refresh", ButtonKind::Outline)
                            .label(if self.schema_loading {
                                "Refreshing…"
                            } else {
                                "Refresh"
                            })
                            .icon("refresh-cw")
                            .font_size(px(11.))
                            .loading(self.schema_loading)
                            .disabled(!self.status.is_connected())
                            .on_click(cx.listener(|this, _, _, cx| this.refresh_schema(cx))),
                    ),
            );

        let (category_label, category_count) = match category {
            SchemaCategory::Nodes => ("node types", schema.node_types().count()),
            SchemaCategory::Edges => ("edge types", schema.edge_types().count()),
            SchemaCategory::Vectors => ("vector indexes", schema.vector_indexes().len()),
            SchemaCategory::Queries => ("stored queries", schema.queries.len()),
        };
        let intro = div()
            .max_w(px(1200.))
            .w_full()
            .mx_auto()
            .mb(px(25.))
            .pb(px(22.))
            .flex()
            .items_end()
            .justify_between()
            .border_b_1()
            .border_color(t.border)
            .child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(8.))
                    .child(workspace_eyebrow("WORKSPACE / STRUCTURE", cx))
                    .child(display_heading("Schema catalog", 36., cx))
                    .child(
                        div()
                            .text_size(px(12.))
                            .text_color(t.ink_secondary)
                            .child("Every type, property and index declared in your graph."),
                    ),
            )
            .when(self.schema.is_some(), |d| {
                d.child(
                    div()
                        .flex()
                        .flex_col()
                        .items_end()
                        .child(
                            div()
                                .font_family(FONT_MONO)
                                .font_weight(FontWeight::BOLD)
                                .text_size(px(26.))
                                .child(SharedString::from(category_count.to_string())),
                        )
                        .child(
                            div()
                                .mt(px(5.))
                                .font_family(FONT_MONO)
                                .font_weight(FontWeight::SEMIBOLD)
                                .text_size(px(10.))
                                .text_color(t.ink_muted)
                                .child(SharedString::from(category_label.to_uppercase())),
                        ),
                )
            });

        let body = if let Some(error) = self.schema_error.clone() {
            page_state(
                "circle-alert",
                "Couldn’t load the schema",
                error,
                Some(("Try again", true)),
                true,
                cx,
            )
        } else if !self.status.is_connected() {
            page_state(
                "circle-dot",
                "Connect to inspect your schema",
                "Choose a local or remote NexaDB server to read its declared node and edge types."
                    .to_owned(),
                Some(("Connect Now", false)),
                false,
                cx,
            )
        } else if self.schema_loading && self.schema.is_none() {
            div()
                .flex()
                .flex_col()
                .items_center()
                .gap(px(8.))
                .py(px(48.))
                .child(spinner("schema-spinner", 22., t.accent))
                .child(display_heading("Reading schema", 23., cx))
                .child(hint(
                    "Reading the declared types from the connected server…",
                    cx,
                ))
                .into_any_element()
        } else {
            self.render_schema_entries(&schema, category, &search, cx)
        };

        div()
            .size_full()
            .flex()
            .flex_col()
            .child(toolbar)
            .child(
                div()
                    .id("schema-content")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scroll()
                    .px(px(48.))
                    .pt(px(28.))
                    .pb(px(50.))
                    .child(intro)
                    .child(div().max_w(px(1200.)).w_full().mx_auto().child(body)),
            )
            .into_any_element()
    }

    fn render_schema_entries(
        &mut self,
        schema: &Schema,
        category: SchemaCategory,
        search: &str,
        cx: &mut Context<Self>,
    ) -> AnyElement {
        let mut list = div().flex().flex_col();
        let mut empty = true;
        match category {
            SchemaCategory::Nodes | SchemaCategory::Edges => {
                let entries = if category == SchemaCategory::Nodes {
                    Schema::filter(schema.node_types(), search)
                } else {
                    Schema::filter(schema.edge_types(), search)
                };
                for entry in entries {
                    empty = false;
                    list = list.child(self.render_type_card(entry, cx));
                }
            }
            SchemaCategory::Vectors => {
                let needle = search.trim().to_lowercase();
                for (owner, property) in schema.vector_indexes() {
                    let key = format!("{}.{}", owner.name, property.name);
                    if !needle.is_empty() && !key.to_lowercase().contains(&needle) {
                        continue;
                    }
                    empty = false;
                    let query = format!(
                        "-- Nearest neighbours by {key}; bind $vector in Parameters as JSON\nSELECT x, similarity(x.{}, $vector) AS score\nFROM (x:{})\nORDER BY similarity(x.{}, $vector) DESC\nLIMIT 10",
                        nql::ident(&property.name),
                        nql::ident(&owner.name),
                        nql::ident(&property.name)
                    );
                    list = list.child(simple_card(
                        &key,
                        "zap",
                        format!("{} · HNSW index", property.ty),
                        vec![
                            ("Type", owner.name.clone()),
                            ("Property", property.name.clone()),
                            ("Elements", format_count(owner.count)),
                        ],
                        query,
                        "Open similarity query",
                        &mut self.schema_expanded,
                        cx,
                    ));
                }
            }
            SchemaCategory::Queries => {
                let needle = search.trim().to_lowercase();
                for q in &schema.queries {
                    if !needle.is_empty() && !q.name.to_lowercase().contains(&needle) {
                        continue;
                    }
                    empty = false;
                    let params: Vec<String> = q.params.iter().map(|p| p.name.clone()).collect();
                    list = list.child(simple_card(
                        &q.name,
                        "braces",
                        format!(
                            "{} parameter{}",
                            q.params.len(),
                            if q.params.len() == 1 { "" } else { "s" }
                        ),
                        vec![("Signature", q.signature()), ("Body", q.body.clone())],
                        nql::call_query(&q.name, &params),
                        "Open CALL in Query",
                        &mut self.schema_expanded,
                        cx,
                    ));
                }
            }
        }
        if empty {
            let (title, text) = if search.trim().is_empty() {
                match category {
                    SchemaCategory::Nodes => (
                        "No node types declared",
                        "Declare one with CREATE NODE in the Query workspace.",
                    ),
                    SchemaCategory::Edges => (
                        "No edge types declared",
                        "Declare one with CREATE EDGE … FROM … TO … in the Query workspace.",
                    ),
                    SchemaCategory::Vectors => (
                        "No vector indexes declared",
                        "A VECTOR(n) property gets an HNSW index automatically.",
                    ),
                    SchemaCategory::Queries => (
                        "No stored queries",
                        "Save one with CREATE QUERY name($param TYPE) AS SELECT …",
                    ),
                }
            } else {
                (
                    "No matching entries",
                    "Nothing in this schema matches the search.",
                )
            };
            return page_state("search", title, text.to_owned(), None, false, cx);
        }
        list.into_any_element()
    }

    fn render_type_card(&mut self, entry: &TypeInfo, cx: &mut Context<Self>) -> AnyElement {
        let t = *theme(cx);
        let key = format!("{}:{}", entry.kind, entry.name);
        let expanded = self.schema_expanded.contains(&key);
        let is_node = entry.is_node();
        let toggle_key = key.clone();
        let summary = div()
            .id(SharedString::from(format!("card-{key}")))
            .min_h(px(68.))
            .px(px(2.))
            .py(px(12.))
            .flex()
            .items_center()
            .gap(px(12.))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                if !this.schema_expanded.remove(&toggle_key) {
                    this.schema_expanded.insert(toggle_key.clone());
                }
                cx.notify();
            }))
            .child(
                div()
                    .size(px(28.))
                    .flex_none()
                    .flex()
                    .items_center()
                    .justify_center()
                    .rounded(px(4.))
                    .bg(t.surface2)
                    .text_color(t.accent)
                    .child(
                        icon(if is_node { "circle-dot" } else { "git-fork" }, 16.)
                            .text_color(t.accent),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .flex()
                    .items_baseline()
                    .gap(px(12.))
                    .child(
                        div()
                            .truncate()
                            .text_size(px(15.))
                            .font_weight(FontWeight::BOLD)
                            .child(SharedString::from(entry.name.clone())),
                    )
                    .when(!is_node, |d| {
                        d.child(
                            div()
                                .truncate()
                                .font_family(FONT_MONO)
                                .text_size(px(11.))
                                .text_color(t.ink_muted)
                                .child(SharedString::from(entry.endpoints())),
                        )
                    }),
            )
            .child(
                div()
                    .font_family(FONT_MONO)
                    .text_size(px(11.))
                    .text_color(t.ink_muted)
                    .child(SharedString::from(format!(
                        "{} {}",
                        format_count(entry.count),
                        if is_node { "nodes" } else { "edges" }
                    ))),
            )
            .child(
                icon(
                    if expanded {
                        "chevron-down"
                    } else {
                        "chevron-right"
                    },
                    16.,
                )
                .text_color(t.ink_muted),
            );

        let mut card = div()
            .border_b_1()
            .border_color(if expanded { t.accent } else { t.border })
            .hover(move |s| s.border_color(t.accent))
            .child(summary);
        if expanded {
            let browse = if is_node {
                nql::browse_node_type(&entry.name)
            } else {
                nql::browse_edge_type(&entry.name)
            };
            let graph = if is_node {
                nql::graph_node_type(&entry.name)
            } else {
                nql::graph_edge_type(&entry.name)
            };
            let insert = if is_node {
                let cols: Vec<&str> = entry
                    .properties
                    .iter()
                    .filter(|p| !p.is_vector())
                    .map(|p| p.name.as_str())
                    .collect();
                format!(
                    "INSERT INTO {} ({})\nVALUES ({})\nRETURNING *;",
                    nql::ident(&entry.name),
                    cols.iter()
                        .map(|c| nql::ident(c))
                        .collect::<Vec<_>>()
                        .join(", "),
                    cols.iter().map(|_| "NULL").collect::<Vec<_>>().join(", ")
                )
            } else {
                format!(
                    "INSERT (a)-[:{} {{}}]->(b)\nFROM (a:{}), (b:{})\nWHERE id(a) = $from AND id(b) = $to\nRETURNING a, b;",
                    nql::ident(&entry.name),
                    nql::ident(entry.from.first().map_or("Node", String::as_str)),
                    nql::ident(entry.to.first().map_or("Node", String::as_str))
                )
            };
            let meta = |label: &'static str, value: String| {
                div()
                    .flex()
                    .flex_col()
                    .gap(px(3.))
                    .text_size(px(11.))
                    .child(div().text_color(t.ink_muted).child(label))
                    .child(div().text_color(t.ink).child(SharedString::from(value)))
            };
            let mut left = div()
                .w(px(180.))
                .flex_none()
                .flex()
                .flex_col()
                .gap(px(16.))
                .child(meta(
                    "Kind",
                    if is_node {
                        "Node type".into()
                    } else {
                        "Edge type".into()
                    },
                ))
                .child(meta("Elements", format_count(entry.count)))
                .child(meta("Properties", entry.properties.len().to_string()));
            if !is_node {
                left = left.child(meta("Endpoints", entry.endpoints()));
            }
            left = left
                .child(
                    Button::new(
                        SharedString::from(format!("browse-{key}")),
                        ButtonKind::Outline,
                    )
                    .label("Open in Query")
                    .font_size(px(11.))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.use_query(browse.clone(), cx)),
                    ),
                )
                .child(
                    Button::new(
                        SharedString::from(format!("graph-{key}")),
                        ButtonKind::Outline,
                    )
                    .label("Draw as graph")
                    .font_size(px(11.))
                    .on_click(cx.listener(move |this, _, _, cx| this.use_query(graph.clone(), cx))),
                )
                .child(
                    Button::new(
                        SharedString::from(format!("insert-{key}")),
                        ButtonKind::Outline,
                    )
                    .label(if is_node {
                        "Insert a node"
                    } else {
                        "Insert an edge"
                    })
                    .icon("plus")
                    .icon_size(12.)
                    .font_size(px(11.))
                    .on_click(
                        cx.listener(move |this, _, _, cx| this.use_query(insert.clone(), cx)),
                    ),
                );

            let mut table = div()
                .flex()
                .flex_col()
                .rounded(px(4.))
                .border_1()
                .border_color(t.hairline)
                .overflow_hidden()
                .child(
                    div()
                        .flex()
                        .bg(t.mix(t.surface2, transparent(), 0.58))
                        .font_family(FONT_MONO)
                        .font_weight(FontWeight::BOLD)
                        .text_size(px(9.))
                        .text_color(t.ink_muted)
                        .child(
                            div()
                                .w(px(160.))
                                .flex_none()
                                .px(px(12.))
                                .py(px(6.))
                                .child("PROPERTY"),
                        )
                        .child(
                            div()
                                .w(px(120.))
                                .flex_none()
                                .px(px(12.))
                                .py(px(6.))
                                .child("TYPE"),
                        )
                        .child(div().flex_1().px(px(12.)).py(px(6.)).child("CONSTRAINTS"))
                        .child(
                            div()
                                .w(px(120.))
                                .flex_none()
                                .px(px(12.))
                                .py(px(6.))
                                .child("INDEX"),
                        ),
                );
            if entry.properties.is_empty() {
                table = table.child(div().p(px(10.)).child(hint(
                    format!(
                        "No properties declared on this {}.",
                        if is_node { "node type" } else { "edge type" }
                    ),
                    cx,
                )));
            }
            for property in &entry.properties {
                table = table.child(
                    div()
                        .flex()
                        .border_t_1()
                        .border_color(t.hairline)
                        .text_size(px(11.))
                        .child(
                            div()
                                .w(px(160.))
                                .flex_none()
                                .px(px(12.))
                                .py(px(10.))
                                .truncate()
                                .font_family(FONT_MONO)
                                .text_color(t.ink)
                                .child(SharedString::from(property.name.clone())),
                        )
                        .child(
                            div()
                                .w(px(120.))
                                .flex_none()
                                .px(px(12.))
                                .py(px(10.))
                                .font_family(FONT_MONO)
                                .text_color(t.syntax.label)
                                .child(SharedString::from(property.ty.clone())),
                        )
                        .child(
                            div()
                                .flex_1()
                                .px(px(12.))
                                .py(px(10.))
                                .text_color(t.ink_secondary)
                                .child(SharedString::from(if property.modifiers().is_empty() {
                                    "—".to_owned()
                                } else {
                                    property.modifiers()
                                })),
                        )
                        .child(
                            div()
                                .w(px(120.))
                                .flex_none()
                                .px(px(12.))
                                .py(px(10.))
                                .font_family(FONT_MONO)
                                .text_color(t.ink_muted)
                                .child(SharedString::from(property.index_summary())),
                        ),
                );
            }
            card = card.child(
                div()
                    .flex()
                    .gap(px(32.))
                    .pl(px(42.))
                    .pt(px(24.))
                    .pb(px(28.))
                    .child(left)
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .flex()
                            .flex_col()
                            .gap(px(8.))
                            .child(
                                div()
                                    .flex()
                                    .justify_between()
                                    .child(
                                        div()
                                            .text_size(px(11.))
                                            .font_weight(FontWeight::BOLD)
                                            .child("PROPERTIES"),
                                    )
                                    .child(div().text_size(px(10.)).text_color(t.ink_muted).child(
                                        SharedString::from(format!(
                                            "{} declared",
                                            entry.properties.len()
                                        )),
                                    )),
                            )
                            .child(table),
                    ),
            );
        }
        card.into_any_element()
    }
}

fn visible_keys(schema: &Schema, category: SchemaCategory, search: &str) -> Vec<String> {
    match category {
        SchemaCategory::Nodes => Schema::filter(schema.node_types(), search)
            .iter()
            .map(|t| format!("{}:{}", t.kind, t.name))
            .collect(),
        SchemaCategory::Edges => Schema::filter(schema.edge_types(), search)
            .iter()
            .map(|t| format!("{}:{}", t.kind, t.name))
            .collect(),
        SchemaCategory::Vectors => schema
            .vector_indexes()
            .iter()
            .map(|(t, p)| format!("{}.{}", t.name, p.name))
            .collect(),
        SchemaCategory::Queries => schema.queries.iter().map(|q| q.name.clone()).collect(),
    }
}

#[allow(clippy::too_many_arguments)]
fn simple_card(
    key: &str,
    icon_name: &'static str,
    meta: String,
    rows: Vec<(&'static str, String)>,
    query: String,
    action: &'static str,
    expanded_set: &mut std::collections::HashSet<String>,
    cx: &mut Context<Atlas>,
) -> AnyElement {
    let t = *theme(cx);
    let expanded = expanded_set.contains(key);
    let toggle = key.to_owned();
    let mut card = div()
        .border_b_1()
        .border_color(if expanded { t.accent } else { t.border })
        .hover(move |s| s.border_color(t.accent))
        .child(
            div()
                .id(SharedString::from(format!("simple-{key}")))
                .min_h(px(68.))
                .py(px(12.))
                .px(px(2.))
                .flex()
                .items_center()
                .gap(px(12.))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.schema_expanded.remove(&toggle) {
                        this.schema_expanded.insert(toggle.clone());
                    }
                    cx.notify();
                }))
                .child(
                    div()
                        .size(px(28.))
                        .flex_none()
                        .flex()
                        .items_center()
                        .justify_center()
                        .rounded(px(4.))
                        .bg(t.surface2)
                        .child(icon(icon_name, 16.).text_color(t.accent)),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_size(px(15.))
                        .font_weight(FontWeight::BOLD)
                        .child(SharedString::from(key.to_owned())),
                )
                .child(
                    div()
                        .font_family(FONT_MONO)
                        .text_size(px(11.))
                        .text_color(t.ink_muted)
                        .child(SharedString::from(meta)),
                )
                .child(
                    icon(
                        if expanded {
                            "chevron-down"
                        } else {
                            "chevron-right"
                        },
                        16.,
                    )
                    .text_color(t.ink_muted),
                ),
        );
    if expanded {
        let mut detail = div()
            .pl(px(42.))
            .pt(px(16.))
            .pb(px(24.))
            .flex()
            .flex_col()
            .gap(px(14.));
        for (label, value) in rows {
            detail = detail.child(
                div()
                    .flex()
                    .flex_col()
                    .gap(px(4.))
                    .text_size(px(11.))
                    .child(div().text_color(t.ink_muted).child(label))
                    .child(
                        div()
                            .font_family(FONT_MONO)
                            .text_color(t.ink)
                            .child(SharedString::from(value)),
                    ),
            );
        }
        detail = detail.child(
            div().child(
                Button::new(
                    SharedString::from(format!("simple-open-{key}")),
                    ButtonKind::Outline,
                )
                .label(action)
                .font_size(px(11.))
                .on_click(cx.listener(move |this, _, _, cx| this.use_query(query.clone(), cx))),
            ),
        );
        card = card.child(detail);
    }
    card.into_any_element()
}

fn page_state(
    icon_name: &'static str,
    title: &'static str,
    text: String,
    action: Option<(&'static str, bool)>,
    error: bool,
    cx: &mut Context<Atlas>,
) -> AnyElement {
    let t = *theme(cx);
    let colour = if error { t.danger } else { t.accent };
    div()
        .min_h(px(340.))
        .flex()
        .flex_col()
        .items_center()
        .justify_center()
        .gap(px(8.))
        .py(px(48.))
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
                .text_color(colour)
                .child(icon(icon_name, 24.).text_color(colour)),
        )
        .child(div().mt(px(6.)).child(display_heading(title, 23., cx)))
        .child(
            div()
                .max_w(px(430.))
                .text_center()
                .text_size(px(13.))
                .line_height(px(20.))
                .text_color(t.ink_muted)
                .child(SharedString::from(text)),
        )
        .when_some(action, |d, (label, refresh)| {
            d.child(
                div().mt(px(8.)).child(
                    Button::new("page-action", ButtonKind::Primary)
                        .label(label)
                        .on_click(cx.listener(move |this, _, window, cx| {
                            if refresh {
                                this.refresh_schema(cx);
                            } else {
                                this.open_connection(window, cx);
                            }
                        })),
                ),
            )
        })
        .into_any_element()
}
