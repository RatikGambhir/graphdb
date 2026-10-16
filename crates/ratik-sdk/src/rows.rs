//! Query results: [`Rows`], [`Row`], [`Node`] and [`Edge`].

use std::fmt;

use serde::Deserialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value as Json};

use crate::error::{Error, Result};

/// What a write changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(default)]
pub struct Stats {
    /// Nodes inserted.
    pub nodes_created: u64,
    /// Nodes deleted.
    pub nodes_deleted: u64,
    /// Edges inserted.
    pub edges_created: u64,
    /// Edges deleted.
    pub edges_deleted: u64,
    /// Properties updated.
    pub properties_set: u64,
    /// Schema changes.
    pub schema_changes: u64,
}

/// A table of results.
#[derive(Debug, Clone, Default, PartialEq, Deserialize)]
pub struct Rows {
    /// Column names.
    pub columns: Vec<String>,
    /// Row values, one per column.
    pub rows: Vec<Vec<Json>>,
    /// What the statement changed.
    #[serde(default)]
    pub stats: Stats,
}

impl Rows {
    /// Number of rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// `true` when there are no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// Iterates over the rows.
    pub fn iter(&self) -> impl Iterator<Item = Row<'_>> {
        self.rows.iter().map(|values| Row {
            columns: &self.columns,
            values,
        })
    }

    /// The first row, if any.
    #[must_use]
    pub fn first(&self) -> Option<Row<'_>> {
        self.iter().next()
    }

    /// Every value of one column, decoded.
    ///
    /// # Errors
    /// Unknown column or a value that does not decode as `T`.
    pub fn column<T: DeserializeOwned>(&self, name: &str) -> Result<Vec<T>> {
        self.iter().map(|r| r.get(name)).collect()
    }

    /// Decodes every row into `T`.
    ///
    /// A row with a single node/edge column decodes *that element* (so
    /// `SELECT u FROM User u` maps straight onto `struct User`); any other row
    /// decodes as an object keyed by column name.
    ///
    /// # Errors
    /// [`Error::Decode`] when a row does not fit `T`.
    pub fn decode<T: DeserializeOwned>(&self) -> Result<Vec<T>> {
        self.iter().map(|r| r.decode()).collect()
    }

    /// Rows as JSON objects keyed by column name.
    #[must_use]
    pub fn to_objects(&self) -> Vec<Map<String, Json>> {
        self.iter().map(|r| r.to_object()).collect()
    }
}

impl<'a> IntoIterator for &'a Rows {
    type Item = Row<'a>;
    type IntoIter = Box<dyn Iterator<Item = Row<'a>> + 'a>;

    fn into_iter(self) -> Self::IntoIter {
        Box::new(self.iter())
    }
}

/// One result row.
#[derive(Debug, Clone, Copy)]
pub struct Row<'a> {
    columns: &'a [String],
    values: &'a [Json],
}

impl<'a> Row<'a> {
    /// Raw JSON value of a column.
    #[must_use]
    pub fn value(&self, column: &str) -> Option<&'a Json> {
        let i = self.columns.iter().position(|c| c == column)?;
        self.values.get(i)
    }

    /// Decodes a column into `T`.
    ///
    /// # Errors
    /// Unknown column or a value that does not decode as `T`.
    pub fn get<T: DeserializeOwned>(&self, column: &str) -> Result<T> {
        let v = self.value(column).ok_or_else(|| Error::Decode {
            what: format!("column '{column}'"),
            reason: format!("no such column (have: {})", self.columns.join(", ")),
        })?;
        decode(v.clone(), &format!("column '{column}'"))
    }

    /// The row as a JSON object.
    #[must_use]
    pub fn to_object(&self) -> Map<String, Json> {
        self.columns
            .iter()
            .cloned()
            .zip(self.values.iter().cloned())
            .collect()
    }

    /// Decodes the whole row; see [`Rows::decode`].
    ///
    /// # Errors
    /// [`Error::Decode`] when the row does not fit `T`.
    pub fn decode<T: DeserializeOwned>(&self) -> Result<T> {
        match self.values {
            [single @ Json::Object(o)] if o.contains_key("_id") => decode(single.clone(), "row"),
            _ => decode(Json::Object(self.to_object()), "row"),
        }
    }

    /// The column as a [`Node`].
    ///
    /// # Errors
    /// When the column is not a node.
    pub fn node(&self, column: &str) -> Result<Node> {
        Node::from_json(self.value(column).cloned().unwrap_or_default())
    }
}

pub(crate) fn decode<T: DeserializeOwned>(v: Json, what: &str) -> Result<T> {
    serde_json::from_value(v).map_err(|e| Error::Decode {
        what: what.to_owned(),
        reason: e.to_string(),
    })
}

fn id_of(map: &Map<String, Json>, key: &str) -> Option<u64> {
    map.get(key).and_then(Json::as_u64)
}

/// A node returned from the database.
#[derive(Debug, Clone, PartialEq)]
pub struct Node {
    /// Unique id.
    pub id: u64,
    /// Node type, e.g. `User`.
    pub label: String,
    /// Declared properties.
    pub props: Map<String, Json>,
}

impl Node {
    pub(crate) fn from_json(v: Json) -> Result<Self> {
        let Json::Object(mut map) = v else {
            return Err(Error::Decode {
                what: "node".into(),
                reason: format!("expected an object, got {v}"),
            });
        };
        let id = id_of(&map, "_id").ok_or_else(|| Error::Decode {
            what: "node".into(),
            reason: "missing _id".into(),
        })?;
        let label = map
            .get("_label")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_owned();
        map.retain(|k, _| !k.starts_with('_'));
        Ok(Self {
            id,
            label,
            props: map,
        })
    }

    /// Decodes one property.
    ///
    /// # Errors
    /// When the property does not decode as `T` (a missing property decodes
    /// as JSON `null`, so use `Option<T>` for nullable ones).
    pub fn get<T: DeserializeOwned>(&self, prop: &str) -> Result<T> {
        decode(
            self.props.get(prop).cloned().unwrap_or_default(),
            &format!("{}.{prop}", self.label),
        )
    }

    /// Decodes the whole node (its properties plus `_id`/`_label`) into `T`.
    ///
    /// # Errors
    /// When the node does not fit `T`.
    pub fn decode<T: DeserializeOwned>(&self) -> Result<T> {
        let mut map = self.props.clone();
        map.insert("_id".into(), self.id.into());
        map.insert("_label".into(), self.label.clone().into());
        decode(Json::Object(map), &self.label)
    }
}

impl fmt::Display for Node {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "({}:{} {})",
            self.id,
            self.label,
            Json::Object(self.props.clone())
        )
    }
}

/// An edge returned from the database.
#[derive(Debug, Clone, PartialEq)]
pub struct Edge {
    /// Unique id.
    pub id: u64,
    /// Edge type, e.g. `FOLLOWS`.
    pub label: String,
    /// Id of the source node.
    pub from: u64,
    /// Id of the target node.
    pub to: u64,
    /// Declared properties.
    pub props: Map<String, Json>,
}

impl Edge {
    pub(crate) fn from_json(v: Json) -> Result<Self> {
        let bad = |reason: &str| Error::Decode {
            what: "edge".into(),
            reason: reason.into(),
        };
        let Json::Object(mut map) = v else {
            return Err(bad("expected an object"));
        };
        let id = id_of(&map, "_id").ok_or_else(|| bad("missing _id"))?;
        let from = id_of(&map, "_from").ok_or_else(|| bad("missing _from"))?;
        let to = id_of(&map, "_to").ok_or_else(|| bad("missing _to"))?;
        let label = map
            .get("_label")
            .and_then(Json::as_str)
            .unwrap_or_default()
            .to_owned();
        map.retain(|k, _| !k.starts_with('_'));
        Ok(Self {
            id,
            label,
            from,
            to,
            props: map,
        })
    }

    /// Decodes one property.
    ///
    /// # Errors
    /// When the property does not decode as `T`.
    pub fn get<T: DeserializeOwned>(&self, prop: &str) -> Result<T> {
        decode(
            self.props.get(prop).cloned().unwrap_or_default(),
            &format!("{}.{prop}", self.label),
        )
    }
}

impl fmt::Display for Edge {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "({})-[{}:{} {}]->({})",
            self.from,
            self.id,
            self.label,
            Json::Object(self.props.clone()),
            self.to
        )
    }
}

/// Anything that identifies a node: a [`Node`], a `&Node` or a raw id.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NodeId(pub u64);

impl From<u64> for NodeId {
    fn from(id: u64) -> Self {
        Self(id)
    }
}

impl From<&Node> for NodeId {
    fn from(n: &Node) -> Self {
        Self(n.id)
    }
}

impl From<Node> for NodeId {
    fn from(n: Node) -> Self {
        Self(n.id)
    }
}
