//! Declaring node and edge types from Rust.

use std::fmt::Write as _;

use serde_json::Value as Json;

use crate::cond::ident;

/// Vector distance metric.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Metric {
    /// Cosine similarity (default).
    #[default]
    Cosine,
    /// Euclidean distance.
    Euclidean,
    /// Inner product.
    Dot,
}

#[derive(Debug, Clone)]
struct PropSpec {
    name: String,
    ty: String,
    required: bool,
    unique: bool,
    indexed: bool,
    searchable: bool,
    metric: Option<Metric>,
    default: Option<String>,
}

/// Starts a node type declaration.
///
/// ```
/// use nexa::prelude::*;
///
/// let user = node("User")
///     .string("name").required().unique()
///     .int("age").indexed()
///     .text("bio")
///     .vector("embedding", 384)
///     .timestamp("joined").default_now();
/// assert!(user.to_nql().starts_with("CREATE NODE IF NOT EXISTS `User`"));
/// ```
pub fn node(name: &str) -> TypeSpec {
    TypeSpec {
        kind: "NODE",
        name: name.to_owned(),
        from: Vec::new(),
        to: Vec::new(),
        props: Vec::new(),
    }
}

/// Starts an edge type declaration: `edge("FOLLOWS").from("User").to("User")`.
pub fn edge(name: &str) -> TypeSpec {
    TypeSpec {
        kind: "EDGE",
        name: name.to_owned(),
        from: Vec::new(),
        to: Vec::new(),
        props: Vec::new(),
    }
}

/// A node or edge type being declared. Property modifiers (`required`,
/// `unique`, `indexed`, ...) apply to the property declared just before them.
#[derive(Debug, Clone)]
#[must_use = "pass the declaration to db.define(...)"]
pub struct TypeSpec {
    kind: &'static str,
    name: String,
    from: Vec<String>,
    to: Vec<String>,
    props: Vec<PropSpec>,
}

macro_rules! types {
    ($($(#[$doc:meta])* $name:ident => $ty:literal),* $(,)?) => {
        $($(#[$doc])* pub fn $name(self, prop: &str) -> Self {
            self.prop(prop, $ty)
        })*
    };
}

impl TypeSpec {
    /// Edge source type (call again to allow several).
    pub fn from(mut self, node_type: &str) -> Self {
        self.from.push(node_type.to_owned());
        self
    }

    /// Edge target type (call again to allow several).
    pub fn to(mut self, node_type: &str) -> Self {
        self.to.push(node_type.to_owned());
        self
    }

    fn prop(mut self, name: &str, ty: impl Into<String>) -> Self {
        self.props.push(PropSpec {
            name: name.to_owned(),
            ty: ty.into(),
            required: false,
            unique: false,
            indexed: false,
            searchable: false,
            metric: None,
            default: None,
        });
        self
    }

    types! {
        /// A `STRING` property.
        string => "STRING",
        /// An `INT` property.
        int => "INT",
        /// A `FLOAT` property.
        float => "FLOAT",
        /// A `BOOL` property.
        bool => "BOOL",
        /// A `TIMESTAMP` property (milliseconds since the epoch).
        timestamp => "TIMESTAMP",
        /// A `JSON` property (any value).
        json => "JSON",
        /// A list of strings (`STRING[]`).
        strings => "STRING[]",
    }

    /// A full-text searchable `STRING` property (BM25 index).
    pub fn text(self, prop: &str) -> Self {
        self.prop(prop, "STRING").searchable()
    }

    /// A `VECTOR(dim)` property with an HNSW index.
    pub fn vector(self, prop: &str, dim: u32) -> Self {
        self.prop(prop, format!("VECTOR({dim})"))
    }

    fn last(&mut self) -> Option<&mut PropSpec> {
        self.props.last_mut()
    }

    /// The previous property must always have a value (`NOT NULL`).
    pub fn required(mut self) -> Self {
        if let Some(p) = self.last() {
            p.required = true;
        }
        self
    }

    /// The previous property's values must be unique (indexed).
    pub fn unique(mut self) -> Self {
        if let Some(p) = self.last() {
            p.unique = true;
        }
        self
    }

    /// Index the previous property for fast lookups and ranges.
    pub fn indexed(mut self) -> Self {
        if let Some(p) = self.last() {
            p.indexed = true;
        }
        self
    }

    /// Full-text index the previous (string) property.
    pub fn searchable(mut self) -> Self {
        if let Some(p) = self.last() {
            p.searchable = true;
        }
        self
    }

    /// Metric of the previous vector property.
    pub fn metric(mut self, metric: Metric) -> Self {
        if let Some(p) = self.last() {
            p.metric = Some(metric);
        }
        self
    }

    /// Default value of the previous property.
    pub fn default(mut self, value: impl Into<Json>) -> Self {
        let lit = literal(&value.into());
        if let Some(p) = self.last() {
            p.default = Some(lit);
        }
        self
    }

    /// The previous property defaults to the insert time.
    pub fn default_now(mut self) -> Self {
        if let Some(p) = self.last() {
            p.default = Some("now()".into());
        }
        self
    }

    /// The idempotent `CREATE ... IF NOT EXISTS` statement.
    #[must_use]
    pub fn to_nql(&self) -> String {
        let mut q = format!("CREATE {} IF NOT EXISTS {}", self.kind, ident(&self.name));
        if self.kind == "EDGE" {
            let join = |v: &[String]| v.iter().map(|n| ident(n)).collect::<Vec<_>>().join(" | ");
            let _ = write!(q, " FROM {} TO {}", join(&self.from), join(&self.to));
        }
        let props: Vec<String> = self
            .props
            .iter()
            .map(|p| {
                let mut s = format!("{} {}", ident(&p.name), p.ty);
                for (on, word) in [
                    (p.required, " NOT NULL"),
                    (p.unique, " UNIQUE"),
                    (p.indexed, " INDEX"),
                    (p.searchable, " SEARCHABLE"),
                ] {
                    if on {
                        s.push_str(word);
                    }
                }
                if let Some(m) = p.metric {
                    s.push_str(match m {
                        Metric::Cosine => " METRIC COSINE",
                        Metric::Euclidean => " METRIC EUCLIDEAN",
                        Metric::Dot => " METRIC DOT",
                    });
                }
                if let Some(d) = &p.default {
                    let _ = write!(s, " DEFAULT {d}");
                }
                s
            })
            .collect();
        if !props.is_empty() || self.kind == "NODE" {
            let _ = write!(q, " ({})", props.join(", "));
        }
        q
    }
}

fn literal(v: &Json) -> String {
    match v {
        Json::String(s) => format!("'{}'", s.replace('\'', "''")),
        Json::Array(items) => format!(
            "[{}]",
            items.iter().map(literal).collect::<Vec<_>>().join(", ")
        ),
        Json::Object(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{}: {}", ident(k), literal(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Json::Null => "NULL".into(),
        other => other.to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn renders_edge_with_props() {
        let e = edge("FOLLOWS")
            .from("User")
            .to("User")
            .int("since")
            .default(2024);
        assert_eq!(
            e.to_nql(),
            "CREATE EDGE IF NOT EXISTS `FOLLOWS` FROM `User` TO `User` (`since` INT DEFAULT 2024)"
        );
    }
}
