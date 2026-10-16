//! `INSERT`, `UPDATE` and `DELETE`.
//!
//! Every write runs in three phases: *read* (match and evaluate everything
//! against the state before the statement), *apply* (emit primitive ops into
//! the transaction) and *return* (evaluate `RETURNING`). This gives the same
//! "all expressions see the old snapshot" semantics SQL has.

use std::collections::{BTreeMap, BTreeSet};

use rustc_hash::FxHashMap;

use crate::error::{Error, Result};
use crate::graph::{Graph, Op};
use crate::rql::ast::{Delete, Dir, Expr, Insert, InsertRows, SelectItem, Update};
use crate::schema::{DefaultValue, Kind, TypeDef, TypeId};
use crate::value::{ElementId, Value};

use super::compile::{CExpr, Compiler, Params, Scope, VarKind};
use super::eval::{Env, eval};
use super::matcher::{bind_where, plan};
use super::select::project;
use super::{QueryResult, Txn};

/// Checks and orders a set of property values for a new element, applying
/// defaults and NOT NULL constraints.
pub(crate) fn build_props(def: &TypeDef, given: Vec<(String, Value)>, now: i64) -> Result<Vec<Value>> {
    let mut slots: Vec<Option<Value>> = vec![None; def.props.len()];
    for (name, v) in given {
        let (slot, p) = def.prop(&name)?;
        if slots[slot].is_some() {
            return Err(Error::query(format!("{}.{name} is given more than once", def.name)));
        }
        slots[slot] = Some(p.ty.coerce(v, &format!("{}.{name}", def.name))?);
    }
    def.props
        .iter()
        .zip(slots)
        .map(|(p, v)| {
            let v = match (v, &p.default) {
                (Some(v), _) => v,
                (None, Some(DefaultValue::Value(d))) => p.ty.coerce(d.clone(), &p.name)?,
                (None, Some(DefaultValue::Now)) => Value::Int(now),
                (None, None) => Value::Null,
            };
            if p.not_null && v.is_null() {
                return Err(Error::constraint(format!("{}.{} is NOT NULL, but no value was given", def.name, p.name)));
            }
            Ok(v)
        })
        .collect()
}

/// Evaluates `RETURNING` for each row and materializes nodes/edges.
fn returning(
    g: &Graph,
    params: &Params,
    scope: &Scope,
    items: &[SelectItem],
    rows: &[Vec<Value>],
    now: i64,
) -> Result<QueryResult> {
    if items.is_empty() {
        return Ok(QueryResult::default());
    }
    let proj = project(g, params, scope, items, None)?;
    let rows = rows
        .iter()
        .map(|row| {
            let env = Env::new(g, row, now);
            proj.exprs.iter().map(|e| eval(e, &env).map(|v| g.materialize(v))).collect::<Result<Vec<_>>>()
        })
        .collect::<Result<_>>()?;
    Ok(QueryResult { columns: proj.names, rows, ..QueryResult::default() })
}

fn check_endpoints(def: &TypeDef, g: &Graph, new_types: &FxHashMap<ElementId, TypeId>, from: ElementId, to: ElementId) -> Result<()> {
    let name_of = |id: ElementId| {
        g.type_of(id)
            .or_else(|| new_types.get(&id).copied())
            .and_then(|t| g.schema.get(t))
            .map(|d| d.name.clone())
            .unwrap_or_default()
    };
    let (f, t) = (name_of(from), name_of(to));
    if !def.from.contains(&f) || !def.to.contains(&t) {
        return Err(Error::constraint(format!(
            "{} edges go from {} to {}, but this one would go from {f} to {t}",
            def.name,
            def.from.join(" | "),
            def.to.join(" | ")
        )));
    }
    Ok(())
}

enum Create {
    Node { var: usize, ty: TypeId, props: Vec<(String, CExpr)> },
    Edge { var: usize, ty: TypeId, from: usize, to: usize, props: Vec<(String, CExpr)> },
}

pub(crate) fn insert(txn: &mut Txn<'_>, params: &Params, ins: &Insert, now: i64) -> Result<QueryResult> {
    let g: &Graph = txn.g;
    let m = bind_where(g, params, &ins.from, ins.where_.as_ref())?;
    let base_width = m.scope.vars.len();
    let mut scope = m.scope.clone();
    let mut creates = Vec::new();
    let mut c = Compiler::new(g, params, &m.scope);
    let compile_props = |c: &mut Compiler<'_>, props: &[(String, Expr)]| {
        props.iter().map(|(k, e)| Ok((k.clone(), c.compile(e)?))).collect::<Result<Vec<_>>>()
    };
    for p in &ins.create {
        let mut vars = Vec::with_capacity(p.nodes.len());
        for n in &p.nodes {
            if let Some(v) = n.var.as_deref().and_then(|v| scope.lookup(v)) {
                let VarKind::Node(types) = &scope.vars[v].kind else {
                    return Err(Error::query(format!("'{}' is an edge, not a node", scope.vars[v].name)));
                };
                if let Some(l) = &n.label {
                    let ty = g.schema.resolve(l, Kind::Node)?.id;
                    if !types.contains(&ty) {
                        return Err(Error::query(format!("'{}' is already bound to a different type", scope.vars[v].name)));
                    }
                }
                if !n.props.is_empty() {
                    return Err(Error::query(format!(
                        "'{}' already exists; use UPDATE to change its properties",
                        scope.vars[v].name
                    )));
                }
                vars.push(v);
                continue;
            }
            let Some(label) = &n.label else {
                let what = n.var.as_deref().map_or_else(|| "()".to_owned(), |v| format!("({v})"));
                return Err(Error::query(format!(
                    "{what} is not bound by FROM — give it a label like {}:User {{...}}) to create a new node",
                    what.trim_end_matches(')')
                )));
            };
            let ty = g.schema.resolve(label, Kind::Node)?.id;
            let var = scope.add(n.var.as_deref(), VarKind::Node(vec![ty]));
            creates.push(Create::Node { var, ty, props: compile_props(&mut c, &n.props)? });
            vars.push(var);
        }
        for (i, r) in p.rels.iter().enumerate() {
            let [label] = r.labels.as_slice() else {
                return Err(Error::query("a new edge needs exactly one type, like -[:FOLLOWS]->"));
            };
            if r.hops.is_some() {
                return Err(Error::query("cannot create a variable-length edge"));
            }
            let ty = g.schema.resolve(label, Kind::Edge)?.id;
            let (from, to) = match r.dir {
                Dir::Out => (vars[i], vars[i + 1]),
                Dir::In => (vars[i + 1], vars[i]),
                Dir::Both => return Err(Error::query("a new edge needs a direction: -[:T]-> or <-[:T]-")),
            };
            if r.var.as_deref().is_some_and(|v| scope.lookup(v).is_some()) {
                return Err(Error::query(format!("edge variable '{}' is already in use", r.var.as_deref().unwrap_or_default())));
            }
            let var = scope.add(r.var.as_deref(), VarKind::Edge(vec![ty]));
            creates.push(Create::Edge { var, ty, from, to, props: compile_props(&mut c, &r.props)? });
        }
    }

    // Phase 1: read.
    let bindings: Vec<Vec<Value>> = if ins.from.is_empty() {
        vec![vec![Value::Null; base_width]]
    } else {
        let matcher = plan(g, &m.scope, &m.patterns, m.conjuncts, None, now);
        let mut rows = Vec::new();
        matcher.run(g, base_width, now, &mut |row| {
            rows.push(row.to_vec());
            Ok(true)
        })?;
        rows
    };
    let mut next_id = g.next_id;
    let mut ops = Vec::new();
    let mut new_types: FxHashMap<ElementId, TypeId> = FxHashMap::default();
    let mut rows = Vec::with_capacity(bindings.len());
    for mut row in bindings {
        row.resize(scope.vars.len(), Value::Null);
        for create in &creates {
            let (var, ty, props) = match create {
                Create::Node { var, ty, props } | Create::Edge { var, ty, props, .. } => (var, ty, props),
            };
            let env = Env::new(g, &row, now);
            let given = props.iter().map(|(k, e)| Ok((k.clone(), eval(e, &env)?))).collect::<Result<Vec<_>>>()?;
            let def = g.schema.get(*ty).ok_or_else(|| Error::schema("type vanished"))?;
            let props = build_props(def, given, now)?;
            let id = next_id;
            next_id += 1;
            match create {
                Create::Node { .. } => {
                    new_types.insert(id, *ty);
                    row[*var] = Value::Node(id);
                    ops.push(Op::AddNode { id, ty: *ty, props });
                }
                Create::Edge { from, to, .. } => {
                    let (Value::Node(f), Value::Node(t)) = (&row[*from], &row[*to]) else {
                        return Err(Error::query("edge endpoints must be nodes"));
                    };
                    check_endpoints(def, g, &new_types, *f, *t)?;
                    ops.push(Op::AddEdge { id, ty: *ty, from: *f, to: *t, props });
                    row[*var] = Value::Edge(id);
                }
            }
        }
        rows.push(row);
    }

    // Phase 2: apply.
    for op in ops {
        txn.apply(op)?;
    }
    // Phase 3: return.
    returning(txn.g, params, &scope, &ins.returning, &rows, now)
}

pub(crate) fn insert_rows(txn: &mut Txn<'_>, params: &Params, ins: &InsertRows, now: i64) -> Result<QueryResult> {
    let g: &Graph = txn.g;
    let def = g.schema.resolve(&ins.label, Kind::Node)?;
    let empty = Scope::default();
    let mut c = Compiler::new(g, params, &empty);
    let env = Env::new(g, &[], now);
    let mut ops = Vec::with_capacity(ins.rows.len());
    let mut rows = Vec::with_capacity(ins.rows.len());
    for (offset, row) in ins.rows.iter().enumerate() {
        let given = row
            .iter()
            .map(|(k, e)| Ok((k.clone(), eval(&c.compile(e)?, &env)?)))
            .collect::<Result<Vec<_>>>()?;
        let id = g.next_id + offset as u64;
        ops.push(Op::AddNode { id, ty: def.id, props: build_props(def, given, now)? });
        rows.push(vec![Value::Node(id)]);
    }
    let mut scope = Scope::default();
    scope.add(Some(&ins.label), VarKind::Node(vec![def.id]));
    for op in ops {
        txn.apply(op)?;
    }
    returning(txn.g, params, &scope, &ins.returning, &rows, now)
}

pub(crate) fn update(txn: &mut Txn<'_>, params: &Params, up: &Update, now: i64) -> Result<QueryResult> {
    let g: &Graph = txn.g;
    let m = bind_where(g, params, &up.from, up.where_.as_ref())?;
    let mut c = Compiler::new(g, params, &m.scope);
    let mut sets = Vec::with_capacity(up.sets.len());
    for (var, prop, e) in &up.sets {
        if prop.starts_with('_') {
            return Err(Error::query(format!("{prop} is read-only")));
        }
        let target = match var {
            Some(v) => {
                let i = m.scope.lookup(v).ok_or_else(|| Error::query(format!("unknown variable '{v}'")))?;
                c.prop(i, prop)?
            }
            None => c.compile(&Expr::Ident(prop.clone()))?,
        };
        let CExpr::Prop { var, .. } = target else {
            return Err(Error::query(format!("cannot SET '{prop}': it is not a property")));
        };
        sets.push((var, prop.clone(), c.compile(e)?));
    }
    let width = m.scope.vars.len();
    let matcher = plan(g, &m.scope, &m.patterns, m.conjuncts, None, now);
    let mut rows = Vec::new();
    matcher.run(g, width, now, &mut |row| {
        rows.push(row.to_vec());
        Ok(true)
    })?;
    // Like SQL, each element is updated at most once even when the pattern
    // reaches it through several paths; RETURNING lists it once too.
    let mut ops = Vec::new();
    let mut touched: BTreeSet<(ElementId, usize)> = BTreeSet::new();
    let mut kept = Vec::with_capacity(rows.len());
    for row in rows {
        let env = Env::new(g, &row, now);
        let mut fresh = sets.is_empty();
        for (var, prop, e) in &sets {
            let (Value::Node(id) | Value::Edge(id)) = row[*var] else { continue };
            let def = g.type_of(id).and_then(|t| g.schema.get(t)).ok_or_else(|| Error::NotFound(format!("element {id}")))?;
            let (slot, p) = def.prop(prop)?;
            if !touched.insert((id, slot)) {
                continue;
            }
            fresh = true;
            let value = p.ty.coerce(eval(e, &env)?, &format!("{}.{prop}", def.name))?;
            if p.not_null && value.is_null() {
                return Err(Error::constraint(format!("{}.{prop} is NOT NULL", def.name)));
            }
            ops.push(Op::SetProp { id, slot, value });
        }
        if fresh {
            kept.push(row);
        }
    }
    let rows = kept;
    for op in ops {
        txn.apply(op)?;
    }
    returning(txn.g, params, &m.scope, &up.returning, &rows, now)
}

pub(crate) fn delete(txn: &mut Txn<'_>, params: &Params, del: &Delete, now: i64) -> Result<QueryResult> {
    let g: &Graph = txn.g;
    let m = bind_where(g, params, &del.from, del.where_.as_ref())?;
    let targets: Vec<usize> = if del.targets.is_empty() {
        let named: Vec<usize> = m.scope.named().map(|(i, _)| i).collect();
        match named.as_slice() {
            [one] => vec![*one],
            [] if m.scope.vars.len() == 1 => vec![0],
            _ => return Err(Error::query("say which variables to delete, e.g. DELETE u, e FROM ...")),
        }
    } else {
        del.targets
            .iter()
            .map(|t| m.scope.lookup(t).ok_or_else(|| Error::query(format!("unknown variable '{t}'"))))
            .collect::<Result<_>>()?
    };
    let width = m.scope.vars.len();
    let matcher = plan(g, &m.scope, &m.patterns, m.conjuncts, None, now);
    let mut rows = Vec::new();
    matcher.run(g, width, now, &mut |row| {
        rows.push(row.to_vec());
        Ok(true)
    })?;
    let (mut nodes, mut edges) = (BTreeSet::new(), BTreeSet::new());
    for row in &rows {
        for t in &targets {
            match row[*t] {
                Value::Node(id) => {
                    nodes.insert(id);
                }
                Value::Edge(id) => {
                    edges.insert(id);
                }
                _ => {}
            }
        }
    }
    // RETURNING sees the elements as they were before deletion.
    let result = returning(g, params, &m.scope, &del.returning, &rows, now)?;
    for id in &nodes {
        if let Some(n) = g.nodes.get(id) {
            edges.extend(n.out.iter().chain(&n.inc).map(|a| a.edge));
        }
    }
    for id in edges {
        txn.apply(Op::DelEdge(id))?;
    }
    for id in nodes {
        txn.apply(Op::DelNode(id))?;
    }
    Ok(result)
}

/// All elements of a type plus every edge touching them (for `DROP ... CASCADE`).
pub(crate) fn delete_all_of_type(txn: &mut Txn<'_>, ty: TypeId) -> Result<()> {
    let g: &Graph = txn.g;
    let members: Vec<ElementId> = g.members(ty).collect();
    let mut edges = BTreeSet::new();
    let mut nodes = Vec::new();
    for id in members {
        match g.nodes.get(&id) {
            Some(n) => {
                edges.extend(n.out.iter().chain(&n.inc).map(|a| a.edge));
                nodes.push(id);
            }
            None => {
                edges.insert(id);
            }
        }
    }
    for id in edges {
        txn.apply(Op::DelEdge(id))?;
    }
    for id in nodes {
        txn.apply(Op::DelNode(id))?;
    }
    Ok(())
}

/// Evaluates constant call arguments into named parameters for a stored query.
pub(crate) fn call_params(
    g: &Graph,
    outer: &Params,
    declared: &[(String, crate::schema::PropType)],
    args: &[Expr],
    name: &str,
) -> Result<Params> {
    if args.len() != declared.len() {
        let sig: Vec<String> = declared.iter().map(|(n, t)| format!("${n} {t}")).collect();
        return Err(Error::query(format!("{name}({}) takes {} argument(s), got {}", sig.join(", "), declared.len(), args.len())));
    }
    let empty = Scope::default();
    let mut c = Compiler::new(g, outer, &empty);
    let env = Env::new(g, &[], 0);
    declared
        .iter()
        .zip(args)
        .map(|((n, ty), e)| Ok((n.clone(), ty.coerce(eval(&c.compile(e)?, &env)?, &format!("${n}"))?)))
        .collect()
}

/// Checks named parameters against a stored query's declaration.
pub(crate) fn check_params(declared: &[(String, crate::schema::PropType)], given: &Params, name: &str) -> Result<Params> {
    let mut out = BTreeMap::new();
    for (n, ty) in declared {
        let v = given.get(n).cloned().unwrap_or_default();
        out.insert(n.clone(), ty.coerce(v, &format!("{name}(${n})"))?);
    }
    if let Some(extra) = given.keys().find(|k| !declared.iter().any(|(n, _)| n == *k)) {
        return Err(Error::query(format!("{name}() has no parameter ${extra}")));
    }
    Ok(out.into_iter().collect())
}
