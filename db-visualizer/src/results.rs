//! View models built from query responses: tables, graphs drawn from the
//! elements inside rows, mutation receipts, and the inspector detail.

#[cfg(test)]
use std::collections::HashMap;
use std::collections::{BTreeMap, HashSet};

use serde_json::{Map, Value};

use crate::client::{Executed, QueryResponse, Stats};
use crate::rql::Analysis;

/// A node as the server serialises it: `{"_id", "_label", ...properties}`.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphNode {
    pub id: u64,
    pub label: String,
    pub properties: BTreeMap<String, Value>,
}

/// An edge: `{"_id", "_label", "_from", "_to", ...properties}`.
#[derive(Clone, Debug, PartialEq)]
pub struct GraphEdge {
    pub id: u64,
    pub label: String,
    pub source: u64,
    pub target: u64,
    pub properties: BTreeMap<String, Value>,
}

/// Nodes plus the edges among them, ready to draw.
#[derive(Clone, Debug, Default, PartialEq)]
pub struct GraphData {
    pub nodes: Vec<GraphNode>,
    pub edges: Vec<GraphEdge>,
    /// Edges whose endpoints were not in the result; they are not drawn.
    pub dangling_edges: usize,
}

impl GraphData {
    /// `true` when there is nothing to draw.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty()
    }

    /// Count of each node label, most common first, ties alphabetically.
    #[cfg(test)]
    #[must_use]
    pub fn label_counts(&self) -> Vec<(String, u64)> {
        let mut counts: HashMap<&str, u64> = HashMap::new();
        for node in &self.nodes {
            *counts.entry(node.label.as_str()).or_default() += 1;
        }
        let mut list: Vec<(String, u64)> =
            counts.into_iter().map(|(k, v)| (k.to_owned(), v)).collect();
        list.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        list
    }
}

/// What kind of element a JSON value is.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ElementKind {
    Node,
    Edge,
}

impl ElementKind {
    /// `Node` / `Edge`.
    #[must_use]
    pub fn title(self) -> &'static str {
        match self {
            Self::Node => "Node",
            Self::Edge => "Edge",
        }
    }
}

/// `Some(kind)` when the value is a serialised node or edge.
#[must_use]
pub fn element_kind(value: &Value) -> Option<ElementKind> {
    let map = value.as_object()?;
    if !map.contains_key("_id") || !map.contains_key("_label") {
        return None;
    }
    Some(if map.contains_key("_from") && map.contains_key("_to") {
        ElementKind::Edge
    } else {
        ElementKind::Node
    })
}

fn as_u64(value: &Value) -> Option<u64> {
    value
        .as_u64()
        .or_else(|| value.as_i64().and_then(|i| u64::try_from(i).ok()))
        .or_else(|| value.as_str().and_then(|s| s.parse().ok()))
}

fn properties_of(map: &Map<String, Value>) -> BTreeMap<String, Value> {
    map.iter()
        .filter(|(k, _)| !k.starts_with('_'))
        .map(|(k, v)| (k.clone(), v.clone()))
        .collect()
}

/// Decodes a node map.
#[must_use]
pub fn node_from_value(value: &Value) -> Option<GraphNode> {
    let map = value.as_object()?;
    Some(GraphNode {
        id: as_u64(map.get("_id")?)?,
        label: map.get("_label")?.as_str()?.to_owned(),
        properties: properties_of(map),
    })
}

/// Decodes an edge map.
#[must_use]
pub fn edge_from_value(value: &Value) -> Option<GraphEdge> {
    let map = value.as_object()?;
    Some(GraphEdge {
        id: as_u64(map.get("_id")?)?,
        label: map.get("_label")?.as_str()?.to_owned(),
        source: as_u64(map.get("_from")?)?,
        target: as_u64(map.get("_to")?)?,
        properties: properties_of(map),
    })
}

/// Collects every node and edge found anywhere in the rows, including inside
/// lists (`collect(...)`), deduplicated by id.
#[derive(Clone, Debug, Default)]
pub struct GraphBuilder {
    nodes: BTreeMap<u64, GraphNode>,
    edges: BTreeMap<u64, GraphEdge>,
}

impl GraphBuilder {
    /// Adds every element in a response.
    pub fn add_response(&mut self, response: &QueryResponse) {
        for row in &response.rows {
            for cell in row {
                self.visit(cell);
            }
        }
    }

    fn visit(&mut self, value: &Value) {
        match value {
            Value::Array(items) => items.iter().for_each(|v| self.visit(v)),
            Value::Object(_) => match element_kind(value) {
                Some(ElementKind::Node) => {
                    if let Some(node) = node_from_value(value) {
                        self.nodes.entry(node.id).or_insert(node);
                    }
                }
                Some(ElementKind::Edge) => {
                    if let Some(edge) = edge_from_value(value) {
                        self.edges.entry(edge.id).or_insert(edge);
                    }
                }
                None => {}
            },
            _ => {}
        }
    }

    /// `true` when no element was found.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.nodes.is_empty() && self.edges.is_empty()
    }

    /// Finishes the graph, dropping edges that leave the node set.
    #[must_use]
    pub fn build(self) -> GraphData {
        let ids: HashSet<u64> = self.nodes.keys().copied().collect();
        let mut dangling = 0;
        let edges: Vec<GraphEdge> = self
            .edges
            .into_values()
            .filter(|e| {
                let ok = ids.contains(&e.source) && ids.contains(&e.target);
                if !ok {
                    dangling += 1;
                }
                ok
            })
            .collect();
        GraphData {
            nodes: self.nodes.into_values().collect(),
            edges,
            dangling_edges: dangling,
        }
    }
}

/// How the Results tab should present a response.
#[derive(Clone, Debug, PartialEq)]
pub enum ResultView {
    /// A plain table.
    Rows,
    /// A single `status` / `plan` / `ddl` column: show as text.
    Text,
    /// `SHOW STATS`.
    Stats,
    /// Two columns, label and count, from a `GROUP BY`.
    GroupCount,
    /// A write: counts plus any `RETURNING` rows.
    Mutation,
}

/// A finished run, with everything the output tabs need.
#[derive(Clone, Debug, PartialEq)]
pub struct Execution {
    pub source: String,
    pub params: Map<String, Value>,
    pub endpoint: String,
    pub request_json: String,
    pub response_json: String,
    pub response: QueryResponse,
    pub stats: Stats,
    pub duration_ms: f64,
    pub server_ms: f64,
    pub view: ResultView,
    pub is_write: bool,
    /// Elements found in the rows, if any.
    pub graph: Option<GraphData>,
    /// `true` when every column is an element (a "pure" graph query).
    pub pure_graph: bool,
    pub summary: String,
}

/// Builds the execution record from one or more executed statements. Several
/// responses arise when a read-only script was run statement by statement;
/// the table shows the last one and the graph unions all of them.
#[must_use]
pub fn build_execution(
    source: &str,
    params: &Map<String, Value>,
    endpoint: &str,
    analysis: &Analysis,
    executed: &[Executed],
) -> Execution {
    let last = executed
        .last()
        .map(|e| e.response.clone())
        .unwrap_or_else(|| QueryResponse {
            columns: Vec::new(),
            rows: Vec::new(),
            stats: Stats::default(),
            time_ms: 0.,
        });
    let mut stats = Stats::default();
    let mut server_ms = 0.;
    let mut duration_ms = 0.;
    let mut builder = GraphBuilder::default();
    for e in executed {
        stats.add(&e.response.stats);
        server_ms += e.response.time_ms;
        duration_ms += e.duration_ms;
        builder.add_response(&e.response);
    }
    let graph = (!builder.is_empty()).then(|| builder.build());
    let is_write = analysis.has_write();
    let pure_graph = graph.is_some()
        && !last.columns.is_empty()
        && executed.iter().all(|e| {
            !e.response.columns.is_empty()
                && e.response
                    .rows
                    .iter()
                    .all(|row| row.iter().all(|cell| element_kind(cell).is_some()))
        });
    let view = if is_write || !stats.is_empty() {
        ResultView::Mutation
    } else if last.columns.len() == 1
        && matches!(last.columns[0].as_str(), "status" | "plan" | "ddl")
    {
        ResultView::Text
    } else if analysis
        .statements
        .last()
        .is_some_and(|s| s.keyword == "SHOW")
        && last.columns.iter().any(|c| c == "node_types")
    {
        ResultView::Stats
    } else if is_group_count(&last, analysis) {
        ResultView::GroupCount
    } else {
        ResultView::Rows
    };
    let response_json = if executed.len() == 1 {
        serde_json::to_string_pretty(&executed[0].raw).unwrap_or_default()
    } else {
        let all: Vec<&Value> = executed.iter().map(|e| &e.raw).collect();
        serde_json::to_string_pretty(&all).unwrap_or_default()
    };
    let summary = describe(analysis, &last, &stats, graph.as_ref(), executed.len());
    Execution {
        source: source.to_owned(),
        params: params.clone(),
        endpoint: endpoint.to_owned(),
        request_json: crate::client::wire_request(source, params),
        response_json: truncate_json(response_json),
        response: last,
        stats,
        duration_ms,
        server_ms,
        view,
        is_write,
        graph,
        pure_graph,
        summary,
    }
}

fn is_group_count(response: &QueryResponse, analysis: &Analysis) -> bool {
    if response.columns.len() != 2 || response.rows.is_empty() || response.rows.len() > 60 {
        return false;
    }
    let grouped = analysis
        .statements
        .last()
        .is_some_and(|s| s.source.to_ascii_uppercase().contains("GROUP BY"));
    grouped
        && response.rows.iter().all(|row| {
            row.len() == 2
                && !matches!(row[0], Value::Object(_) | Value::Array(_))
                && row[1].as_u64().is_some()
        })
}

fn describe(
    analysis: &Analysis,
    last: &QueryResponse,
    stats: &Stats,
    graph: Option<&GraphData>,
    count: usize,
) -> String {
    let mut parts = Vec::new();
    if !stats.is_empty() {
        if stats.nodes_created > 0 {
            parts.push(format!("+{} nodes", stats.nodes_created));
        }
        if stats.edges_created > 0 {
            parts.push(format!("+{} edges", stats.edges_created));
        }
        if stats.nodes_deleted > 0 {
            parts.push(format!("−{} nodes", stats.nodes_deleted));
        }
        if stats.edges_deleted > 0 {
            parts.push(format!("−{} edges", stats.edges_deleted));
        }
        if stats.properties_set > 0 {
            parts.push(format!("{} props set", stats.properties_set));
        }
        if stats.schema_changes > 0 {
            parts.push(format!("{} schema changes", stats.schema_changes));
        }
    }
    if let Some(graph) = graph {
        parts.push(format!(
            "graph of {} nodes, {} edges",
            graph.nodes.len(),
            graph.edges.len()
        ));
    } else if !last.columns.is_empty() {
        let rows = last.rows.len();
        parts.push(format!("{rows} row{}", if rows == 1 { "" } else { "s" }));
    }
    if count > 1 {
        parts.push(format!("{count} statements"));
    }
    if parts.is_empty() {
        parts.push(analysis.summary().to_lowercase());
    }
    parts.join(" · ")
}

const MAX_WIRE_JSON: usize = 400 * 1024;

fn truncate_json(text: String) -> String {
    if text.len() <= MAX_WIRE_JSON {
        return text;
    }
    let mut cut = MAX_WIRE_JSON;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    format!(
        "{}\n… ({} more bytes not shown)",
        &text[..cut],
        text.len() - cut
    )
}

/// A node or edge with everything attached to it, for the inspector.
#[derive(Clone, Debug, PartialEq)]
pub struct EntityDetail {
    pub kind: ElementKind,
    pub id: u64,
    pub label: String,
    pub properties: BTreeMap<String, Value>,
    /// Incident edges (nodes only).
    pub edges: Vec<GraphEdge>,
    /// Neighbouring nodes (nodes only).
    pub neighbours: Vec<GraphNode>,
    /// Degree reported by the server, when it was asked for.
    pub degree: Option<u64>,
    /// Endpoints (edges only).
    pub endpoints: Option<(Option<GraphNode>, Option<GraphNode>)>,
}

/// Builds a node detail from `node_query` and `incident_edges_query` responses.
#[must_use]
pub fn node_detail(
    node_response: &QueryResponse,
    edges_response: &QueryResponse,
) -> Option<EntityDetail> {
    let row = node_response.rows.first()?;
    let node = node_from_value(row.first()?)?;
    let degree = row.get(1).and_then(as_u64);
    let mut builder = GraphBuilder::default();
    builder.add_response(edges_response);
    let graph = builder.build();
    let mut neighbours: Vec<GraphNode> = graph
        .nodes
        .into_iter()
        .filter(|n| n.id != node.id)
        .collect();
    neighbours.sort_by_key(|n| n.id);
    let mut edges = graph.edges;
    edges.sort_by_key(|e| e.id);
    Some(EntityDetail {
        kind: ElementKind::Node,
        id: node.id,
        label: node.label,
        properties: node.properties,
        edges,
        neighbours,
        degree,
        endpoints: None,
    })
}

/// Builds an edge detail from an `edge_query` response.
#[must_use]
pub fn edge_detail(response: &QueryResponse) -> Option<EntityDetail> {
    let row = response.rows.first()?;
    let source = row.first().and_then(node_from_value);
    let edge = edge_from_value(row.get(1)?)?;
    let target = row.get(2).and_then(node_from_value);
    Some(EntityDetail {
        kind: ElementKind::Edge,
        id: edge.id,
        label: edge.label,
        properties: edge.properties,
        edges: Vec::new(),
        neighbours: Vec::new(),
        degree: None,
        endpoints: Some((source, target)),
    })
}

/// Presentation of a cell: elements become `(User#1 {name: "Ada"})`, lists and
/// maps are JSON, `NULL` is explicit.
#[must_use]
pub fn stringify(value: &Value) -> String {
    match value {
        Value::Null => "NULL".to_owned(),
        Value::String(s) => s.clone(),
        Value::Bool(b) => b.to_string(),
        Value::Number(n) => n.to_string(),
        Value::Object(map) => match element_kind(value) {
            Some(ElementKind::Edge) => format!(
                "[{}#{} {}→{}]",
                map.get("_label").and_then(Value::as_str).unwrap_or("?"),
                map.get("_id").map(stringify).unwrap_or_default(),
                map.get("_from").map(stringify).unwrap_or_default(),
                map.get("_to").map(stringify).unwrap_or_default(),
            ),
            Some(ElementKind::Node) => {
                let props: Vec<String> = map
                    .iter()
                    .filter(|(k, v)| !k.starts_with('_') && !v.is_null())
                    .map(|(k, v)| format!("{k}: {}", serde_json::to_string(v).unwrap_or_default()))
                    .collect();
                format!(
                    "({}#{} {{{}}})",
                    map.get("_label").and_then(Value::as_str).unwrap_or("?"),
                    map.get("_id").map(stringify).unwrap_or_default(),
                    props.join(", ")
                )
            }
            None => serde_json::to_string(value).unwrap_or_default(),
        },
        Value::Array(_) => serde_json::to_string(value).unwrap_or_default(),
    }
}

/// A caption for a node: a `name`-like property, else the label and id.
#[must_use]
pub fn node_caption(node: &GraphNode) -> String {
    for key in ["name", "title", "label", "username", "email", "id", "slug"] {
        match node.properties.get(key) {
            Some(Value::String(s)) if !s.is_empty() => return truncate_chars(s, 28),
            Some(Value::Number(n)) => return n.to_string(),
            _ => {}
        }
    }
    format!("{} {}", node.label, node.id)
}

/// Cuts a string to `max` characters with an ellipsis.
#[must_use]
pub fn truncate_chars(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.to_owned()
    } else {
        let cut: String = text.chars().take(max.saturating_sub(1)).collect();
        format!("{cut}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn response(columns: &[&str], rows: Vec<Vec<Value>>) -> QueryResponse {
        QueryResponse {
            columns: columns.iter().map(|c| (*c).to_owned()).collect(),
            rows,
            stats: Stats::default(),
            time_ms: 1.,
        }
    }

    #[test]
    fn extracts_elements_and_drops_dangling_edges() {
        let r = response(
            &["a", "e", "b"],
            vec![
                vec![
                    json!({"_id": 1, "_label": "User", "name": "Ada"}),
                    json!({"_id": 9, "_label": "FOLLOWS", "_from": 1, "_to": 2}),
                    json!({"_id": 2, "_label": "User", "name": "Alan"}),
                ],
                vec![
                    json!({"_id": 1, "_label": "User", "name": "Ada"}),
                    json!({"_id": 10, "_label": "WROTE", "_from": 1, "_to": 7}),
                    json!(null),
                ],
            ],
        );
        let mut b = GraphBuilder::default();
        b.add_response(&r);
        let g = b.build();
        assert_eq!(g.nodes.len(), 2);
        assert_eq!(g.edges.len(), 1);
        assert_eq!(g.dangling_edges, 1);
        assert_eq!(g.label_counts(), vec![("User".to_owned(), 2)]);
    }

    #[test]
    fn classifies_views() {
        let analysis = Analysis::of("SHOW STATS");
        let exec = Executed {
            response: response(
                &[
                    "nodes",
                    "edges",
                    "node_types",
                    "edge_types",
                    "stored_queries",
                ],
                vec![vec![json!(1), json!(2), json!(3), json!(4), json!(0)]],
            ),
            raw: json!({}),
            duration_ms: 1.,
        };
        let e = build_execution("SHOW STATS", &Map::new(), "x", &analysis, &[exec]);
        assert_eq!(e.view, ResultView::Stats);

        let analysis = Analysis::of("INSERT INTO T {a: 1}");
        let exec = Executed {
            response: QueryResponse {
                columns: vec![],
                rows: vec![],
                stats: Stats {
                    nodes_created: 1,
                    ..Default::default()
                },
                time_ms: 0.1,
            },
            raw: json!({}),
            duration_ms: 1.,
        };
        let e = build_execution("INSERT INTO T {a: 1}", &Map::new(), "x", &analysis, &[exec]);
        assert_eq!(e.view, ResultView::Mutation);
        assert!(e.summary.contains("+1 nodes"));
    }

    #[test]
    fn stringifies_elements_compactly() {
        let node = json!({"_id": 1, "_label": "User", "name": "Ada", "bio": null});
        assert_eq!(stringify(&node), r#"(User#1 {name: "Ada"})"#);
        let edge = json!({"_id": 5, "_label": "FOLLOWS", "_from": 1, "_to": 2});
        assert_eq!(stringify(&edge), "[FOLLOWS#5 1→2]");
        assert_eq!(stringify(&json!(null)), "NULL");
    }
}
