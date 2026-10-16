//! In-memory graph store and the primitive operations that mutate it.
//!
//! Every change to the database — schema or data — is one [`Op`]. Applying an
//! op validates it, updates every index, and returns the inverse op, which is
//! how transactions roll back. The same ops are what the write-ahead log and
//! snapshots store, so recovery replays exactly the code paths that ran live.

use std::collections::{BTreeMap, BTreeSet};

use rustc_hash::FxHashMap;
use serde::{Deserialize, Serialize};

use crate::error::{Error, Result};
use crate::index::PropIndex;
use crate::index::text::TextIndex;
use crate::index::vector::Hnsw;
use crate::schema::{Kind, PropDef, PropType, Schema, TypeDef, TypeId};
use crate::value::{ElementId, Value};

/// One adjacency entry: an incident edge and the node at its other end.
#[derive(Debug, Clone, Copy)]
pub(crate) struct Adj {
    pub ty: TypeId,
    pub edge: ElementId,
    pub other: ElementId,
}

#[derive(Debug)]
pub(crate) struct NodeRec {
    pub ty: TypeId,
    pub props: Vec<Value>,
    pub out: Vec<Adj>,
    pub inc: Vec<Adj>,
}

#[derive(Debug)]
pub(crate) struct EdgeRec {
    pub ty: TypeId,
    pub from: ElementId,
    pub to: ElementId,
    pub props: Vec<Value>,
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

/// A primitive, invertible change.
#[derive(Debug, Clone, Serialize, Deserialize)]
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
    Counters {
        next_id: ElementId,
        next_type_id: TypeId,
    },
    /// Undo of `DropProp`; never written to the log.
    RestoreProp {
        ty: TypeId,
        pos: usize,
        def: PropDef,
        values: Vec<(ElementId, Value)>,
    },
}

/// All secondary indexes, keyed by type then property name.
#[derive(Debug, Default)]
pub(crate) struct Indexes {
    pub btree: FxHashMap<TypeId, FxHashMap<String, PropIndex>>,
    pub text: FxHashMap<TypeId, FxHashMap<String, TextIndex>>,
    pub vectors: FxHashMap<TypeId, FxHashMap<String, Hnsw>>,
}

impl Indexes {
    fn add(&mut self, ty: TypeId, def: &PropDef, id: ElementId, value: &Value) {
        if def.has_btree() {
            self.btree
                .entry(ty)
                .or_default()
                .entry(def.name.clone())
                .or_default()
                .insert(value, id);
        }
        if def.searchable
            && let Value::String(s) = value
        {
            self.text
                .entry(ty)
                .or_default()
                .entry(def.name.clone())
                .or_default()
                .insert(id, s);
        }
        if let (PropType::Vector(dim), Value::Vector(v)) = (&def.ty, value) {
            self.vectors
                .entry(ty)
                .or_default()
                .entry(def.name.clone())
                .or_insert_with(|| Hnsw::new(*dim as usize, def.metric))
                .insert(id, v);
        }
    }

    fn remove(&mut self, ty: TypeId, def: &PropDef, id: ElementId, value: &Value) {
        if let Some(idx) = self.btree.get_mut(&ty).and_then(|m| m.get_mut(&def.name)) {
            idx.remove(value, id);
        }
        if let (Some(idx), Value::String(s)) = (
            self.text.get_mut(&ty).and_then(|m| m.get_mut(&def.name)),
            value,
        ) {
            idx.remove(id, s);
        }
        if let Some(idx) = self.vectors.get_mut(&ty).and_then(|m| m.get_mut(&def.name)) {
            idx.remove(id);
        }
    }

    fn drop_prop(&mut self, ty: TypeId, name: &str) {
        if let Some(m) = self.btree.get_mut(&ty) {
            m.remove(name);
        }
        if let Some(m) = self.text.get_mut(&ty) {
            m.remove(name);
        }
        if let Some(m) = self.vectors.get_mut(&ty) {
            m.remove(name);
        }
    }

    fn drop_type(&mut self, ty: TypeId) {
        self.btree.remove(&ty);
        self.text.remove(&ty);
        self.vectors.remove(&ty);
    }

    pub(crate) fn btree(&self, ty: TypeId, prop: &str) -> Option<&PropIndex> {
        self.btree.get(&ty)?.get(prop)
    }

    pub(crate) fn text(&self, ty: TypeId, prop: &str) -> Option<&TextIndex> {
        self.text.get(&ty)?.get(prop)
    }

    pub(crate) fn vector(&self, ty: TypeId, prop: &str) -> Option<&Hnsw> {
        self.vectors.get(&ty)?.get(prop)
    }
}

/// The whole database state.
#[derive(Debug, Default)]
pub(crate) struct Graph {
    pub schema: Schema,
    pub nodes: FxHashMap<ElementId, NodeRec>,
    pub edges: FxHashMap<ElementId, EdgeRec>,
    /// Members of each type, ordered by id (= insertion order).
    pub members: FxHashMap<TypeId, BTreeSet<ElementId>>,
    pub idx: Indexes,
    pub queries: BTreeMap<String, StoredQuery>,
    pub next_id: ElementId,
}

impl Graph {
    /// Type and properties of a node or edge.
    pub(crate) fn element(&self, id: ElementId) -> Option<(TypeId, &[Value])> {
        self.nodes
            .get(&id)
            .map(|n| (n.ty, n.props.as_slice()))
            .or_else(|| self.edges.get(&id).map(|e| (e.ty, e.props.as_slice())))
    }

    pub(crate) fn type_of(&self, id: ElementId) -> Option<TypeId> {
        self.element(id).map(|(t, _)| t)
    }

    pub(crate) fn member_count(&self, ty: TypeId) -> usize {
        self.members.get(&ty).map_or(0, BTreeSet::len)
    }

    pub(crate) fn members(&self, ty: TypeId) -> impl Iterator<Item = ElementId> + '_ {
        self.members.get(&ty).into_iter().flatten().copied()
    }

    fn props_mut(&mut self, id: ElementId) -> Option<(TypeId, &mut Vec<Value>)> {
        if let Some(n) = self.nodes.get_mut(&id) {
            return Some((n.ty, &mut n.props));
        }
        self.edges.get_mut(&id).map(|e| (e.ty, &mut e.props))
    }

    fn type_def(&self, ty: TypeId) -> Result<&TypeDef> {
        self.schema
            .get(ty)
            .ok_or_else(|| Error::Corrupt(format!("unknown type id {ty}")))
    }

    fn check_unique(&self, def: &TypeDef, id: ElementId, props: &[Value]) -> Result<()> {
        for (p, v) in def.props.iter().zip(props) {
            if !p.unique {
                continue;
            }
            if let Some(other) = self
                .idx
                .btree(def.id, &p.name)
                .and_then(|i| i.conflict(v, id))
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

    /// Expands node/edge references into maps, recursively.
    pub(crate) fn materialize(&self, v: Value) -> Value {
        match v {
            Value::Node(id) | Value::Edge(id) => {
                let mut map = BTreeMap::new();
                map.insert("_id".to_owned(), Value::Int(id as i64));
                let Some((ty, props)) = self.element(id) else {
                    return Value::Map(map);
                };
                let Some(def) = self.schema.get(ty) else {
                    return Value::Map(map);
                };
                map.insert("_label".to_owned(), Value::String(def.name.clone()));
                if let Some(e) = self.edges.get(&id) {
                    map.insert("_from".to_owned(), Value::Int(e.from as i64));
                    map.insert("_to".to_owned(), Value::Int(e.to as i64));
                }
                for (p, value) in def.props.iter().zip(props) {
                    map.insert(p.name.clone(), value.clone());
                }
                Value::Map(map)
            }
            Value::List(items) => {
                Value::List(items.into_iter().map(|i| self.materialize(i)).collect())
            }
            Value::Map(map) => Value::Map(
                map.into_iter()
                    .map(|(k, v)| (k, self.materialize(v)))
                    .collect(),
            ),
            other => other,
        }
    }

    /// Applies `op`, returning the op that undoes it. On error nothing changed.
    pub(crate) fn apply(&mut self, op: &Op) -> Result<Op> {
        match op {
            Op::CreateType(def) => {
                if self.schema.by_name(&def.name).is_some() || self.schema.get(def.id).is_some() {
                    return Err(Error::schema(format!("type '{}' already exists", def.name)));
                }
                self.schema.insert(def.clone());
                self.members.entry(def.id).or_default();
                Ok(Op::DropType(def.id))
            }
            Op::DropType(ty) => {
                let count = self.member_count(*ty);
                let def = self.type_def(*ty)?;
                if count > 0 {
                    return Err(Error::schema(format!(
                        "cannot drop {}: it still has {count} elements (use CASCADE to delete them)",
                        def.name
                    )));
                }
                let def = self
                    .schema
                    .remove(*ty)
                    .ok_or_else(|| Error::Corrupt("type vanished".into()))?;
                self.members.remove(ty);
                self.idx.drop_type(*ty);
                Ok(Op::CreateType(def))
            }
            Op::AddProp { ty, def, fill } => {
                let tdef = self.type_def(*ty)?;
                if tdef.slot(&def.name).is_some() {
                    return Err(Error::schema(format!(
                        "{} already has a property '{}'",
                        tdef.name, def.name
                    )));
                }
                if def.unique && !fill.is_null() && self.member_count(*ty) > 1 {
                    return Err(Error::constraint(format!(
                        "cannot add UNIQUE property {}.{} with a non-NULL default to existing elements",
                        tdef.name, def.name
                    )));
                }
                let members: Vec<ElementId> = self.members(*ty).collect();
                for id in &members {
                    if let Some((_, props)) = self.props_mut(*id) {
                        props.push(fill.clone());
                    }
                }
                for id in members {
                    self.idx.add(*ty, def, id, fill);
                }
                if let Some(t) = self.schema.get_mut(*ty) {
                    t.props.push(def.clone());
                }
                Ok(Op::DropProp {
                    ty: *ty,
                    name: def.name.clone(),
                })
            }
            Op::DropProp { ty, name } => {
                let (pos, def) = {
                    let (pos, def) = self.type_def(*ty)?.prop(name)?;
                    (pos, def.clone())
                };
                let members: Vec<ElementId> = self.members(*ty).collect();
                let mut values = Vec::with_capacity(members.len());
                for id in members {
                    if let Some((_, props)) = self.props_mut(id) {
                        values.push((id, props.remove(pos)));
                    }
                }
                self.idx.drop_prop(*ty, name);
                if let Some(t) = self.schema.get_mut(*ty) {
                    t.props.remove(pos);
                }
                Ok(Op::RestoreProp {
                    ty: *ty,
                    pos,
                    def,
                    values,
                })
            }
            Op::RestoreProp {
                ty,
                pos,
                def,
                values,
            } => {
                for (id, v) in values {
                    if let Some((_, props)) = self.props_mut(*id) {
                        props.insert(*pos, v.clone());
                    }
                    self.idx.add(*ty, def, *id, v);
                }
                if let Some(t) = self.schema.get_mut(*ty) {
                    t.props.insert(*pos, def.clone());
                }
                Ok(Op::DropProp {
                    ty: *ty,
                    name: def.name.clone(),
                })
            }
            Op::SetFlags {
                ty,
                name,
                unique,
                indexed,
                searchable,
            } => {
                let tdef = self.type_def(*ty)?;
                let (pos, old) = tdef.prop(name)?;
                let old = old.clone();
                let mut new = old.clone();
                new.unique = *unique;
                new.indexed = *indexed;
                new.searchable = *searchable;
                new.validate(&tdef.name)?;
                let owner = tdef.name.clone();
                let members: Vec<(ElementId, Value)> = self
                    .members(*ty)
                    .filter_map(|id| self.element(id).map(|(_, p)| (id, p[pos].clone())))
                    .collect();
                if new.unique {
                    let mut probe = PropIndex::default();
                    for (id, v) in &members {
                        probe.insert(v, *id);
                    }
                    if let Some(dup) = probe.first_duplicate() {
                        return Err(Error::constraint(format!(
                            "cannot make {owner}.{name} UNIQUE: value {} appears more than once",
                            crate::schema::literal(dup)
                        )));
                    }
                }
                self.idx.drop_prop(*ty, name);
                for (id, v) in &members {
                    self.idx.add(*ty, &new, *id, v);
                }
                if let Some(t) = self.schema.get_mut(*ty) {
                    t.props[pos] = new;
                }
                Ok(Op::SetFlags {
                    ty: *ty,
                    name: name.clone(),
                    unique: old.unique,
                    indexed: old.indexed,
                    searchable: old.searchable,
                })
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
                self.check_unique(def, *id, props)?;
                let Self { schema, idx, .. } = self;
                if let Some(def) = schema.get(*ty) {
                    for (p, v) in def.props.iter().zip(props) {
                        idx.add(*ty, p, *id, v);
                    }
                }
                self.nodes.insert(
                    *id,
                    NodeRec {
                        ty: *ty,
                        props: props.clone(),
                        out: Vec::new(),
                        inc: Vec::new(),
                    },
                );
                self.members.entry(*ty).or_default().insert(*id);
                self.next_id = self.next_id.max(id + 1);
                Ok(Op::DelNode(*id))
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
                if !self.nodes.contains_key(from) || !self.nodes.contains_key(to) {
                    return Err(Error::NotFound(format!(
                        "edge endpoint {from} or {to} does not exist"
                    )));
                }
                self.check_unique(def, *id, props)?;
                let Self { schema, idx, .. } = self;
                if let Some(def) = schema.get(*ty) {
                    for (p, v) in def.props.iter().zip(props) {
                        idx.add(*ty, p, *id, v);
                    }
                }
                if let Some(n) = self.nodes.get_mut(from) {
                    n.out.push(Adj {
                        ty: *ty,
                        edge: *id,
                        other: *to,
                    });
                }
                if let Some(n) = self.nodes.get_mut(to) {
                    n.inc.push(Adj {
                        ty: *ty,
                        edge: *id,
                        other: *from,
                    });
                }
                self.edges.insert(
                    *id,
                    EdgeRec {
                        ty: *ty,
                        from: *from,
                        to: *to,
                        props: props.clone(),
                    },
                );
                self.members.entry(*ty).or_default().insert(*id);
                self.next_id = self.next_id.max(id + 1);
                Ok(Op::DelEdge(*id))
            }
            Op::SetProp { id, slot, value } => {
                let (ty, props) = self
                    .element(*id)
                    .ok_or_else(|| Error::NotFound(format!("element {id}")))?;
                let def = self.type_def(ty)?;
                let p = def
                    .props
                    .get(*slot)
                    .ok_or_else(|| Error::Corrupt("bad property slot".into()))?
                    .clone();
                let old = props[*slot].clone();
                if p.unique
                    && let Some(other) = self
                        .idx
                        .btree(ty, &p.name)
                        .and_then(|i| i.conflict(value, *id))
                {
                    return Err(Error::constraint(format!(
                        "{}.{} must be UNIQUE, but {} is already used by element {other}",
                        def.name,
                        p.name,
                        crate::schema::literal(value)
                    )));
                }
                self.idx.remove(ty, &p, *id, &old);
                self.idx.add(ty, &p, *id, value);
                if let Some((_, props)) = self.props_mut(*id) {
                    props[*slot] = value.clone();
                }
                Ok(Op::SetProp {
                    id: *id,
                    slot: *slot,
                    value: old,
                })
            }
            Op::DelNode(id) => {
                let node = self
                    .nodes
                    .get(id)
                    .ok_or_else(|| Error::NotFound(format!("node {id}")))?;
                if !node.out.is_empty() || !node.inc.is_empty() {
                    return Err(Error::Corrupt(format!(
                        "node {id} deleted while it still has edges"
                    )));
                }
                let def = self.type_def(node.ty)?.clone();
                let node = self
                    .nodes
                    .remove(id)
                    .ok_or_else(|| Error::NotFound(format!("node {id}")))?;
                for (p, v) in def.props.iter().zip(&node.props) {
                    self.idx.remove(node.ty, p, *id, v);
                }
                if let Some(m) = self.members.get_mut(&node.ty) {
                    m.remove(id);
                }
                Ok(Op::AddNode {
                    id: *id,
                    ty: node.ty,
                    props: node.props,
                })
            }
            Op::DelEdge(id) => {
                let edge = self
                    .edges
                    .remove(id)
                    .ok_or_else(|| Error::NotFound(format!("edge {id}")))?;
                if let Some(n) = self.nodes.get_mut(&edge.from)
                    && let Some(i) = n.out.iter().position(|a| a.edge == *id)
                {
                    n.out.swap_remove(i);
                }
                if let Some(n) = self.nodes.get_mut(&edge.to)
                    && let Some(i) = n.inc.iter().position(|a| a.edge == *id)
                {
                    n.inc.swap_remove(i);
                }
                if let Some(def) = self.schema.get(edge.ty).cloned() {
                    for (p, v) in def.props.iter().zip(&edge.props) {
                        self.idx.remove(edge.ty, p, *id, v);
                    }
                }
                if let Some(m) = self.members.get_mut(&edge.ty) {
                    m.remove(id);
                }
                Ok(Op::AddEdge {
                    id: *id,
                    ty: edge.ty,
                    from: edge.from,
                    to: edge.to,
                    props: edge.props,
                })
            }
            Op::PutQuery(q) => Ok(match self.queries.insert(q.name.clone(), q.clone()) {
                Some(old) => Op::PutQuery(old),
                None => Op::DropQuery(q.name.clone()),
            }),
            Op::DropQuery(name) => {
                let old = self
                    .queries
                    .remove(name)
                    .ok_or_else(|| Error::NotFound(format!("query '{name}'")))?;
                Ok(Op::PutQuery(old))
            }
            Op::Counters {
                next_id,
                next_type_id,
            } => {
                self.next_id = self.next_id.max(*next_id);
                self.schema.bump_type_id(*next_type_id);
                Ok(Op::Counters {
                    next_id: *next_id,
                    next_type_id: *next_type_id,
                })
            }
        }
    }

    fn check_new_id(&self, id: ElementId) -> Result<()> {
        if self.nodes.contains_key(&id) || self.edges.contains_key(&id) {
            return Err(Error::Corrupt(format!("element id {id} already exists")));
        }
        Ok(())
    }

    /// Ops that rebuild this exact state from scratch (used for snapshots).
    pub(crate) fn snapshot_ops(&self) -> impl Iterator<Item = Op> + '_ {
        let mut types: Vec<&TypeDef> = self.schema.types().collect();
        types.sort_by_key(|t| (t.kind == Kind::Edge, t.id));
        let mut node_ids: Vec<ElementId> = self.nodes.keys().copied().collect();
        node_ids.sort_unstable();
        let mut edge_ids: Vec<ElementId> = self.edges.keys().copied().collect();
        edge_ids.sort_unstable();
        types
            .into_iter()
            .map(|t| Op::CreateType(t.clone()))
            .chain(node_ids.into_iter().filter_map(|id| {
                self.nodes.get(&id).map(|n| Op::AddNode {
                    id,
                    ty: n.ty,
                    props: n.props.clone(),
                })
            }))
            .chain(edge_ids.into_iter().filter_map(|id| {
                self.edges.get(&id).map(|e| Op::AddEdge {
                    id,
                    ty: e.ty,
                    from: e.from,
                    to: e.to,
                    props: e.props.clone(),
                })
            }))
            .chain(self.queries.values().map(|q| Op::PutQuery(q.clone())))
            .chain(std::iter::once(Op::Counters {
                next_id: self.next_id,
                next_type_id: self.schema.next_type_id(),
            }))
    }
}
