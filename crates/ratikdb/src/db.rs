//! The embeddable database handle.

use std::path::{Path, PathBuf};

use parking_lot::RwLock;

use crate::error::{Error, Result};
use crate::exec::eval::now_millis;
use crate::exec::{Params, QueryResult, Txn, is_read_only, prepare_call, run_read, run_write};
use crate::graph::Graph;
use crate::rql::parse;
use crate::schema::Kind;
use crate::storage::{SyncMode, Wal};

/// Tuning knobs for [`Database::open_with`].
#[derive(Debug, Clone)]
pub struct Options {
    /// When the write-ahead log is flushed to disk.
    pub sync: SyncMode,
    /// Compact the log into a snapshot once it grows past this many bytes.
    pub snapshot_after_bytes: u64,
}

impl Default for Options {
    fn default() -> Self {
        Self { sync: SyncMode::Always, snapshot_after_bytes: 64 * 1024 * 1024 }
    }
}

/// Size of the database, as reported by [`Database::stats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct DbStats {
    /// Number of nodes.
    pub nodes: usize,
    /// Number of edges.
    pub edges: usize,
    /// Declared node types.
    pub node_types: usize,
    /// Declared edge types.
    pub edge_types: usize,
    /// Stored queries.
    pub stored_queries: usize,
    /// Bytes in the write-ahead log since the last snapshot.
    pub wal_bytes: u64,
}

#[derive(Debug)]
struct Engine {
    graph: Graph,
    wal: Option<Wal>,
    options: Options,
}

/// A RatikDB database: thread-safe, embeddable, and durable when opened on a
/// directory.
///
/// Reads run concurrently; writes are serialized and atomic — a script either
/// commits entirely or not at all.
///
/// ```
/// use ratikdb::Database;
///
/// let db = Database::in_memory();
/// db.execute("CREATE NODE User (name STRING UNIQUE, age INT)").unwrap();
/// db.execute("INSERT INTO User (name, age) VALUES ('Ada', 36), ('Linus', 28)").unwrap();
/// let r = db.execute("SELECT name FROM User WHERE age > 30").unwrap();
/// assert_eq!(r.rows, vec![vec!["Ada".into()]]);
/// ```
#[derive(Debug)]
pub struct Database {
    engine: RwLock<Engine>,
    path: Option<PathBuf>,
}

impl Database {
    /// A database that lives only in memory.
    #[must_use]
    pub fn in_memory() -> Self {
        Self { engine: RwLock::new(Engine { graph: Graph::default(), wal: None, options: Options::default() }), path: None }
    }

    /// Opens (or creates) a durable database in `dir`.
    ///
    /// # Errors
    /// I/O failures, corrupted files, or [`Error::Locked`] when another
    /// process has the directory open.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(dir, Options::default())
    }

    /// Like [`Database::open`] with explicit [`Options`].
    ///
    /// # Errors
    /// See [`Database::open`].
    pub fn open_with(dir: impl AsRef<Path>, options: Options) -> Result<Self> {
        let dir = dir.as_ref();
        let (graph, wal) = Wal::open(dir, options.sync)?;
        Ok(Self { engine: RwLock::new(Engine { graph, wal: Some(wal), options }), path: Some(dir.to_path_buf()) })
    }

    /// Directory backing this database (`None` when in memory).
    #[must_use]
    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    /// Runs an RQL script without parameters. See [`Database::execute_with`].
    ///
    /// # Errors
    /// See [`Database::execute_with`].
    pub fn execute(&self, rql: &str) -> Result<QueryResult> {
        self.execute_with(rql, &Params::new())
    }

    /// Runs an RQL script (one or more `;`-separated statements) and returns
    /// the result of the last statement.
    ///
    /// The whole script is one transaction: if any statement fails, nothing
    /// it or earlier statements did is kept.
    ///
    /// # Errors
    /// Syntax, schema, type and constraint errors, plus I/O failures while
    /// logging the commit.
    pub fn execute_with(&self, rql: &str, params: &Params) -> Result<QueryResult> {
        let stmts = parse(rql)?;
        self.run(&stmts, params)
    }

    /// Calls a stored query (`CREATE QUERY`) with named parameters.
    ///
    /// # Errors
    /// [`Error::NotFound`] for unknown queries, type errors for bad arguments,
    /// and anything the query itself can raise.
    pub fn call(&self, name: &str, params: &Params) -> Result<QueryResult> {
        let (stmts, params) = prepare_call(&self.engine.read().graph, name, params)?;
        self.run(&stmts, &params)
    }

    fn run(&self, stmts: &[crate::rql::ast::Statement], params: &Params) -> Result<QueryResult> {
        let now = now_millis();
        {
            let engine = self.engine.read();
            if stmts.iter().all(|s| is_read_only(&engine.graph, s, 0)) {
                let mut last = QueryResult::default();
                for s in stmts {
                    last = run_read(&engine.graph, params, s, now, 0)?;
                }
                return Ok(last);
            }
        }
        let mut guard = self.engine.write();
        let engine = &mut *guard;
        let mut txn = Txn::new(&mut engine.graph);
        let mut last = QueryResult::default();
        for s in stmts {
            match run_write(&mut txn, params, s, now, 0) {
                Ok(r) => last = r,
                Err(e) => {
                    txn.rollback();
                    return Err(e);
                }
            }
        }
        if !txn.log.is_empty()
            && let Some(wal) = engine.wal.as_mut()
            && let Err(e) = wal.append(&txn.log)
        {
            txn.rollback();
            return Err(e);
        }
        drop(txn);
        if let Some(wal) = engine.wal.as_mut()
            && wal.size() > engine.options.snapshot_after_bytes
        {
            // The commit is already durable in the log; a failed compaction
            // only means the log stays long until the next attempt.
            let _ = wal.snapshot(&engine.graph);
        }
        Ok(last)
    }

    /// Compacts the write-ahead log into a snapshot now.
    ///
    /// # Errors
    /// I/O failures. A no-op for in-memory databases.
    pub fn snapshot(&self) -> Result<()> {
        let mut guard = self.engine.write();
        let engine = &mut *guard;
        match engine.wal.as_mut() {
            Some(wal) => wal.snapshot(&engine.graph),
            None => Ok(()),
        }
    }

    /// The schema as runnable `CREATE` statements.
    #[must_use]
    pub fn schema_ddl(&self) -> String {
        self.engine.read().graph.schema.ddl()
    }

    /// The schema as JSON (used by the web console and SDKs).
    #[must_use]
    pub fn schema_json(&self) -> serde_json::Value {
        let engine = self.engine.read();
        let g = &engine.graph;
        let types: Vec<serde_json::Value> = g
            .schema
            .types()
            .map(|t| {
                serde_json::json!({
                    "name": t.name,
                    "kind": if t.kind == Kind::Node { "node" } else { "edge" },
                    "from": t.from,
                    "to": t.to,
                    "count": g.member_count(t.id),
                    "properties": t.props.iter().map(|p| serde_json::json!({
                        "name": p.name,
                        "type": p.ty.to_string(),
                        "nullable": !p.not_null,
                        "unique": p.unique,
                        "indexed": p.indexed || p.unique,
                        "searchable": p.searchable,
                    })).collect::<Vec<_>>(),
                })
            })
            .collect();
        let queries: Vec<serde_json::Value> = g
            .queries
            .values()
            .map(|q| {
                serde_json::json!({
                    "name": q.name,
                    "params": q.params.iter().map(|(n, t)| serde_json::json!({"name": n, "type": t.to_string()})).collect::<Vec<_>>(),
                    "body": q.body,
                })
            })
            .collect();
        serde_json::json!({ "types": types, "queries": queries, "ddl": g.schema.ddl() })
    }

    /// Element and type counts.
    #[must_use]
    pub fn stats(&self) -> DbStats {
        let engine = self.engine.read();
        let g = &engine.graph;
        DbStats {
            nodes: g.nodes.len(),
            edges: g.edges.len(),
            node_types: g.schema.of_kind(Kind::Node).count(),
            edge_types: g.schema.of_kind(Kind::Edge).count(),
            stored_queries: g.queries.len(),
            wal_bytes: engine.wal.as_ref().map_or(0, Wal::size),
        }
    }
}

impl Drop for Database {
    fn drop(&mut self) {
        // Leave a compact snapshot behind on clean shutdown so the next open
        // is fast. Errors are ignored: the log alone is always sufficient.
        let engine = self.engine.get_mut();
        if let Some(wal) = engine.wal.as_mut()
            && wal.size() > 1024
        {
            let _ = wal.snapshot(&engine.graph);
        }
    }
}

impl From<Error> for std::io::Error {
    fn from(e: Error) -> Self {
        match e {
            Error::Io(io) => io,
            other => Self::other(other),
        }
    }
}
