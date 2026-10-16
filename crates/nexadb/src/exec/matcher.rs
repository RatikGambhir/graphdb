//! Graph pattern binding, planning and matching.
//!
//! A pattern such as `(u:User {name: $n})-[:FOLLOWS]->(f)` is compiled into a
//! linear plan: one *scan* that binds a starting variable (through the
//! cheapest index available) followed by *expands* that walk adjacency lists.
//! Every `WHERE` conjunct runs at the first step where all its variables are
//! bound, so filters prune as early as possible.

use std::collections::VecDeque;
use std::ops::Bound;

use rustc_hash::FxHashSet;

use crate::error::{Error, Result};
use crate::graph::{Direction, Graph};
use crate::nql::ast::{BinOp, Dir, Expr, Pattern};
use crate::schema::{Kind, TypeId};
use crate::value::{ElementId, Value};

use super::compile::{CExpr, Compiler, Meta, Params, Scope, VarKind};
use super::eval::{Env, eval, truthy};

#[derive(Debug, Clone)]
pub(crate) struct CRel {
    pub var: usize,
    pub types: Option<Vec<TypeId>>,
    pub dir: Dir,
    pub hops: Option<(u32, Option<u32>)>,
}

#[derive(Debug, Clone)]
pub(crate) struct CPattern {
    pub nodes: Vec<usize>,
    pub rels: Vec<CRel>,
}

/// Inline `{prop: value}` constraints waiting to be compiled once every
/// variable is known.
pub(crate) type InlineProps = Vec<(usize, String, Expr)>;

fn type_names(g: &Graph, types: &[TypeId]) -> String {
    let names: Vec<&str> = types
        .iter()
        .filter_map(|t| g.schema.get(*t))
        .map(|d| d.name.as_str())
        .collect();
    names.join(" | ")
}

/// Declares the variables of `patterns` in `scope`, checking every label
/// against the schema and inferring the possible types of unlabeled nodes
/// from the edges around them.
pub(crate) fn bind_patterns(
    g: &Graph,
    scope: &mut Scope,
    patterns: &[Pattern],
    inline: &mut InlineProps,
) -> Result<Vec<CPattern>> {
    let all_nodes: Vec<TypeId> = g.schema.of_kind(Kind::Node).map(|t| t.id).collect();
    let mut out = Vec::with_capacity(patterns.len());
    for p in patterns {
        let mut nodes = Vec::with_capacity(p.nodes.len());
        let mut explicit = Vec::with_capacity(p.nodes.len());
        for n in &p.nodes {
            let label = n
                .label
                .as_deref()
                .map(|l| g.schema.resolve(l, Kind::Node).map(|d| d.id))
                .transpose()?;
            explicit.push(label.is_some());
            let existing = n.var.as_deref().and_then(|v| scope.lookup(v));
            let var = match existing {
                Some(v) => {
                    let var_name = scope.vars[v].name.clone();
                    let VarKind::Node(types) = &mut scope.vars[v].kind else {
                        return Err(Error::query(format!(
                            "'{var_name}' is an edge, but is used as a node here"
                        )));
                    };
                    if let Some(l) = label {
                        if !types.contains(&l) {
                            return Err(Error::query(format!(
                                "'{}' was already bound as {}, so it cannot also be {}",
                                var_name,
                                type_names(g, types),
                                type_names(g, &[l])
                            )));
                        }
                        *types = vec![l];
                    }
                    v
                }
                None => scope.add(
                    n.var.as_deref(),
                    VarKind::Node(label.map_or_else(|| all_nodes.clone(), |l| vec![l])),
                ),
            };
            inline.extend(n.props.iter().map(|(k, e)| (var, k.clone(), e.clone())));
            nodes.push(var);
        }
        let mut rels = Vec::with_capacity(p.rels.len());
        for r in &p.rels {
            let types = if r.labels.is_empty() {
                None
            } else {
                Some(
                    r.labels
                        .iter()
                        .map(|l| g.schema.resolve(l, Kind::Edge).map(|d| d.id))
                        .collect::<Result<Vec<_>>>()?,
                )
            };
            if r.hops.is_some() && (r.var.is_some() || !r.props.is_empty()) {
                return Err(Error::query(
                    "variable-length edges (-[*1..3]->) cannot have a variable or inline properties",
                ));
            }
            if let Some((min, Some(max))) = r.hops
                && min > max
            {
                return Err(Error::query(format!("invalid hop range *{min}..{max}")));
            }
            let var = match r.var.as_deref().and_then(|v| scope.lookup(v)) {
                Some(v) => {
                    if !matches!(scope.vars[v].kind, VarKind::Edge(_)) {
                        return Err(Error::query(format!(
                            "'{}' is a node, but is used as an edge here",
                            scope.vars[v].name
                        )));
                    }
                    v
                }
                None if r.hops.is_some() => scope.add(None, VarKind::Path),
                None => {
                    let ts = types
                        .clone()
                        .unwrap_or_else(|| g.schema.of_kind(Kind::Edge).map(|t| t.id).collect());
                    scope.add(r.var.as_deref(), VarKind::Edge(ts))
                }
            };
            inline.extend(r.props.iter().map(|(k, e)| (var, k.clone(), e.clone())));
            rels.push(CRel {
                var,
                types,
                dir: r.dir,
                hops: r.hops,
            });
        }
        // Endpoint inference: an edge type constrains the nodes it touches.
        for (i, rel) in rels.iter().enumerate() {
            let Some(types) = &rel.types else { continue };
            let names = |pick: fn(&crate::schema::TypeDef) -> &Vec<String>| -> Vec<TypeId> {
                types
                    .iter()
                    .filter_map(|t| g.schema.get(*t))
                    .flat_map(|d| {
                        pick(d)
                            .iter()
                            .filter_map(|n| g.schema.by_name(n).map(|t| t.id))
                            .collect::<Vec<_>>()
                    })
                    .collect()
            };
            let (from, to) = (names(|d| &d.from), names(|d| &d.to));
            let both: Vec<TypeId> = from.iter().chain(&to).copied().collect();
            let (left, right) = match rel.dir {
                Dir::Out => (from.clone(), to.clone()),
                Dir::In => (to.clone(), from.clone()),
                Dir::Both => (both.clone(), both),
            };
            for (node_idx, allowed, side) in [(i, left, "source"), (i + 1, right, "target")] {
                let var = nodes[node_idx];
                let VarKind::Node(current) = &mut scope.vars[var].kind else {
                    continue;
                };
                let narrowed: Vec<TypeId> = current
                    .iter()
                    .copied()
                    .filter(|t| allowed.contains(t))
                    .collect();
                if narrowed.is_empty() {
                    let def = types.iter().filter_map(|t| g.schema.get(*t)).next();
                    let shape = def.map_or_else(String::new, |d| {
                        format!(
                            "{} goes from {} to {}",
                            d.name,
                            d.from.join(" | "),
                            d.to.join(" | ")
                        )
                    });
                    let what = if explicit[node_idx] {
                        type_names(g, current)
                    } else {
                        "this node".into()
                    };
                    return Err(Error::schema(format!(
                        "{shape}, so {what} cannot be the {side} of this edge"
                    )));
                }
                *current = narrowed;
            }
        }
        out.push(CPattern { nodes, rels });
    }
    Ok(out)
}

/// Compiles inline `{prop: value}` constraints into equality predicates.
pub(crate) fn inline_conjuncts(c: &mut Compiler<'_>, inline: &InlineProps) -> Result<Vec<CExpr>> {
    inline
        .iter()
        .map(|(var, prop, e)| {
            Ok(CExpr::Bin(
                BinOp::Eq,
                Box::new(c.prop(*var, prop)?),
                Box::new(c.compile(e)?),
            ))
        })
        .collect()
}

#[derive(Debug, Clone)]
pub(crate) struct VectorHint {
    pub var: usize,
    pub prop: String,
    pub query: Vec<f32>,
    pub k: usize,
}

#[derive(Debug, Clone)]
pub(crate) enum Access {
    Scan,
    Ids(Vec<ElementId>),
    Eq {
        prop: String,
        values: Vec<Value>,
    },
    Range {
        prop: String,
        lo: Bound<Value>,
        hi: Bound<Value>,
    },
    Text {
        prop: String,
        terms: Vec<String>,
    },
    Vector {
        prop: String,
        query: Vec<f32>,
        k: usize,
    },
}

#[derive(Debug, Clone)]
pub(crate) enum Step {
    Scan {
        var: usize,
        types: Vec<TypeId>,
        access: Access,
        estimate: usize,
    },
    EdgeScan {
        rel: usize,
        types: Vec<TypeId>,
        left: usize,
        right: usize,
        dir: Dir,
    },
    Expand {
        from: usize,
        to: usize,
        rel: usize,
        dir: Dir,
        types: Option<Vec<TypeId>>,
        to_types: Option<Vec<TypeId>>,
        to_bound: bool,
        rel_bound: bool,
        hops: Option<(u32, Option<u32>)>,
    },
}

#[derive(Debug, Clone, Default)]
pub(crate) struct MatchPlan {
    pub steps: Vec<Step>,
    /// `filters[i]` runs right after step `i` binds its variables.
    pub filters: Vec<Vec<CExpr>>,
    /// Filters that depend on no variable at all.
    pub pre: Vec<CExpr>,
}

fn flip(d: Dir) -> Dir {
    match d {
        Dir::Out => Dir::In,
        Dir::In => Dir::Out,
        Dir::Both => Dir::Both,
    }
}

fn const_of(g: &Graph, e: &CExpr, now: i64) -> Option<Value> {
    if let CExpr::Const(v) = e {
        return Some(v.clone());
    }
    if e.is_const() {
        eval(e, &Env::new(g, &[], now)).ok()
    } else {
        None
    }
}

fn prop_of(e: &CExpr, var: usize) -> Option<&str> {
    match e {
        CExpr::Prop { var: v, name, .. } if *v == var => Some(name),
        _ => None,
    }
}

/// Index probes used for costing stop counting here: beyond this, exact
/// numbers do not change which plan wins.
const PROBE_CAP: usize = 10_000;

fn type_count(g: &Graph, ty: TypeId) -> usize {
    g.member_count(ty).map_or(usize::MAX / 8, |n| {
        usize::try_from(n).unwrap_or(usize::MAX / 8)
    })
}

fn eq_cost(g: &Graph, types: &[TypeId], prop: &str, values: &[Value]) -> usize {
    types
        .iter()
        .flat_map(|t| values.iter().map(move |v| (t, v)))
        .map(|(t, v)| {
            g.index_eq(*t, prop, v, PROBE_CAP)
                .map_or(PROBE_CAP, |ids| ids.len())
        })
        .sum()
}

/// `None` when the range cannot use the index (e.g. `age > 1.5` on an INT).
fn range_cost(
    g: &Graph,
    types: &[TypeId],
    prop: &str,
    lo: &Bound<Value>,
    hi: &Bound<Value>,
) -> Option<usize> {
    let mut total = 0;
    for t in types {
        match g.index_range(*t, prop, lo.as_ref(), hi.as_ref(), PROBE_CAP) {
            Ok(Some(ids)) => total += ids.len(),
            Ok(None) => return None,
            Err(_) => total += PROBE_CAP,
        }
    }
    Some(total)
}

/// Picks the cheapest way to produce candidates for `var`.
fn choose_access(
    g: &Graph,
    var: usize,
    types: &[TypeId],
    conjuncts: &[CExpr],
    hint: Option<&VectorHint>,
    now: i64,
) -> (Access, usize) {
    let scan_cost: usize = types.iter().map(|t| type_count(g, *t)).sum();
    let mut best = (Access::Scan, scan_cost);
    let indexed = |prop: &str| types.iter().all(|t| g.has_btree(*t, prop));
    let mut consider = |access: Access, cost: usize| {
        if cost < best.1 || (cost == best.1 && matches!(best.0, Access::Scan)) {
            best = (access, cost);
        }
    };
    if let Some(h) = hint
        && h.var == var
        && types.iter().all(|t| g.vector_index(*t, &h.prop).is_some())
    {
        consider(
            Access::Vector {
                prop: h.prop.clone(),
                query: h.query.clone(),
                k: h.k,
            },
            h.k.min(scan_cost),
        );
    }
    for c in conjuncts {
        match c {
            CExpr::Bin(op @ (BinOp::Eq | BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge), a, b) => {
                let (side, value, op) = if let Some(v) = const_of(g, b, now) {
                    (a.as_ref(), v, *op)
                } else if let Some(v) = const_of(g, a, now) {
                    let mirrored = match op {
                        BinOp::Lt => BinOp::Gt,
                        BinOp::Le => BinOp::Ge,
                        BinOp::Gt => BinOp::Lt,
                        BinOp::Ge => BinOp::Le,
                        other => *other,
                    };
                    (b.as_ref(), v, mirrored)
                } else {
                    continue;
                };
                if value.is_null() {
                    continue;
                }
                if let CExpr::Meta {
                    var: v,
                    meta: Meta::Id,
                } = side
                    && *v == var
                    && op == BinOp::Eq
                    && let Value::Int(id) = value
                {
                    consider(Access::Ids(vec![id as u64]), 1);
                    continue;
                }
                let Some(prop) = prop_of(side, var) else {
                    continue;
                };
                if !indexed(prop) {
                    continue;
                }
                let (lo, hi) = match op {
                    BinOp::Eq => {
                        let cost = eq_cost(g, types, prop, std::slice::from_ref(&value));
                        consider(
                            Access::Eq {
                                prop: prop.to_owned(),
                                values: vec![value],
                            },
                            cost,
                        );
                        continue;
                    }
                    BinOp::Lt => (Bound::Unbounded, Bound::Excluded(value)),
                    BinOp::Le => (Bound::Unbounded, Bound::Included(value)),
                    BinOp::Gt => (Bound::Excluded(value), Bound::Unbounded),
                    _ => (Bound::Included(value), Bound::Unbounded),
                };
                if let Some(cost) = range_cost(g, types, prop, &lo, &hi) {
                    consider(
                        Access::Range {
                            prop: prop.to_owned(),
                            lo,
                            hi,
                        },
                        cost,
                    );
                }
            }
            CExpr::Between(a, lo, hi, false) => {
                let (Some(prop), Some(lo), Some(hi)) =
                    (prop_of(a, var), const_of(g, lo, now), const_of(g, hi, now))
                else {
                    continue;
                };
                if indexed(prop) && !lo.is_null() && !hi.is_null() {
                    let (lo, hi) = (Bound::Included(lo), Bound::Included(hi));
                    if let Some(cost) = range_cost(g, types, prop, &lo, &hi) {
                        consider(
                            Access::Range {
                                prop: prop.to_owned(),
                                lo,
                                hi,
                            },
                            cost,
                        );
                    }
                }
            }
            CExpr::In(a, list, false) => {
                let (Some(prop), Some(Value::List(values))) =
                    (prop_of(a, var), const_of(g, list, now))
                else {
                    continue;
                };
                if indexed(prop) {
                    let cost = eq_cost(g, types, prop, &values);
                    consider(
                        Access::Eq {
                            prop: prop.to_owned(),
                            values,
                        },
                        cost,
                    );
                }
            }
            CExpr::Text {
                var: v,
                prop,
                terms,
                matches: true,
            } if *v == var && types.iter().all(|t| g.has_text(*t, prop)) => {
                let cost = types
                    .iter()
                    .map(|t| {
                        g.text_candidates(*t, prop, terms)
                            .map_or(PROBE_CAP, |ids| ids.len())
                    })
                    .sum();
                consider(
                    Access::Text {
                        prop: prop.clone(),
                        terms: terms.clone(),
                    },
                    cost,
                );
            }
            _ => {}
        }
    }
    best
}

/// Builds the step-by-step plan for matching `patterns`.
pub(crate) fn plan(
    g: &Graph,
    scope: &Scope,
    patterns: &[CPattern],
    conjuncts: Vec<CExpr>,
    hint: Option<&VectorHint>,
    now: i64,
) -> MatchPlan {
    let mut bound: FxHashSet<usize> = FxHashSet::default();
    let mut steps = Vec::new();
    let node_types = |v: usize| match &scope.vars[v].kind {
        VarKind::Node(t) | VarKind::Edge(t) => t.clone(),
        VarKind::Path => Vec::new(),
    };
    let all_nodes = g.schema.of_kind(Kind::Node).count();
    let restrict = |types: Vec<TypeId>| (types.len() < all_nodes).then_some(types);

    for p in patterns {
        let start = if let Some(i) = p.nodes.iter().position(|v| bound.contains(v)) {
            i
        } else {
            let mut best: Option<(usize, Access, usize)> = None;
            for (i, var) in p.nodes.iter().enumerate() {
                let (access, cost) =
                    choose_access(g, *var, &node_types(*var), &conjuncts, hint, now);
                if best.as_ref().is_none_or(|b| cost < b.2) {
                    best = Some((i, access, cost));
                }
            }
            let (i, access, cost) = best.unwrap_or((0, Access::Scan, 0));
            // An edge-type scan beats a full node scan when edges are rarer.
            let edge_start = p.rels.iter().enumerate().find_map(|(ri, r)| {
                let types = r.types.clone()?;
                if r.hops.is_some() || bound.contains(&r.var) {
                    return None;
                }
                let ecost: usize = types.iter().map(|t| type_count(g, *t)).sum();
                (matches!(access, Access::Scan) && ecost < cost).then_some((ri, types, ecost))
            });
            if let Some((ri, types, _)) = edge_start {
                let r = &p.rels[ri];
                steps.push(Step::EdgeScan {
                    rel: r.var,
                    types,
                    left: p.nodes[ri],
                    right: p.nodes[ri + 1],
                    dir: r.dir,
                });
                bound.extend([r.var, p.nodes[ri], p.nodes[ri + 1]]);
                // Walk out from both ends of the scanned edge.
                for (k, rel) in p.rels.iter().enumerate().skip(ri + 1) {
                    push_expand(
                        &mut steps,
                        &mut bound,
                        p.nodes[k],
                        p.nodes[k + 1],
                        rel,
                        rel.dir,
                        &node_types,
                        &restrict,
                    );
                }
                for k in (0..ri).rev() {
                    let rel = &p.rels[k];
                    push_expand(
                        &mut steps,
                        &mut bound,
                        p.nodes[k + 1],
                        p.nodes[k],
                        rel,
                        flip(rel.dir),
                        &node_types,
                        &restrict,
                    );
                }
                continue;
            }
            steps.push(Step::Scan {
                var: p.nodes[i],
                types: node_types(p.nodes[i]),
                access,
                estimate: cost,
            });
            bound.insert(p.nodes[i]);
            i
        };
        for (k, rel) in p.rels.iter().enumerate().skip(start) {
            push_expand(
                &mut steps,
                &mut bound,
                p.nodes[k],
                p.nodes[k + 1],
                rel,
                rel.dir,
                &node_types,
                &restrict,
            );
        }
        for k in (0..start).rev() {
            let rel = &p.rels[k];
            push_expand(
                &mut steps,
                &mut bound,
                p.nodes[k + 1],
                p.nodes[k],
                rel,
                flip(rel.dir),
                &node_types,
                &restrict,
            );
        }
    }

    // Attach each filter to the earliest step that binds all of its variables.
    let mut filters = vec![Vec::new(); steps.len()];
    let mut pre = Vec::new();
    let mut bound_after: Vec<FxHashSet<usize>> = Vec::with_capacity(steps.len());
    let mut acc = FxHashSet::default();
    for s in &steps {
        match s {
            Step::Scan { var, .. } => {
                acc.insert(*var);
            }
            Step::EdgeScan {
                rel, left, right, ..
            } => acc.extend([*rel, *left, *right]),
            Step::Expand { to, rel, .. } => acc.extend([*to, *rel]),
        }
        bound_after.push(acc.clone());
    }
    for c in conjuncts {
        let mut vars = Vec::new();
        c.vars(&mut vars);
        match bound_after
            .iter()
            .position(|b| vars.iter().all(|v| b.contains(v)))
        {
            Some(i) if !vars.is_empty() => filters[i].push(c),
            _ => pre.push(c),
        }
    }
    MatchPlan {
        steps,
        filters,
        pre,
    }
}

#[allow(clippy::too_many_arguments)]
fn push_expand(
    steps: &mut Vec<Step>,
    bound: &mut FxHashSet<usize>,
    from: usize,
    to: usize,
    rel: &CRel,
    dir: Dir,
    node_types: &dyn Fn(usize) -> Vec<TypeId>,
    restrict: &dyn Fn(Vec<TypeId>) -> Option<Vec<TypeId>>,
) {
    let to_bound = bound.contains(&to);
    let rel_bound = bound.contains(&rel.var);
    steps.push(Step::Expand {
        from,
        to,
        rel: rel.var,
        dir,
        types: rel.types.clone(),
        to_types: if to_bound {
            None
        } else {
            restrict(node_types(to))
        },
        to_bound,
        rel_bound,
        hops: rel.hops,
    });
    bound.insert(to);
    bound.insert(rel.var);
}

/// Callback receiving each complete binding; return `false` to stop.
pub(crate) type Emit<'e> = dyn FnMut(&[Value]) -> Result<bool> + 'e;

impl MatchPlan {
    /// Runs the plan, calling `emit` for every binding.
    pub(crate) fn run(&self, g: &Graph, width: usize, now: i64, emit: &mut Emit<'_>) -> Result<()> {
        let mut row = vec![Value::Null; width];
        for f in &self.pre {
            if !truthy(&eval(f, &Env::new(g, &row, now))?) {
                return Ok(());
            }
        }
        self.step(0, g, &mut row, now, emit).map(|_| ())
    }

    fn passes(&self, i: usize, g: &Graph, row: &[Value], now: i64) -> Result<bool> {
        let env = Env::new(g, row, now);
        for f in &self.filters[i] {
            if !truthy(&eval(f, &env)?) {
                return Ok(false);
            }
        }
        Ok(true)
    }

    fn next(
        &self,
        i: usize,
        g: &Graph,
        row: &mut Vec<Value>,
        now: i64,
        emit: &mut Emit<'_>,
    ) -> Result<bool> {
        if !self.passes(i, g, row, now)? {
            return Ok(true);
        }
        self.step(i + 1, g, row, now, emit)
    }

    fn step(
        &self,
        i: usize,
        g: &Graph,
        row: &mut Vec<Value>,
        now: i64,
        emit: &mut Emit<'_>,
    ) -> Result<bool> {
        let Some(step) = self.steps.get(i) else {
            return emit(row);
        };
        match step {
            Step::Scan {
                var, types, access, ..
            } => {
                for id in candidates(g, types, access)? {
                    row[*var] = Value::Node(id?);
                    if !self.next(i, g, row, now, emit)? {
                        return Ok(false);
                    }
                }
            }
            Step::EdgeScan {
                rel,
                types,
                left,
                right,
                dir,
            } => {
                for t in types {
                    for id in g.members(*t) {
                        let id = id?;
                        let Some((from, to)) = g.element(id)?.and_then(|e| e.ends) else {
                            continue;
                        };
                        let orientations: &[(u64, u64)] = match dir {
                            Dir::Out => &[(from, to)],
                            Dir::In => &[(to, from)],
                            Dir::Both if from == to => &[(from, to)],
                            Dir::Both => &[(from, to), (to, from)],
                        };
                        for (l, r) in orientations {
                            row[*rel] = Value::Edge(id);
                            row[*left] = Value::Node(*l);
                            row[*right] = Value::Node(*r);
                            if !self.next(i, g, row, now, emit)? {
                                return Ok(false);
                            }
                        }
                    }
                }
            }
            Step::Expand {
                from,
                to,
                rel,
                dir,
                types,
                to_types,
                to_bound,
                rel_bound,
                hops,
            } => {
                let Value::Node(src) = row[*from] else {
                    return Ok(true);
                };
                let target_ok = |row: &[Value], other: u64| -> Result<bool> {
                    if *to_bound {
                        return Ok(row[*to] == Value::Node(other));
                    }
                    Ok(match to_types {
                        None => true,
                        Some(ts) => g.type_of(other)?.is_some_and(|t| ts.contains(&t)),
                    })
                };
                if let Some((min, max)) = hops {
                    for other in reachable(g, src, *dir, types.as_deref(), *min, *max)? {
                        if !target_ok(row, other)? {
                            continue;
                        }
                        row[*to] = Value::Node(other);
                        if !self.next(i, g, row, now, emit)? {
                            return Ok(false);
                        }
                    }
                    return Ok(true);
                }
                for a in g.adjacent(src, direction(*dir), types.as_deref())? {
                    if !target_ok(row, a.other)? {
                        continue;
                    }
                    if *rel_bound && row[*rel] != Value::Edge(a.edge) {
                        continue;
                    }
                    row[*rel] = Value::Edge(a.edge);
                    row[*to] = Value::Node(a.other);
                    if !self.next(i, g, row, now, emit)? {
                        return Ok(false);
                    }
                }
            }
        }
        Ok(true)
    }

    /// Human-readable plan, one line per step (for `EXPLAIN`).
    pub(crate) fn describe(&self, g: &Graph, scope: &Scope) -> Vec<String> {
        let name = |v: usize| {
            let var = &scope.vars[v];
            if var.anon {
                String::new()
            } else {
                var.name.clone()
            }
        };
        let types = |ts: &[TypeId]| type_names(g, ts);
        let mut out = Vec::new();
        if !self.pre.is_empty() {
            out.push(format!("Filter  {} constant condition(s)", self.pre.len()));
        }
        for (i, s) in self.steps.iter().enumerate() {
            let line = match s {
                Step::Scan {
                    var,
                    types: ts,
                    access,
                    estimate,
                } => {
                    let how = match access {
                        Access::Scan => "full scan".to_owned(),
                        Access::Ids(ids) => format!("id lookup {ids:?}"),
                        Access::Eq { prop, values } => {
                            format!("index {prop} = {} value(s)", values.len())
                        }
                        Access::Range { prop, .. } => format!("index range on {prop}"),
                        Access::Text { prop, terms } => {
                            format!("BM25 index on {prop} for {terms:?}")
                        }
                        Access::Vector { prop, k, .. } => {
                            format!("HNSW vector index on {prop}, top {k}")
                        }
                    };
                    format!(
                        "Scan    ({}:{}) via {how}  (~{estimate} rows)",
                        name(*var),
                        types(ts)
                    )
                }
                Step::EdgeScan {
                    rel,
                    types: ts,
                    left,
                    right,
                    ..
                } => format!(
                    "Scan    ({})-[{}:{}]-({}) via edge type scan",
                    name(*left),
                    name(*rel),
                    types(ts),
                    name(*right)
                ),
                Step::Expand {
                    from,
                    to,
                    rel,
                    dir,
                    types: ts,
                    hops,
                    ..
                } => {
                    let label = ts
                        .as_deref()
                        .map(|t| format!(":{}", types(t)))
                        .unwrap_or_default();
                    let hop = hops.map_or_else(String::new, |(a, b)| {
                        format!("*{a}..{}", b.map_or_else(String::new, |b| b.to_string()))
                    });
                    let body = format!("[{}{label}{hop}]", name(*rel));
                    let arrow = match dir {
                        Dir::Out => format!("-{body}->"),
                        Dir::In => format!("<-{body}-"),
                        Dir::Both => format!("-{body}-"),
                    };
                    format!("Expand  ({}){arrow}({})", name(*from), name(*to))
                }
            };
            out.push(line);
            if !self.filters[i].is_empty() {
                out.push(format!("  Filter {} condition(s)", self.filters[i].len()));
            }
        }
        out
    }
}

fn direction(d: Dir) -> Direction {
    match d {
        Dir::Out => Direction::Out,
        Dir::In => Direction::In,
        Dir::Both => Direction::Both,
    }
}

type Candidates<'g> = Box<dyn Iterator<Item = Result<ElementId>> + 'g>;

fn candidates<'g>(g: &'g Graph, types: &'g [TypeId], access: &Access) -> Result<Candidates<'g>> {
    let mut ids: Vec<ElementId> = Vec::new();
    match access {
        Access::Scan => return Ok(Box::new(types.iter().flat_map(move |t| g.members(*t)))),
        Access::Ids(wanted) => {
            for id in wanted {
                if let Some(e) = g.element(*id)?
                    && e.ends.is_none()
                    && types.contains(&e.ty)
                {
                    ids.push(*id);
                }
            }
        }
        Access::Eq { prop, values } => {
            for t in types {
                for v in values {
                    ids.extend(g.index_eq(*t, prop, v, usize::MAX)?);
                }
            }
        }
        Access::Range { prop, lo, hi } => {
            for t in types {
                let found = g.index_range(*t, prop, lo.as_ref(), hi.as_ref(), usize::MAX)?;
                ids.extend(found.ok_or_else(|| Error::query("index range is not usable"))?);
            }
        }
        Access::Text { prop, terms } => {
            for t in types {
                ids.extend(g.text_candidates(*t, prop, terms)?);
            }
        }
        Access::Vector { prop, query, k } => {
            // Results are re-ranked exactly by ORDER BY, so order here is moot.
            for t in types {
                if let Some(index) = g.vector_index(*t, prop) {
                    ids.extend(index.search(query, *k, (*k).max(64)));
                }
            }
            return Ok(Box::new(ids.into_iter().map(Ok)));
        }
    }
    ids.sort_unstable();
    ids.dedup();
    Ok(Box::new(ids.into_iter().map(Ok)))
}

/// Nodes reachable from `src` in `min..=max` hops (breadth-first; each node
/// once, at its shortest distance).
fn reachable(
    g: &Graph,
    src: u64,
    dir: Dir,
    types: Option<&[TypeId]>,
    min: u32,
    max: Option<u32>,
) -> Result<Vec<u64>> {
    let mut out = Vec::new();
    if min == 0 {
        out.push(src);
    }
    let mut seen: FxHashSet<u64> = FxHashSet::default();
    seen.insert(src);
    let mut frontier: VecDeque<u64> = VecDeque::from([src]);
    let mut depth = 0u32;
    while !frontier.is_empty() && max.is_none_or(|m| depth < m) {
        depth += 1;
        let mut next = VecDeque::new();
        for n in frontier {
            for a in g.adjacent(n, direction(dir), types)? {
                if !seen.insert(a.other) {
                    continue;
                }
                next.push_back(a.other);
                if depth >= min {
                    out.push(a.other);
                }
            }
        }
        frontier = next;
    }
    Ok(out)
}

/// Compiles patterns plus a `WHERE` clause into a scope and a match plan.
pub(crate) struct Matched {
    pub scope: Scope,
    pub patterns: Vec<CPattern>,
    pub conjuncts: Vec<CExpr>,
}

pub(crate) fn bind_where(
    g: &Graph,
    params: &Params,
    patterns: &[Pattern],
    where_: Option<&Expr>,
) -> Result<Matched> {
    let mut scope = Scope::default();
    let mut inline = InlineProps::new();
    let patterns = bind_patterns(g, &mut scope, patterns, &mut inline)?;
    let mut c = Compiler::new(g, params, &scope);
    let mut conjuncts = inline_conjuncts(&mut c, &inline)?;
    if let Some(w) = where_ {
        c.compile(w)?.conjuncts(&mut conjuncts);
    }
    Ok(Matched {
        scope,
        patterns,
        conjuncts,
    })
}
