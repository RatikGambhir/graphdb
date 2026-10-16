//! The strict schema: every node and edge type, and every property on them,
//! must be declared before data can be written or read.

use std::collections::BTreeMap;
use std::fmt::{self, Write as _};

use serde::{Deserialize, Serialize};

use crate::error::{Error, Result, hint};
use crate::value::Value;

/// Numeric identifier of a declared type. Stable for the type's lifetime and
/// never reused after a `DROP`.
pub type TypeId = u32;

/// Type of a declared property.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum PropType {
    /// `BOOL`
    Bool,
    /// `INT`
    Int,
    /// `FLOAT`
    Float,
    /// `STRING`
    String,
    /// `TIMESTAMP` — milliseconds since the Unix epoch.
    Timestamp,
    /// `VECTOR(n)` — a dense float vector of exactly `n` dimensions.
    Vector(u32),
    /// `JSON` — any value.
    Json,
    /// `T[]` — a list whose items are all `T`.
    List(Box<PropType>),
}

impl fmt::Display for PropType {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Bool => f.write_str("BOOL"),
            Self::Int => f.write_str("INT"),
            Self::Float => f.write_str("FLOAT"),
            Self::String => f.write_str("STRING"),
            Self::Timestamp => f.write_str("TIMESTAMP"),
            Self::Vector(n) => write!(f, "VECTOR({n})"),
            Self::Json => f.write_str("JSON"),
            Self::List(inner) => write!(f, "{inner}[]"),
        }
    }
}

impl PropType {
    /// Checks `value` against this type, converting where the conversion is
    /// lossless (`INT` → `FLOAT`, number list → `VECTOR`).
    ///
    /// # Errors
    /// Returns [`Error::Type`] when the value does not fit.
    pub fn coerce(&self, value: Value, what: &str) -> Result<Value> {
        let mismatch = |v: &Value| {
            Error::ty(format!(
                "{what} expects {self}, got {} ({v})",
                v.type_name()
            ))
        };
        match (self, value) {
            (_, Value::Null) => Ok(Value::Null),
            (Self::Bool, v @ Value::Bool(_)) | (Self::Int | Self::Timestamp, v @ Value::Int(_)) => {
                Ok(v)
            }
            (Self::Float, Value::Int(i)) => Ok(Value::Float(i as f64)),
            (Self::Float, v @ Value::Float(_)) | (Self::String, v @ Value::String(_)) => Ok(v),
            (Self::Vector(dim), v @ (Value::Vector(_) | Value::List(_))) => {
                let Some(vector) = v.as_vector() else {
                    return Err(mismatch(&v));
                };
                if vector.len() != *dim as usize {
                    return Err(Error::ty(format!(
                        "{what} expects VECTOR({dim}), got a vector with {} dimensions",
                        vector.len()
                    )));
                }
                Ok(Value::Vector(vector))
            }
            (Self::Json, v @ (Value::Node(_) | Value::Edge(_))) => Err(mismatch(&v)),
            (Self::Json, v) => Ok(v),
            (Self::List(inner), Value::List(items)) => items
                .into_iter()
                .map(|item| match item {
                    Value::Null => Err(Error::ty(format!("{what} may not contain NULL items"))),
                    item => inner.coerce(item, what),
                })
                .collect::<Result<_>>()
                .map(Value::List),
            (_, v) => Err(mismatch(&v)),
        }
    }

    pub(crate) fn is_indexable(&self) -> bool {
        matches!(
            self,
            Self::Bool | Self::Int | Self::Float | Self::String | Self::Timestamp
        )
    }
}

/// Distance metric of a vector index.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub enum Metric {
    /// Cosine similarity — use with `ORDER BY similarity(...) DESC`.
    #[default]
    Cosine,
    /// Euclidean distance — use with `ORDER BY distance(...) ASC`.
    Euclidean,
    /// Inner product — use with `ORDER BY dot(...) DESC`.
    Dot,
}

impl fmt::Display for Metric {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Cosine => "COSINE",
            Self::Euclidean => "EUCLIDEAN",
            Self::Dot => "DOT",
        })
    }
}

/// Default for a property that was not supplied on insert.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub enum DefaultValue {
    /// A constant.
    Value(Value),
    /// The insert time, `now()`.
    Now,
}

/// A declared property.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PropDef {
    /// Property name.
    pub name: String,
    /// Declared type.
    pub ty: PropType,
    /// `NOT NULL`
    pub not_null: bool,
    /// `UNIQUE` (implies an index).
    pub unique: bool,
    /// `INDEX` — B-tree index for equality and range lookups.
    pub indexed: bool,
    /// `SEARCHABLE` — BM25 full-text index.
    pub searchable: bool,
    /// Vector metric (only meaningful for `VECTOR(n)`).
    pub metric: Metric,
    /// `DEFAULT ...`
    pub default: Option<DefaultValue>,
}

impl PropDef {
    /// A plain nullable property with no index.
    #[must_use]
    pub fn new(name: impl Into<String>, ty: PropType) -> Self {
        Self {
            name: name.into(),
            ty,
            not_null: false,
            unique: false,
            indexed: false,
            searchable: false,
            metric: Metric::default(),
            default: None,
        }
    }

    pub(crate) fn has_btree(&self) -> bool {
        self.unique || self.indexed
    }

    pub(crate) fn validate(&self, owner: &str) -> Result<()> {
        let at = format!("{owner}.{}", self.name);
        if self.name.starts_with('_') {
            return Err(Error::schema(format!(
                "{at}: property names starting with '_' are reserved (_id, _label, _from, _to)"
            )));
        }
        if self.has_btree() && !self.ty.is_indexable() {
            return Err(Error::schema(format!(
                "{at}: {} properties cannot be INDEX or UNIQUE",
                self.ty
            )));
        }
        if self.searchable && self.ty != PropType::String {
            return Err(Error::schema(format!(
                "{at}: only STRING properties can be SEARCHABLE"
            )));
        }
        if let Some(DefaultValue::Value(v)) = &self.default {
            self.ty.coerce(v.clone(), &format!("default of {at}"))?;
        }
        if matches!(self.default, Some(DefaultValue::Now))
            && !matches!(self.ty, PropType::Timestamp | PropType::Int)
        {
            return Err(Error::schema(format!(
                "{at}: DEFAULT now() requires TIMESTAMP or INT"
            )));
        }
        Ok(())
    }
}

impl fmt::Display for PropDef {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{} {}", self.name, self.ty)?;
        if self.not_null {
            f.write_str(" NOT NULL")?;
        }
        if self.unique {
            f.write_str(" UNIQUE")?;
        } else if self.indexed {
            f.write_str(" INDEX")?;
        }
        if self.searchable {
            f.write_str(" SEARCHABLE")?;
        }
        if matches!(self.ty, PropType::Vector(_)) && self.metric != Metric::Cosine {
            write!(f, " METRIC {}", self.metric)?;
        }
        match &self.default {
            Some(DefaultValue::Now) => f.write_str(" DEFAULT now()")?,
            Some(DefaultValue::Value(v)) => write!(f, " DEFAULT {}", literal(v))?,
            None => {}
        }
        Ok(())
    }
}

/// Renders a value as an NQL literal.
pub(crate) fn literal(v: &Value) -> String {
    match v {
        Value::String(s) => format!("'{}'", s.replace('\'', "''")),
        Value::List(items) => format!(
            "[{}]",
            items.iter().map(literal).collect::<Vec<_>>().join(", ")
        ),
        Value::Map(map) => format!(
            "{{{}}}",
            map.iter()
                .map(|(k, v)| format!("{k}: {}", literal(v)))
                .collect::<Vec<_>>()
                .join(", ")
        ),
        Value::Vector(items) => format!(
            "[{}]",
            items
                .iter()
                .map(f32::to_string)
                .collect::<Vec<_>>()
                .join(", ")
        ),
        other => other.to_string(),
    }
}

/// Whether a type describes nodes or edges.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Kind {
    /// A node type (`CREATE NODE`).
    Node,
    /// An edge type (`CREATE EDGE`).
    Edge,
}

impl fmt::Display for Kind {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::Node => "NODE",
            Self::Edge => "EDGE",
        })
    }
}

/// A declared node or edge type.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TypeDef {
    /// Stable id.
    pub id: TypeId,
    /// Node or edge.
    pub kind: Kind,
    /// Label, e.g. `User` or `FOLLOWS`.
    pub name: String,
    /// Declared properties, in declaration order (this is also storage order).
    pub props: Vec<PropDef>,
    /// Edge types only: node types an edge may start at.
    pub from: Vec<String>,
    /// Edge types only: node types an edge may end at.
    pub to: Vec<String>,
}

impl TypeDef {
    /// Position of a property in storage order.
    #[must_use]
    pub fn slot(&self, prop: &str) -> Option<usize> {
        self.props.iter().position(|p| p.name == prop)
    }

    /// The property definition, or a helpful schema error.
    ///
    /// # Errors
    /// [`Error::Schema`] when the property is not declared.
    pub fn prop(&self, prop: &str) -> Result<(usize, &PropDef)> {
        self.slot(prop).map(|i| (i, &self.props[i])).ok_or_else(|| {
            Error::schema(format!(
                "{} has no property '{prop}'{}",
                self.name,
                hint(prop, self.props.iter().map(|p| p.name.as_str()))
            ))
        })
    }

    /// Renders the type as the `CREATE` statement that would recreate it.
    #[must_use]
    pub fn ddl(&self) -> String {
        let mut out = format!("CREATE {} {}", self.kind, self.name);
        if self.kind == Kind::Edge {
            let _ = write!(
                out,
                " FROM {} TO {}",
                self.from.join(" | "),
                self.to.join(" | ")
            );
        }
        if !self.props.is_empty() || self.kind == Kind::Node {
            out.push_str(" (\n");
            for (i, p) in self.props.iter().enumerate() {
                let sep = if i + 1 == self.props.len() { "" } else { "," };
                let _ = writeln!(out, "  {p}{sep}");
            }
            out.push(')');
        }
        out.push(';');
        out
    }
}

/// All declared types.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
pub struct Schema {
    types: BTreeMap<TypeId, TypeDef>,
    by_name: BTreeMap<String, TypeId>,
    next_type_id: TypeId,
}

impl Schema {
    /// Looks up a type by id.
    #[must_use]
    pub fn get(&self, id: TypeId) -> Option<&TypeDef> {
        self.types.get(&id)
    }

    pub(crate) fn get_mut(&mut self, id: TypeId) -> Option<&mut TypeDef> {
        self.types.get_mut(&id)
    }

    /// Looks up a type by its label.
    #[must_use]
    pub fn by_name(&self, name: &str) -> Option<&TypeDef> {
        self.by_name.get(name).and_then(|id| self.types.get(id))
    }

    /// Looks up a type by label and kind, with a helpful error.
    ///
    /// # Errors
    /// [`Error::Schema`] when no such type is declared.
    pub fn resolve(&self, name: &str, kind: Kind) -> Result<&TypeDef> {
        match self.by_name(name) {
            Some(t) if t.kind == kind => Ok(t),
            Some(t) => Err(Error::schema(format!(
                "'{name}' is declared as {} type, not {} type",
                article(t.kind),
                article(kind)
            ))),
            None => {
                let names = self
                    .types
                    .values()
                    .filter(|t| t.kind == kind)
                    .map(|t| t.name.as_str());
                let suggestion = hint(name, names);
                let kind_name = kind.to_string().to_lowercase();
                if !suggestion.is_empty() {
                    return Err(Error::schema(format!(
                        "unknown {kind_name} type '{name}'{suggestion}"
                    )));
                }
                let declare = match kind {
                    Kind::Node => format!("CREATE NODE {name} (...)"),
                    Kind::Edge => format!("CREATE EDGE {name} FROM ... TO ..."),
                };
                Err(Error::schema(format!(
                    "unknown {kind_name} type '{name}' — declare it first with `{declare}`"
                )))
            }
        }
    }

    /// Iterates over all types.
    pub fn types(&self) -> impl Iterator<Item = &TypeDef> {
        self.types.values()
    }

    /// Iterates over types of one kind.
    pub fn of_kind(&self, kind: Kind) -> impl Iterator<Item = &TypeDef> {
        self.types.values().filter(move |t| t.kind == kind)
    }

    pub(crate) fn next_type_id(&self) -> TypeId {
        self.next_type_id
    }

    pub(crate) fn insert(&mut self, def: TypeDef) {
        self.next_type_id = self.next_type_id.max(def.id + 1);
        self.by_name.insert(def.name.clone(), def.id);
        self.types.insert(def.id, def);
    }

    pub(crate) fn remove(&mut self, id: TypeId) -> Option<TypeDef> {
        let def = self.types.remove(&id)?;
        self.by_name.remove(&def.name);
        Some(def)
    }

    /// Edge types whose endpoints reference node type `name`.
    pub(crate) fn edges_referencing<'a>(
        &'a self,
        name: &'a str,
    ) -> impl Iterator<Item = &'a TypeDef> + 'a {
        self.of_kind(Kind::Edge)
            .filter(move |e| e.from.iter().chain(&e.to).any(|n| n == name))
    }

    /// The whole schema as runnable DDL.
    #[must_use]
    pub fn ddl(&self) -> String {
        let mut parts: Vec<String> = self.of_kind(Kind::Node).map(TypeDef::ddl).collect();
        parts.extend(self.of_kind(Kind::Edge).map(TypeDef::ddl));
        parts.join("\n\n")
    }
}

fn article(kind: Kind) -> &'static str {
    match kind {
        Kind::Node => "a node",
        Kind::Edge => "an edge",
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn coercion_is_strict_but_lossless() {
        assert_eq!(
            PropType::Float.coerce(Value::Int(3), "x").unwrap(),
            Value::Float(3.0)
        );
        assert!(PropType::Int.coerce(Value::Float(3.5), "x").is_err());
        assert!(PropType::String.coerce(Value::Int(1), "x").is_err());
        let v =
            PropType::Vector(2).coerce(Value::List(vec![Value::Int(1), Value::Float(0.5)]), "x");
        assert_eq!(v.unwrap(), Value::Vector(vec![1.0, 0.5]));
        assert!(
            PropType::Vector(3)
                .coerce(Value::Vector(vec![1.0]), "x")
                .is_err()
        );
        let list = PropType::List(Box::new(PropType::String));
        assert!(
            list.coerce(Value::List(vec!["a".into(), Value::Int(1)]), "x")
                .is_err()
        );
    }
}
