//! `SELECT`: projection, aggregation, ordering and paging over pattern matches.

use std::cmp::Ordering;
use std::collections::{BTreeMap, BTreeSet};

use crate::error::{Error, Result};
use crate::graph::Graph;
use crate::nql::ast::{Expr, Select, SelectItem};
use crate::schema::Kind;
use crate::value::{OrdValue, Value};

use super::compile::{AggFn, AggSpec, CExpr, Compiler, Params, Scope, VarKind};
use super::eval::{Env, eval, truthy};
use super::matcher::{MatchPlan, VectorHint, bind_where, plan};

/// A compiled projection list.
pub(crate) struct Projection {
    pub names: Vec<String>,
    pub exprs: Vec<CExpr>,
}

/// Expands `*` and compiles each item; aggregates are collected into `aggs`
/// when allowed.
pub(crate) fn project(
    g: &Graph,
    params: &Params,
    scope: &Scope,
    items: &[SelectItem],
    aggs: Option<&mut Vec<AggSpec>>,
) -> Result<Projection> {
    let mut names = Vec::new();
    let mut exprs = Vec::new();
    let mut c = Compiler {
        g,
        params,
        scope,
        aliases: &[],
        aggs,
    };
    for item in items {
        match item {
            SelectItem::Star => {
                let named: Vec<(usize, &super::compile::Var)> = scope
                    .named()
                    .filter(|(_, v)| v.kind != VarKind::Path)
                    .collect();
                match named.as_slice() {
                    // One variable: spread its properties into columns, SQL style.
                    [(v, var)] => {
                        let (VarKind::Node(types) | VarKind::Edge(types)) = &var.kind else {
                            continue;
                        };
                        names.push("_id".to_owned());
                        exprs.push(c.prop(*v, "_id")?);
                        if matches!(var.kind, VarKind::Edge(_)) {
                            for meta in ["_from", "_to"] {
                                names.push(meta.to_owned());
                                exprs.push(c.prop(*v, meta)?);
                            }
                        }
                        if types.len() > 1 {
                            names.push("_label".to_owned());
                            exprs.push(c.prop(*v, "_label")?);
                        }
                        let mut seen = BTreeSet::new();
                        for t in types {
                            let Some(def) = g.schema.get(*t) else {
                                continue;
                            };
                            for p in &def.props {
                                if seen.insert(p.name.clone()) {
                                    names.push(p.name.clone());
                                    exprs.push(c.prop(*v, &p.name)?);
                                }
                            }
                        }
                    }
                    [] => return Err(Error::query("SELECT * needs at least one named variable")),
                    many => {
                        for (v, var) in many {
                            names.push(var.name.clone());
                            exprs.push(CExpr::Var(*v));
                        }
                    }
                }
            }
            SelectItem::Expr { expr, alias } => {
                names.push(alias.clone().unwrap_or_else(|| expr.to_string()));
                exprs.push(c.compile(expr)?);
            }
        }
    }
    Ok(Projection { names, exprs })
}

pub(crate) struct SelectPlan {
    pub scope: Scope,
    pub matcher: MatchPlan,
    pub proj: Projection,
    pub aggs: Vec<AggSpec>,
    pub group_by: Vec<CExpr>,
    pub having: Option<CExpr>,
    pub order: Vec<(CExpr, bool)>,
    pub distinct: bool,
    pub limit: Option<usize>,
    pub offset: usize,
}

fn count_arg(g: &Graph, params: &Params, e: Option<&Expr>, what: &str) -> Result<Option<usize>> {
    let Some(e) = e else { return Ok(None) };
    let scope = Scope::default();
    let v = eval(
        &Compiler::new(g, params, &scope).compile(e)?,
        &Env::new(g, &[], 0),
    )?;
    match v {
        Value::Int(n) if n >= 0 => Ok(Some(usize::try_from(n).unwrap_or(usize::MAX))),
        Value::Null => Ok(None),
        v => Err(Error::query(format!(
            "{what} must be a non-negative integer, got {v}"
        ))),
    }
}

pub(crate) fn plan_select(g: &Graph, params: &Params, s: &Select, now: i64) -> Result<SelectPlan> {
    let m = bind_where(g, params, &s.from, s.where_.as_ref())?;
    let mut aggs = Vec::new();
    let proj = project(g, params, &m.scope, &s.items, Some(&mut aggs))?;

    // GROUP BY may name output columns.
    let group_by = s
        .group_by
        .iter()
        .map(|e| {
            if let Expr::Ident(n) = e
                && m.scope.lookup(n).is_none()
                && let Some(i) = proj.names.iter().position(|c| c == n)
            {
                return Ok(proj.exprs[i].clone());
            }
            Compiler::new(g, params, &m.scope).compile(e)
        })
        .collect::<Result<Vec<_>>>()?;

    let mut c = Compiler {
        g,
        params,
        scope: &m.scope,
        aliases: &proj.names,
        aggs: Some(&mut aggs),
    };
    let having = s.having.as_ref().map(|h| c.compile(h)).transpose()?;
    let order = s
        .order_by
        .iter()
        .map(|(e, desc)| Ok((c.compile(e)?, *desc)))
        .collect::<Result<Vec<_>>>()?;
    let limit = count_arg(g, params, s.limit.as_ref(), "LIMIT")?;
    let offset = count_arg(g, params, s.offset.as_ref(), "OFFSET")?.unwrap_or(0);
    let aggregate = !aggs.is_empty() || !group_by.is_empty();
    if having.is_some() && !aggregate {
        return Err(Error::query("HAVING requires GROUP BY or an aggregate"));
    }

    // `ORDER BY similarity(x.emb, $v) DESC LIMIT k` can use the HNSW index.
    let hint = match (order.first(), limit) {
        (Some((key, desc)), Some(limit)) if !aggregate && !s.distinct && s.from.len() == 1 => {
            let key = match key {
                CExpr::Col(i) => &proj.exprs[*i],
                k => k,
            };
            vector_hint(g, key, *desc, limit + offset, m.conjuncts.len())
        }
        _ => None,
    };
    let matcher = plan(g, &m.scope, &m.patterns, m.conjuncts, hint.as_ref(), now);
    Ok(SelectPlan {
        scope: m.scope,
        matcher,
        proj,
        aggs,
        group_by,
        having,
        order,
        distinct: s.distinct,
        limit,
        offset,
    })
}

fn vector_hint(g: &Graph, key: &CExpr, desc: bool, k: usize, filters: usize) -> Option<VectorHint> {
    let CExpr::Call(f, args) = key else {
        return None;
    };
    let (metric, larger_is_closer) = f.metric()?;
    if desc != larger_is_closer {
        return None;
    }
    let ((CExpr::Prop { var, name, slots }, CExpr::Const(q))
    | (CExpr::Const(q), CExpr::Prop { var, name, slots })) = (args.first()?, args.get(1)?)
    else {
        return None;
    };
    let query = q.as_vector()?;
    let matches_metric = slots.iter().all(|(t, s)| {
        g.schema
            .get(*t)
            .is_some_and(|d| d.kind == Kind::Node && d.props[*s].metric == metric)
    });
    // Over-fetch when other filters may discard candidates.
    let k = if filters > 0 { k.saturating_mul(10) } else { k };
    matches_metric.then(|| VectorHint {
        var: *var,
        prop: name.clone(),
        query,
        k,
    })
}

#[derive(Debug)]
enum Acc {
    Count(i64),
    Sum {
        int: i64,
        float: f64,
        is_float: bool,
        any: bool,
    },
    Avg {
        sum: f64,
        n: u64,
    },
    Min(Option<Value>),
    Max(Option<Value>),
    Collect(Vec<Value>),
}

impl Acc {
    fn new(f: AggFn) -> Self {
        match f {
            AggFn::Count => Self::Count(0),
            AggFn::Sum => Self::Sum {
                int: 0,
                float: 0.0,
                is_float: false,
                any: false,
            },
            AggFn::Avg => Self::Avg { sum: 0.0, n: 0 },
            AggFn::Min => Self::Min(None),
            AggFn::Max => Self::Max(None),
            AggFn::Collect => Self::Collect(Vec::new()),
        }
    }

    fn add(&mut self, v: Value) -> Result<()> {
        if v.is_null() {
            return Ok(());
        }
        match self {
            Self::Count(n) => *n += 1,
            Self::Sum {
                int,
                float,
                is_float,
                any,
            } => {
                *any = true;
                match v {
                    Value::Int(i) if !*is_float => {
                        *int = int
                            .checked_add(i)
                            .ok_or_else(|| Error::query("integer overflow in sum()"))?;
                    }
                    v => {
                        let x = v.as_f64().ok_or_else(|| {
                            Error::ty(format!("sum() expects numbers, got {}", v.type_name()))
                        })?;
                        if !*is_float {
                            *is_float = true;
                            *float = *int as f64;
                        }
                        *float += x;
                    }
                }
            }
            Self::Avg { sum, n } => {
                *sum += v.as_f64().ok_or_else(|| {
                    Error::ty(format!("avg() expects numbers, got {}", v.type_name()))
                })?;
                *n += 1;
            }
            Self::Min(cur) => {
                if cur
                    .as_ref()
                    .is_none_or(|c| v.total_cmp(c) == Ordering::Less)
                {
                    *cur = Some(v);
                }
            }
            Self::Max(cur) => {
                if cur
                    .as_ref()
                    .is_none_or(|c| v.total_cmp(c) == Ordering::Greater)
                {
                    *cur = Some(v);
                }
            }
            Self::Collect(items) => items.push(v),
        }
        Ok(())
    }

    fn finish(self) -> Value {
        match self {
            Self::Count(n) => Value::Int(n),
            Self::Sum { any: false, .. } => Value::Null,
            Self::Sum {
                float,
                is_float: true,
                ..
            } => Value::Float(float),
            Self::Sum { int, .. } => Value::Int(int),
            Self::Avg { n: 0, .. } => Value::Null,
            Self::Avg { sum, n } => Value::Float(sum / n as f64),
            Self::Min(v) | Self::Max(v) => v.unwrap_or_default(),
            Self::Collect(items) => Value::List(items),
        }
    }
}

struct Group {
    first: Vec<Value>,
    accs: Vec<Acc>,
    seen: Vec<BTreeSet<OrdValue>>,
}

/// Orders `a` vs `b` for ORDER BY. NULLs always sort last, in either direction.
fn order_cmp(a: &[Value], b: &[Value], dirs: &[(CExpr, bool)]) -> Ordering {
    for ((x, y), (_, desc)) in a.iter().zip(b).zip(dirs) {
        let ord = match (x.is_null(), y.is_null()) {
            (true, true) => Ordering::Equal,
            (true, false) => Ordering::Greater,
            (false, true) => Ordering::Less,
            _ if *desc => x.total_cmp(y).reverse(),
            _ => x.total_cmp(y),
        };
        if ord != Ordering::Equal {
            return ord;
        }
    }
    Ordering::Equal
}

impl SelectPlan {
    /// Runs the query, returning column names and *unmaterialized* rows.
    pub(crate) fn run(&self, g: &Graph, now: i64) -> Result<(Vec<String>, Vec<Vec<Value>>)> {
        let width = self.scope.vars.len();
        let mut out: Vec<(Vec<Value>, Vec<Value>)> = Vec::new();
        let aggregate = !self.aggs.is_empty() || !self.group_by.is_empty();
        let early_stop = self.order.is_empty() && !self.distinct && !aggregate;
        let want = self.limit.map(|l| l.saturating_add(self.offset));

        let finish_row =
            |row: &[Value], aggs: &[Value]| -> Result<Option<(Vec<Value>, Vec<Value>)>> {
                let env = Env {
                    g,
                    row,
                    cols: &[],
                    aggs,
                    now,
                };
                let cols = self
                    .proj
                    .exprs
                    .iter()
                    .map(|e| eval(e, &env))
                    .collect::<Result<Vec<_>>>()?;
                let env = Env { cols: &cols, ..env };
                if let Some(h) = &self.having
                    && !truthy(&eval(h, &env)?)
                {
                    return Ok(None);
                }
                let keys = self
                    .order
                    .iter()
                    .map(|(e, _)| eval(e, &env))
                    .collect::<Result<Vec<_>>>()?;
                Ok(Some((cols, keys)))
            };

        if aggregate {
            let mut index: BTreeMap<Vec<OrdValue>, usize> = BTreeMap::new();
            let mut groups: Vec<Group> = Vec::new();
            self.matcher.run(g, width, now, &mut |row| {
                let env = Env::new(g, row, now);
                let key = self
                    .group_by
                    .iter()
                    .map(|e| eval(e, &env).map(OrdValue))
                    .collect::<Result<Vec<_>>>()?;
                let gi = *index.entry(key).or_insert_with(|| {
                    groups.push(Group {
                        first: row.to_vec(),
                        accs: self.aggs.iter().map(|a| Acc::new(a.func)).collect(),
                        seen: vec![BTreeSet::new(); self.aggs.len()],
                    });
                    groups.len() - 1
                });
                let group = &mut groups[gi];
                for (i, spec) in self.aggs.iter().enumerate() {
                    let v = match &spec.arg {
                        None => Value::Bool(true),
                        Some(e) => eval(e, &env)?,
                    };
                    if spec.distinct && !v.is_null() && !group.seen[i].insert(OrdValue(v.clone())) {
                        continue;
                    }
                    group.accs[i].add(v)?;
                }
                Ok(true)
            })?;
            if groups.is_empty() && self.group_by.is_empty() {
                // `SELECT count(*) FROM T` on an empty type still returns one row.
                groups.push(Group {
                    first: vec![Value::Null; width],
                    accs: self.aggs.iter().map(|a| Acc::new(a.func)).collect(),
                    seen: Vec::new(),
                });
            }
            for group in groups {
                let aggs: Vec<Value> = group.accs.into_iter().map(Acc::finish).collect();
                if let Some(r) = finish_row(&group.first, &aggs)? {
                    out.push(r);
                }
            }
        } else {
            self.matcher.run(g, width, now, &mut |row| {
                if let Some(r) = finish_row(row, &[])? {
                    out.push(r);
                }
                Ok(!(early_stop && want.is_some_and(|w| out.len() >= w)))
            })?;
        }

        if self.distinct {
            let mut seen = BTreeSet::new();
            out.retain(|(cols, _)| {
                seen.insert(cols.iter().cloned().map(OrdValue).collect::<Vec<_>>())
            });
        }
        if !self.order.is_empty() {
            out.sort_by(|a, b| order_cmp(&a.1, &b.1, &self.order));
        }
        let rows = out
            .into_iter()
            .skip(self.offset)
            .take(self.limit.unwrap_or(usize::MAX))
            .map(|(cols, _)| cols)
            .collect();
        Ok((self.proj.names.clone(), rows))
    }
}
