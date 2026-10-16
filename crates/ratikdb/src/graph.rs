//! The disk-resident graph store.
//!
//! Everything lives in [redb](https://www.redb.org) B-trees inside one file,
//! so the graph can be far larger than RAM: a query reads only the pages it
//! touches, and redb's page cache keeps the hot ones in memory.
//!
//! | table | key → value |
//! |---|---|
//! | `nodes` | id → (type, props) |
//! | `edges` | id → (type, from, to, props) |
//! | `adj_out` / `adj_in` | (node, edge type, edge) → other node |
//! | `members` | (type, id) → () |
//! | `counts` | type → number of elements |
//! | `prop_index` | (type, property, order-preserving value bytes, id) → () |
//! | `text_postings` | (type, property, term, id) → term frequency |
//! | `text_df` / `text_doclen` / `text_stats` | BM25 statistics |
//! | `meta` | schema, stored queries, id counter |
//!
//! Adjacency keys start with the node id and the edge type, so "all
//! `FOLLOWS` edges of node 42" is one contiguous range scan.
//!
//! A [`Graph`] wraps one redb transaction. Mutations go through
//! [`Graph::apply`] one primitive [`Op`] at a time; dropping an uncommitted
//! write `Graph` aborts the transaction and undoes them all. The schema and
//! stored queries are small and kept in memory as copy-on-write `Arc`s. The
//! HNSW vector indexes live in memory too and change only after a commit.

use std::cell::RefCell;
use std::collections::{BTreeMap, BTreeSet};
use std::ops::Bound;
use std::rc::Rc;
use std::sync::Arc;

use redb::{ReadOnlyTable, ReadTransaction, ReadableTable, TableDefinition, WriteTransaction};
use rustc_hash::{FxHashMap, FxHashSet};
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::index::text::{bm25, tokenize};
use crate::index::vector::Hnsw;
use crate::schema::{Kind, Metric, PropDef, PropType, Schema, TypeDef, TypeId};
use crate::value::{ElementId, OrdValue, Value};

type Props<'a> = &'a [u8];

pub(crate) const NODES: TableDefinition<u64, (u32, Props)> = TableDefinition::new("nodes");
pub(crate) const EDGES: TableDefinition<u64, (u32, u64, u64, Props)> =
    TableDefinition::new("edges");
pub(crate) const OUT: TableDefinition<(u64, u32, u64), u64> = TableDefinition::new("adj_out");
pub(crate) const IN: TableDefinition<(u64, u32, u64), u64> = TableDefinition::new("adj_in");
pub(crate) const MEMBERS: TableDefinition<(u32, u64), ()> = TableDefinition::new("members");
pub(crate) const COUNTS: TableDefinition<u32, u64> = TableDefinition::new("counts");
pub(crate) const PROP_INDEX: TableDefinition<(u32, &str, &[u8], u64), ()> =
    TableDefinition::new("prop_index");
pub(crate) const POSTINGS: TableDefinition<(u32, &str, &str, u64), u32> =
    TableDefinition::new("text_postings");
pub(crate) const DOC_FREQ: TableDefinition<(u32, &str, &str), u64> =
    TableDefinition::new("text_df");
pub(crate) const DOC_LEN: TableDefinition<(u32, &str, u64), u32> =
    TableDefinition::new("text_doclen");
pub(crate) const TEXT_STATS: TableDefinition<(u32, &str), (u64, u64)> =
    TableDefinition::new("text_stats");
pub(crate) const META: TableDefinition<&str, &[u8]> = TableDefinition::new("meta");

/// Creates every table so later read transactions can open them.
pub(crate) fn create_tables(wtx: &WriteTransaction) -> Result<()> {
    wtx.open_table(NODES)?;
    wtx.open_table(EDGES)?;
    wtx.open_table(OUT)?;
    wtx.open_table(IN)?;
    wtx.open_table(MEMBERS)?;
    wtx.open_table(COUNTS)?;
    wtx.open_table(PROP_INDEX)?;
    wtx.open_table(POSTINGS)?;
    wtx.open_table(DOC_FREQ)?;
    wtx.open_table(DOC_LEN)?;
    wtx.open_table(TEXT_STATS)?;
    wtx.open_table(META)?;
    Ok(())
}

/// One adjacency entry: an incident edge and the node at its other end.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Adj {
    pub edge: ElementId,
    pub other: ElementId,
}

/// Which adjacency lists to read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Direction {
    Out,
    In,
    Both,
}

/// A decoded node or edge.
#[derive(Debug, Clone)]
pub(crate) struct Elem {
    pub ty: TypeId,
    pub props: Rc<Vec<Value>>,
    /// `(from, to)` for edges, `None` for nodes.
    pub ends: Option<(ElementId, ElementId)>,
}

/// A named, parameterised query stored in the database (`CREATE QUERY`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StoredQuery {
    /// Name used to call it.
    pub name: String,
    /// Declared parameters, in call order.
    pub params: Vec<(String, PropType)>,
    /// Source text of the body statement.
    pub body: String,
}

/// A primitive change. Every write is a sequence of these.
#[derive(Debug, Clone)]
pub(crate) enum Op {
    CreateType(TypeDef),
    DropType(TypeId),
    AddProp {
        ty: TypeId,
        def: PropDef,
        fill: Value,
    },
    DropProp {
        ty: TypeId,
        name: String,
    },
    SetFlags {
        ty: TypeId,
        name: String,
        unique: bool,
        indexed: bool,
        searchable: bool,
    },
    AddNode {
        id: ElementId,
        ty: TypeId,
        props: Vec<Value>,
    },
    AddEdge {
        id: ElementId,
        ty: TypeId,
        from: ElementId,
        to: ElementId,
        props: Vec<Value>,
    },
    SetProp {
        id: ElementId,
        slot: usize,
        value: Value,
    },
    DelNode(ElementId),
    DelEdge(ElementId),
    PutQuery(StoredQuery),
    DropQuery(String),
}

/// In-memory HNSW indexes, keyed by (type, property).
pub(crate) type VectorIndexes = FxHashMap<(TypeId, String), Hnsw>;

/// A vector-index change, applied to the in-memory HNSW after commit.
#[derive(Debug, Clone)]
pub(crate) enum VecOp {
    Put {
        key: (TypeId, String),
        id: ElementId,
        vector: Vec<f32>,
        dim: u32,
        metric: Metric,
    },
    Remove {
        key: (TypeId, String),
        id: ElementId,
    },
    Drop {
        key: (TypeId, String),
    },
}

/// Shared, committed state the engine keeps between transactions.
#[derive(Debug, Clone, Default)]
pub(crate) struct State {
    pub schema: Arc<Schema>,
    pub queries: Arc<BTreeMap<String, StoredQuery>>,
    pub next_id: ElementId,
    pub vectors: Arc<VectorIndexes>,
}

impl State {
    /// Installs the result of a committed transaction.
    pub(crate) fn install(&mut self, c: Committed) {
        self.schema = c.schema;
        self.queries = c.queries;
        self.next_id = c.next_id;
        if c.vec_ops.is_empty() {
            return;
        }
        let vectors = Arc::make_mut(&mut self.vectors);
        for op in c.vec_ops {
            match op {
                VecOp::Put {
                    key,
                    id,
                    vector,
                    dim,
                    metric,
                } => vectors
                    .entry(key)
                    .or_insert_with(|| Hnsw::new(dim as usize, metric))
                    .insert(id, &vector),
                VecOp::Remove { key, id } => {
                    if let Some(h) = vectors.get_mut(&key) {
                        h.remove(id);
                    }
                }
                VecOp::Drop { key } => {
                    vectors.remove(&key);
                }
            }
        }
    }
}

/// Everything a committed transaction hands back to the engine.
#[derive(Debug)]
pub(crate) struct Committed {
    pub schema: Arc<Schema>,
    pub queries: Arc<BTreeMap<String, StoredQuery>>,
    pub next_id: ElementId,
    pub vec_ops: Vec<VecOp>,
}

/// Order-preserving bytes for an indexable value, so byte order in the
/// B-tree equals value order (`-5 < 3`, `-0.5 < 0.25`, `"a" < "b"`).
pub(crate) fn index_key(v: &Value) -> Option<Vec<u8>> {
    match v {
        Value::Bool(b) => Some(vec![u8::from(*b)]),
        Value::Int(i) => Some(((*i as u64) ^ (1 << 63)).to_be_bytes().to_vec()),
        Value::Float(f) => {
            let bits = f.to_bits();
            let key = if bits >> 63 == 1 {
                !bits
            } else {
                bits | (1 << 63)
            };
            Some(key.to_be_bytes().to_vec())
        }
        Value::String(s) => Some(s.as_bytes().to_vec()),
        _ => None,
    }
}

fn encode_props(props: &[Value]) -> Result<Vec<u8>> {
    postcard::to_stdvec(props).map_err(|e| Error::Corrupt(format!("encode: {e}")))
}

fn decode_props(bytes: &[u8]) -> Result<Vec<Value>> {
    postcard::from_bytes(bytes).map_err(|e| Error::Corrupt(format!("decode: {e}")))
}

struct ReadTables {
    nodes: ReadOnlyTable<u64, (u32, Props<'static>)>,
    edges: ReadOnlyTable<u64, (u32, u64, u64, Props<'static>)>,
    out: ReadOnlyTable<(u64, u32, u64), u64>,
    inc: ReadOnlyTable<(u64, u32, u64), u64>,
    members: ReadOnlyTable<(u32, u64), ()>,
    counts: ReadOnlyTable<u32, u64>,
    props: ReadOnlyTable<(u32, &'static str, &'static [u8], u64), ()>,
    postings: ReadOnlyTable<(u32, &'static str, &'static str, u64), u32>,
    df: ReadOnlyTable<(u32, &'static str, &'static str), u64>,
    doclen: ReadOnlyTable<(u32, &'static str, u64), u32>,
    textstats: ReadOnlyTable<(u32, &'static str), (u64, u64)>,
}

enum Tx {
    Read(Box<ReadTables>),
    Write(Box<WriteTransaction>),
}

/// Runs `$body` with `$t` bound to the table, for either transaction kind.
/// In a write transaction the table is opened for just this access, so the
/// body must return owned data.
macro_rules! with_table {
    ($g:expr, $field:ident, $def:expr, |$t:ident| $body:expr) => {
        match &$g.tx {
            Tx::Read(r) => {
                let $t = &r.$field;
                $body
            }
            Tx::Write(w) => {
                let opened = w.open_table($def)?;
                let $t = &opened;
                $body
            }
        }
    };
}

/// Runs `$body` with `$t` bound to a writable table, discarding its value
/// (redb hands back guards that borrow the table). A `Result` left without
/// `?` still triggers the `unused_must_use` lint.
macro_rules! write_table {
    ($g:expr, $def:expr, |$t:ident| $body:expr) => {{
        let Tx::Write(w) = &$g.tx else {
            return Err(Error::query("cannot write in a read-only transaction"));
        };
        let mut opened = w.open_table($def)?;
        let $t = &mut opened;
        $body;
    }};
}

/// Like `write_table!`, but returns the body's (owned) value.
macro_rules! write_table_get {
    ($g:expr, $def:expr, |$t:ident| $body:expr) => {{
        let Tx::Write(w) = &$g.tx else {
            return Err(Error::query("cannot write in a read-only transaction"));
        };
        let mut opened = w.open_table($def)?;
        let $t = &mut opened;
        let value = $body;
        value
    }};
}

const CACHE_LIMIT: usize = 100_000;

/// A transaction-scoped view of the database.
pub(crate) struct Graph {
    pub schema: Arc<Schema>,
    pub queries: Arc<BTreeMap<String, StoredQuery>>,
    pub next_id: ElementId,
    vectors: Arc<VectorIndexes>,
    vec_ops: Vec<VecOp>,
    dirty_vectors: FxHashSet<(TypeId, String)>,
    tx: Tx,
    cache: RefCell<FxHashMap<ElementId, Elem>>,
    meta_changed: bool,
}

impl std::fmt::Debug for Graph {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Graph")
            .field("next_id", &self.next_id)
            .finish_non_exhaustive()
    }
}

impl Graph {
    fn new(state: &State, tx: Tx) -> Self {
        Self {
            schema: state.schema.clone(),
            queries: state.queries.clone(),
            next_id: state.next_id,
            vectors: state.vectors.clone(),
            vec_ops: Vec::new(),
            dirty_vectors: FxHashSet::default(),
            tx,
            cache: RefCell::new(FxHashMap::default()),
            meta_changed: false,
        }
    }

    /// A read-only view of one consistent snapshot.
    pub(crate) fn read(state: &State, rtx: &ReadTransaction) -> Result<Self> {
        let tables = ReadTables {
            nodes: rtx.open_table(NODES)?,
            edges: rtx.open_table(EDGES)?,
            out: rtx.open_table(OUT)?,
            inc: rtx.open_table(IN)?,
            members: rtx.open_table(MEMBERS)?,
            counts: rtx.open_table(COUNTS)?,
            props: rtx.open_table(PROP_INDEX)?,
            postings: rtx.open_table(POSTINGS)?,
            df: rtx.open_table(DOC_FREQ)?,
            doclen: rtx.open_table(DOC_LEN)?,
            textstats: rtx.open_table(TEXT_STATS)?,
        };
        Ok(Self::new(state, Tx::Read(Box::new(tables))))
    }

    /// A writable view. Call [`Graph::commit`] to keep the changes.
    pub(crate) fn write(state: &State, wtx: WriteTransaction) -> Self {
        Self::new(state, Tx::Write(Box::new(wtx)))
    }

    /// Persists metadata and commits.
    pub(crate) fn commit(self) -> Result<Committed> {
        let Tx::Write(wtx) = self.tx else {
            return Err(Error::query("cannot commit a read-only transaction"));
        };
        {
            let mut meta = wtx.open_table(META)?;
            meta.insert("next_id", self.next_id.to_be_bytes().as_slice())?;
            if self.meta_changed {
                let encode = |e: postcard::Error| Error::Corrupt(e.to_string());
                let schema = postcard::to_stdvec(&*self.schema).map_err(encode)?;
                let queries = postcard::to_stdvec(&*self.queries).map_err(encode)?;
                meta.insert("schema", schema.as_slice())?;
                meta.insert("queries", queries.as_slice())?;
            }
        }
        wtx.commit()?;
        Ok(Committed {
            schema: self.schema,
            queries: self.queries,
            next_id: self.next_id,
            vec_ops: self.vec_ops,
        })
    }

    // ── reads ─────────────────────────────────────────────────────────────

    /// A node or edge, decoded (and cached for the rest of the transaction).
    pub(crate) fn element(&self, id: ElementId) -> Result<Option<Elem>> {
        if let Some(e) = self.cache.borrow().get(&id) {
            return Ok(Some(e.clone()));
        }
        let node = with_table!(self, nodes, NODES, |t| match t.get(id)? {
            Some(g) => {
                let (ty, bytes) = g.value();
                Some((ty, decode_props(bytes)?))
            }
            None => None,
        });
        let elem = match node {
            Some((ty, props)) => Some(Elem {
                ty,
                props: Rc::new(props),
                ends: None,
            }),
            None => with_table!(self, edges, EDGES, |t| match t.get(id)? {
                Some(g) => {
                    let (ty, from, to, bytes) = g.value();
                    Some(Elem {
                        ty,
                        props: Rc::new(decode_props(bytes)?),
                        ends: Some((from, to)),
                    })
                }
                None => None,
            }),
        };
        if let Some(e) = &elem {
            let mut cache = self.cache.borrow_mut();
            if cache.len() >= CACHE_LIMIT {
                cache.clear();
            }
            cache.insert(id, e.clone());
        }
        Ok(elem)
    }

    pub(crate) fn type_of(&self, id: ElementId) -> Result<Option<TypeId>> {
        Ok(self.element(id)?.map(|e| e.ty))
    }

    /// Number of elements of a type.
    pub(crate) fn member_count(&self, ty: TypeId) -> Result<u64> {
        with_table!(self, counts, COUNTS, |t| Ok(t
            .get(ty)?
            .map_or(0, |g| g.value())))
    }

    /// Total nodes (or edges) across all declared types.
    pub(crate) fn count_kind(&self, kind: Kind) -> Result<u64> {
        self.schema
            .of_kind(kind)
            .map(|t| self.member_count(t.id))
            .sum()
    }

    /// Up to `limit` member ids of `ty` that are `>= from`, ascending.
    fn member_chunk(&self, ty: TypeId, from: ElementId, limit: usize) -> Result<Vec<ElementId>> {
        with_table!(self, members, MEMBERS, |t| {
            let mut out = Vec::with_capacity(limit);
            for entry in t.range((ty, from)..=(ty, u64::MAX))?.take(limit) {
                out.push(entry?.0.value().1);
            }
            Ok::<_, Error>(out)
        })
    }

    /// Streams every member of a type in id order, reading in chunks so a
    /// `LIMIT` can stop early without touching the rest.
    pub(crate) fn members(&self, ty: TypeId) -> Members<'_> {
        Members {
            g: self,
            ty,
            next: 0,
            buf: Vec::new().into_iter(),
            done: false,
        }
    }

    /// All member ids (for bulk schema changes).
    pub(crate) fn member_ids(&self, ty: TypeId) -> Result<Vec<ElementId>> {
        self.members(ty).collect()
    }

    fn adjacency_range(
        &self,
        outgoing: bool,
        node: ElementId,
        lo: u32,
        hi: u32,
        skip_self: bool,
        out: &mut Vec<Adj>,
    ) -> Result<()> {
        let mut push = |entry: (u64, u32, u64), other: u64| {
            if !(skip_self && other == node) {
                out.push(Adj {
                    edge: entry.2,
                    other,
                });
            }
        };
        if outgoing {
            with_table!(self, out, OUT, |t| {
                for entry in t.range((node, lo, 0)..=(node, hi, u64::MAX))? {
                    let (k, v) = entry?;
                    push(k.value(), v.value());
                }
                Ok::<_, Error>(())
            })
        } else {
            with_table!(self, inc, IN, |t| {
                for entry in t.range((node, lo, 0)..=(node, hi, u64::MAX))? {
                    let (k, v) = entry?;
                    push(k.value(), v.value());
                }
                Ok::<_, Error>(())
            })
        }
    }

    /// Edges touching `node`, optionally restricted to some edge types.
    pub(crate) fn adjacent(
        &self,
        node: ElementId,
        dir: Direction,
        types: Option<&[TypeId]>,
    ) -> Result<Vec<Adj>> {
        let mut out = Vec::new();
        for (outgoing, wanted) in [(true, Direction::Out), (false, Direction::In)] {
            if dir != wanted && dir != Direction::Both {
                continue;
            }
            // In `Both` mode a self-loop is already in the outgoing list.
            let skip_self = !outgoing && dir == Direction::Both;
            match types {
                Some(ts) => {
                    for ty in ts {
                        self.adjacency_range(outgoing, node, *ty, *ty, skip_self, &mut out)?;
                    }
                }
                None => self.adjacency_range(outgoing, node, 0, u32::MAX, skip_self, &mut out)?,
            }
        }
        Ok(out)
    }

    /// Number of incident edges.
    pub(crate) fn degree(&self, node: ElementId, out: bool, inc: bool) -> Result<u64> {
        let range = (node, 0, 0)..=(node, u32::MAX, u64::MAX);
        let mut n = 0;
        if out {
            n += with_table!(self, out, OUT, |t| t.range(range.clone())?.count()) as u64;
        }
        if inc {
            n += with_table!(self, inc, IN, |t| t.range(range.clone())?.count()) as u64;
        }
        Ok(n)
    }

    fn prop_def(&self, ty: TypeId, prop: &str) -> Option<&PropDef> {
        let def = self.schema.get(ty)?;
        def.slot(prop).map(|s| &def.props[s])
    }

    /// `true` if `(ty, prop)` has a B-tree index.
    pub(crate) fn has_btree(&self, ty: TypeId, prop: &str) -> bool {
        self.prop_def(ty, prop).is_some_and(PropDef::has_btree)
    }

    /// `true` if `(ty, prop)` has a BM25 index.
    pub(crate) fn has_text(&self, ty: TypeId, prop: &str) -> bool {
        self.prop_def(ty, prop).is_some_and(|p| p.searchable)
    }

    /// Converts a lookup value to the column's type and encodes it; `None`
    /// when the value can never be stored in that column.
    fn lookup_key(&self, ty: TypeId, prop: &str, v: &Value) -> Option<Vec<u8>> {
        let def = self.prop_def(ty, prop)?;
        index_key(&def.ty.coerce(v.clone(), prop).ok()?)
    }

    /// Ids with `prop = value` (at most `cap`), via the B-tree index.
    pub(crate) fn index_eq(
        &self,
        ty: TypeId,
        prop: &str,
        v: &Value,
        cap: usize,
    ) -> Result<Vec<ElementId>> {
        let Some(key) = self.lookup_key(ty, prop, v) else {
            return Ok(Vec::new());
        };
        let k = key.as_slice();
        with_table!(self, props, PROP_INDEX, |t| {
            let mut out = Vec::new();
            for entry in t
                .range((ty, prop, k, 0)..=(ty, prop, k, u64::MAX))?
                .take(cap)
            {
                out.push(entry?.0.value().3);
            }
            Ok::<_, Error>(out)
        })
    }

    /// Ids with `prop` in a range (at most `cap`), ascending by id. `None`
    /// when the bounds cannot be expressed in the column's type, in which
    /// case the caller must not use the index.
    pub(crate) fn index_range(
        &self,
        ty: TypeId,
        prop: &str,
        lo: Bound<&Value>,
        hi: Bound<&Value>,
        cap: usize,
    ) -> Result<Option<Vec<ElementId>>> {
        let enc = |b: Bound<&Value>| -> Option<Bound<Vec<u8>>> {
            Some(match b {
                Bound::Included(v) => Bound::Included(self.lookup_key(ty, prop, v)?),
                Bound::Excluded(v) => Bound::Excluded(self.lookup_key(ty, prop, v)?),
                Bound::Unbounded => Bound::Unbounded,
            })
        };
        let (Some(lo), Some(hi)) = (enc(lo), enc(hi)) else {
            return Ok(None);
        };
        if let (Bound::Included(a) | Bound::Excluded(a), Bound::Included(b) | Bound::Excluded(b)) =
            (&lo, &hi)
            && a > b
        {
            return Ok(Some(Vec::new()));
        }
        // The property name followed by NUL sorts after every key of `prop`.
        let after = format!("{prop}\0");
        let start: Bound<(u32, &str, &[u8], u64)> = match &lo {
            Bound::Included(k) => Bound::Included((ty, prop, k.as_slice(), 0)),
            Bound::Excluded(k) => Bound::Excluded((ty, prop, k.as_slice(), u64::MAX)),
            Bound::Unbounded => Bound::Included((ty, prop, &[], 0)),
        };
        let end: Bound<(u32, &str, &[u8], u64)> = match &hi {
            Bound::Included(k) => Bound::Included((ty, prop, k.as_slice(), u64::MAX)),
            Bound::Excluded(k) => Bound::Excluded((ty, prop, k.as_slice(), 0)),
            Bound::Unbounded => Bound::Excluded((ty, after.as_str(), &[], 0)),
        };
        with_table!(self, props, PROP_INDEX, |t| {
            let mut out = Vec::new();
            for entry in t.range::<(u32, &str, &[u8], u64)>((start, end))?.take(cap) {
                out.push(entry?.0.value().3);
            }
            out.sort_unstable();
            Ok::<_, Error>(Some(out))
        })
    }

    /// Another element (not `except`) already holding `value` in a column.
    fn index_conflict(
        &self,
        ty: TypeId,
        prop: &str,
        v: &Value,
        except: ElementId,
    ) -> Result<Option<ElementId>> {
        if v.is_null() {
            return Ok(None);
        }
        Ok(self
            .index_eq(ty, prop, v, 2)?
            .into_iter()
            .find(|id| *id != except))
    }

    /// Documents containing any of the terms, ascending by id.
    pub(crate) fn text_candidates(
        &self,
        ty: TypeId,
        prop: &str,
        terms: &[String],
    ) -> Result<Vec<ElementId>> {
        let mut ids = Vec::new();
        for term in terms {
            let term = term.as_str();
            with_table!(self, postings, POSTINGS, |t| {
                for entry in t.range((ty, prop, term, 0)..=(ty, prop, term, u64::MAX))? {
                    ids.push(entry?.0.value().3);
                }
                Ok::<_, Error>(())
            })?;
        }
        ids.sort_unstable();
        ids.dedup();
        Ok(ids)
    }

    fn term_freq(&self, ty: TypeId, prop: &str, term: &str, id: ElementId) -> Result<Option<u32>> {
        with_table!(self, postings, POSTINGS, |t| Ok(t
            .get((ty, prop, term, id))?
            .map(|g| g.value())))
    }

    /// `true` when the document contains any of the terms.
    pub(crate) fn text_matches(
        &self,
        ty: TypeId,
        prop: &str,
        id: ElementId,
        terms: &[String],
    ) -> Result<bool> {
        for term in terms {
            if self.term_freq(ty, prop, term, id)?.is_some() {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Okapi BM25 score of a document (0 when no term matches).
    pub(crate) fn text_score(
        &self,
        ty: TypeId,
        prop: &str,
        id: ElementId,
        terms: &[String],
    ) -> Result<f64> {
        let len = with_table!(self, doclen, DOC_LEN, |t| t
            .get((ty, prop, id))?
            .map(|g| g.value()));
        let Some(len) = len else {
            return Ok(0.0);
        };
        let (docs, total) = with_table!(self, textstats, TEXT_STATS, |t| t
            .get((ty, prop))?
            .map_or((0, 0), |g| g.value()));
        let mut score = 0.0;
        for term in terms {
            let Some(tf) = self.term_freq(ty, prop, term, id)? else {
                continue;
            };
            let df = with_table!(self, df, DOC_FREQ, |t| t
                .get((ty, prop, term.as_str()))?
                .map_or(0, |g| g.value()));
            score += bm25(tf, df, docs, total, len);
        }
        Ok(score)
    }

    /// The committed HNSW index for `(ty, prop)`, unless this transaction
    /// changed it (callers then fall back to an exact scan).
    pub(crate) fn vector_index(&self, ty: TypeId, prop: &str) -> Option<&Hnsw> {
        let key = (ty, prop.to_owned());
        if self.dirty_vectors.contains(&key) {
            return None;
        }
        self.vectors.get(&key)
    }

    /// Expands node/edge references into maps, recursively.
    pub(crate) fn materialize(&self, v: Value) -> Result<Value> {
        Ok(match v {
            Value::Node(id) | Value::Edge(id) => {
                let mut map = BTreeMap::new();
                map.insert("_id".to_owned(), Value::Int(id as i64));
                let Some(e) = self.element(id)? else {
                    return Ok(Value::Map(map));
                };
                let Some(def) = self.schema.get(e.ty) else {
                    return Ok(Value::Map(map));
                };
                map.insert("_label".to_owned(), Value::String(def.name.clone()));
                if let Some((from, to)) = e.ends {
                    map.insert("_from".to_owned(), Value::Int(from as i64));
                    map.insert("_to".to_owned(), Value::Int(to as i64));
                }
                for (p, value) in def.props.iter().zip(e.props.iter()) {
                    map.insert(p.name.clone(), value.clone());
                }
                Value::Map(map)
            }
            Value::List(items) => Value::List(
                items
                    .into_iter()
                    .map(|i| self.materialize(i))
                    .collect::<Result<_>>()?,
            ),
            Value::Map(map) => Value::Map(
                map.into_iter()
                    .map(|(k, v)| Ok((k, self.materialize(v)?)))
                    .collect::<Result<_>>()?,
            ),
            other => other,
        })
    }

    // ── writes ────────────────────────────────────────────────────────────

    fn type_def(&self, ty: TypeId) -> Result<TypeDef> {
        self.schema
            .get(ty)
            .cloned()
            .ok_or_else(|| Error::Corrupt(format!("unknown type id {ty}")))
    }

    fn schema_mut(&mut self) -> &mut Schema {
        self.meta_changed = true;
        Arc::make_mut(&mut self.schema)
    }

    fn load(&self, id: ElementId) -> Result<Elem> {
        self.element(id)?
            .ok_or_else(|| Error::NotFound(format!("element {id}")))
    }

    fn store(&mut self, id: ElementId, e: &Elem) -> Result<()> {
        let bytes = encode_props(&e.props)?;
        match e.ends {
            None => write_table!(self, NODES, |t| t.insert(id, (e.ty, bytes.as_slice()))?),
            Some((from, to)) => {
                write_table!(self, EDGES, |t| t
                    .insert(id, (e.ty, from, to, bytes.as_slice()))?)
            }
        };
        self.cache.borrow_mut().insert(id, e.clone());
        Ok(())
    }

    fn add_count(&mut self, ty: TypeId, delta: i64) -> Result<()> {
        write_table!(self, COUNTS, |t| {
            let cur = t.get(ty)?.map_or(0, |g| g.value());
            t.insert(ty, cur.saturating_add_signed(delta))?;
        });
        Ok(())
    }

    fn check_unique(&self, def: &TypeDef, id: ElementId, props: &[Value]) -> Result<()> {
        for (p, v) in def.props.iter().zip(props) {
            if p.unique
                && let Some(other) = self.index_conflict(def.id, &p.name, v, id)?
            {
                return Err(Error::constraint(format!(
                    "{}.{} must be UNIQUE, but {} is already used by element {other}",
                    def.name,
                    p.name,
                    crate::schema::literal(v)
                )));
            }
        }
        Ok(())
    }

    fn text_add(&mut self, ty: TypeId, prop: &str, id: ElementId, text: &str) -> Result<()> {
        let tokens = tokenize(text);
        let len = u32::try_from(tokens.len()).unwrap_or(u32::MAX);
        let mut tf: BTreeMap<String, u32> = BTreeMap::new();
        for token in tokens {
            *tf.entry(token).or_default() += 1;
        }
        write_table!(self, POSTINGS, |t| {
            for (term, n) in &tf {
                t.insert((ty, prop, term.as_str(), id), *n)?;
            }
        });
        write_table!(self, DOC_FREQ, |t| {
            for term in tf.keys() {
                let cur = t.get((ty, prop, term.as_str()))?.map_or(0, |g| g.value());
                t.insert((ty, prop, term.as_str()), cur + 1)?;
            }
        });
        write_table!(self, DOC_LEN, |t| t.insert((ty, prop, id), len)?);
        write_table!(self, TEXT_STATS, |t| {
            let (docs, total) = t.get((ty, prop))?.map_or((0, 0), |g| g.value());
            t.insert((ty, prop), (docs + 1, total + u64::from(len)))?;
        });
        Ok(())
    }

    fn text_remove(&mut self, ty: TypeId, prop: &str, id: ElementId, text: &str) -> Result<()> {
        let len = write_table_get!(self, DOC_LEN, |t| t
            .remove((ty, prop, id))?
            .map(|g| g.value()));
        let Some(len) = len else {
            return Ok(());
        };
        let terms: BTreeSet<String> = tokenize(text).into_iter().collect();
        write_table!(self, POSTINGS, |t| {
            for term in &terms {
                t.remove((ty, prop, term.as_str(), id))?;
            }
        });
        write_table!(self, DOC_FREQ, |t| {
            for term in &terms {
                let cur = t.get((ty, prop, term.as_str()))?.map_or(0, |g| g.value());
                if cur <= 1 {
                    t.remove((ty, prop, term.as_str()))?;
                } else {
                    t.insert((ty, prop, term.as_str()), cur - 1)?;
                }
            }
        });
        write_table!(self, TEXT_STATS, |t| {
            let (docs, total) = t.get((ty, prop))?.map_or((0, 0), |g| g.value());
            if docs <= 1 {
                t.remove((ty, prop))?;
            } else {
                t.insert((ty, prop), (docs - 1, total.saturating_sub(u64::from(len))))?;
            }
        });
        Ok(())
    }

    /// Adds `value` to every index of `def`. `vectors: false` leaves the HNSW
    /// alone (used when only B-tree/text flags change).
    fn index_add(
        &mut self,
        ty: TypeId,
        def: &PropDef,
        id: ElementId,
        value: &Value,
        vectors: bool,
    ) -> Result<()> {
        if def.has_btree()
            && let Some(key) = index_key(value)
        {
            write_table!(self, PROP_INDEX, |t| t
                .insert((ty, def.name.as_str(), key.as_slice(), id), ())?);
        }
        if def.searchable
            && let Value::String(s) = value
        {
            self.text_add(ty, &def.name, id, s)?;
        }
        if vectors && let (PropType::Vector(dim), Value::Vector(v)) = (&def.ty, value) {
            let key = (ty, def.name.clone());
            self.dirty_vectors.insert(key.clone());
            self.vec_ops.push(VecOp::Put {
                key,
                id,
                vector: v.clone(),
                dim: *dim,
                metric: def.metric,
            });
        }
        Ok(())
    }

    fn index_remove(
        &mut self,
        ty: TypeId,
        def: &PropDef,
        id: ElementId,
        value: &Value,
        vectors: bool,
    ) -> Result<()> {
        if def.has_btree()
            && let Some(key) = index_key(value)
        {
            write_table!(self, PROP_INDEX, |t| t.remove((
                ty,
                def.name.as_str(),
                key.as_slice(),
                id
            ))?);
        }
        if def.searchable
            && let Value::String(s) = value
        {
            self.text_remove(ty, &def.name, id, s)?;
        }
        if vectors && matches!(def.ty, PropType::Vector(_)) && !value.is_null() {
            let key = (ty, def.name.clone());
            self.dirty_vectors.insert(key.clone());
            self.vec_ops.push(VecOp::Remove { key, id });
        }
        Ok(())
    }

    fn check_new_id(&self, id: ElementId) -> Result<()> {
        if self.element(id)?.is_some() {
            return Err(Error::Corrupt(format!("element id {id} already exists")));
        }
        Ok(())
    }

    /// Applies one primitive change. On error the caller drops the
    /// transaction, so partial effects never become visible.
    pub(crate) fn apply(&mut self, op: &Op) -> Result<()> {
        match op {
            Op::CreateType(def) => {
                if self.schema.by_name(&def.name).is_some() || self.schema.get(def.id).is_some() {
                    return Err(Error::schema(format!("type '{}' already exists", def.name)));
                }
                self.schema_mut().insert(def.clone());
            }
            Op::DropType(ty) => {
                let def = self.type_def(*ty)?;
                let count = self.member_count(*ty)?;
                if count > 0 {
                    return Err(Error::schema(format!(
                        "cannot drop {}: it still has {count} elements (use CASCADE to delete them)",
                        def.name
                    )));
                }
                for p in &def.props {
                    if matches!(p.ty, PropType::Vector(_)) {
                        let key = (*ty, p.name.clone());
                        self.dirty_vectors.insert(key.clone());
                        self.vec_ops.push(VecOp::Drop { key });
                    }
                }
                write_table!(self, COUNTS, |t| t.remove(*ty)?);
                self.schema_mut().remove(*ty);
            }
            Op::AddProp { ty, def, fill } => {
                let tdef = self.type_def(*ty)?;
                if tdef.slot(&def.name).is_some() {
                    return Err(Error::schema(format!(
                        "{} already has a property '{}'",
                        tdef.name, def.name
                    )));
                }
                let ids = self.member_ids(*ty)?;
                if def.unique && !fill.is_null() && ids.len() > 1 {
                    return Err(Error::constraint(format!(
                        "cannot add UNIQUE property {}.{} with a non-NULL default to existing elements",
                        tdef.name, def.name
                    )));
                }
                for id in ids {
                    let mut e = self.load(id)?;
                    Rc::make_mut(&mut e.props).push(fill.clone());
                    self.store(id, &e)?;
                    self.index_add(*ty, def, id, fill, true)?;
                }
                if let Some(t) = self.schema_mut().get_mut(*ty) {
                    t.props.push(def.clone());
                }
            }
            Op::DropProp { ty, name } => {
                let tdef = self.type_def(*ty)?;
                let (pos, def) = tdef.prop(name).map(|(i, d)| (i, d.clone()))?;
                for id in self.member_ids(*ty)? {
                    let mut e = self.load(id)?;
                    let old = Rc::make_mut(&mut e.props).remove(pos);
                    self.index_remove(*ty, &def, id, &old, false)?;
                    self.store(id, &e)?;
                }
                if matches!(def.ty, PropType::Vector(_)) {
                    let key = (*ty, name.clone());
                    self.dirty_vectors.insert(key.clone());
                    self.vec_ops.push(VecOp::Drop { key });
                }
                if let Some(t) = self.schema_mut().get_mut(*ty) {
                    t.props.remove(pos);
                }
            }
            Op::SetFlags {
                ty,
                name,
                unique,
                indexed,
                searchable,
            } => {
                let tdef = self.type_def(*ty)?;
                let (pos, old) = tdef.prop(name).map(|(i, d)| (i, d.clone()))?;
                let mut new = old.clone();
                new.unique = *unique;
                new.indexed = *indexed;
                new.searchable = *searchable;
                new.validate(&tdef.name)?;
                let mut values = Vec::new();
                for id in self.member_ids(*ty)? {
                    values.push((id, self.load(id)?.props[pos].clone()));
                }
                if new.unique {
                    let mut seen = BTreeSet::new();
                    if let Some((_, dup)) = values
                        .iter()
                        .find(|(_, v)| !v.is_null() && !seen.insert(OrdValue(v.clone())))
                    {
                        return Err(Error::constraint(format!(
                            "cannot make {}.{name} UNIQUE: value {} appears more than once",
                            tdef.name,
                            crate::schema::literal(dup)
                        )));
                    }
                }
                for (id, v) in &values {
                    self.index_remove(*ty, &old, *id, v, false)?;
                    self.index_add(*ty, &new, *id, v, false)?;
                }
                if let Some(t) = self.schema_mut().get_mut(*ty) {
                    t.props[pos] = new;
                }
            }
            Op::AddNode { id, ty, props } => {
                self.check_new_id(*id)?;
                let def = self.type_def(*ty)?;
                if def.kind != Kind::Node || def.props.len() != props.len() {
                    return Err(Error::Corrupt(format!(
                        "malformed node insert for {}",
                        def.name
                    )));
                }
                self.check_unique(&def, *id, props)?;
                for (p, v) in def.props.iter().zip(props) {
                    self.index_add(*ty, p, *id, v, true)?;
                }
                let elem = Elem {
                    ty: *ty,
                    props: Rc::new(props.clone()),
                    ends: None,
                };
                self.store(*id, &elem)?;
                write_table!(self, MEMBERS, |t| t.insert((*ty, *id), ())?);
                self.add_count(*ty, 1)?;
                self.next_id = self.next_id.max(id + 1);
            }
            Op::AddEdge {
                id,
                ty,
                from,
                to,
                props,
            } => {
                self.check_new_id(*id)?;
                let def = self.type_def(*ty)?;
                if def.kind != Kind::Edge || def.props.len() != props.len() {
                    return Err(Error::Corrupt(format!(
                        "malformed edge insert for {}",
                        def.name
                    )));
                }
                for end in [from, to] {
                    if self.element(*end)?.is_none_or(|e| e.ends.is_some()) {
                        return Err(Error::NotFound(format!(
                            "edge endpoint {end} is not a node"
                        )));
                    }
                }
                self.check_unique(&def, *id, props)?;
                for (p, v) in def.props.iter().zip(props) {
                    self.index_add(*ty, p, *id, v, true)?;
                }
                let elem = Elem {
                    ty: *ty,
                    props: Rc::new(props.clone()),
                    ends: Some((*from, *to)),
                };
                self.store(*id, &elem)?;
                write_table!(self, OUT, |t| t.insert((*from, *ty, *id), *to)?);
                write_table!(self, IN, |t| t.insert((*to, *ty, *id), *from)?);
                write_table!(self, MEMBERS, |t| t.insert((*ty, *id), ())?);
                self.add_count(*ty, 1)?;
                self.next_id = self.next_id.max(id + 1);
            }
            Op::SetProp { id, slot, value } => {
                let mut e = self.load(*id)?;
                let def = self.type_def(e.ty)?;
                let p = def
                    .props
                    .get(*slot)
                    .ok_or_else(|| Error::Corrupt("bad property slot".into()))?
                    .clone();
                if p.unique
                    && let Some(other) = self.index_conflict(e.ty, &p.name, value, *id)?
                {
                    return Err(Error::constraint(format!(
                        "{}.{} must be UNIQUE, but {} is already used by element {other}",
                        def.name,
                        p.name,
                        crate::schema::literal(value)
                    )));
                }
                let old = std::mem::replace(&mut Rc::make_mut(&mut e.props)[*slot], value.clone());
                self.index_remove(e.ty, &p, *id, &old, true)?;
                self.index_add(e.ty, &p, *id, value, true)?;
                self.store(*id, &e)?;
            }
            Op::DelNode(id) => {
                let e = self.load(*id)?;
                if e.ends.is_some() {
                    return Err(Error::Corrupt(format!(
                        "element {id} is an edge, not a node"
                    )));
                }
                if self.degree(*id, true, true)? > 0 {
                    return Err(Error::Corrupt(format!(
                        "node {id} deleted while it still has edges"
                    )));
                }
                let def = self.type_def(e.ty)?;
                for (p, v) in def.props.iter().zip(e.props.iter()) {
                    self.index_remove(e.ty, p, *id, v, true)?;
                }
                write_table!(self, NODES, |t| t.remove(*id)?);
                write_table!(self, MEMBERS, |t| t.remove((e.ty, *id))?);
                self.add_count(e.ty, -1)?;
                self.cache.borrow_mut().remove(id);
            }
            Op::DelEdge(id) => {
                let e = self.load(*id)?;
                let Some((from, to)) = e.ends else {
                    return Err(Error::Corrupt(format!(
                        "element {id} is a node, not an edge"
                    )));
                };
                if let Some(def) = self.schema.get(e.ty).cloned() {
                    for (p, v) in def.props.iter().zip(e.props.iter()) {
                        self.index_remove(e.ty, p, *id, v, true)?;
                    }
                }
                write_table!(self, OUT, |t| t.remove((from, e.ty, *id))?);
                write_table!(self, IN, |t| t.remove((to, e.ty, *id))?);
                write_table!(self, EDGES, |t| t.remove(*id)?);
                write_table!(self, MEMBERS, |t| t.remove((e.ty, *id))?);
                self.add_count(e.ty, -1)?;
                self.cache.borrow_mut().remove(id);
            }
            Op::PutQuery(q) => {
                self.meta_changed = true;
                Arc::make_mut(&mut self.queries).insert(q.name.clone(), q.clone());
            }
            Op::DropQuery(name) => {
                self.meta_changed = true;
                if Arc::make_mut(&mut self.queries).remove(name).is_none() {
                    return Err(Error::NotFound(format!("query '{name}'")));
                }
            }
        }
        Ok(())
    }
}

/// Streams the members of a type in chunks. See [`Graph::members`].
#[derive(Debug)]
pub(crate) struct Members<'g> {
    g: &'g Graph,
    ty: TypeId,
    next: ElementId,
    buf: std::vec::IntoIter<ElementId>,
    done: bool,
}

impl Iterator for Members<'_> {
    type Item = Result<ElementId>;

    fn next(&mut self) -> Option<Self::Item> {
        const CHUNK: usize = 1024;
        loop {
            if let Some(id) = self.buf.next() {
                return Some(Ok(id));
            }
            if self.done {
                return None;
            }
            match self.g.member_chunk(self.ty, self.next, CHUNK) {
                Ok(chunk) => {
                    self.done = chunk.len() < CHUNK;
                    match chunk.last() {
                        Some(&last) if last < u64::MAX => self.next = last + 1,
                        _ => self.done = true,
                    }
                    self.buf = chunk.into_iter();
                }
                Err(e) => {
                    self.done = true;
                    return Some(Err(e));
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn index_keys_sort_like_values() {
        let ints = [i64::MIN, -5, -1, 0, 3, i64::MAX];
        let keys: Vec<_> = ints
            .iter()
            .map(|i| index_key(&Value::Int(*i)).unwrap())
            .collect();
        assert!(keys.windows(2).all(|w| w[0] < w[1]));
        let floats = [f64::NEG_INFINITY, -2.5, -0.5, 0.0, 0.25, 1e9];
        let keys: Vec<_> = floats
            .iter()
            .map(|f| index_key(&Value::Float(*f)).unwrap())
            .collect();
        assert!(keys.windows(2).all(|w| w[0] < w[1]));
    }
}
