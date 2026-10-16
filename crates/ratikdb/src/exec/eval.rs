//! Expression evaluation.

use std::cmp::Ordering;

use crate::error::{Error, Result};
use crate::graph::Graph;
use crate::index::vector::dot;
use crate::rql::ast::BinOp;
use crate::value::Value;

use super::compile::{CExpr, Func, Meta};

/// Everything an expression may read while being evaluated.
#[derive(Clone, Copy)]
pub(crate) struct Env<'a> {
    pub g: &'a Graph,
    pub row: &'a [Value],
    pub cols: &'a [Value],
    pub aggs: &'a [Value],
    pub now: i64,
}

impl<'a> Env<'a> {
    pub(crate) fn new(g: &'a Graph, row: &'a [Value], now: i64) -> Self {
        Self { g, row, cols: &[], aggs: &[], now }
    }
}

pub(crate) fn now_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |d| i64::try_from(d.as_millis()).unwrap_or(i64::MAX))
}

/// SQL truthiness: only `TRUE` passes a filter.
pub(crate) fn truthy(v: &Value) -> bool {
    matches!(v, Value::Bool(true))
}

fn element_id(v: &Value) -> Option<u64> {
    match v {
        Value::Node(id) | Value::Edge(id) => Some(*id),
        _ => None,
    }
}

fn prop_by_name(g: &Graph, id: u64, name: &str) -> Value {
    let Some((ty, props)) = g.element(id) else {
        return Value::Null;
    };
    match name {
        "_id" => return Value::Int(id as i64),
        "_label" => return g.schema.get(ty).map_or(Value::Null, |d| Value::String(d.name.clone())),
        "_from" => return g.edges.get(&id).map_or(Value::Null, |e| Value::Int(e.from as i64)),
        "_to" => return g.edges.get(&id).map_or(Value::Null, |e| Value::Int(e.to as i64)),
        _ => {}
    }
    g.schema
        .get(ty)
        .and_then(|d| d.slot(name))
        .map_or(Value::Null, |s| props[s].clone())
}

fn like(text: &str, pattern: &str) -> bool {
    // Iterative wildcard match: `%` = any run, `_` = one char.
    let (t, p): (Vec<char>, Vec<char>) = (text.chars().collect(), pattern.chars().collect());
    let (mut ti, mut pi) = (0, 0);
    let (mut star, mut mark) = (None, 0);
    while ti < t.len() {
        if pi < p.len() && (p[pi] == '_' || p[pi] == t[ti]) {
            ti += 1;
            pi += 1;
        } else if pi < p.len() && p[pi] == '%' {
            star = Some(pi);
            mark = ti;
            pi += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ti = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '%')
}

fn arith(op: BinOp, a: &Value, b: &Value) -> Result<Value> {
    use Value::{Float, Int};
    if a.is_null() || b.is_null() {
        return Ok(Value::Null);
    }
    let overflow = || Error::query(format!("integer overflow in {a} {op:?} {b}"));
    Ok(match (op, a, b) {
        (BinOp::Add, Int(x), Int(y)) => Int(x.checked_add(*y).ok_or_else(overflow)?),
        (BinOp::Sub, Int(x), Int(y)) => Int(x.checked_sub(*y).ok_or_else(overflow)?),
        (BinOp::Mul, Int(x), Int(y)) => Int(x.checked_mul(*y).ok_or_else(overflow)?),
        (BinOp::Div | BinOp::Mod, Int(_), Int(0)) => return Err(Error::query("division by zero")),
        (BinOp::Div, Int(x), Int(y)) => Int(x / y),
        (BinOp::Mod, Int(x), Int(y)) => Int(x % y),
        (BinOp::Add, Value::String(x), Value::String(y)) => Value::String(format!("{x}{y}")),
        (BinOp::Add, Value::List(x), Value::List(y)) => Value::List(x.iter().chain(y).cloned().collect()),
        (op, a, b) => {
            let (Some(x), Some(y)) = (a.as_f64(), b.as_f64()) else {
                return Err(Error::ty(format!("cannot apply {op:?} to {} and {}", a.type_name(), b.type_name())));
            };
            Float(match op {
                BinOp::Add => x + y,
                BinOp::Sub => x - y,
                BinOp::Mul => x * y,
                BinOp::Div => x / y,
                BinOp::Mod => x % y,
                _ => unreachable!("arith is only called with arithmetic operators"),
            })
        }
    })
}

fn vectors(a: &Value, b: &Value, f: &str) -> Result<Option<(Vec<f32>, Vec<f32>)>> {
    if a.is_null() || b.is_null() {
        return Ok(None);
    }
    let (Some(x), Some(y)) = (a.as_vector(), b.as_vector()) else {
        return Err(Error::ty(format!("{f}() needs two vectors")));
    };
    if x.len() != y.len() {
        return Err(Error::ty(format!("{f}() got vectors of different lengths ({} and {})", x.len(), y.len())));
    }
    Ok(Some((x, y)))
}

pub(crate) fn eval(e: &CExpr, env: &Env<'_>) -> Result<Value> {
    Ok(match e {
        CExpr::Const(v) => v.clone(),
        CExpr::Var(i) => env.row[*i].clone(),
        CExpr::Col(i) => env.cols.get(*i).cloned().unwrap_or_default(),
        CExpr::Agg(i) => env.aggs.get(*i).cloned().unwrap_or_default(),
        CExpr::Prop { var, slots, .. } => {
            let Some(id) = element_id(&env.row[*var]) else {
                return Ok(Value::Null);
            };
            let Some((ty, props)) = env.g.element(id) else {
                return Ok(Value::Null);
            };
            slots.iter().find(|(t, _)| *t == ty).map_or(Value::Null, |(_, s)| props[*s].clone())
        }
        CExpr::Meta { var, meta } => {
            let Some(id) = element_id(&env.row[*var]) else {
                return Ok(Value::Null);
            };
            let name = match meta {
                Meta::Id => "_id",
                Meta::Label => "_label",
                Meta::From => "_from",
                Meta::To => "_to",
            };
            prop_by_name(env.g, id, name)
        }
        CExpr::Field(base, name) => match eval(base, env)? {
            Value::Map(mut m) => m.remove(name).unwrap_or_default(),
            Value::Node(id) | Value::Edge(id) => prop_by_name(env.g, id, name),
            Value::Null => Value::Null,
            other => return Err(Error::ty(format!("cannot read .{name} of {}", other.type_name()))),
        },
        CExpr::Index(base, idx) => match (eval(base, env)?, eval(idx, env)?) {
            (Value::List(items), Value::Int(i)) => {
                let len = items.len() as i64;
                let i = if i < 0 { len + i } else { i };
                usize::try_from(i).ok().and_then(|i| items.into_iter().nth(i)).unwrap_or_default()
            }
            (Value::Vector(items), Value::Int(i)) => {
                usize::try_from(i).ok().and_then(|i| items.get(i).copied()).map_or(Value::Null, Value::from)
            }
            (Value::Map(mut m), Value::String(k)) => m.remove(&k).unwrap_or_default(),
            (Value::Null, _) | (_, Value::Null) => Value::Null,
            (b, i) => return Err(Error::ty(format!("cannot index {} with {}", b.type_name(), i.type_name()))),
        },
        CExpr::Not(a) => match eval(a, env)? {
            Value::Bool(b) => Value::Bool(!b),
            Value::Null => Value::Null,
            v => return Err(Error::ty(format!("NOT expects BOOL, got {}", v.type_name()))),
        },
        CExpr::Neg(a) => match eval(a, env)? {
            Value::Int(i) => Value::Int(i.checked_neg().ok_or_else(|| Error::query("integer overflow"))?),
            Value::Float(f) => Value::Float(-f),
            Value::Null => Value::Null,
            v => return Err(Error::ty(format!("cannot negate {}", v.type_name()))),
        },
        CExpr::Bin(BinOp::And, a, b) => {
            // Three-valued logic with short-circuit on FALSE.
            let l = eval(a, env)?;
            if l == Value::Bool(false) {
                return Ok(l);
            }
            let r = eval(b, env)?;
            match (l, r) {
                (_, Value::Bool(false)) => Value::Bool(false),
                (Value::Bool(true), Value::Bool(true)) => Value::Bool(true),
                _ => Value::Null,
            }
        }
        CExpr::Bin(BinOp::Or, a, b) => {
            let l = eval(a, env)?;
            if l == Value::Bool(true) {
                return Ok(l);
            }
            let r = eval(b, env)?;
            match (l, r) {
                (_, Value::Bool(true)) => Value::Bool(true),
                (Value::Bool(false), Value::Bool(false)) => Value::Bool(false),
                _ => Value::Null,
            }
        }
        CExpr::Bin(op, a, b) => {
            let (l, r) = (eval(a, env)?, eval(b, env)?);
            match op {
                BinOp::Eq => l.sql_eq(&r).map_or(Value::Null, Value::Bool),
                BinOp::Ne => l.sql_eq(&r).map_or(Value::Null, |b| Value::Bool(!b)),
                BinOp::Lt | BinOp::Le | BinOp::Gt | BinOp::Ge => l.sql_cmp(&r).map_or(Value::Null, |o| {
                    Value::Bool(match op {
                        BinOp::Lt => o == Ordering::Less,
                        BinOp::Le => o != Ordering::Greater,
                        BinOp::Gt => o == Ordering::Greater,
                        _ => o != Ordering::Less,
                    })
                }),
                BinOp::Concat => match (&l, &r) {
                    (Value::Null, _) | (_, Value::Null) => Value::Null,
                    (Value::List(_), Value::List(_)) => arith(BinOp::Add, &l, &r)?,
                    _ => Value::String(format!("{l}{r}")),
                },
                _ => arith(*op, &l, &r)?,
            }
        }
        CExpr::Call(f, args) => call(*f, args, env)?,
        CExpr::Text { var, prop, terms, matches } => {
            let Some(id) = element_id(&env.row[*var]) else {
                return Ok(Value::Null);
            };
            let Some(ty) = env.g.type_of(id) else {
                return Ok(Value::Null);
            };
            let Some(idx) = env.g.idx.text(ty, prop) else {
                return Ok(if *matches { Value::Bool(false) } else { Value::Float(0.0) });
            };
            if *matches { Value::Bool(idx.matches(id, terms)) } else { Value::Float(idx.score(id, terms)) }
        }
        CExpr::List(items) => Value::List(items.iter().map(|i| eval(i, env)).collect::<Result<_>>()?),
        CExpr::Map(items) => {
            Value::Map(items.iter().map(|(k, v)| Ok((k.clone(), eval(v, env)?))).collect::<Result<_>>()?)
        }
        CExpr::Case { operand, whens, else_ } => {
            let subject = operand.as_ref().map(|o| eval(o, env)).transpose()?;
            for (w, t) in whens {
                let w = eval(w, env)?;
                let hit = match &subject {
                    Some(s) => s.sql_eq(&w) == Some(true),
                    None => truthy(&w),
                };
                if hit {
                    return eval(t, env);
                }
            }
            match else_ {
                Some(e) => eval(e, env)?,
                None => Value::Null,
            }
        }
        CExpr::IsNull(a, negated) => Value::Bool(eval(a, env)?.is_null() != *negated),
        CExpr::In(a, list, negated) => {
            let needle = eval(a, env)?;
            let haystack = match eval(list, env)? {
                Value::List(items) => items,
                Value::Null => return Ok(Value::Null),
                v => return Err(Error::ty(format!("IN expects a list, got {}", v.type_name()))),
            };
            if needle.is_null() {
                return Ok(Value::Null);
            }
            let found = haystack.iter().any(|h| needle.sql_eq(h) == Some(true));
            Value::Bool(found != *negated)
        }
        CExpr::Like { expr, pattern, ci, negated } => match (eval(expr, env)?, eval(pattern, env)?) {
            (Value::String(s), Value::String(p)) => {
                let hit = if *ci { like(&s.to_lowercase(), &p.to_lowercase()) } else { like(&s, &p) };
                Value::Bool(hit != *negated)
            }
            (Value::Null, _) | (_, Value::Null) => Value::Null,
            (a, _) => return Err(Error::ty(format!("LIKE expects STRING, got {}", a.type_name()))),
        },
        CExpr::Between(a, lo, hi, negated) => {
            let (v, lo, hi) = (eval(a, env)?, eval(lo, env)?, eval(hi, env)?);
            match (v.sql_cmp(&lo), v.sql_cmp(&hi)) {
                (Some(l), Some(h)) => Value::Bool((l != Ordering::Less && h != Ordering::Greater) != *negated),
                _ => Value::Null,
            }
        }
    })
}

fn call(f: Func, args: &[CExpr], env: &Env<'_>) -> Result<Value> {
    if f == Func::Coalesce {
        for a in args {
            let v = eval(a, env)?;
            if !v.is_null() {
                return Ok(v);
            }
        }
        return Ok(Value::Null);
    }
    let vals: Vec<Value> = args.iter().map(|a| eval(a, env)).collect::<Result<_>>()?;
    let num = |i: usize, name: &str| -> Result<Option<f64>> {
        match &vals[i] {
            Value::Null => Ok(None),
            v => v.as_f64().map(Some).ok_or_else(|| Error::ty(format!("{name}() expects a number, got {}", v.type_name()))),
        }
    };
    let string = |i: usize, name: &str| -> Result<Option<String>> {
        match &vals[i] {
            Value::Null => Ok(None),
            Value::String(s) => Ok(Some(s.clone())),
            v => Err(Error::ty(format!("{name}() expects a string, got {}", v.type_name()))),
        }
    };
    let degree = |out: bool, inc: bool| -> Value {
        match &vals[0] {
            Value::Node(id) => env.g.nodes.get(id).map_or(Value::Null, |n| {
                Value::Int((if out { n.out.len() } else { 0 } + if inc { n.inc.len() } else { 0 }) as i64)
            }),
            _ => Value::Null,
        }
    };
    Ok(match f {
        Func::Coalesce => unreachable!("handled above"),
        Func::Now => Value::Int(env.now),
        Func::Id => element_id(&vals[0]).map_or(Value::Null, |id| Value::Int(id as i64)),
        Func::Label => element_id(&vals[0]).map_or(Value::Null, |id| prop_by_name(env.g, id, "_label")),
        Func::Lower => string(0, "lower")?.map_or(Value::Null, |s| Value::String(s.to_lowercase())),
        Func::Upper => string(0, "upper")?.map_or(Value::Null, |s| Value::String(s.to_uppercase())),
        Func::Trim => string(0, "trim")?.map_or(Value::Null, |s| Value::String(s.trim().to_owned())),
        Func::Length => match &vals[0] {
            Value::String(s) => Value::Int(s.chars().count() as i64),
            Value::List(l) => Value::Int(l.len() as i64),
            Value::Vector(v) => Value::Int(v.len() as i64),
            Value::Map(m) => Value::Int(m.len() as i64),
            Value::Null => Value::Null,
            v => return Err(Error::ty(format!("length() expects a string or list, got {}", v.type_name()))),
        },
        Func::Abs => match &vals[0] {
            Value::Int(i) => Value::Int(i.checked_abs().ok_or_else(|| Error::query("integer overflow"))?),
            _ => num(0, "abs")?.map_or(Value::Null, |x| Value::Float(x.abs())),
        },
        Func::Round => {
            let digits = if vals.len() > 1 { num(1, "round")?.unwrap_or(0.0) } else { 0.0 };
            match &vals[0] {
                Value::Int(i) => Value::Int(*i),
                _ => num(0, "round")?.map_or(Value::Null, |x| {
                    let p = 10f64.powi(digits as i32);
                    Value::Float((x * p).round() / p)
                }),
            }
        }
        Func::Floor => num(0, "floor")?.map_or(Value::Null, |x| Value::Int(x.floor() as i64)),
        Func::Ceil => num(0, "ceil")?.map_or(Value::Null, |x| Value::Int(x.ceil() as i64)),
        Func::Sqrt => num(0, "sqrt")?.map_or(Value::Null, |x| Value::Float(x.sqrt())),
        Func::Pow => match (num(0, "pow")?, num(1, "pow")?) {
            (Some(a), Some(b)) => Value::Float(a.powf(b)),
            _ => Value::Null,
        },
        Func::Similarity => vectors(&vals[0], &vals[1], "similarity")?.map_or(Value::Null, |(a, b)| {
            let (na, nb) = (dot(&a, &a).sqrt(), dot(&b, &b).sqrt());
            Value::Float(if na == 0.0 || nb == 0.0 { 0.0 } else { f64::from(dot(&a, &b) / (na * nb)) })
        }),
        Func::Distance => vectors(&vals[0], &vals[1], "distance")?.map_or(Value::Null, |(a, b)| {
            Value::Float(f64::from(a.iter().zip(&b).map(|(x, y)| (x - y) * (x - y)).sum::<f32>().sqrt()))
        }),
        Func::Dot => vectors(&vals[0], &vals[1], "dot")?.map_or(Value::Null, |(a, b)| Value::Float(f64::from(dot(&a, &b)))),
        Func::ToString => match &vals[0] {
            Value::Null => Value::Null,
            v => Value::String(v.to_string()),
        },
        Func::ToInt => match &vals[0] {
            Value::Int(i) => Value::Int(*i),
            Value::Float(f) => Value::Int(*f as i64),
            Value::Bool(b) => Value::Int(i64::from(*b)),
            Value::String(s) => s.trim().parse().map(Value::Int).unwrap_or(Value::Null),
            _ => Value::Null,
        },
        Func::ToFloat => match &vals[0] {
            Value::String(s) => s.trim().parse().map(Value::Float).unwrap_or(Value::Null),
            v => v.as_f64().map_or(Value::Null, Value::Float),
        },
        Func::Substring => match (string(0, "substring")?, num(1, "substring")?) {
            (Some(s), Some(start)) => {
                // 1-based like SQL.
                let start = (start as i64 - 1).max(0) as usize;
                let len = if vals.len() > 2 { num(2, "substring")?.map(|l| l.max(0.0) as usize) } else { None };
                let it = s.chars().skip(start);
                Value::String(match len {
                    Some(l) => it.take(l).collect(),
                    None => it.collect(),
                })
            }
            _ => Value::Null,
        },
        Func::StartsWith | Func::EndsWith | Func::Contains => match (&vals[0], &vals[1]) {
            (Value::String(a), Value::String(b)) => Value::Bool(match f {
                Func::StartsWith => a.starts_with(b.as_str()),
                Func::EndsWith => a.ends_with(b.as_str()),
                _ => a.contains(b.as_str()),
            }),
            (Value::List(items), needle) if f == Func::Contains => {
                Value::Bool(items.iter().any(|i| i.sql_eq(needle) == Some(true)))
            }
            _ => Value::Null,
        },
        Func::Replace => match (string(0, "replace")?, string(1, "replace")?, string(2, "replace")?) {
            (Some(s), Some(from), Some(to)) => Value::String(s.replace(&from, &to)),
            _ => Value::Null,
        },
        Func::Split => match (string(0, "split")?, string(1, "split")?) {
            (Some(s), Some(sep)) => Value::List(s.split(sep.as_str()).map(Value::from).collect()),
            _ => Value::Null,
        },
        Func::Degree => degree(true, true),
        Func::OutDegree => degree(true, false),
        Func::InDegree => degree(false, true),
        Func::Keys => match &vals[0] {
            Value::Map(m) => Value::List(m.keys().map(|k| Value::from(k.as_str())).collect()),
            Value::Node(id) | Value::Edge(id) => env
                .g
                .type_of(*id)
                .and_then(|t| env.g.schema.get(t))
                .map_or(Value::Null, |d| Value::List(d.props.iter().map(|p| Value::from(p.name.as_str())).collect())),
            _ => Value::Null,
        },
        Func::Greatest | Func::Least => {
            let want = if f == Func::Greatest { Ordering::Greater } else { Ordering::Less };
            vals.iter()
                .filter(|v| !v.is_null())
                .fold(None::<&Value>, |best, v| match best {
                    Some(b) if v.total_cmp(b) != want => Some(b),
                    _ => Some(v),
                })
                .cloned()
                .unwrap_or_default()
        }
    })
}

#[cfg(test)]
mod tests {
    use super::like;

    #[test]
    fn like_wildcards() {
        assert!(like("Alice", "A%"));
        assert!(like("Alice", "%li%"));
        assert!(like("Alice", "A_ice"));
        assert!(!like("Alice", "B%"));
        assert!(like("", "%"));
        assert!(!like("abc", "ab"));
    }
}
