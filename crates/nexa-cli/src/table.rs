//! Pretty terminal tables for query results.

use nexa::Rows;
use serde_json::Value as J;

use crate::brand::{self, CYAN, ULTRAVIOLET, VOLT};

const MAX_CELL: usize = 48;

fn plain(v: &J) -> String {
    match v {
        J::Null => "NULL".into(),
        J::String(s) => s.clone(),
        J::Object(map) if map.contains_key("_id") && map.contains_key("_label") => {
            let label = map["_label"].as_str().unwrap_or_default();
            let id = &map["_id"];
            let props: Vec<String> = map
                .iter()
                .filter(|(k, v)| !k.starts_with('_') && !v.is_null())
                .map(|(k, v)| format!("{k}: {}", short(v)))
                .collect();
            if map.contains_key("_from") {
                format!("[{label}#{id} {}→{}]", map["_from"], map["_to"])
            } else {
                format!("({label}#{id} {{{}}})", props.join(", "))
            }
        }
        other => other.to_string(),
    }
}

fn short(v: &J) -> String {
    match v {
        J::String(s) => format!("\"{s}\""),
        J::Array(items) if items.len() > 4 => format!("[{} items]", items.len()),
        other => other.to_string(),
    }
}

fn truncate(s: &str, width: usize) -> String {
    let s = s.replace('\n', "⏎");
    if s.chars().count() <= width {
        return s;
    }
    let mut out: String = s.chars().take(width.saturating_sub(1)).collect();
    out.push('…');
    out
}

fn paint(cell: &str, v: &J) -> String {
    match v {
        J::Null => brand::dim(cell),
        J::Number(_) => brand::rgb(cell, CYAN),
        J::Bool(_) => brand::rgb(cell, VOLT),
        J::Object(_) => brand::rgb(cell, ULTRAVIOLET),
        _ => cell.to_owned(),
    }
}

/// Renders rows as a boxed table.
pub fn render(rows: &Rows) -> String {
    if rows.columns.is_empty() {
        return String::new();
    }
    let cells: Vec<Vec<String>> = rows
        .rows
        .iter()
        .map(|r| r.iter().map(|v| truncate(&plain(v), MAX_CELL)).collect())
        .collect();
    let widths: Vec<usize> = rows
        .columns
        .iter()
        .enumerate()
        .map(|(i, c)| {
            cells
                .iter()
                .map(|r| r.get(i).map_or(0, |s| s.chars().count()))
                .max()
                .unwrap_or(0)
                .max(c.chars().count())
        })
        .collect();
    let line = |l: &str, m: &str, r: &str| {
        let segs: Vec<String> = widths.iter().map(|w| "─".repeat(w + 2)).collect();
        brand::dim(&format!("{l}{}{r}", segs.join(m)))
    };
    let bar = brand::dim("│");
    let mut out = vec![line("╭", "┬", "╮")];
    let header: Vec<String> = rows
        .columns
        .iter()
        .zip(&widths)
        .map(|(c, w)| format!(" {} ", brand::bold(&brand::gradient(&format!("{c:<w$}")))))
        .collect();
    out.push(format!("{bar}{}{bar}", header.join(&bar)));
    out.push(line("├", "┼", "┤"));
    for (row, values) in cells.iter().zip(&rows.rows) {
        let parts: Vec<String> = row
            .iter()
            .zip(values)
            .zip(&widths)
            .map(|((s, v), w)| {
                let pad = w - s.chars().count();
                let aligned = if v.is_number() {
                    format!("{}{s}", " ".repeat(pad))
                } else {
                    format!("{s}{}", " ".repeat(pad))
                };
                format!(" {} ", paint(&aligned, v))
            })
            .collect();
        out.push(format!("{bar}{}{bar}", parts.join(&bar)));
    }
    out.push(line("╰", "┴", "╯"));
    out.join("\n")
}

/// `3 rows · 1.24 ms · +2 nodes` summary line.
pub fn footer(rows: &Rows, ms: f64) -> String {
    let n = rows.len();
    let mut parts = vec![
        format!("{n} row{}", if n == 1 { "" } else { "s" }),
        format!("{ms:.2} ms"),
    ];
    let s = rows.stats;
    for (count, what) in [
        (s.nodes_created, "+{} nodes"),
        (s.edges_created, "+{} edges"),
        (s.nodes_deleted, "−{} nodes"),
        (s.edges_deleted, "−{} edges"),
        (s.properties_set, "{} props set"),
        (s.schema_changes, "{} schema changes"),
    ] {
        if count > 0 {
            parts.push(what.replace("{}", &count.to_string()));
        }
    }
    brand::dim(&parts.join(" · "))
}
