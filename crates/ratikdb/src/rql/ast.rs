//! Syntax tree for RQL statements.

use std::fmt;

use crate::schema::{Kind, PropDef, PropType, literal};
use crate::value::Value;

/// A parsed RQL statement.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Statement {
    /// `CREATE NODE` / `CREATE EDGE`
    CreateType {
        /// Node or edge.
        kind: Kind,
        /// Label.
        name: String,
        /// `IF NOT EXISTS`
        if_not_exists: bool,
        /// Declared properties.
        props: Vec<PropDef>,
        /// Edge source types.
        from: Vec<String>,
        /// Edge target types.
        to: Vec<String>,
    },
    /// `DROP NODE` / `DROP EDGE`
    DropType {
        /// Node or edge.
        kind: Kind,
        /// Label.
        name: String,
        /// `IF EXISTS`
        if_exists: bool,
        /// `CASCADE` — delete all elements of the type first.
        cascade: bool,
    },
    /// `ALTER NODE|EDGE T ADD prop TYPE ...`
    AlterAdd {
        /// Node or edge.
        kind: Kind,
        /// Label.
        name: String,
        /// New property.
        prop: PropDef,
    },
    /// `ALTER NODE|EDGE T DROP prop`
    AlterDrop {
        /// Node or edge.
        kind: Kind,
        /// Label.
        name: String,
        /// Property to remove.
        prop: String,
    },
    /// `CREATE [UNIQUE|SEARCH] INDEX ON T(prop)`
    CreateIndex {
        /// Label.
        name: String,
        /// Property.
        prop: String,
        /// Which index.
        flavor: IndexFlavor,
    },
    /// `DROP INDEX ON T(prop)` — removes every index on the property.
    DropIndex {
        /// Label.
        name: String,
        /// Property.
        prop: String,
    },
    /// `CREATE [OR REPLACE] QUERY name($p TYPE, ...) AS <statement>`
    CreateQuery {
        /// Query name.
        name: String,
        /// `OR REPLACE`
        or_replace: bool,
        /// Typed parameters.
        params: Vec<(String, PropType)>,
        /// Body source text.
        body: String,
        /// Parsed body (validated at creation).
        stmt: Box<Statement>,
    },
    /// `DROP QUERY name`
    DropQuery {
        /// Query name.
        name: String,
        /// `IF EXISTS`
        if_exists: bool,
    },
    /// `CALL name(arg, ...)`
    Call {
        /// Query name.
        name: String,
        /// Positional arguments.
        args: Vec<Expr>,
    },
    /// `SHOW ...`
    Show(Show),
    /// `DESCRIBE T`
    Describe(String),
    /// `EXPLAIN <statement>`
    Explain(Box<Statement>),
    /// `SELECT ...`
    Select(Box<Select>),
    /// `INSERT (pattern) ... [FROM ...]`
    Insert(Box<Insert>),
    /// `INSERT INTO T (cols) VALUES ...` / `INSERT INTO T {..}, {..}`
    InsertRows(Box<InsertRows>),
    /// `UPDATE ... SET ...`
    Update(Box<Update>),
    /// `DELETE ... FROM ...`
    Delete(Box<Delete>),
}

impl Statement {
    /// `true` for statements that never modify the database.
    #[must_use]
    pub fn is_read_only(&self) -> bool {
        match self {
            Self::Select(_) | Self::Show(_) | Self::Describe(_) | Self::Explain(_) => true,
            Self::CreateType { .. }
            | Self::DropType { .. }
            | Self::AlterAdd { .. }
            | Self::AlterDrop { .. }
            | Self::CreateIndex { .. }
            | Self::DropIndex { .. }
            | Self::CreateQuery { .. }
            | Self::DropQuery { .. }
            | Self::Call { .. }
            | Self::Insert(_)
            | Self::InsertRows(_)
            | Self::Update(_)
            | Self::Delete(_) => false,
        }
    }
}

/// Kind of secondary index.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexFlavor {
    /// B-tree index (equality + range).
    BTree,
    /// Unique B-tree index.
    Unique,
    /// BM25 full-text index.
    Search,
}

/// `SHOW` targets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Show {
    /// Whole schema as DDL.
    Schema,
    /// Node types with counts.
    Nodes,
    /// Edge types with counts.
    Edges,
    /// Stored queries.
    Queries,
    /// Indexes.
    Indexes,
    /// Database statistics.
    Stats,
}

/// `SELECT` statement.
#[derive(Debug, Clone, Default)]
pub struct Select {
    /// `DISTINCT`
    pub distinct: bool,
    /// Projection list.
    pub items: Vec<SelectItem>,
    /// Graph patterns (comma separated).
    pub from: Vec<Pattern>,
    /// `WHERE`
    pub where_: Option<Expr>,
    /// `GROUP BY`
    pub group_by: Vec<Expr>,
    /// `HAVING`
    pub having: Option<Expr>,
    /// `ORDER BY` (expression, descending?)
    pub order_by: Vec<(Expr, bool)>,
    /// `LIMIT`
    pub limit: Option<Expr>,
    /// `OFFSET`
    pub offset: Option<Expr>,
}

/// One projected item.
#[derive(Debug, Clone)]
pub enum SelectItem {
    /// `*`
    Star,
    /// `expr [AS alias]`
    Expr {
        /// The expression.
        expr: Expr,
        /// Column name override.
        alias: Option<String>,
    },
}

/// A path pattern: `(a:User)-[:FOLLOWS]->(b)`.
#[derive(Debug, Clone, Default)]
pub struct Pattern {
    /// Node patterns; always one more than `rels`.
    pub nodes: Vec<NodePat>,
    /// Relationship patterns between consecutive nodes.
    pub rels: Vec<RelPat>,
}

/// `(var:Label {prop: value})`
#[derive(Debug, Clone, Default)]
pub struct NodePat {
    /// Variable name.
    pub var: Option<String>,
    /// Node type.
    pub label: Option<String>,
    /// Inline equality constraints / insert values.
    pub props: Vec<(String, Expr)>,
}

/// Direction of a relationship pattern.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Dir {
    /// `-[]->`
    Out,
    /// `<-[]-`
    In,
    /// `-[]-`
    Both,
}

/// `-[var:TYPE|OTHER*1..3 {prop: value}]->`
#[derive(Debug, Clone)]
pub struct RelPat {
    /// Variable name.
    pub var: Option<String>,
    /// Allowed edge types (empty = any).
    pub labels: Vec<String>,
    /// Direction.
    pub dir: Dir,
    /// Variable-length hop range.
    pub hops: Option<(u32, Option<u32>)>,
    /// Inline equality constraints / insert values.
    pub props: Vec<(String, Expr)>,
}

/// `INSERT pattern, ... [FROM pattern, ... [WHERE expr]] [RETURNING ...]`
#[derive(Debug, Clone, Default)]
pub struct Insert {
    /// Patterns to create.
    pub create: Vec<Pattern>,
    /// Patterns matching existing elements to connect.
    pub from: Vec<Pattern>,
    /// Filter for `from`.
    pub where_: Option<Expr>,
    /// `RETURNING`
    pub returning: Vec<SelectItem>,
}

/// `INSERT INTO Label (a, b) VALUES (1, 2), ...`
#[derive(Debug, Clone, Default)]
pub struct InsertRows {
    /// Node type.
    pub label: String,
    /// One list of `(prop, value)` per row.
    pub rows: Vec<Vec<(String, Expr)>>,
    /// `RETURNING`
    pub returning: Vec<SelectItem>,
}

/// `UPDATE pattern SET var.prop = expr, ... [WHERE expr] [RETURNING ...]`
#[derive(Debug, Clone, Default)]
pub struct Update {
    /// Patterns selecting what to update.
    pub from: Vec<Pattern>,
    /// Assignments: (variable, property, value).
    pub sets: Vec<(Option<String>, String, Expr)>,
    /// Filter.
    pub where_: Option<Expr>,
    /// `RETURNING`
    pub returning: Vec<SelectItem>,
}

/// `DELETE [var, ...] FROM pattern [WHERE expr] [RETURNING ...]`
#[derive(Debug, Clone, Default)]
pub struct Delete {
    /// Variables to delete (empty = the only variable in the pattern).
    pub targets: Vec<String>,
    /// Patterns.
    pub from: Vec<Pattern>,
    /// Filter.
    pub where_: Option<Expr>,
    /// `RETURNING` (evaluated before deletion).
    pub returning: Vec<SelectItem>,
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    /// `NOT`
    Not,
    /// `-`
    Neg,
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    /// `OR`
    Or,
    /// `AND`
    And,
    /// `=`
    Eq,
    /// `!=` / `<>`
    Ne,
    /// `<`
    Lt,
    /// `<=`
    Le,
    /// `>`
    Gt,
    /// `>=`
    Ge,
    /// `+`
    Add,
    /// `-`
    Sub,
    /// `*`
    Mul,
    /// `/`
    Div,
    /// `%`
    Mod,
    /// `||`
    Concat,
}

impl BinOp {
    fn symbol(self) -> &'static str {
        match self {
            Self::Or => "OR",
            Self::And => "AND",
            Self::Eq => "=",
            Self::Ne => "!=",
            Self::Lt => "<",
            Self::Le => "<=",
            Self::Gt => ">",
            Self::Ge => ">=",
            Self::Add => "+",
            Self::Sub => "-",
            Self::Mul => "*",
            Self::Div => "/",
            Self::Mod => "%",
            Self::Concat => "||",
        }
    }
}

/// An expression.
#[derive(Debug, Clone)]
#[non_exhaustive]
pub enum Expr {
    /// Literal value.
    Lit(Value),
    /// `$name`
    Param(String),
    /// A bare name: a variable, an alias, or a property of the only variable.
    Ident(String),
    /// `expr.prop`
    Prop(Box<Expr>, String),
    /// `expr[index]`
    Index(Box<Expr>, Box<Expr>),
    /// Unary operator.
    Unary(UnOp, Box<Expr>),
    /// Binary operator.
    Binary(BinOp, Box<Expr>, Box<Expr>),
    /// Function call.
    Func {
        /// Lower-cased function name.
        name: String,
        /// Arguments.
        args: Vec<Expr>,
        /// `COUNT(DISTINCT x)`
        distinct: bool,
        /// `COUNT(*)`
        star: bool,
    },
    /// `[a, b]`
    List(Vec<Expr>),
    /// `{k: v}`
    Map(Vec<(String, Expr)>),
    /// `CASE [operand] WHEN .. THEN .. [ELSE ..] END`
    Case {
        /// Simple-case operand.
        operand: Option<Box<Expr>>,
        /// `WHEN`/`THEN` pairs.
        whens: Vec<(Expr, Expr)>,
        /// `ELSE`
        else_: Option<Box<Expr>>,
    },
    /// `x IS [NOT] NULL`
    IsNull(Box<Expr>, bool),
    /// `x [NOT] IN list`
    In(Box<Expr>, Box<Expr>, bool),
    /// `x [NOT] LIKE|ILIKE pattern`
    Like {
        /// Subject.
        expr: Box<Expr>,
        /// Pattern with `%` and `_`.
        pattern: Box<Expr>,
        /// `ILIKE`
        case_insensitive: bool,
        /// `NOT`
        negated: bool,
    },
    /// `x [NOT] BETWEEN lo AND hi`
    Between(Box<Expr>, Box<Expr>, Box<Expr>, bool),
}

impl Expr {
    pub(crate) fn bin(op: BinOp, l: Self, r: Self) -> Self {
        Self::Binary(op, Box::new(l), Box::new(r))
    }
}

impl fmt::Display for Expr {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Lit(v) => f.write_str(&literal(v)),
            Self::Param(p) => write!(f, "${p}"),
            Self::Ident(n) => f.write_str(n),
            Self::Prop(e, p) => write!(f, "{e}.{p}"),
            Self::Index(e, i) => write!(f, "{e}[{i}]"),
            Self::Unary(UnOp::Not, e) => write!(f, "NOT {e}"),
            Self::Unary(UnOp::Neg, e) => write!(f, "-{e}"),
            Self::Binary(op, l, r) => write!(f, "{l} {} {r}", op.symbol()),
            Self::Func { name, args, distinct, star } => {
                write!(f, "{name}(")?;
                if *star {
                    f.write_str("*")?;
                }
                if *distinct {
                    f.write_str("DISTINCT ")?;
                }
                for (i, a) in args.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{a}")?;
                }
                f.write_str(")")
            }
            Self::List(items) => {
                f.write_str("[")?;
                for (i, a) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{a}")?;
                }
                f.write_str("]")
            }
            Self::Map(items) => {
                f.write_str("{")?;
                for (i, (k, v)) in items.iter().enumerate() {
                    if i > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{k}: {v}")?;
                }
                f.write_str("}")
            }
            Self::Case { .. } => f.write_str("case"),
            Self::IsNull(e, neg) => write!(f, "{e} IS {}NULL", if *neg { "NOT " } else { "" }),
            Self::In(e, l, neg) => write!(f, "{e} {}IN {l}", if *neg { "NOT " } else { "" }),
            Self::Like { expr, pattern, case_insensitive, negated } => write!(
                f,
                "{expr} {}{} {pattern}",
                if *negated { "NOT " } else { "" },
                if *case_insensitive { "ILIKE" } else { "LIKE" }
            ),
            Self::Between(e, lo, hi, neg) => {
                write!(f, "{e} {}BETWEEN {lo} AND {hi}", if *neg { "NOT " } else { "" })
            }
        }
    }
}
