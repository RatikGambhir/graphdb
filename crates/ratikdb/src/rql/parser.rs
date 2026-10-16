//! Recursive-descent parser for RQL.

use crate::error::{Error, Result};
use crate::rql::ast::*;
use crate::rql::lexer::{Tok, Token, tokenize};
use crate::schema::{DefaultValue, Kind, Metric, PropDef, PropType};
use crate::value::Value;

/// Words that end an expression or clause and therefore cannot be used as
/// bare identifiers (quote them with backticks to use them as names).
const RESERVED: &[&str] = &[
    "SELECT", "FROM", "WHERE", "GROUP", "BY", "HAVING", "ORDER", "LIMIT", "OFFSET", "AND", "OR", "NOT", "AS", "ASC",
    "DESC", "SET", "RETURNING", "VALUES", "INTO", "IS", "NULL", "TRUE", "FALSE", "IN", "LIKE", "ILIKE", "BETWEEN",
    "CASE", "WHEN", "THEN", "ELSE", "END", "DISTINCT",
];

/// Parses a script of `;`-separated statements.
///
/// # Errors
/// [`Error::Syntax`] pointing at the first offending token.
pub fn parse(src: &str) -> Result<Vec<Statement>> {
    let mut p = Parser { src, toks: tokenize(src)?, i: 0 };
    let mut out = Vec::new();
    loop {
        while p.eat_sym(";") {}
        if p.at_eof() {
            break;
        }
        out.push(p.statement()?);
        if !p.eat_sym(";") && !p.at_eof() {
            return Err(p.err("expected ';' or end of input"));
        }
    }
    Ok(out)
}

struct Parser<'a> {
    src: &'a str,
    toks: Vec<Token>,
    i: usize,
}

impl Parser<'_> {
    fn peek(&self) -> &Tok {
        &self.toks[self.i].tok
    }

    fn peek_at(&self, n: usize) -> &Tok {
        &self.toks[(self.i + n).min(self.toks.len() - 1)].tok
    }

    fn at_eof(&self) -> bool {
        matches!(self.peek(), Tok::Eof)
    }

    fn bump(&mut self) -> Tok {
        let t = self.toks[self.i].tok.clone();
        if self.i < self.toks.len() - 1 {
            self.i += 1;
        }
        t
    }

    fn describe(tok: &Tok) -> String {
        match tok {
            Tok::Ident(s) => format!("'{s}'"),
            Tok::Quoted(s) => format!("`{s}`"),
            Tok::Str(s) => format!("string '{s}'"),
            Tok::Int(i) => format!("number {i}"),
            Tok::Float(f) => format!("number {f}"),
            Tok::Param(p) => format!("${p}"),
            Tok::Sym(s) => format!("'{s}'"),
            Tok::Eof => "end of input".into(),
        }
    }

    fn err(&self, msg: impl Into<String>) -> Error {
        let t = &self.toks[self.i];
        Error::Syntax { line: t.line, col: t.col, msg: format!("{}, found {}", msg.into(), Self::describe(&t.tok)) }
    }

    fn is_kw_at(&self, n: usize, kw: &str) -> bool {
        matches!(self.peek_at(n), Tok::Ident(s) if s.eq_ignore_ascii_case(kw))
    }

    fn is_kw(&self, kw: &str) -> bool {
        self.is_kw_at(0, kw)
    }

    fn eat_kw(&mut self, kw: &str) -> bool {
        let hit = self.is_kw(kw);
        if hit {
            self.bump();
        }
        hit
    }

    fn expect_kw(&mut self, kw: &str) -> Result<()> {
        if self.eat_kw(kw) { Ok(()) } else { Err(self.err(format!("expected {kw}"))) }
    }

    fn is_sym(&self, s: &str) -> bool {
        matches!(self.peek(), Tok::Sym(x) if *x == s)
    }

    fn eat_sym(&mut self, s: &str) -> bool {
        let hit = self.is_sym(s);
        if hit {
            self.bump();
        }
        hit
    }

    fn expect_sym(&mut self, s: &str) -> Result<()> {
        if self.eat_sym(s) { Ok(()) } else { Err(self.err(format!("expected '{s}'"))) }
    }

    fn is_reserved(s: &str) -> bool {
        RESERVED.iter().any(|r| r.eq_ignore_ascii_case(s))
    }

    /// A name (type, property, variable). Reserved words must be quoted.
    fn name(&mut self, what: &str) -> Result<String> {
        match self.peek().clone() {
            Tok::Ident(s) if !Self::is_reserved(&s) => {
                self.bump();
                Ok(s)
            }
            Tok::Quoted(s) => {
                self.bump();
                Ok(s)
            }
            _ => Err(self.err(format!("expected {what}"))),
        }
    }

    /// A name that may be a keyword (used after `.` and in DDL positions).
    fn any_name(&mut self, what: &str) -> Result<String> {
        match self.peek().clone() {
            Tok::Ident(s) | Tok::Quoted(s) => {
                self.bump();
                Ok(s)
            }
            _ => Err(self.err(format!("expected {what}"))),
        }
    }

    fn is_name(&self) -> bool {
        match self.peek() {
            Tok::Ident(s) => !Self::is_reserved(s),
            Tok::Quoted(_) => true,
            _ => false,
        }
    }

    fn u32_lit(&mut self, what: &str) -> Result<u32> {
        match self.peek() {
            Tok::Int(n) if *n >= 0 => {
                let n = u32::try_from(*n).map_err(|_| self.err(format!("{what} is too large")))?;
                self.bump();
                Ok(n)
            }
            _ => Err(self.err(format!("expected {what}"))),
        }
    }

    // ---------------------------------------------------------------- statements

    fn statement(&mut self) -> Result<Statement> {
        let kw = match self.peek() {
            Tok::Ident(s) => s.to_ascii_uppercase(),
            _ => return Err(self.err("expected a statement (SELECT, INSERT, UPDATE, DELETE, CREATE, ...)")),
        };
        match kw.as_str() {
            "SELECT" => self.select().map(|s| Statement::Select(Box::new(s))),
            "INSERT" => self.insert(),
            "UPDATE" => self.update(),
            "DELETE" => self.delete(),
            "CREATE" => self.create(),
            "DROP" => self.drop(),
            "ALTER" => self.alter(),
            "SHOW" => self.show(),
            "DESCRIBE" | "DESC" => {
                self.bump();
                let _ = self.eat_kw("NODE") || self.eat_kw("EDGE");
                Ok(Statement::Describe(self.any_name("a type name")?))
            }
            "EXPLAIN" => {
                self.bump();
                Ok(Statement::Explain(Box::new(self.statement()?)))
            }
            "CALL" => {
                self.bump();
                let name = self.any_name("a query name")?;
                self.expect_sym("(")?;
                let args = self.expr_list(")")?;
                Ok(Statement::Call { name, args })
            }
            _ => Err(self.err("expected a statement (SELECT, INSERT, UPDATE, DELETE, CREATE, ...)")),
        }
    }

    fn kind(&mut self) -> Result<Kind> {
        if self.eat_kw("NODE") {
            Ok(Kind::Node)
        } else if self.eat_kw("EDGE") {
            Ok(Kind::Edge)
        } else {
            Err(self.err("expected NODE or EDGE"))
        }
    }

    fn create(&mut self) -> Result<Statement> {
        self.expect_kw("CREATE")?;
        let or_replace = if self.eat_kw("OR") {
            self.expect_kw("REPLACE")?;
            true
        } else {
            false
        };
        if self.eat_kw("QUERY") {
            return self.create_query(or_replace);
        }
        if or_replace {
            return Err(self.err("OR REPLACE is only supported for CREATE QUERY"));
        }
        if self.is_kw("UNIQUE") || self.is_kw("SEARCH") || self.is_kw("INDEX") {
            let flavor = if self.eat_kw("UNIQUE") {
                IndexFlavor::Unique
            } else if self.eat_kw("SEARCH") {
                IndexFlavor::Search
            } else {
                IndexFlavor::BTree
            };
            self.expect_kw("INDEX")?;
            let (name, prop) = self.on_type_prop()?;
            return Ok(Statement::CreateIndex { name, prop, flavor });
        }
        let kind = self.kind()?;
        let if_not_exists = if self.eat_kw("IF") {
            self.expect_kw("NOT")?;
            self.expect_kw("EXISTS")?;
            true
        } else {
            false
        };
        let name = self.any_name("a type name")?;
        let (mut from, mut to) = (Vec::new(), Vec::new());
        if kind == Kind::Edge {
            self.expect_kw("FROM")?;
            from = self.label_alternatives()?;
            self.expect_kw("TO")?;
            to = self.label_alternatives()?;
        }
        let mut props = Vec::new();
        if self.eat_sym("(") {
            if !self.eat_sym(")") {
                loop {
                    props.push(self.prop_def()?);
                    if self.eat_sym(")") {
                        break;
                    }
                    self.expect_sym(",")?;
                }
            }
        }
        Ok(Statement::CreateType { kind, name, if_not_exists, props, from, to })
    }

    fn label_alternatives(&mut self) -> Result<Vec<String>> {
        let mut out = vec![self.any_name("a node type")?];
        while self.eat_sym("|") {
            out.push(self.any_name("a node type")?);
        }
        Ok(out)
    }

    fn on_type_prop(&mut self) -> Result<(String, String)> {
        self.expect_kw("ON")?;
        let name = self.any_name("a type name")?;
        self.expect_sym("(")?;
        let prop = self.any_name("a property name")?;
        self.expect_sym(")")?;
        Ok((name, prop))
    }

    fn prop_type(&mut self) -> Result<PropType> {
        let word = self.any_name("a type (STRING, INT, FLOAT, BOOL, TIMESTAMP, VECTOR(n), JSON)")?;
        let mut ty = match word.to_ascii_uppercase().as_str() {
            "BOOL" | "BOOLEAN" => PropType::Bool,
            "INT" | "INTEGER" | "BIGINT" | "LONG" => PropType::Int,
            "FLOAT" | "DOUBLE" | "REAL" | "NUMBER" | "DECIMAL" => PropType::Float,
            "STRING" | "TEXT" | "VARCHAR" | "CHAR" => {
                if self.eat_sym("(") {
                    self.u32_lit("a length")?;
                    self.expect_sym(")")?;
                }
                PropType::String
            }
            "TIMESTAMP" | "DATETIME" => PropType::Timestamp,
            "JSON" | "ANY" => PropType::Json,
            "VECTOR" => {
                self.expect_sym("(")?;
                let n = self.u32_lit("the vector dimension")?;
                if n == 0 {
                    return Err(self.err("vector dimension must be at least 1"));
                }
                self.expect_sym(")")?;
                PropType::Vector(n)
            }
            other => {
                self.i -= 1;
                return Err(self.err(format!(
                    "unknown type '{other}' (expected STRING, INT, FLOAT, BOOL, TIMESTAMP, VECTOR(n), JSON or T[])"
                )));
            }
        };
        while self.is_sym("[") && matches!(self.peek_at(1), Tok::Sym("]")) {
            self.bump();
            self.bump();
            ty = PropType::List(Box::new(ty));
        }
        Ok(ty)
    }

    fn prop_def(&mut self) -> Result<PropDef> {
        let name = self.any_name("a property name")?;
        let ty = self.prop_type()?;
        let mut def = PropDef::new(name, ty);
        loop {
            if self.eat_kw("NOT") {
                self.expect_kw("NULL")?;
                def.not_null = true;
            } else if self.eat_kw("NULL") {
                def.not_null = false;
            } else if self.eat_kw("UNIQUE") {
                def.unique = true;
            } else if self.eat_kw("PRIMARY") {
                self.expect_kw("KEY")?;
                def.unique = true;
                def.not_null = true;
            } else if self.eat_kw("INDEX") || self.eat_kw("INDEXED") {
                def.indexed = true;
            } else if self.eat_kw("SEARCHABLE") || self.eat_kw("SEARCH") {
                def.searchable = true;
            } else if self.eat_kw("METRIC") {
                let m = self.any_name("COSINE, EUCLIDEAN or DOT")?;
                def.metric = match m.to_ascii_uppercase().as_str() {
                    "COSINE" => Metric::Cosine,
                    "EUCLIDEAN" | "L2" => Metric::Euclidean,
                    "DOT" | "IP" => Metric::Dot,
                    _ => {
                        self.i -= 1;
                        return Err(self.err("expected COSINE, EUCLIDEAN or DOT"));
                    }
                };
            } else if self.eat_kw("DEFAULT") {
                let e = self.unary()?;
                def.default = Some(match e {
                    Expr::Func { name, args, .. } if name == "now" && args.is_empty() => DefaultValue::Now,
                    e => DefaultValue::Value(
                        const_value(&e).ok_or_else(|| self.err("DEFAULT must be a constant or now()"))?,
                    ),
                });
            } else {
                return Ok(def);
            }
        }
    }

    fn create_query(&mut self, or_replace: bool) -> Result<Statement> {
        let name = self.any_name("a query name")?;
        let mut params = Vec::new();
        self.expect_sym("(")?;
        if !self.eat_sym(")") {
            loop {
                let pname = match self.peek().clone() {
                    Tok::Param(p) => {
                        self.bump();
                        p
                    }
                    _ => self.any_name("a parameter like $name")?,
                };
                params.push((pname, self.prop_type()?));
                if self.eat_sym(")") {
                    break;
                }
                self.expect_sym(",")?;
            }
        }
        self.expect_kw("AS")?;
        let start = self.toks[self.i].pos;
        let stmt = self.statement()?;
        if matches!(stmt, Statement::CreateQuery { .. }) {
            return Err(self.err("a stored query cannot define another stored query"));
        }
        let end = self.toks[self.i].pos;
        let body = self.src[start..end].trim().to_owned();
        Ok(Statement::CreateQuery { name, or_replace, params, body, stmt: Box::new(stmt) })
    }

    fn drop(&mut self) -> Result<Statement> {
        self.expect_kw("DROP")?;
        if self.eat_kw("INDEX") {
            let (name, prop) = self.on_type_prop()?;
            return Ok(Statement::DropIndex { name, prop });
        }
        if self.eat_kw("QUERY") {
            let if_exists = self.if_exists()?;
            return Ok(Statement::DropQuery { name: self.any_name("a query name")?, if_exists });
        }
        let kind = self.kind()?;
        let if_exists = self.if_exists()?;
        let name = self.any_name("a type name")?;
        let cascade = self.eat_kw("CASCADE");
        Ok(Statement::DropType { kind, name, if_exists, cascade })
    }

    fn if_exists(&mut self) -> Result<bool> {
        if self.eat_kw("IF") {
            self.expect_kw("EXISTS")?;
            return Ok(true);
        }
        Ok(false)
    }

    fn alter(&mut self) -> Result<Statement> {
        self.expect_kw("ALTER")?;
        let kind = self.kind()?;
        let name = self.any_name("a type name")?;
        if self.eat_kw("ADD") {
            let _ = self.eat_kw("PROPERTY") || self.eat_kw("COLUMN");
            Ok(Statement::AlterAdd { kind, name, prop: self.prop_def()? })
        } else if self.eat_kw("DROP") {
            let _ = self.eat_kw("PROPERTY") || self.eat_kw("COLUMN");
            Ok(Statement::AlterDrop { kind, name, prop: self.any_name("a property name")? })
        } else {
            Err(self.err("expected ADD or DROP"))
        }
    }

    fn show(&mut self) -> Result<Statement> {
        self.expect_kw("SHOW")?;
        let what = self.any_name("SCHEMA, NODES, EDGES, QUERIES, INDEXES or STATS")?;
        let show = match what.to_ascii_uppercase().as_str() {
            "SCHEMA" => Show::Schema,
            "NODES" | "NODE" => Show::Nodes,
            "EDGES" | "EDGE" => Show::Edges,
            "QUERIES" => Show::Queries,
            "INDEXES" | "INDICES" => Show::Indexes,
            "STATS" | "STATUS" => Show::Stats,
            _ => {
                self.i -= 1;
                return Err(self.err("expected SCHEMA, NODES, EDGES, QUERIES, INDEXES or STATS"));
            }
        };
        Ok(Statement::Show(show))
    }

    fn select(&mut self) -> Result<Select> {
        self.expect_kw("SELECT")?;
        let mut s = Select { distinct: self.eat_kw("DISTINCT"), items: self.select_items()?, ..Select::default() };
        if self.eat_kw("FROM") {
            s.from = self.from_list()?;
        }
        if self.eat_kw("WHERE") {
            s.where_ = Some(self.expr()?);
        }
        if self.eat_kw("GROUP") {
            self.expect_kw("BY")?;
            loop {
                s.group_by.push(self.expr()?);
                if !self.eat_sym(",") {
                    break;
                }
            }
        }
        if self.eat_kw("HAVING") {
            s.having = Some(self.expr()?);
        }
        if self.eat_kw("ORDER") {
            self.expect_kw("BY")?;
            loop {
                let e = self.expr()?;
                let desc = if self.eat_kw("DESC") {
                    true
                } else {
                    self.eat_kw("ASC");
                    false
                };
                s.order_by.push((e, desc));
                if !self.eat_sym(",") {
                    break;
                }
            }
        }
        loop {
            if s.limit.is_none() && self.eat_kw("LIMIT") {
                s.limit = Some(self.expr()?);
            } else if s.offset.is_none() && (self.eat_kw("OFFSET") || self.eat_kw("SKIP")) {
                s.offset = Some(self.expr()?);
            } else {
                break;
            }
        }
        Ok(s)
    }

    fn select_items(&mut self) -> Result<Vec<SelectItem>> {
        let mut items = Vec::new();
        loop {
            if self.eat_sym("*") {
                items.push(SelectItem::Star);
            } else {
                let expr = self.expr()?;
                let alias = if self.eat_kw("AS") {
                    Some(self.any_name("an alias")?)
                } else if self.is_name() {
                    Some(self.name("an alias")?)
                } else {
                    None
                };
                items.push(SelectItem::Expr { expr, alias });
            }
            if !self.eat_sym(",") {
                return Ok(items);
            }
        }
    }

    fn returning(&mut self) -> Result<Vec<SelectItem>> {
        if self.eat_kw("RETURNING") { self.select_items() } else { Ok(Vec::new()) }
    }

    fn from_list(&mut self) -> Result<Vec<Pattern>> {
        let mut out = Vec::new();
        loop {
            if self.is_sym("(") {
                out.push(self.pattern()?);
            } else {
                // SQL sugar: `FROM User` or `FROM User AS u` / `FROM User u`.
                let label = self.name("a node type or a pattern like (u:User)")?;
                let var = if self.eat_kw("AS") || self.is_name() { self.name("an alias")? } else { label.clone() };
                out.push(Pattern { nodes: vec![NodePat { var: Some(var), label: Some(label), props: Vec::new() }], rels: Vec::new() });
            }
            if !self.eat_sym(",") {
                return Ok(out);
            }
        }
    }

    fn pattern(&mut self) -> Result<Pattern> {
        let mut p = Pattern { nodes: vec![self.node_pat()?], rels: Vec::new() };
        while self.is_sym("-") || (self.is_sym("<") && matches!(self.peek_at(1), Tok::Sym("-"))) {
            p.rels.push(self.rel_pat()?);
            p.nodes.push(self.node_pat()?);
        }
        Ok(p)
    }

    fn node_pat(&mut self) -> Result<NodePat> {
        self.expect_sym("(")?;
        let mut n = NodePat::default();
        if self.is_name() {
            n.var = Some(self.name("a variable")?);
        }
        if self.eat_sym(":") {
            n.label = Some(self.any_name("a node type")?);
        }
        if self.is_sym("{") {
            n.props = self.prop_map()?;
        }
        self.expect_sym(")")?;
        Ok(n)
    }

    fn rel_pat(&mut self) -> Result<RelPat> {
        let incoming = self.eat_sym("<");
        self.expect_sym("-")?;
        if !self.is_sym("[") {
            return Err(self.err("expected '[' (write edges as -[:TYPE]-> or <-[:TYPE]-)"));
        }
        self.bump();
        let mut r = RelPat { var: None, labels: Vec::new(), dir: Dir::Both, hops: None, props: Vec::new() };
        if self.is_name() {
            r.var = Some(self.name("a variable")?);
        }
        if self.eat_sym(":") {
            r.labels = self.label_alternatives()?;
        }
        if self.eat_sym("*") {
            let min = if matches!(self.peek(), Tok::Int(_)) { Some(self.u32_lit("a hop count")?) } else { None };
            r.hops = Some(if self.eat_sym("..") {
                let max = if matches!(self.peek(), Tok::Int(_)) { Some(self.u32_lit("a hop count")?) } else { None };
                (min.unwrap_or(1), max)
            } else {
                min.map_or((1, None), |m| (m, Some(m)))
            });
        }
        if self.is_sym("{") {
            r.props = self.prop_map()?;
        }
        self.expect_sym("]")?;
        r.dir = if incoming {
            self.expect_sym("-")?;
            Dir::In
        } else if self.eat_sym("->") {
            Dir::Out
        } else {
            self.expect_sym("-")?;
            Dir::Both
        };
        Ok(r)
    }

    fn prop_map(&mut self) -> Result<Vec<(String, Expr)>> {
        self.expect_sym("{")?;
        let mut out = Vec::new();
        if self.eat_sym("}") {
            return Ok(out);
        }
        loop {
            let key = match self.peek().clone() {
                Tok::Str(s) => {
                    self.bump();
                    s
                }
                _ => self.any_name("a property name")?,
            };
            self.expect_sym(":")?;
            out.push((key, self.expr()?));
            if self.eat_sym("}") {
                return Ok(out);
            }
            self.expect_sym(",")?;
        }
    }

    fn insert(&mut self) -> Result<Statement> {
        self.expect_kw("INSERT")?;
        if self.eat_kw("INTO") {
            let label = self.any_name("a node type")?;
            let mut rows = Vec::new();
            if self.is_sym("{") {
                loop {
                    rows.push(self.prop_map()?);
                    if !self.eat_sym(",") {
                        break;
                    }
                }
            } else {
                self.expect_sym("(")?;
                let mut cols = vec![self.any_name("a property name")?];
                while self.eat_sym(",") {
                    cols.push(self.any_name("a property name")?);
                }
                self.expect_sym(")")?;
                self.expect_kw("VALUES")?;
                loop {
                    self.expect_sym("(")?;
                    let values = self.expr_list(")")?;
                    if values.len() != cols.len() {
                        return Err(self.err(format!("expected {} values to match the column list", cols.len())));
                    }
                    rows.push(cols.iter().cloned().zip(values).collect());
                    if !self.eat_sym(",") {
                        break;
                    }
                }
            }
            let returning = self.returning()?;
            return Ok(Statement::InsertRows(Box::new(InsertRows { label, rows, returning })));
        }
        let mut ins = Insert::default();
        loop {
            ins.create.push(self.pattern()?);
            if !self.eat_sym(",") {
                break;
            }
        }
        if self.eat_kw("FROM") {
            ins.from = self.from_list()?;
        }
        if self.eat_kw("WHERE") {
            ins.where_ = Some(self.expr()?);
        }
        ins.returning = self.returning()?;
        Ok(Statement::Insert(Box::new(ins)))
    }

    fn update(&mut self) -> Result<Statement> {
        self.expect_kw("UPDATE")?;
        let mut u = Update { from: self.from_list()?, ..Update::default() };
        self.expect_kw("SET")?;
        loop {
            let first = self.any_name("a property to set")?;
            let (var, prop) =
                if self.eat_sym(".") { (Some(first), self.any_name("a property name")?) } else { (None, first) };
            self.expect_sym("=")?;
            u.sets.push((var, prop, self.expr()?));
            if !self.eat_sym(",") {
                break;
            }
        }
        if self.eat_kw("WHERE") {
            u.where_ = Some(self.expr()?);
        }
        u.returning = self.returning()?;
        Ok(Statement::Update(Box::new(u)))
    }

    fn delete(&mut self) -> Result<Statement> {
        self.expect_kw("DELETE")?;
        let mut d = Delete::default();
        while !self.is_kw("FROM") {
            d.targets.push(self.name("a variable to delete or FROM")?);
            if !self.eat_sym(",") {
                break;
            }
        }
        self.expect_kw("FROM")?;
        d.from = self.from_list()?;
        if self.eat_kw("WHERE") {
            d.where_ = Some(self.expr()?);
        }
        d.returning = self.returning()?;
        Ok(Statement::Delete(Box::new(d)))
    }

    // --------------------------------------------------------------- expressions

    fn expr_list(&mut self, close: &str) -> Result<Vec<Expr>> {
        let mut out = Vec::new();
        if self.eat_sym(close) {
            return Ok(out);
        }
        loop {
            out.push(self.expr()?);
            if self.eat_sym(close) {
                return Ok(out);
            }
            self.expect_sym(",")?;
        }
    }

    fn expr(&mut self) -> Result<Expr> {
        let mut l = self.and()?;
        while self.eat_kw("OR") {
            l = Expr::bin(BinOp::Or, l, self.and()?);
        }
        Ok(l)
    }

    fn and(&mut self) -> Result<Expr> {
        let mut l = self.not()?;
        while self.eat_kw("AND") {
            l = Expr::bin(BinOp::And, l, self.not()?);
        }
        Ok(l)
    }

    fn not(&mut self) -> Result<Expr> {
        if self.eat_kw("NOT") {
            return Ok(Expr::Unary(UnOp::Not, Box::new(self.not()?)));
        }
        self.comparison()
    }

    fn comparison(&mut self) -> Result<Expr> {
        let l = self.additive()?;
        let op = match self.peek() {
            Tok::Sym("=" | "==") => Some(BinOp::Eq),
            Tok::Sym("!=" | "<>") => Some(BinOp::Ne),
            Tok::Sym("<") => Some(BinOp::Lt),
            Tok::Sym("<=") => Some(BinOp::Le),
            Tok::Sym(">") => Some(BinOp::Gt),
            Tok::Sym(">=") => Some(BinOp::Ge),
            _ => None,
        };
        if let Some(op) = op {
            self.bump();
            return Ok(Expr::bin(op, l, self.additive()?));
        }
        if self.eat_kw("IS") {
            let negated = self.eat_kw("NOT");
            self.expect_kw("NULL")?;
            return Ok(Expr::IsNull(Box::new(l), negated));
        }
        let negated = self.is_kw("NOT")
            && ["IN", "LIKE", "ILIKE", "BETWEEN"].iter().any(|k| self.is_kw_at(1, k))
            && self.eat_kw("NOT");
        if self.eat_kw("IN") {
            return Ok(Expr::In(Box::new(l), Box::new(self.additive()?), negated));
        }
        if self.is_kw("LIKE") || self.is_kw("ILIKE") {
            let case_insensitive = self.eat_kw("ILIKE");
            if !case_insensitive {
                self.bump();
            }
            return Ok(Expr::Like {
                expr: Box::new(l),
                pattern: Box::new(self.additive()?),
                case_insensitive,
                negated,
            });
        }
        if self.eat_kw("BETWEEN") {
            let lo = self.additive()?;
            self.expect_kw("AND")?;
            let hi = self.additive()?;
            return Ok(Expr::Between(Box::new(l), Box::new(lo), Box::new(hi), negated));
        }
        if negated {
            return Err(self.err("expected IN, LIKE or BETWEEN after NOT"));
        }
        Ok(l)
    }

    fn additive(&mut self) -> Result<Expr> {
        let mut l = self.multiplicative()?;
        loop {
            let op = match self.peek() {
                Tok::Sym("+") => BinOp::Add,
                Tok::Sym("-") => BinOp::Sub,
                Tok::Sym("||") => BinOp::Concat,
                _ => return Ok(l),
            };
            self.bump();
            l = Expr::bin(op, l, self.multiplicative()?);
        }
    }

    fn multiplicative(&mut self) -> Result<Expr> {
        let mut l = self.unary()?;
        loop {
            let op = match self.peek() {
                Tok::Sym("*") => BinOp::Mul,
                Tok::Sym("/") => BinOp::Div,
                Tok::Sym("%") => BinOp::Mod,
                _ => return Ok(l),
            };
            self.bump();
            l = Expr::bin(op, l, self.unary()?);
        }
    }

    fn unary(&mut self) -> Result<Expr> {
        if self.eat_sym("-") {
            return Ok(match self.unary()? {
                Expr::Lit(Value::Int(i)) => Expr::Lit(Value::Int(-i)),
                Expr::Lit(Value::Float(f)) => Expr::Lit(Value::Float(-f)),
                e => Expr::Unary(UnOp::Neg, Box::new(e)),
            });
        }
        if self.eat_sym("+") {
            return self.unary();
        }
        let mut e = self.primary()?;
        loop {
            if self.eat_sym(".") {
                e = Expr::Prop(Box::new(e), self.any_name("a property name")?);
            } else if self.eat_sym("[") {
                let idx = self.expr()?;
                self.expect_sym("]")?;
                e = Expr::Index(Box::new(e), Box::new(idx));
            } else {
                return Ok(e);
            }
        }
    }

    fn primary(&mut self) -> Result<Expr> {
        match self.peek().clone() {
            Tok::Int(i) => {
                self.bump();
                Ok(Expr::Lit(Value::Int(i)))
            }
            Tok::Float(f) => {
                self.bump();
                Ok(Expr::Lit(Value::Float(f)))
            }
            Tok::Str(s) => {
                self.bump();
                Ok(Expr::Lit(Value::String(s)))
            }
            Tok::Param(p) => {
                self.bump();
                Ok(Expr::Param(p))
            }
            Tok::Sym("(") => {
                self.bump();
                let e = self.expr()?;
                self.expect_sym(")")?;
                Ok(e)
            }
            Tok::Sym("[") => {
                self.bump();
                Ok(Expr::List(self.expr_list("]")?))
            }
            Tok::Sym("{") => Ok(Expr::Map(self.prop_map()?)),
            Tok::Quoted(s) => {
                self.bump();
                Ok(Expr::Ident(s))
            }
            Tok::Ident(s) => {
                let upper = s.to_ascii_uppercase();
                match upper.as_str() {
                    "TRUE" => {
                        self.bump();
                        Ok(Expr::Lit(Value::Bool(true)))
                    }
                    "FALSE" => {
                        self.bump();
                        Ok(Expr::Lit(Value::Bool(false)))
                    }
                    "NULL" => {
                        self.bump();
                        Ok(Expr::Lit(Value::Null))
                    }
                    "CASE" => self.case(),
                    _ if matches!(self.peek_at(1), Tok::Sym("(")) && !Self::is_reserved(&s) => {
                        self.bump();
                        self.bump();
                        let name = s.to_ascii_lowercase();
                        if self.eat_sym("*") {
                            self.expect_sym(")")?;
                            return Ok(Expr::Func { name, args: Vec::new(), distinct: false, star: true });
                        }
                        let distinct = self.eat_kw("DISTINCT");
                        let args = self.expr_list(")")?;
                        Ok(Expr::Func { name, args, distinct, star: false })
                    }
                    _ if !Self::is_reserved(&s) => {
                        self.bump();
                        Ok(Expr::Ident(s))
                    }
                    _ => Err(self.err("expected an expression")),
                }
            }
            _ => Err(self.err("expected an expression")),
        }
    }

    fn case(&mut self) -> Result<Expr> {
        self.expect_kw("CASE")?;
        let operand = if self.is_kw("WHEN") { None } else { Some(Box::new(self.expr()?)) };
        let mut whens = Vec::new();
        while self.eat_kw("WHEN") {
            let cond = self.expr()?;
            self.expect_kw("THEN")?;
            whens.push((cond, self.expr()?));
        }
        if whens.is_empty() {
            return Err(self.err("expected WHEN"));
        }
        let else_ = if self.eat_kw("ELSE") { Some(Box::new(self.expr()?)) } else { None };
        self.expect_kw("END")?;
        Ok(Expr::Case { operand, whens, else_ })
    }
}

/// Evaluates an expression made only of literals.
pub(crate) fn const_value(e: &Expr) -> Option<Value> {
    match e {
        Expr::Lit(v) => Some(v.clone()),
        Expr::List(items) => items.iter().map(const_value).collect::<Option<Vec<_>>>().map(Value::List),
        Expr::Map(items) => items
            .iter()
            .map(|(k, v)| const_value(v).map(|v| (k.clone(), v)))
            .collect::<Option<_>>()
            .map(Value::Map),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn one(src: &str) -> Statement {
        let mut v = parse(src).unwrap_or_else(|e| panic!("{src}: {e}"));
        assert_eq!(v.len(), 1);
        v.remove(0)
    }

    #[test]
    fn parses_ddl() {
        let Statement::CreateType { props, .. } = one(
            "CREATE NODE User (name STRING NOT NULL UNIQUE, age INT DEFAULT 0, bio TEXT SEARCHABLE, \
             emb VECTOR(3) METRIC DOT, tags STRING[], created TIMESTAMP DEFAULT now())",
        ) else {
            panic!()
        };
        assert_eq!(props.len(), 6);
        assert!(props[0].unique && props[0].not_null);
        assert_eq!(props[3].metric, Metric::Dot);
        assert_eq!(props[4].ty, PropType::List(Box::new(PropType::String)));
        assert_eq!(props[5].default, Some(DefaultValue::Now));
        let Statement::CreateType { from, to, .. } = one("CREATE EDGE LIKES FROM User | Bot TO Post (at INT)") else {
            panic!()
        };
        assert_eq!((from.len(), to.len()), (2, 1));
    }

    #[test]
    fn parses_select_with_patterns() {
        let Statement::Select(s) = one(
            "SELECT u.name, count(*) AS n FROM (u:User {name: $n})-[f:FOLLOWS*1..3]->(v), Post p \
             WHERE v.age >= 18 AND NOT v.name LIKE 'A%' GROUP BY u.name ORDER BY n DESC LIMIT 10 OFFSET 5",
        ) else {
            panic!()
        };
        assert_eq!(s.items.len(), 2);
        assert_eq!(s.from.len(), 2);
        assert_eq!(s.from[0].rels[0].hops, Some((1, Some(3))));
        assert_eq!(s.from[1].nodes[0].var.as_deref(), Some("p"));
        assert!(s.order_by[0].1);
    }

    #[test]
    fn parses_writes() {
        assert!(matches!(one("INSERT INTO User (name, age) VALUES ('a', 1), ('b', 2)"), Statement::InsertRows(r) if r.rows.len() == 2));
        assert!(matches!(
            one("INSERT (a)-[:FOLLOWS {since: 2024}]->(b) FROM User a, User b WHERE a.name = 'x' AND b.name = 'y'"),
            Statement::Insert(_)
        ));
        assert!(matches!(one("UPDATE User SET age = age + 1 WHERE name = 'x' RETURNING *"), Statement::Update(_)));
        assert!(matches!(one("DELETE e FROM (a)<-[e:FOLLOWS]-(b)"), Statement::Delete(d) if d.targets == ["e"]));
    }

    #[test]
    fn stored_query_keeps_body_text() {
        let Statement::CreateQuery { body, params, .. } =
            one("CREATE QUERY by_name($name STRING) AS SELECT * FROM User WHERE name = $name;")
        else {
            panic!()
        };
        assert_eq!(body, "SELECT * FROM User WHERE name = $name");
        assert_eq!(params[0].0, "name");
    }

    #[test]
    fn reports_position_of_errors() {
        let err = parse("SELECT name\nFROM User WHERE").unwrap_err();
        assert!(err.to_string().contains("2:16"), "{err}");
    }
}
