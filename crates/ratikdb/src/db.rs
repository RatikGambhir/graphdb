//! The embeddable database handle.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use parking_lot::RwLock;
use redb::ReadableDatabase as _;

use crate::error::{Error, Result};
use crate::exec::eval::now_millis;
use crate::exec::{Params, QueryResult, Txn, is_read_only, prepare_call, run_read, run_write};
use crate::graph::{Graph, META, State, VectorIndexes, create_tables};
use crate::index::vector::Hnsw;
use crate::rql::ast::Statement;
use crate::rql::parse;
use crate::schema::{Kind, PropType};
use crate::value::Value;

/// Name of the database file inside the data directory.
pub const DATA_FILE: &str = "ratik.redb";

/// When commits are flushed to stable storage.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SyncMode {
    /// Flush every commit before acknowledging it (safest; default). On
    /// macOS this is `F_FULLFSYNC`, which also flushes the drive's own write
    /// cache — plain `fsync` there does not guarantee the data hit the disk.
    #[default]
    Always,
    /// Let commits become durable lazily (fastest). A crash can lose the
    /// most recent commits but never corrupts the database; a clean shutdown
    /// or [`Database::snapshot`] makes everything durable.
    Never,
}

/// Tuning knobs for [`Database::open_with`].
#[derive(Debug, Clone)]
pub struct Options {
    /// When commits are flushed to disk.
    pub sync: SyncMode,
    /// Size of the in-memory page cache, in MiB. Data beyond this stays on
    /// disk and is read on demand.
    pub cache_mb: usize,
}

impl Default for Options {
    fn default() -> Self {
        Self {
            sync: SyncMode::Always,
            cache_mb: 256,
        }
    }
}

/// Size of the database, as reported by [`Database::stats`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize)]
pub struct DbStats {
    /// Number of nodes.
    pub nodes: u64,
    /// Number of edges.
    pub edges: u64,
    /// Declared node types.
    pub node_types: usize,
    /// Declared edge types.
    pub edge_types: usize,
    /// Stored queries.
    pub stored_queries: usize,
    /// Size of the database file in bytes (0 in memory).
    pub disk_bytes: u64,
}

struct Engine {
    db: redb::Database,
    state: State,
    sync: SyncMode,
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Engine")
            .field("sync", &self.sync)
            .finish_non_exhaustive()
    }
}

/// A RatikDB database: thread-safe, embeddable, and stored on disk when
/// opened on a directory.
///
/// Data lives in B-trees in a single file (`ratik.redb`); only the pages a
/// query touches are read, so the database can be much larger than memory.
/// Reads run concurrently; writes are serialized and atomic — a script
/// either commits entirely or not at all.
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
        let db = redb::Database::builder()
            .create_with_backend(redb::backends::InMemoryBackend::new())
            .expect("creating an in-memory database cannot fail");
        Self::init(db, SyncMode::Never, None)
            .expect("initialising an in-memory database cannot fail")
    }

    /// Opens (or creates) a database in `dir`.
    ///
    /// # Errors
    /// I/O failures, a corrupted file, or [`Error::Locked`] when another
    /// process has the database open.
    pub fn open(dir: impl AsRef<Path>) -> Result<Self> {
        Self::open_with(dir, Options::default())
    }

    /// Like [`Database::open`] with explicit [`Options`].
    ///
    /// # Errors
    /// See [`Database::open`].
    pub fn open_with(dir: impl AsRef<Path>, options: Options) -> Result<Self> {
        let dir = dir.as_ref();
        std::fs::create_dir_all(dir)?;
        let db = redb::Database::builder()
            .set_cache_size(options.cache_mb.max(1) * 1024 * 1024)
            .create(dir.join(DATA_FILE))
            .map_err(|e| match Error::from(e) {
                Error::Locked(_) => Error::Locked(dir.display().to_string()),
                other => other,
            })?;
        Self::init(db, options.sync, Some(dir.to_path_buf()))
    }

    fn init(db: redb::Database, sync: SyncMode, path: Option<PathBuf>) -> Result<Self> {
        let wtx = db.begin_write()?;
        create_tables(&wtx)?;
        wtx.commit()?;

        let rtx = db.begin_read()?;
        let meta = rtx.open_table(META)?;
        let decode = |e: postcard::Error| Error::Corrupt(format!("metadata: {e}"));
        let mut state = State::default();
        if let Some(bytes) = meta.get("schema")? {
            state.schema = Arc::new(postcard::from_bytes(bytes.value()).map_err(decode)?);
        }
        if let Some(bytes) = meta.get("queries")? {
            state.queries = Arc::new(postcard::from_bytes(bytes.value()).map_err(decode)?);
        }
        if let Some(bytes) = meta.get("next_id")? {
            let raw: [u8; 8] = bytes
                .value()
                .try_into()
                .map_err(|_| Error::Corrupt("metadata: bad id counter".into()))?;
            state.next_id = u64::from_be_bytes(raw);
        }
        drop(meta);
        state.vectors = Arc::new(rebuild_vectors(&state, &rtx)?);
        drop(rtx);
        Ok(Self {
            engine: RwLock::new(Engine { db, state, sync }),
            path,
        })
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
    /// Syntax, schema, type and constraint errors, plus storage failures.
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
        let (stmts, params) = prepare_call(&self.engine.read().state.queries, name, params)?;
        self.run(&stmts, &params)
    }

    fn run(&self, stmts: &[Statement], params: &Params) -> Result<QueryResult> {
        let now = now_millis();
        {
            let engine = self.engine.read();
            if stmts
                .iter()
                .all(|s| is_read_only(&engine.state.queries, s, 0))
            {
                let rtx = engine.db.begin_read()?;
                let g = Graph::read(&engine.state, &rtx)?;
                let mut last = QueryResult::default();
                for s in stmts {
                    last = run_read(&g, params, s, now, 0)?;
                }
                return Ok(last);
            }
        }
        let mut engine = self.engine.write();
        let mut wtx = engine.db.begin_write()?;
        if engine.sync == SyncMode::Never {
            wtx.set_durability(redb::Durability::None)
                .map_err(|e| Error::Storage(e.to_string()))?;
        }
        let mut g = Graph::write(&engine.state, wtx);
        let mut txn = Txn::new(&mut g);
        let mut last = QueryResult::default();
        for s in stmts {
            // On error `g` is dropped, which aborts the transaction.
            last = run_write(&mut txn, params, s, now, 0)?;
        }
        let committed = g.commit()?;
        engine.state.install(committed);
        Ok(last)
    }

    /// Makes every commit durable and compacts the database file, returning
    /// space freed by deletes to the operating system.
    ///
    /// # Errors
    /// Storage failures.
    pub fn snapshot(&self) -> Result<()> {
        let mut engine = self.engine.write();
        let wtx = engine.db.begin_write()?;
        wtx.commit()?;
        engine
            .db
            .compact()
            .map_err(|e| Error::Storage(e.to_string()))?;
        Ok(())
    }

    fn reader<T>(&self, f: impl FnOnce(&Graph) -> Result<T>) -> Result<T> {
        let engine = self.engine.read();
        let rtx = engine.db.begin_read()?;
        let g = Graph::read(&engine.state, &rtx)?;
        f(&g)
    }

    /// The schema as runnable `CREATE` statements.
    #[must_use]
    pub fn schema_ddl(&self) -> String {
        self.engine.read().state.schema.ddl()
    }

    /// The schema as JSON (used by the web console and SDKs).
    ///
    /// # Errors
    /// Storage failures while counting elements.
    pub fn schema_json(&self) -> Result<serde_json::Value> {
        self.reader(|g| {
            let mut types = Vec::new();
            for t in g.schema.types() {
                types.push(serde_json::json!({
                    "name": t.name,
                    "kind": if t.kind == Kind::Node { "node" } else { "edge" },
                    "from": t.from,
                    "to": t.to,
                    "count": g.member_count(t.id)?,
                    "properties": t.props.iter().map(|p| serde_json::json!({
                        "name": p.name,
                        "type": p.ty.to_string(),
                        "nullable": !p.not_null,
                        "unique": p.unique,
                        "indexed": p.indexed || p.unique,
                        "searchable": p.searchable,
                    })).collect::<Vec<_>>(),
                }));
            }
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
            Ok(serde_json::json!({ "types": types, "queries": queries, "ddl": g.schema.ddl() }))
        })
    }

    /// Element and type counts.
    ///
    /// # Errors
    /// Storage failures.
    pub fn stats(&self) -> Result<DbStats> {
        let disk_bytes = self
            .path
            .as_ref()
            .and_then(|p| std::fs::metadata(p.join(DATA_FILE)).ok())
            .map_or(0, |m| m.len());
        self.reader(|g| {
            Ok(DbStats {
                nodes: g.count_kind(Kind::Node)?,
                edges: g.count_kind(Kind::Edge)?,
                node_types: g.schema.of_kind(Kind::Node).count(),
                edge_types: g.schema.of_kind(Kind::Edge).count(),
                stored_queries: g.queries.len(),
                disk_bytes,
            })
        })
    }
}

/// Vector indexes are kept in memory; rebuild them from the vectors stored
/// on disk.
fn rebuild_vectors(state: &State, rtx: &redb::ReadTransaction) -> Result<VectorIndexes> {
    let mut out = VectorIndexes::default();
    let g = Graph::read(state, rtx)?;
    for t in state.schema.types() {
        for (slot, p) in t.props.iter().enumerate() {
            let PropType::Vector(dim) = p.ty else {
                continue;
            };
            let mut index = Hnsw::new(dim as usize, p.metric);
            for id in g.members(t.id) {
                let id = id?;
                if let Some(e) = g.element(id)?
                    && let Value::Vector(v) = &e.props[slot]
                {
                    index.insert(id, v);
                }
            }
            out.insert((t.id, p.name.clone()), index);
        }
    }
    Ok(out)
}

impl Drop for Database {
    fn drop(&mut self) {
        // With lazy durability, one durable (empty) commit flushes everything
        // committed before it. Errors are ignored: there is no one to report
        // them to, and the database stays consistent either way.
        let engine = self.engine.get_mut();
        if engine.sync == SyncMode::Never
            && let Ok(wtx) = engine.db.begin_write()
        {
            let _ = wtx.commit();
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
