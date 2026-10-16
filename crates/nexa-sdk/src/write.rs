//! Builders that create nodes and edges.

use serde::Serialize;
use serde_json::{Map, Value as Json};

use crate::Nexa;
use crate::cond::{Bag, ident};
use crate::error::{Error, Result};
use crate::rows::{Edge, Node, NodeId};

fn props_literal(props: &Map<String, Json>, bag: &mut Bag) -> String {
    let fields: Vec<String> = props
        .iter()
        .map(|(k, v)| format!("{}: {}", ident(k), bag.push(v.clone())))
        .collect();
    format!("{{{}}}", fields.join(", "))
}

pub(crate) fn to_props(value: impl Serialize) -> Result<Map<String, Json>> {
    match serde_json::to_value(value) {
        Ok(Json::Object(map)) => Ok(map),
        Ok(other) => Err(Error::Usage(format!(
            "properties must serialize to an object, got {other}"
        ))),
        Err(e) => Err(Error::Usage(format!("could not serialize properties: {e}"))),
    }
}

/// Creates one node. Returned by [`Nexa::create`].
///
/// ```no_run
/// # use nexa::prelude::*;
/// # fn main() -> nexa::Result<()> {
/// # let db = Nexa::connect("http://127.0.0.1:7285")?;
/// let ada = db.create("User").set("name", "Ada").set("age", 36).run()?;
/// println!("created {} #{}", ada.label, ada.id);
/// # Ok(()) }
/// ```
#[derive(Debug, Clone)]
#[must_use = "nothing is created until .run()"]
pub struct Create<'db> {
    db: &'db Nexa,
    label: String,
    props: Map<String, Json>,
    error: Option<Error>,
}

impl<'db> Create<'db> {
    pub(crate) fn new(db: &'db Nexa, label: &str) -> Self {
        Self {
            db,
            label: label.to_owned(),
            props: Map::new(),
            error: None,
        }
    }

    /// Sets one property.
    pub fn set(mut self, prop: &str, value: impl Into<Json>) -> Self {
        self.props.insert(prop.to_owned(), value.into());
        self
    }

    /// Sets every field of a serializable struct or map as properties.
    pub fn from(mut self, value: impl Serialize) -> Self {
        match to_props(value) {
            Ok(map) => self.props.extend(map),
            Err(e) => self.error = Some(e),
        }
        self
    }

    /// Inserts the node and returns it (with its new id and any defaults).
    ///
    /// # Errors
    /// Schema, type and constraint errors (e.g. a duplicate UNIQUE value).
    pub fn run(self) -> Result<Node> {
        if let Some(e) = self.error {
            return Err(e);
        }
        let mut bag = Bag::default();
        let label = ident(&self.label);
        let q = format!(
            "INSERT INTO {label} {} RETURNING {label}",
            props_literal(&self.props, &mut bag)
        );
        let mut rows = self.db.run_nql(&q, bag.params)?.rows;
        let row = rows
            .pop()
            .and_then(|mut r| r.pop())
            .ok_or_else(|| Error::NotFound(self.label.clone()))?;
        Node::from_json(row)
    }
}

/// Creates many nodes of one type in a single atomic statement.
pub(crate) fn create_many<T: Serialize>(
    db: &Nexa,
    label: &str,
    items: impl IntoIterator<Item = T>,
) -> Result<Vec<Node>> {
    let mut bag = Bag::default();
    let rows = items
        .into_iter()
        .map(|item| to_props(item).map(|p| props_literal(&p, &mut bag)))
        .collect::<Result<Vec<_>>>()?;
    if rows.is_empty() {
        return Ok(Vec::new());
    }
    let label = ident(label);
    let q = format!("INSERT INTO {label} {} RETURNING {label}", rows.join(", "));
    db.run_nql(&q, bag.params)?
        .rows
        .into_iter()
        .map(|mut r| Node::from_json(r.swap_remove(0)))
        .collect()
}

/// Connects two nodes with a new edge. Returned by [`Nexa::link`].
///
/// ```no_run
/// # use nexa::prelude::*;
/// # fn main() -> nexa::Result<()> {
/// # let db = Nexa::connect("http://127.0.0.1:7285")?;
/// # let (ada, linus) = (1u64, 2u64);
/// db.link(ada, "FOLLOWS", linus).set("since", 2024).run()?;
/// # Ok(()) }
/// ```
#[derive(Debug, Clone)]
#[must_use = "nothing is created until .run()"]
pub struct Link<'db> {
    db: &'db Nexa,
    from: NodeId,
    label: String,
    to: NodeId,
    props: Map<String, Json>,
    error: Option<Error>,
}

impl<'db> Link<'db> {
    pub(crate) fn new(db: &'db Nexa, from: NodeId, label: &str, to: NodeId) -> Self {
        Self {
            db,
            from,
            label: label.to_owned(),
            to,
            props: Map::new(),
            error: None,
        }
    }

    /// Sets one edge property.
    pub fn set(mut self, prop: &str, value: impl Into<Json>) -> Self {
        self.props.insert(prop.to_owned(), value.into());
        self
    }

    /// Sets every field of a serializable value as edge properties.
    pub fn from(mut self, value: impl Serialize) -> Self {
        match to_props(value) {
            Ok(map) => self.props.extend(map),
            Err(e) => self.error = Some(e),
        }
        self
    }

    /// Creates the edge and returns it.
    ///
    /// # Errors
    /// [`Error::NotFound`] if either node is missing; schema errors if the
    /// edge type does not connect those node types.
    pub fn run(self) -> Result<Edge> {
        if let Some(e) = self.error {
            return Err(e);
        }
        let mut bag = Bag::default();
        let props = props_literal(&self.props, &mut bag);
        let (a, b) = (bag.push(self.from.0.into()), bag.push(self.to.0.into()));
        let q = format!(
            "INSERT (a)-[e:{} {props}]->(b) FROM (a), (b) WHERE a.`_id` = {a} AND b.`_id` = {b} RETURNING e",
            ident(&self.label)
        );
        let mut rows = self.db.run_nql(&q, bag.params)?.rows;
        let row = rows
            .pop()
            .and_then(|mut r| r.pop())
            .ok_or_else(|| Error::NotFound(format!("node {} or {}", self.from.0, self.to.0)))?;
        Edge::from_json(row)
    }
}
