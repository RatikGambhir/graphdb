//! Name resolution and type checking: turns syntax-level [`Expr`]s into
//! [`CExpr`]s whose every variable and property has been checked against the
//! schema.

use std::collections::HashMap;

use crate::error::{Error, Result, hint};
use crate::graph::Graph;
use crate::index::text::tokenize;
use crate::rql::ast::{BinOp, Expr, UnOp};
use crate::schema::{Kind, Metric, TypeId};
use crate::value::Value;

/// Query parameters (`$name` → value).
pub type Params = HashMap<String, Value>;

#[derive(Debug, Clone, PartialEq)]
pub(crate) enum VarKind {
    Node(Vec<TypeId>),
    Edge(Vec<TypeId>),
    /// Variable-length edge (bound to nothing; cannot be referenced).
    Path,
}

#[derive(Debug, Clone)]
pub(crate) struct Var {
    pub name: String,
    pub kind: VarKind,
    pub anon: bool,
}

#[derive(Debug, Clone, Default)]
pub(crate) struct Scope {
    pub vars: Vec<Var>,
}

impl Scope {
    pub(crate) fn lookup(&self, name: &str) -> Option<usize> {
        self.vars.iter().position(|v| !v.anon && v.name == name)
    }

    pub(crate) fn add(&mut self, name: Option<&str>, kind: VarKind) -> usize {
        let anon = name.is_none();
        let name = name.map_or_else(|| format!("_{}", self.vars.len()), str::to_owned);
        self.vars.push(Var { name, kind, anon });
        self.vars.len() - 1
    }

    pub(crate) fn named(&self) -> impl Iterator<Item = (usize, &Var)> {
        self.vars.iter().enumerate().filter(|(_, v)| !v.anon)
    }
}

/// `_id`, `_label`, `_from`, `_to` pseudo-properties.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Meta {
    Id,
    Label,
    From,
    To,
}

impl Meta {
    fn parse(name: &str) -> Option<Self> {
        match name {
            "_id" => Some(Self::Id),
            "_label" => Some(Self::Label),
            "_from" => Some(Self::From),
            "_to" => Some(Self::To),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Func {
    Id,
    Label,
    Lower,
    Upper,
    Length,
    Coalesce,
    Abs,
    Round,
    Floor,
    Ceil,
    Sqrt,
    Pow,
    Now,
    Similarity,
    Distance,
    Dot,
    ToString,
    ToInt,
    ToFloat,
    Substring,
    Trim,
    StartsWith,
    EndsWith,
    Contains,
    Replace,
    Split,
    Degree,
    OutDegree,
    InDegree,
    Keys,
    Greatest,
    Least,
}

impl Func {
    fn resolve(name: &str) -> Option<(Self, usize, usize)> {
        let f = match name {
            "id" => (Self::Id, 1, 1),
            "label" | "type" => (Self::Label, 1, 1),
            "lower" | "lowercase" => (Self::Lower, 1, 1),
            "upper" | "uppercase" => (Self::Upper, 1, 1),
            "length" | "len" | "size" | "char_length" | "cardinality" => (Self::Length, 1, 1),
            "coalesce" | "ifnull" => (Self::Coalesce, 1, usize::MAX),
            "abs" => (Self::Abs, 1, 1),
            "round" => (Self::Round, 1, 2),
            "floor" => (Self::Floor, 1, 1),
            "ceil" | "ceiling" => (Self::Ceil, 1, 1),
            "sqrt" => (Self::Sqrt, 1, 1),
            "pow" | "power" => (Self::Pow, 2, 2),
            "now" => (Self::Now, 0, 0),
            "similarity" | "cosine_similarity" => (Self::Similarity, 2, 2),
            "distance" | "euclidean_distance" | "l2_distance" => (Self::Distance, 2, 2),
            "dot" | "inner_product" => (Self::Dot, 2, 2),
            "str" | "to_string" | "tostring" => (Self::ToString, 1, 1),
            "int" | "to_int" | "toint" | "integer" => (Self::ToInt, 1, 1),
            "float" | "to_float" | "tofloat" => (Self::ToFloat, 1, 1),
            "substring" | "substr" => (Self::Substring, 2, 3),
            "trim" => (Self::Trim, 1, 1),
            "starts_with" | "startswith" => (Self::StartsWith, 2, 2),
            "ends_with" | "endswith" => (Self::EndsWith, 2, 2),
            "contains" => (Self::Contains, 2, 2),
            "replace" => (Self::Replace, 3, 3),
            "split" => (Self::Split, 2, 2),
            "degree" => (Self::Degree, 1, 1),
            "out_degree" => (Self::OutDegree, 1, 1),
            "in_degree" => (Self::InDegree, 1, 1),
            "keys" => (Self::Keys, 1, 1),
            "greatest" => (Self::Greatest, 1, usize::MAX),
            "least" => (Self::Least, 1, usize::MAX),
            _ => return None,
        };
        Some(f)
    }

    /// The vector metric this function corresponds to, and whether larger is closer.
    pub(crate) fn metric(self) -> Option<(Metric, bool)> {
        match self {
            Self::Similarity => Some((Metric::Cosine, true)),
            Self::Distance => Some((Metric::Euclidean, false)),
            Self::Dot => Some((Metric::Dot, true)),
            _ => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum AggFn {
    Count,
    Sum,
    Avg,
    Min,
    Max,
    Collect,
}

#[derive(Debug, Clone)]
pub(crate) struct AggSpec {
    pub func: AggFn,
    pub arg: Option<CExpr>,
    pub distinct: bool,
}

/// A resolved, schema-checked expression.
#[derive(Debug, Clone)]
pub(crate) enum CExpr {
    Const(Value),
    Var(usize),
    /// Property of a node/edge variable; `slots` maps each possible type to
    /// the property's storage slot.
    Prop {
        var: usize,
        name: String,
        slots: Vec<(TypeId, usize)>,
    },
    Meta {
        var: usize,
        meta: Meta,
    },
    /// Dynamic `.field` access on a computed value (maps, nodes from lists).
    Field(Box<CExpr>, String),
    Index(Box<CExpr>, Box<CExpr>),
    Not(Box<CExpr>),
    Neg(Box<CExpr>),
    Bin(BinOp, Box<CExpr>, Box<CExpr>),
    Call(Func, Vec<CExpr>),
    /// BM25 score (or a boolean match) of a SEARCHABLE property.
    Text {
        var: usize,
        prop: String,
        terms: Vec<String>,
        matches: bool,
    },
    Agg(usize),
    Col(usize),
    List(Vec<CExpr>),
    Map(Vec<(String, CExpr)>),
    Case {
        operand: Option<Box<CExpr>>,
        whens: Vec<(CExpr, CExpr)>,
        else_: Option<Box<CExpr>>,
    },
    IsNull(Box<CExpr>, bool),
    In(Box<CExpr>, Box<CExpr>, bool),
    Like {
        expr: Box<CExpr>,
        pattern: Box<CExpr>,
        ci: bool,
        negated: bool,
    },
    Between(Box<CExpr>, Box<CExpr>, Box<CExpr>, bool),
}

impl CExpr {
    /// Every variable the expression reads.
    pub(crate) fn vars(&self, out: &mut Vec<usize>) {
        match self {
            Self::Var(v)
            | Self::Prop { var: v, .. }
            | Self::Meta { var: v, .. }
            | Self::Text { var: v, .. } => {
                if !out.contains(v) {
                    out.push(*v);
                }
            }
            Self::Const(_) | Self::Agg(_) | Self::Col(_) => {}
            Self::Field(e, _) | Self::Not(e) | Self::Neg(e) | Self::IsNull(e, _) => e.vars(out),
            Self::Index(a, b) | Self::Bin(_, a, b) | Self::In(a, b, _) => {
                a.vars(out);
                b.vars(out);
            }
            Self::Like { expr, pattern, .. } => {
                expr.vars(out);
                pattern.vars(out);
            }
            Self::Between(a, b, c, _) => {
                a.vars(out);
                b.vars(out);
                c.vars(out);
            }
            Self::Call(_, args) | Self::List(args) => args.iter().for_each(|a| a.vars(out)),
            Self::Map(items) => items.iter().for_each(|(_, a)| a.vars(out)),
            Self::Case {
                operand,
                whens,
                else_,
            } => {
                if let Some(o) = operand {
                    o.vars(out);
                }
                for (w, t) in whens {
                    w.vars(out);
                    t.vars(out);
                }
                if let Some(e) = else_ {
                    e.vars(out);
                }
            }
        }
    }

    /// `true` when the value does not depend on any row (safe to fold).
    pub(crate) fn is_const(&self) -> bool {
        let mut vars = Vec::new();
        self.vars(&mut vars);
        vars.is_empty() && !self.has_row_refs()
    }

    fn has_row_refs(&self) -> bool {
        match self {
            Self::Agg(_) | Self::Col(_) => true,
            Self::Const(_)
            | Self::Var(_)
            | Self::Prop { .. }
            | Self::Meta { .. }
            | Self::Text { .. }
            | Self::Call(Func::Now, _) => false,
            Self::Field(e, _) | Self::Not(e) | Self::Neg(e) | Self::IsNull(e, _) => {
                e.has_row_refs()
            }
            Self::Index(a, b) | Self::Bin(_, a, b) | Self::In(a, b, _) => {
                a.has_row_refs() || b.has_row_refs()
            }
            Self::Like { expr, pattern, .. } => expr.has_row_refs() || pattern.has_row_refs(),
            Self::Between(a, b, c, _) => a.has_row_refs() || b.has_row_refs() || c.has_row_refs(),
            Self::Call(_, args) | Self::List(args) => args.iter().any(Self::has_row_refs),
            Self::Map(items) => items.iter().any(|(_, a)| a.has_row_refs()),
            Self::Case {
                operand,
                whens,
                else_,
            } => {
                operand.as_ref().is_some_and(|o| o.has_row_refs())
                    || whens
                        .iter()
                        .any(|(w, t)| w.has_row_refs() || t.has_row_refs())
                    || else_.as_ref().is_some_and(|e| e.has_row_refs())
            }
        }
    }

    /// Splits a predicate into its top-level `AND` conjuncts.
    pub(crate) fn conjuncts(self, out: &mut Vec<Self>) {
        match self {
            Self::Bin(BinOp::And, a, b) => {
                a.conjuncts(out);
                b.conjuncts(out);
            }
            other => out.push(other),
        }
    }
}

/// Resolves names in one scope.
pub(crate) struct Compiler<'a> {
    pub g: &'a Graph,
    pub params: &'a Params,
    pub scope: &'a Scope,
    /// Output column names visible to ORDER BY / HAVING.
    pub aliases: &'a [String],
    /// Where aggregate calls are collected; `None` forbids aggregates.
    pub aggs: Option<&'a mut Vec<AggSpec>>,
}

impl<'a> Compiler<'a> {
    pub(crate) fn new(g: &'a Graph, params: &'a Params, scope: &'a Scope) -> Self {
        Self {
            g,
            params,
            scope,
            aliases: &[],
            aggs: None,
        }
    }

    fn types_of(&self, var: usize) -> (Kind, &[TypeId]) {
        match &self.scope.vars[var].kind {
            VarKind::Node(t) => (Kind::Node, t),
            VarKind::Edge(t) => (Kind::Edge, t),
            VarKind::Path => (Kind::Edge, &[]),
        }
    }

    /// Resolves `var.prop`, checking the property exists on the variable's types.
    pub(crate) fn prop(&self, var: usize, name: &str) -> Result<CExpr> {
        let v = &self.scope.vars[var];
        if v.kind == VarKind::Path {
            return Err(Error::query(format!(
                "'{}' is a variable-length edge and has no properties",
                v.name
            )));
        }
        if let Some(meta) = Meta::parse(name) {
            if matches!(meta, Meta::From | Meta::To) && matches!(v.kind, VarKind::Node(_)) {
                return Err(Error::schema(format!(
                    "{name} only exists on edges, and '{}' is a node",
                    v.name
                )));
            }
            return Ok(CExpr::Meta { var, meta });
        }
        let (_, types) = self.types_of(var);
        let slots: Vec<(TypeId, usize)> = types
            .iter()
            .filter_map(|t| {
                self.g
                    .schema
                    .get(*t)
                    .and_then(|d| d.slot(name).map(|s| (*t, s)))
            })
            .collect();
        if slots.is_empty() {
            let defs: Vec<_> = types.iter().filter_map(|t| self.g.schema.get(*t)).collect();
            let names: Vec<&str> = defs
                .iter()
                .flat_map(|d| d.props.iter().map(|p| p.name.as_str()))
                .collect();
            let owner = match defs.as_slice() {
                [one] => one.name.clone(),
                _ => format!("'{}'", v.name),
            };
            return Err(Error::schema(format!(
                "{owner} has no property '{name}'{}",
                hint(name, names)
            )));
        }
        Ok(CExpr::Prop {
            var,
            name: name.to_owned(),
            slots,
        })
    }

    /// Resolves a bare name: alias → variable → property of the one variable
    /// that declares it.
    fn ident(&self, name: &str) -> Result<CExpr> {
        if let Some(i) = self.aliases.iter().position(|a| a == name) {
            return Ok(CExpr::Col(i));
        }
        if let Some(v) = self.scope.lookup(name) {
            return Ok(CExpr::Var(v));
        }
        let owners: Vec<usize> = (0..self.scope.vars.len())
            .filter(|&v| {
                if self.scope.vars[v].kind == VarKind::Path {
                    return false;
                }
                if Meta::parse(name).is_some() {
                    return true;
                }
                let (_, types) = self.types_of(v);
                types.iter().any(|t| {
                    self.g
                        .schema
                        .get(*t)
                        .is_some_and(|d| d.slot(name).is_some())
                })
            })
            .collect();
        match owners.as_slice() {
            [v] => self.prop(*v, name),
            [] => {
                let mut names: Vec<&str> =
                    self.scope.named().map(|(_, v)| v.name.as_str()).collect();
                names.extend(self.aliases.iter().map(String::as_str));
                for v in 0..self.scope.vars.len() {
                    let (_, types) = self.types_of(v);
                    for t in types {
                        if let Some(d) = self.g.schema.get(*t) {
                            names.extend(d.props.iter().map(|p| p.name.as_str()));
                        }
                    }
                }
                Err(Error::schema(format!(
                    "unknown column or variable '{name}'{}",
                    hint(name, names)
                )))
            }
            many => {
                let vars: Vec<String> = many
                    .iter()
                    .map(|v| format!("{}.{name}", self.scope.vars[*v].name))
                    .collect();
                Err(Error::query(format!(
                    "'{name}' is ambiguous — write one of: {}",
                    vars.join(", ")
                )))
            }
        }
    }

    pub(crate) fn compile(&mut self, e: &Expr) -> Result<CExpr> {
        Ok(match e {
            Expr::Lit(v) => CExpr::Const(v.clone()),
            Expr::Param(p) => CExpr::Const(
                self.params
                    .get(p)
                    .cloned()
                    .ok_or_else(|| Error::query(format!("missing value for parameter ${p}")))?,
            ),
            Expr::Ident(n) => self.ident(n)?,
            Expr::Prop(base, name) => match base.as_ref() {
                Expr::Ident(v) if self.scope.lookup(v).is_some() => {
                    let var = self.scope.lookup(v).unwrap_or_default();
                    self.prop(var, name)?
                }
                Expr::Ident(v) if !self.aliases.contains(v) => match self.ident(v) {
                    // `profile.rank`: a field inside a property of the only variable.
                    Ok(prop @ CExpr::Prop { .. }) => CExpr::Field(Box::new(prop), name.clone()),
                    _ => {
                        let mut names: Vec<&str> =
                            self.scope.named().map(|(_, v)| v.name.as_str()).collect();
                        names.extend(self.aliases.iter().map(String::as_str));
                        return Err(Error::query(format!(
                            "unknown variable '{v}'{}",
                            hint(v, names)
                        )));
                    }
                },
                other => CExpr::Field(Box::new(self.compile(other)?), name.clone()),
            },
            Expr::Index(a, b) => {
                CExpr::Index(Box::new(self.compile(a)?), Box::new(self.compile(b)?))
            }
            Expr::Unary(UnOp::Not, a) => CExpr::Not(Box::new(self.compile(a)?)),
            Expr::Unary(UnOp::Neg, a) => CExpr::Neg(Box::new(self.compile(a)?)),
            Expr::Binary(op, a, b) => {
                CExpr::Bin(*op, Box::new(self.compile(a)?), Box::new(self.compile(b)?))
            }
            Expr::List(items) => {
                let items: Vec<CExpr> = items
                    .iter()
                    .map(|i| self.compile(i))
                    .collect::<Result<_>>()?;
                // Fold constant lists (e.g. vector literals) so the planner can use them.
                match items
                    .iter()
                    .map(|i| {
                        if let CExpr::Const(v) = i {
                            Some(v.clone())
                        } else {
                            None
                        }
                    })
                    .collect()
                {
                    Some(values) => CExpr::Const(Value::List(values)),
                    None => CExpr::List(items),
                }
            }
            Expr::Map(items) => CExpr::Map(
                items
                    .iter()
                    .map(|(k, v)| Ok((k.clone(), self.compile(v)?)))
                    .collect::<Result<_>>()?,
            ),
            Expr::Case {
                operand,
                whens,
                else_,
            } => CExpr::Case {
                operand: operand
                    .as_ref()
                    .map(|o| self.compile(o).map(Box::new))
                    .transpose()?,
                whens: whens
                    .iter()
                    .map(|(w, t)| Ok((self.compile(w)?, self.compile(t)?)))
                    .collect::<Result<_>>()?,
                else_: else_
                    .as_ref()
                    .map(|e| self.compile(e).map(Box::new))
                    .transpose()?,
            },
            Expr::IsNull(a, neg) => CExpr::IsNull(Box::new(self.compile(a)?), *neg),
            Expr::In(a, b, neg) => {
                CExpr::In(Box::new(self.compile(a)?), Box::new(self.compile(b)?), *neg)
            }
            Expr::Like {
                expr,
                pattern,
                case_insensitive,
                negated,
            } => CExpr::Like {
                expr: Box::new(self.compile(expr)?),
                pattern: Box::new(self.compile(pattern)?),
                ci: *case_insensitive,
                negated: *negated,
            },
            Expr::Between(a, lo, hi, neg) => CExpr::Between(
                Box::new(self.compile(a)?),
                Box::new(self.compile(lo)?),
                Box::new(self.compile(hi)?),
                *neg,
            ),
            Expr::Func {
                name,
                args,
                distinct,
                star,
            } => self.func(name, args, *distinct, *star)?,
        })
    }

    fn func(&mut self, name: &str, args: &[Expr], distinct: bool, star: bool) -> Result<CExpr> {
        let agg = match name {
            "count" => Some(AggFn::Count),
            "sum" => Some(AggFn::Sum),
            "avg" | "mean" => Some(AggFn::Avg),
            "min" => Some(AggFn::Min),
            "max" => Some(AggFn::Max),
            "collect" | "array_agg" | "list" => Some(AggFn::Collect),
            _ => None,
        };
        if let Some(func) = agg {
            if star && func != AggFn::Count {
                return Err(Error::query(format!(
                    "{name}(*) is not valid; only count(*) is"
                )));
            }
            if !star && args.len() != 1 {
                return Err(Error::query(format!("{name}() takes exactly one argument")));
            }
            let arg = if star {
                None
            } else {
                let mut inner = Compiler {
                    g: self.g,
                    params: self.params,
                    scope: self.scope,
                    aliases: &[],
                    aggs: None,
                };
                Some(inner.compile(&args[0]).map_err(|e| match e {
                    Error::Query(m) if m.contains("aggregate") => {
                        Error::query("aggregates cannot be nested")
                    }
                    e => e,
                })?)
            };
            let Some(aggs) = self.aggs.as_deref_mut() else {
                return Err(Error::query(format!(
                    "aggregate {name}() is not allowed here (use it in SELECT, HAVING or ORDER BY)"
                )));
            };
            aggs.push(AggSpec {
                func,
                arg,
                distinct,
            });
            return Ok(CExpr::Agg(aggs.len() - 1));
        }
        if matches!(name, "bm25" | "matches" | "search_score") {
            return self.text_fn(name, args);
        }
        let Some((func, min, max)) = Func::resolve(name) else {
            return Err(Error::query(format!("unknown function {name}()")));
        };
        if star || distinct {
            return Err(Error::query(format!(
                "{name}() does not accept * or DISTINCT"
            )));
        }
        if args.len() < min || args.len() > max {
            let expected = match (min, max) {
                (a, b) if a == b => format!("{a}"),
                (a, usize::MAX) => format!("at least {a}"),
                (a, b) => format!("{a} to {b}"),
            };
            return Err(Error::query(format!(
                "{name}() takes {expected} arguments, got {}",
                args.len()
            )));
        }
        let args = args
            .iter()
            .map(|a| self.compile(a))
            .collect::<Result<_>>()?;
        Ok(CExpr::Call(func, args))
    }

    fn text_fn(&mut self, name: &str, args: &[Expr]) -> Result<CExpr> {
        let usage = || Error::query(format!("usage: {name}(var.property, 'search terms')"));
        let [target, query] = args else {
            return Err(usage());
        };
        let CExpr::Prop {
            var,
            name: prop,
            slots,
        } = self.compile(target)?
        else {
            return Err(usage());
        };
        for (ty, slot) in &slots {
            let def = self.g.schema.get(*ty).ok_or_else(usage)?;
            if !def.props[*slot].searchable {
                return Err(Error::schema(format!(
                    "{}.{prop} is not SEARCHABLE — run `CREATE SEARCH INDEX ON {}({prop})` first",
                    def.name, def.name
                )));
            }
        }
        let terms = match self.compile(query)? {
            CExpr::Const(Value::String(q)) => tokenize(&q),
            _ => {
                return Err(Error::query(format!(
                    "{name}() needs a constant search string or parameter"
                )));
            }
        };
        Ok(CExpr::Text {
            var,
            prop,
            terms,
            matches: name == "matches",
        })
    }
}
