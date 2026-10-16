//! The dynamic value type used for properties, parameters and query results.

use std::cmp::Ordering;
use std::collections::BTreeMap;
use std::fmt;

use serde::{Deserialize, Serialize};

/// Identifier of a node or an edge. Node and edge ids share one sequence, so
/// an id is unique across the whole database and never reused.
pub type ElementId = u64;

/// A RatikDB value.
///
/// `Node` and `Edge` are references produced while a query runs; they are
/// never stored as property values and are expanded into maps (with `_id` and
/// `_label` keys) before results leave the engine.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[non_exhaustive]
pub enum Value {
    /// Absence of a value.
    #[default]
    Null,
    /// A boolean.
    Bool(bool),
    /// A 64-bit signed integer.
    Int(i64),
    /// A 64-bit float.
    Float(f64),
    /// A UTF-8 string.
    String(String),
    /// An ordered list of values.
    List(Vec<Value>),
    /// String-keyed map.
    Map(BTreeMap<String, Value>),
    /// A dense float vector (embeddings).
    Vector(Vec<f32>),
    /// Reference to a node (query-time only).
    Node(ElementId),
    /// Reference to an edge (query-time only).
    Edge(ElementId),
}

impl Value {
    /// Human-readable type name, used in error messages.
    #[must_use]
    pub fn type_name(&self) -> &'static str {
        match self {
            Self::Null => "NULL",
            Self::Bool(_) => "BOOL",
            Self::Int(_) => "INT",
            Self::Float(_) => "FLOAT",
            Self::String(_) => "STRING",
            Self::List(_) => "LIST",
            Self::Map(_) => "MAP",
            Self::Vector(_) => "VECTOR",
            Self::Node(_) => "NODE",
            Self::Edge(_) => "EDGE",
        }
    }

    /// `true` for [`Value::Null`].
    #[must_use]
    pub fn is_null(&self) -> bool {
        matches!(self, Self::Null)
    }

    /// Numeric view of the value, if it is a number.
    #[must_use]
    pub fn as_f64(&self) -> Option<f64> {
        match self {
            Self::Int(i) => Some(*i as f64),
            Self::Float(f) => Some(*f),
            _ => None,
        }
    }

    /// Borrow the string, if this is a string.
    #[must_use]
    pub fn as_str(&self) -> Option<&str> {
        match self {
            Self::String(s) => Some(s),
            _ => None,
        }
    }

    /// Interprets the value as a float vector (a `VECTOR` or a list of numbers).
    #[must_use]
    pub fn as_vector(&self) -> Option<Vec<f32>> {
        match self {
            Self::Vector(v) => Some(v.clone()),
            Self::List(items) => items.iter().map(|v| v.as_f64().map(|f| f as f32)).collect(),
            _ => None,
        }
    }

    fn rank(&self) -> u8 {
        match self {
            Self::Null => 0,
            Self::Bool(_) => 1,
            Self::Int(_) | Self::Float(_) => 2,
            Self::String(_) => 3,
            Self::List(_) => 4,
            Self::Map(_) => 5,
            Self::Vector(_) => 6,
            Self::Node(_) => 7,
            Self::Edge(_) => 8,
        }
    }

    /// Total order over all values, used for sorting, grouping and indexes.
    ///
    /// Values of different kinds order by kind (`NULL` first); numbers compare
    /// numerically regardless of `INT`/`FLOAT`.
    #[must_use]
    pub fn total_cmp(&self, other: &Self) -> Ordering {
        match (self, other) {
            (Self::Int(a), Self::Int(b)) => a.cmp(b),
            (Self::Int(_) | Self::Float(_), Self::Int(_) | Self::Float(_)) => {
                let (a, b) = (self.as_f64().unwrap_or_default(), other.as_f64().unwrap_or_default());
                a.total_cmp(&b)
            }
            (Self::Bool(a), Self::Bool(b)) => a.cmp(b),
            (Self::String(a), Self::String(b)) => a.cmp(b),
            (Self::List(a), Self::List(b)) => cmp_seq(a, b),
            (Self::Map(a), Self::Map(b)) => {
                for ((ka, va), (kb, vb)) in a.iter().zip(b.iter()) {
                    let ord = ka.cmp(kb).then_with(|| va.total_cmp(vb));
                    if ord != Ordering::Equal {
                        return ord;
                    }
                }
                a.len().cmp(&b.len())
            }
            (Self::Vector(a), Self::Vector(b)) => {
                for (x, y) in a.iter().zip(b.iter()) {
                    let ord = x.total_cmp(y);
                    if ord != Ordering::Equal {
                        return ord;
                    }
                }
                a.len().cmp(&b.len())
            }
            (Self::Node(a), Self::Node(b)) | (Self::Edge(a), Self::Edge(b)) => a.cmp(b),
            _ => self.rank().cmp(&other.rank()),
        }
    }

    /// SQL equality: `None` when either side is `NULL`.
    #[must_use]
    pub fn sql_eq(&self, other: &Self) -> Option<bool> {
        if self.is_null() || other.is_null() {
            return None;
        }
        let comparable = self.rank() == other.rank();
        Some(comparable && self.total_cmp(other) == Ordering::Equal)
    }

    /// SQL ordering comparison: `None` when either side is `NULL` or the
    /// values are of incomparable kinds.
    #[must_use]
    pub fn sql_cmp(&self, other: &Self) -> Option<Ordering> {
        if self.is_null() || other.is_null() || self.rank() != other.rank() {
            return None;
        }
        Some(self.total_cmp(other))
    }

    /// Converts from JSON. Integral numbers become `INT`, others `FLOAT`.
    #[must_use]
    pub fn from_json(json: serde_json::Value) -> Self {
        match json {
            serde_json::Value::Null => Self::Null,
            serde_json::Value::Bool(b) => Self::Bool(b),
            serde_json::Value::Number(n) => n
                .as_i64()
                .map_or_else(|| Self::Float(n.as_f64().unwrap_or(f64::NAN)), Self::Int),
            serde_json::Value::String(s) => Self::String(s),
            serde_json::Value::Array(items) => Self::List(items.into_iter().map(Self::from_json).collect()),
            serde_json::Value::Object(map) => {
                Self::Map(map.into_iter().map(|(k, v)| (k, Self::from_json(v))).collect())
            }
        }
    }

    /// Converts to JSON. Node/edge references become `{"_ref": id}`; results
    /// produced by the engine have already been expanded, so this only
    /// matters for hand-built values.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        use serde_json::Value as J;
        match self {
            Self::Null => J::Null,
            Self::Bool(b) => J::Bool(*b),
            Self::Int(i) => J::from(*i),
            Self::Float(f) => serde_json::Number::from_f64(*f).map_or(J::Null, J::Number),
            Self::String(s) => J::String(s.clone()),
            Self::List(items) => J::Array(items.iter().map(Self::to_json).collect()),
            Self::Map(map) => J::Object(map.iter().map(|(k, v)| (k.clone(), v.to_json())).collect()),
            Self::Vector(v) => J::Array(
                v.iter()
                    .map(|f| serde_json::Number::from_f64(f64::from(*f)).map_or(J::Null, J::Number))
                    .collect(),
            ),
            Self::Node(id) | Self::Edge(id) => serde_json::json!({ "_ref": id }),
        }
    }
}

fn cmp_seq(a: &[Value], b: &[Value]) -> Ordering {
    for (x, y) in a.iter().zip(b.iter()) {
        let ord = x.total_cmp(y);
        if ord != Ordering::Equal {
            return ord;
        }
    }
    a.len().cmp(&b.len())
}

impl fmt::Display for Value {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Null => f.write_str("NULL"),
            Self::Bool(b) => write!(f, "{b}"),
            Self::Int(i) => write!(f, "{i}"),
            Self::Float(x) => write!(f, "{x}"),
            Self::String(s) => write!(f, "{s}"),
            Self::List(items) => {
                f.write_str("[")?;
                for (i, item) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{item}")?;
                }
                f.write_str("]")
            }
            Self::Map(map) => {
                f.write_str("{")?;
                for (i, (k, v)) in map.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                f.write_str("}")
            }
            Self::Vector(v) => write!(f, "<vector dim={}>", v.len()),
            Self::Node(id) => write!(f, "<node {id}>"),
            Self::Edge(id) => write!(f, "<edge {id}>"),
        }
    }
}

macro_rules! impl_from {
    ($($t:ty => $variant:ident $(as $cast:ty)?),* $(,)?) => {
        $(impl From<$t> for Value {
            fn from(v: $t) -> Self {
                Self::$variant(v $(as $cast)?)
            }
        })*
    };
}

impl_from! {
    bool => Bool,
    i64 => Int,
    i32 => Int as i64,
    u32 => Int as i64,
    f64 => Float,
    f32 => Float as f64,
    String => String,
    Vec<Value> => List,
    Vec<f32> => Vector,
}

impl From<&str> for Value {
    fn from(v: &str) -> Self {
        Self::String(v.to_owned())
    }
}

impl<T: Into<Value>> From<Option<T>> for Value {
    fn from(v: Option<T>) -> Self {
        v.map_or(Self::Null, Into::into)
    }
}

/// Wrapper giving [`Value`] a total `Ord`, so it can key B-trees.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub(crate) struct OrdValue(pub Value);

impl PartialEq for OrdValue {
    fn eq(&self, other: &Self) -> bool {
        self.0.total_cmp(&other.0) == Ordering::Equal
    }
}

impl Eq for OrdValue {}

impl PartialOrd for OrdValue {
    fn partial_cmp(&self, other: &Self) -> Option<Ordering> {
        Some(self.cmp(other))
    }
}

impl Ord for OrdValue {
    fn cmp(&self, other: &Self) -> Ordering {
        self.0.total_cmp(&other.0)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn numbers_compare_across_int_and_float() {
        assert_eq!(Value::Int(2).total_cmp(&Value::Float(2.0)), Ordering::Equal);
        assert_eq!(Value::Int(1).sql_cmp(&Value::Float(1.5)), Some(Ordering::Less));
        assert_eq!(Value::Int(1).sql_eq(&Value::Null), None);
        assert_eq!(Value::Int(1).sql_eq(&Value::from("1")), Some(false));
    }

    #[test]
    fn json_round_trip() {
        let json = serde_json::json!({"a": [1, 2.5, "x", null, true]});
        assert_eq!(Value::from_json(json.clone()).to_json(), json);
    }
}
