//! Statement execution and transactions.

pub(crate) mod compile;
mod ddl;
pub(crate) mod eval;
pub(crate) mod matcher;
mod select;
mod write;

use serde::Serialize;

use crate::error::{Error, Result};
use crate::graph::{Graph, Op};
use crate::rql::ast::{Show, Statement};
use crate::rql::parse;
use crate::schema::Kind;
use crate::value::Value;

pub use compile::Params;
use matcher::{bind_where, plan};

const MAX_CALL_DEPTH: u32 = 16;

/// Counts of what a statement changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize)]
pub struct Stats {
    /// Nodes inserted.
    pub nodes_created: u64,
    /// Nodes deleted.
    pub nodes_deleted: u64,
    /// Edges inserted.
    pub edges_created: u64,
    /// Edges deleted.
    pub edges_deleted: u64,
    /// Properties updated.
    pub properties_set: u64,
    /// Schema/stored-query changes.
    pub schema_changes: u64,
}

impl Stats {
    /// `true` when nothing changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }
}

/// The result of running RQL: named columns, rows, and change counts.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct QueryResult {
    /// Column names.
    pub columns: Vec<String>,
    /// Rows, each with one value per column. Nodes and edges are maps with
    /// `_id` and `_label` keys (plus `_from`/`_to` for edges).
    pub rows: Vec<Vec<Value>>,
    /// What the statement changed.
    pub stats: Stats,
}

impl QueryResult {
    pub(crate) fn status(msg: String) -> Self {
        Self { columns: vec!["status".into()], rows: vec![vec![Value::String(msg)]], stats: Stats::default() }
    }

    fn table(columns: &[&str], rows: Vec<Vec<Value>>) -> Self {
        Self { columns: columns.iter().map(|c| (*c).to_owned()).collect(), rows, stats: Stats::default() }
    }

    /// Number of rows.
    #[must_use]
    pub fn len(&self) -> usize {
        self.rows.len()
    }

    /// `true` when there are no rows.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.rows.is_empty()
    }

    /// The value at `row`, `column`.
    #[must_use]
    pub fn get(&self, row: usize, column: &str) -> Option<&Value> {
        let c = self.columns.iter().position(|n| n == column)?;
        self.rows.get(row)?.get(c)
    }

    /// Rows as JSON objects keyed by column name.
    #[must_use]
    pub fn to_objects(&self) -> Vec<serde_json::Map<String, serde_json::Value>> {
        self.rows
            .iter()
            .map(|r| self.columns.iter().cloned().zip(r.iter().map(Value::to_json)).collect())
            .collect()
    }

    /// Wire format used by the HTTP API: `{columns, rows, stats}`.
    #[must_use]
    pub fn to_json(&self) -> serde_json::Value {
        serde_json::json!({
            "columns": self.columns,
            "rows": self.rows.iter().map(|r| r.iter().map(Value::to_json).collect::<Vec<_>>()).collect::<Vec<_>>(),
            "stats": self.stats,
        })
    }
}

/// An open write transaction: everything applied is logged for the WAL and
/// can be undone.
pub(crate) struct Txn<'g> {
    pub g: &'g mut Graph,
    pub log: Vec<Op>,
    undo: Vec<Op>,
    pub stats: Stats,
}

pub(crate) struct Savepoint {
    len: usize,
    stats: Stats,
}

impl<'g> Txn<'g> {
    pub(crate) fn new(g: &'g mut Graph) -> Self {
        Self { g, log: Vec::new(), undo: Vec::new(), stats: Stats::default() }
    }

    pub(crate) fn apply(&mut self, op: Op) -> Result<()> {
        let inverse = self.g.apply(&op)?;
        let s = &mut self.stats;
        match &op {
            Op::AddNode { .. } => s.nodes_created += 1,
            Op::AddEdge { .. } => s.edges_created += 1,
            Op::DelNode(_) => s.nodes_deleted += 1,
            Op::DelEdge(_) => s.edges_deleted += 1,
            Op::SetProp { .. } => s.properties_set += 1,
            Op::CreateType(_)
            | Op::DropType(_)
            | Op::AddProp { .. }
            | Op::DropProp { .. }
            | Op::SetFlags { .. }
            | Op::PutQuery(_)
            | Op::DropQuery(_) => s.schema_changes += 1,
            Op::Counters { .. } | Op::RestoreProp { .. } => {}
        }
        self.undo.push(inverse);
        self.log.push(op);
        Ok(())
    }

    pub(crate) fn savepoint(&self) -> Savepoint {
        Savepoint { len: self.undo.len(), stats: self.stats }
    }

    pub(crate) fn rollback_to(&mut self, sp: Savepoint) {
        while self.undo.len() > sp.len {
            if let Some(op) = self.undo.pop() {
                // Inverse ops restore a state that existed moments ago, so
                // they cannot fail; ignoring the result is safe.
                let _ = self.g.apply(&op);
            }
            self.log.pop();
        }
        self.stats = sp.stats;
    }

    pub(crate) fn rollback(mut self) {
        self.rollback_to(Savepoint { len: 0, stats: Stats::default() });
    }
}

/// `true` if running `stmt` cannot modify the database (resolving `CALL`s).
pub(crate) fn is_read_only(g: &Graph, stmt: &Statement, depth: u32) -> bool {
    match stmt {
        Statement::Call { name, .. } if depth < MAX_CALL_DEPTH => g.queries.get(name).is_some_and(|q| {
            parse(&q.body).is_ok_and(|stmts| stmts.iter().all(|s| is_read_only(g, s, depth + 1)))
        }),
        s => s.is_read_only(),
    }
}

fn int(v: usize) -> Value {
    Value::Int(i64::try_from(v).unwrap_or(i64::MAX))
}

/// Runs a read-only statement.
pub(crate) fn run_read(g: &Graph, params: &Params, stmt: &Statement, now: i64, depth: u32) -> Result<QueryResult> {
    match stmt {
        Statement::Select(s) => {
            let plan = select::plan_select(g, params, s, now)?;
            let (columns, rows) = plan.run(g, now)?;
            let rows = rows.into_iter().map(|r| r.into_iter().map(|v| g.materialize(v)).collect()).collect();
            Ok(QueryResult { columns, rows, stats: Stats::default() })
        }
        Statement::Explain(inner) => {
            let lines = match inner.as_ref() {
                Statement::Select(s) => {
                    let plan = select::plan_select(g, params, s, now)?;
                    let mut lines = plan.matcher.describe(g, &plan.scope);
                    if !plan.aggs.is_empty() || !plan.group_by.is_empty() {
                        lines.push(format!("Aggregate  {} function(s), {} group key(s)", plan.aggs.len(), plan.group_by.len()));
                    }
                    if !plan.order.is_empty() {
                        lines.push(format!("Sort  {} key(s)", plan.order.len()));
                    }
                    if let Some(l) = plan.limit {
                        lines.push(format!("Limit  {l} (offset {})", plan.offset));
                    }
                    lines
                }
                Statement::Update(u) => describe_match(g, params, &u.from, u.where_.as_ref(), now, "Update")?,
                Statement::Delete(d) => describe_match(g, params, &d.from, d.where_.as_ref(), now, "Delete")?,
                Statement::Insert(i) if !i.from.is_empty() => {
                    describe_match(g, params, &i.from, i.where_.as_ref(), now, "Insert")?
                }
                other => vec![format!("{} (no graph matching)", statement_name(other))],
            };
            Ok(QueryResult::table(&["plan"], lines.into_iter().map(|l| vec![Value::String(l)]).collect()))
        }
        Statement::Show(what) => Ok(show(g, *what)),
        Statement::Describe(name) => {
            let def = g
                .schema
                .by_name(name)
                .ok_or_else(|| Error::schema(format!("unknown type '{name}'{}", crate::error::hint(name, g.schema.types().map(|t| t.name.as_str())))))?;
            let rows = def
                .props
                .iter()
                .map(|p| {
                    let index = match (&p.ty, p.unique, p.indexed, p.searchable) {
                        (crate::schema::PropType::Vector(_), ..) => format!("HNSW ({})", p.metric),
                        (_, true, _, s) => if s { "UNIQUE, SEARCH" } else { "UNIQUE" }.to_owned(),
                        (_, _, true, s) => if s { "INDEX, SEARCH" } else { "INDEX" }.to_owned(),
                        (_, _, _, true) => "SEARCH".to_owned(),
                        _ => String::new(),
                    };
                    vec![
                        Value::String(p.name.clone()),
                        Value::String(p.ty.to_string()),
                        Value::Bool(!p.not_null),
                        Value::String(index),
                        p.default.as_ref().map_or(Value::Null, |d| match d {
                            crate::schema::DefaultValue::Now => Value::String("now()".into()),
                            crate::schema::DefaultValue::Value(v) => Value::String(crate::schema::literal(v)),
                        }),
                    ]
                })
                .collect();
            Ok(QueryResult::table(&["property", "type", "nullable", "index", "default"], rows))
        }
        Statement::Call { name, args } => {
            if depth >= MAX_CALL_DEPTH {
                return Err(Error::query("stored queries are nested too deeply"));
            }
            let q = g.queries.get(name).ok_or_else(|| Error::NotFound(format!("query '{name}'")))?;
            let call_params = write::call_params(g, params, &q.params, args, name)?;
            let mut last = QueryResult::default();
            for s in parse(&q.body)? {
                if !is_read_only(g, &s, depth + 1) {
                    return Err(Error::query(format!("stored query '{name}' writes data")));
                }
                last = run_read(g, &call_params, &s, now, depth + 1)?;
            }
            Ok(last)
        }
        other => Err(Error::query(format!("{} needs a write transaction", statement_name(other)))),
    }
}

fn describe_match(
    g: &Graph,
    params: &Params,
    from: &[crate::rql::ast::Pattern],
    where_: Option<&crate::rql::ast::Expr>,
    now: i64,
    action: &str,
) -> Result<Vec<String>> {
    let m = bind_where(g, params, from, where_)?;
    let p = plan(g, &m.scope, &m.patterns, m.conjuncts, None, now);
    let mut lines = p.describe(g, &m.scope);
    lines.push(action.to_owned());
    Ok(lines)
}

fn statement_name(s: &Statement) -> &'static str {
    match s {
        Statement::CreateType { .. } => "CREATE",
        Statement::DropType { .. } => "DROP",
        Statement::AlterAdd { .. } | Statement::AlterDrop { .. } => "ALTER",
        Statement::CreateIndex { .. } => "CREATE INDEX",
        Statement::DropIndex { .. } => "DROP INDEX",
        Statement::CreateQuery { .. } => "CREATE QUERY",
        Statement::DropQuery { .. } => "DROP QUERY",
        Statement::Call { .. } => "CALL",
        Statement::Show(_) => "SHOW",
        Statement::Describe(_) => "DESCRIBE",
        Statement::Explain(_) => "EXPLAIN",
        Statement::Select(_) => "SELECT",
        Statement::Insert(_) | Statement::InsertRows(_) => "INSERT",
        Statement::Update(_) => "UPDATE",
        Statement::Delete(_) => "DELETE",
    }
}

fn show(g: &Graph, what: Show) -> QueryResult {
    let s = |v: &str| Value::String(v.to_owned());
    let props = |t: &crate::schema::TypeDef| s(&t.props.iter().map(|p| format!("{} {}", p.name, p.ty)).collect::<Vec<_>>().join(", "));
    match what {
        Show::Schema => QueryResult::table(&["ddl"], g.schema.types().map(|t| vec![s(&t.ddl())]).collect()),
        Show::Nodes => QueryResult::table(
            &["type", "count", "properties"],
            g.schema.of_kind(Kind::Node).map(|t| vec![s(&t.name), int(g.member_count(t.id)), props(t)]).collect(),
        ),
        Show::Edges => QueryResult::table(
            &["type", "from", "to", "count", "properties"],
            g.schema
                .of_kind(Kind::Edge)
                .map(|t| vec![s(&t.name), s(&t.from.join(" | ")), s(&t.to.join(" | ")), int(g.member_count(t.id)), props(t)])
                .collect(),
        ),
        Show::Queries => QueryResult::table(
            &["name", "params", "body"],
            g.queries
                .values()
                .map(|q| {
                    let sig: Vec<String> = q.params.iter().map(|(n, t)| format!("${n} {t}")).collect();
                    vec![s(&q.name), s(&sig.join(", ")), s(&q.body)]
                })
                .collect(),
        ),
        Show::Indexes => {
            let mut rows = Vec::new();
            for t in g.schema.types() {
                for p in &t.props {
                    let mut kinds = Vec::new();
                    if p.unique {
                        kinds.push("UNIQUE".to_owned());
                    } else if p.indexed {
                        kinds.push("BTREE".to_owned());
                    }
                    if p.searchable {
                        kinds.push("BM25".to_owned());
                    }
                    if let crate::schema::PropType::Vector(n) = p.ty {
                        let size = g.idx.vector(t.id, &p.name).map_or(0, crate::index::vector::Hnsw::len);
                        kinds.push(format!("HNSW {n}d {} ({size} vectors)", p.metric));
                    }
                    for k in kinds {
                        rows.push(vec![s(&t.name), s(&p.name), s(&k)]);
                    }
                }
            }
            QueryResult::table(&["type", "property", "index"], rows)
        }
        Show::Stats => QueryResult::table(
            &["nodes", "edges", "node_types", "edge_types", "stored_queries"],
            vec![vec![
                int(g.nodes.len()),
                int(g.edges.len()),
                int(g.schema.of_kind(Kind::Node).count()),
                int(g.schema.of_kind(Kind::Edge).count()),
                int(g.queries.len()),
            ]],
        ),
    }
}

/// Runs any statement inside a write transaction.
pub(crate) fn run_write(txn: &mut Txn<'_>, params: &Params, stmt: &Statement, now: i64, depth: u32) -> Result<QueryResult> {
    let mut result = match stmt {
        Statement::Call { name, args } => {
            if depth >= MAX_CALL_DEPTH {
                return Err(Error::query("stored queries are nested too deeply"));
            }
            let q = txn.g.queries.get(name).cloned().ok_or_else(|| Error::NotFound(format!("query '{name}'")))?;
            let call_params = write::call_params(txn.g, params, &q.params, args, name)?;
            let mut last = QueryResult::default();
            for s in parse(&q.body)? {
                last = run_write(txn, &call_params, &s, now, depth + 1)?;
            }
            last
        }
        s if s.is_read_only() => run_read(txn.g, params, s, now, depth)?,
        Statement::CreateType { kind, name, if_not_exists, props, from, to } => {
            ddl::create_type(txn, *kind, name, *if_not_exists, props, from, to)?
        }
        Statement::DropType { kind, name, if_exists, cascade } => ddl::drop_type(txn, *kind, name, *if_exists, *cascade)?,
        Statement::AlterAdd { kind, name, prop } => ddl::alter_add(txn, *kind, name, prop, now)?,
        Statement::AlterDrop { kind, name, prop } => ddl::alter_drop(txn, *kind, name, prop)?,
        Statement::CreateIndex { name, prop, flavor } => ddl::set_index(txn, name, prop, Some(*flavor))?,
        Statement::DropIndex { name, prop } => ddl::set_index(txn, name, prop, None)?,
        Statement::CreateQuery { name, or_replace, params: ps, body, stmt } => {
            ddl::create_query(txn, name, *or_replace, ps, body, stmt, now, depth)?
        }
        Statement::DropQuery { name, if_exists } => ddl::drop_query(txn, name, *if_exists)?,
        Statement::Insert(i) => write::insert(txn, params, i, now)?,
        Statement::InsertRows(i) => write::insert_rows(txn, params, i, now)?,
        Statement::Update(u) => write::update(txn, params, u, now)?,
        Statement::Delete(d) => write::delete(txn, params, d, now)?,
        other => return Err(Error::query(format!("{} is not supported here", statement_name(other)))),
    };
    result.stats = txn.stats;
    Ok(result)
}

/// Checks a stored query's named parameters and returns its parsed body.
pub(crate) fn prepare_call(g: &Graph, name: &str, params: &Params) -> Result<(Vec<Statement>, Params)> {
    let q = g
        .queries
        .get(name)
        .ok_or_else(|| Error::NotFound(format!("query '{name}'{}", crate::error::hint(name, g.queries.keys().map(String::as_str)))))?;
    Ok((parse(&q.body)?, write::check_params(&q.params, params, name)?))
}
