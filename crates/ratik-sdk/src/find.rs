//! The fluent read/traverse/update/delete builder returned by [`Ratik::find`].

use std::ops::{RangeFrom, RangeInclusive};

use serde::de::DeserializeOwned;
use serde_json::{Map, Value as Json};

use crate::Ratik;
use crate::cond::{Bag, Cond, ident, prop};
use crate::error::{Error, Result};
use crate::rows::{Edge, Node, Rows, decode};

/// How many hops a traversal may take: `2`, `1..=3` or `1..`.
pub trait Hops {
    /// `(min, max)`; `None` max = unbounded.
    fn range(self) -> (u32, Option<u32>);
}

impl Hops for u32 {
    fn range(self) -> (u32, Option<u32>) {
        (self, Some(self))
    }
}

impl Hops for RangeInclusive<u32> {
    fn range(self) -> (u32, Option<u32>) {
        (*self.start(), Some(*self.end()))
    }
}

impl Hops for RangeFrom<u32> {
    fn range(self) -> (u32, Option<u32>) {
        (self.start, None)
    }
}

#[derive(Debug, Clone, Copy)]
enum Dir {
    Out,
    In,
    Both,
}

#[derive(Debug, Clone)]
struct Hop {
    edge: Option<String>,
    dir: Dir,
    hops: Option<(u32, Option<u32>)>,
    edge_filters: Vec<Cond>,
    node_type: Option<String>,
    node_filters: Vec<Cond>,
}

#[derive(Debug, Clone)]
enum OrderKey {
    Prop(String, bool),
    Nearest(String, Json),
    Relevance(String, String),
}

/// A query that starts at nodes of one type and optionally walks edges.
///
/// Every method returns the builder, and nothing runs until a terminal
/// method (`all`, `first`, `count`, `update`, `delete`, ...) is called.
///
/// ```no_run
/// # use ratik::prelude::*;
/// # fn main() -> ratik::Result<()> {
/// # let db = Ratik::connect("http://127.0.0.1:7285")?;
/// let friends_of_ada = db
///     .find("User")
///     .with("name", "Ada")
///     .outgoing("FOLLOWS")
///     .filter(prop("age").gte(21))
///     .order_by("name")
///     .limit(10)
///     .all()?;
/// # Ok(()) }
/// ```
#[derive(Debug, Clone)]
#[must_use = "a query does nothing until you call a method like .all() or .count()"]
pub struct Find<'db> {
    db: &'db Ratik,
    label: String,
    start_filters: Vec<Cond>,
    hops: Vec<Hop>,
    order: Vec<OrderKey>,
    limit: Option<u64>,
    skip: Option<u64>,
    distinct: bool,
}

impl<'db> Find<'db> {
    pub(crate) fn new(db: &'db Ratik, label: &str) -> Self {
        Self {
            db,
            label: label.to_owned(),
            start_filters: Vec::new(),
            hops: Vec::new(),
            order: Vec::new(),
            limit: None,
            skip: None,
            distinct: false,
        }
    }

    fn current_filters(&mut self) -> &mut Vec<Cond> {
        match self.hops.last_mut() {
            Some(h) => &mut h.node_filters,
            None => &mut self.start_filters,
        }
    }

    // ── filtering ─────────────────────────────────────────────────────────

    /// Keeps nodes whose `prop` equals `value`. Reads like English:
    /// `db.find("User").with("name", "Ada")`.
    pub fn with(self, prop_name: &str, value: impl Into<Json>) -> Self {
        self.filter(prop(prop_name).eq(value))
    }

    /// Keeps nodes matching a condition built with [`prop`](crate::prop).
    pub fn filter(mut self, cond: Cond) -> Self {
        self.current_filters().push(cond);
        self
    }

    /// Keeps the node with this id.
    pub fn id(self, id: u64) -> Self {
        self.filter(prop("_id").eq(id))
    }

    /// Restricts the current nodes to one type (useful after an edge that
    /// can lead to several types).
    pub fn of_type(mut self, label: &str) -> Self {
        match self.hops.last_mut() {
            Some(h) => h.node_type = Some(label.to_owned()),
            None => self.label = label.to_owned(),
        }
        self
    }

    // ── traversal ─────────────────────────────────────────────────────────

    fn hop(mut self, edge: &str, dir: Dir) -> Self {
        self.hops.push(Hop {
            edge: (!edge.is_empty()).then(|| edge.to_owned()),
            dir,
            hops: None,
            edge_filters: Vec::new(),
            node_type: None,
            node_filters: Vec::new(),
        });
        self
    }

    /// Follows `edge` edges forward: `(current)-[:EDGE]->(next)`.
    pub fn outgoing(self, edge: &str) -> Self {
        self.hop(edge, Dir::Out)
    }

    /// Follows `edge` edges backward: `(current)<-[:EDGE]-(next)`.
    pub fn incoming(self, edge: &str) -> Self {
        self.hop(edge, Dir::In)
    }

    /// Follows `edge` edges in either direction.
    pub fn both(self, edge: &str) -> Self {
        self.hop(edge, Dir::Both)
    }

    /// Lets the previous traversal repeat: `.outgoing("FOLLOWS").hops(1..=3)`
    /// finds everyone within three follows.
    pub fn hops(mut self, range: impl Hops) -> Self {
        if let Some(h) = self.hops.last_mut() {
            h.hops = Some(range.range());
        }
        self
    }

    /// Filters the edge of the previous traversal:
    /// `.outgoing("FOLLOWS").where_edge(prop("since").gte(2020))`.
    pub fn where_edge(mut self, cond: Cond) -> Self {
        if let Some(h) = self.hops.last_mut() {
            h.edge_filters.push(cond);
        }
        self
    }

    // ── ranking & paging ──────────────────────────────────────────────────

    /// Sorts ascending by a property.
    pub fn order_by(mut self, prop_name: &str) -> Self {
        self.order.push(OrderKey::Prop(prop_name.to_owned(), false));
        self
    }

    /// Sorts descending by a property.
    pub fn order_by_desc(mut self, prop_name: &str) -> Self {
        self.order.push(OrderKey::Prop(prop_name.to_owned(), true));
        self
    }

    /// Ranks by vector similarity to `vector` (uses the HNSW index).
    /// Returns the 10 closest unless you set [`limit`](Self::limit).
    pub fn nearest<V: Into<Json>>(mut self, vector_prop: &str, vector: impl IntoIterator<Item = V>) -> Self {
        let v = Json::Array(vector.into_iter().map(Into::into).collect());
        self.order.insert(0, OrderKey::Nearest(vector_prop.to_owned(), v));
        self
    }

    /// Full-text search on a `SEARCHABLE` property, best matches first.
    pub fn search(mut self, text_prop: &str, text: &str) -> Self {
        self = self.filter(prop(text_prop).matches(text));
        self.order.insert(0, OrderKey::Relevance(text_prop.to_owned(), text.to_owned()));
        self
    }

    /// At most `n` results.
    pub fn limit(mut self, n: u64) -> Self {
        self.limit = Some(n);
        self
    }

    /// Skips the first `n` results.
    pub fn skip(mut self, n: u64) -> Self {
        self.skip = Some(n);
        self
    }

    /// Removes duplicate results (e.g. a friend reached via two paths).
    pub fn distinct(mut self) -> Self {
        self.distinct = true;
        self
    }

    // ── rendering ─────────────────────────────────────────────────────────

    fn target(&self) -> String {
        format!("n{}", self.hops.len())
    }

    /// `FROM ... WHERE ...` (shared by every terminal).
    fn render_match(&self, bag: &mut Bag) -> Result<(String, Vec<String>)> {
        let mut pattern = format!("(n0:{})", ident(&self.label));
        let mut wheres: Vec<String> = self.start_filters.iter().map(|c| c.render("n0", bag)).collect();
        for (i, h) in self.hops.iter().enumerate() {
            let (n, e) = (format!("n{}", i + 1), format!("e{}", i + 1));
            if h.hops.is_some() && !h.edge_filters.is_empty() {
                return Err(Error::Usage("where_edge() cannot be combined with hops()".into()));
            }
            let mut body = if h.hops.is_some() { String::new() } else { e.clone() };
            if let Some(label) = &h.edge {
                body.push(':');
                body.push_str(&ident(label));
            }
            if let Some((min, max)) = h.hops {
                body.push_str(&format!("*{min}..{}", max.map(|m| m.to_string()).unwrap_or_default()));
            }
            let node = match &h.node_type {
                Some(t) => format!("({n}:{})", ident(t)),
                None => format!("({n})"),
            };
            pattern.push_str(&match h.dir {
                Dir::Out => format!("-[{body}]->{node}"),
                Dir::In => format!("<-[{body}]-{node}"),
                Dir::Both => format!("-[{body}]-{node}"),
            });
            wheres.extend(h.edge_filters.iter().map(|c| c.render(&e, bag)));
            wheres.extend(h.node_filters.iter().map(|c| c.render(&n, bag)));
        }
        Ok((pattern, wheres))
    }

    fn render_select(&self, projection: &str, bag: &mut Bag) -> Result<String> {
        let (pattern, wheres) = self.render_match(bag)?;
        let target = self.target();
        let mut q = format!("SELECT {}{projection} FROM {pattern}", if self.distinct { "DISTINCT " } else { "" });
        if !wheres.is_empty() {
            q.push_str(&format!(" WHERE {}", wheres.join(" AND ")));
        }
        if !self.order.is_empty() {
            let keys: Vec<String> = self
                .order
                .iter()
                .map(|k| match k {
                    OrderKey::Prop(p, desc) => format!("{target}.{}{}", ident(p), if *desc { " DESC" } else { "" }),
                    OrderKey::Nearest(p, v) => format!("similarity({target}.{}, {}) DESC", ident(p), bag.push(v.clone())),
                    OrderKey::Relevance(p, text) => {
                        format!("bm25({target}.{}, {}) DESC", ident(p), bag.push(Json::String(text.clone())))
                    }
                })
                .collect();
            q.push_str(&format!(" ORDER BY {}", keys.join(", ")));
        }
        let default_limit = matches!(self.order.first(), Some(OrderKey::Nearest(..))).then_some(10);
        if let Some(n) = self.limit.or(default_limit) {
            q.push_str(&format!(" LIMIT {n}"));
        }
        if let Some(n) = self.skip {
            q.push_str(&format!(" OFFSET {n}"));
        }
        Ok(q)
    }

    /// The RQL and parameters this builder will send — handy for debugging
    /// or for pasting into `ratik shell`.
    ///
    /// # Errors
    /// [`Error::Usage`] for invalid combinations.
    pub fn to_rql(&self) -> Result<(String, Map<String, Json>)> {
        let mut bag = Bag::default();
        let q = self.render_select(&self.target(), &mut bag)?;
        Ok((q, bag.params))
    }

    fn fetch(&self, projection: &str) -> Result<Rows> {
        let mut bag = Bag::default();
        let q = self.render_select(projection, &mut bag)?;
        self.db.run_rql(&q, bag.params)
    }

    // ── terminals ─────────────────────────────────────────────────────────

    /// Every matching node.
    ///
    /// # Errors
    /// Schema errors (unknown type/property), connection errors.
    pub fn all(&self) -> Result<Vec<Node>> {
        let target = self.target();
        self.fetch(&target)?.rows.into_iter().map(|mut r| Node::from_json(r.swap_remove(0))).collect()
    }

    /// The first matching node, if any.
    ///
    /// # Errors
    /// See [`Find::all`].
    pub fn first(&self) -> Result<Option<Node>> {
        Ok(self.clone().limit(1).all()?.into_iter().next())
    }

    /// Exactly one node, or [`Error::NotFound`].
    ///
    /// # Errors
    /// See [`Find::all`].
    pub fn one(&self) -> Result<Node> {
        self.first()?.ok_or_else(|| Error::NotFound(self.label.clone()))
    }

    /// Every matching node decoded into your own type.
    ///
    /// # Errors
    /// See [`Find::all`], plus decode errors.
    pub fn all_as<T: DeserializeOwned>(&self) -> Result<Vec<T>> {
        self.all()?.iter().map(Node::decode).collect()
    }

    /// The first matching node decoded into your own type.
    ///
    /// # Errors
    /// See [`Find::all`], plus decode errors.
    pub fn first_as<T: DeserializeOwned>(&self) -> Result<Option<T>> {
        self.first()?.as_ref().map(Node::decode).transpose()
    }

    /// One property of every match: `db.find("User").pluck::<String>("name")`.
    ///
    /// # Errors
    /// See [`Find::all`], plus decode errors.
    pub fn pluck<T: DeserializeOwned>(&self, prop_name: &str) -> Result<Vec<T>> {
        let col = format!("{}.{}", self.target(), ident(prop_name));
        self.fetch(&format!("{col} AS {}", ident(prop_name)))?
            .rows
            .into_iter()
            .map(|mut r| decode(r.swap_remove(0), prop_name))
            .collect()
    }

    /// Several properties as a table.
    ///
    /// # Errors
    /// See [`Find::all`].
    pub fn select(&self, props: &[&str]) -> Result<Rows> {
        let target = self.target();
        let cols: Vec<String> = props.iter().map(|p| format!("{target}.{} AS {}", ident(p), ident(p))).collect();
        self.fetch(&cols.join(", "))
    }

    /// The edges walked by the last traversal.
    ///
    /// # Errors
    /// [`Error::Usage`] when there is no traversal or it uses `hops()`.
    pub fn edges(&self) -> Result<Vec<Edge>> {
        let Some(last) = self.hops.last() else {
            return Err(Error::Usage("edges() needs a traversal such as .outgoing(\"FOLLOWS\")".into()));
        };
        if last.hops.is_some() {
            return Err(Error::Usage("edges() is not available after hops()".into()));
        }
        let e = format!("e{}", self.hops.len());
        self.fetch(&e)?.rows.into_iter().map(|mut r| Edge::from_json(r.swap_remove(0))).collect()
    }

    /// Number of matches.
    ///
    /// # Errors
    /// See [`Find::all`].
    pub fn count(&self) -> Result<u64> {
        let mut q = self.clone();
        q.order.clear();
        q.limit = None;
        q.skip = None;
        let projection = if q.distinct { format!("count(DISTINCT {})", q.target()) } else { "count(*)".into() };
        q.distinct = false;
        let rows = q.fetch(&projection)?;
        rows.first().map_or(Ok(0), |r| r.get(&rows.columns[0]))
    }

    /// `true` if anything matches.
    ///
    /// # Errors
    /// See [`Find::all`].
    pub fn exists(&self) -> Result<bool> {
        Ok(self.first()?.is_some())
    }

    fn reject_paging(&self, what: &str) -> Result<()> {
        if self.limit.is_some() || self.skip.is_some() || !self.order.is_empty() {
            return Err(Error::Usage(format!("{what} cannot be combined with order_by/limit/skip/nearest/search")));
        }
        Ok(())
    }

    /// Starts an update of every match: `.update().set("age", 37).run()`.
    pub fn update(self) -> Update<'db> {
        Update { find: self, sets: Vec::new() }
    }

    /// Deletes every match (and, for nodes, their edges). Returns how many
    /// nodes were deleted.
    ///
    /// # Errors
    /// See [`Find::all`].
    pub fn delete(&self) -> Result<u64> {
        self.reject_paging("delete()")?;
        let mut bag = Bag::default();
        let (pattern, wheres) = self.render_match(&mut bag)?;
        let mut q = format!("DELETE {} FROM {pattern}", self.target());
        if !wheres.is_empty() {
            q.push_str(&format!(" WHERE {}", wheres.join(" AND ")));
        }
        Ok(self.db.run_rql(&q, bag.params)?.stats.nodes_deleted)
    }
}

/// Changes properties of everything a [`Find`] matches.
#[derive(Debug, Clone)]
#[must_use = "an update does nothing until .run()"]
pub struct Update<'db> {
    find: Find<'db>,
    sets: Vec<(String, Json, bool)>,
}

impl Update<'_> {
    /// Sets a property.
    pub fn set(mut self, prop_name: &str, value: impl Into<Json>) -> Self {
        self.sets.push((prop_name.to_owned(), value.into(), false));
        self
    }

    /// Adds to a numeric property: `.increment("likes", 1)`.
    pub fn increment(mut self, prop_name: &str, by: impl Into<Json>) -> Self {
        self.sets.push((prop_name.to_owned(), by.into(), true));
        self
    }

    /// Runs the update and returns the updated nodes.
    ///
    /// # Errors
    /// Schema/type/constraint errors; [`Error::Usage`] when nothing is set.
    pub fn run(self) -> Result<Vec<Node>> {
        self.find.reject_paging("update()")?;
        if self.sets.is_empty() {
            return Err(Error::Usage("update() needs at least one .set(...)".into()));
        }
        let mut bag = Bag::default();
        let (pattern, wheres) = self.find.render_match(&mut bag)?;
        let t = self.find.target();
        let sets: Vec<String> = self
            .sets
            .into_iter()
            .map(|(p, v, add)| {
                let p = ident(&p);
                let v = bag.push(v);
                if add { format!("{t}.{p} = {t}.{p} + {v}") } else { format!("{t}.{p} = {v}") }
            })
            .collect();
        let mut q = format!("UPDATE {pattern} SET {}", sets.join(", "));
        if !wheres.is_empty() {
            q.push_str(&format!(" WHERE {}", wheres.join(" AND ")));
        }
        q.push_str(&format!(" RETURNING {t}"));
        self.find.db.run_rql(&q, bag.params)?.rows.into_iter().map(|mut r| Node::from_json(r.swap_remove(0))).collect()
    }
}
