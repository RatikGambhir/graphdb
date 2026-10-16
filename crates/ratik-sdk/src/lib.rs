//! # Ratik — the RatikDB Rust SDK
//!
//! Two ways to talk to RatikDB, one API:
//!
//! * **The DSL** — chain plain-English methods; the SDK writes the query.
//! * **RQL** — SQL with graph patterns, whenever you want full control.
//!
//! ```no_run
//! use ratik::prelude::*;
//!
//! fn main() -> ratik::Result<()> {
//!     let db = Ratik::connect("http://127.0.0.1:7285")?;
//!
//!     // Declare the schema (idempotent).
//!     db.define(node("User").string("name").required().unique().int("age"))?;
//!     db.define(edge("FOLLOWS").from("User").to("User").int("since"))?;
//!
//!     // Write.
//!     let ada = db.create("User").set("name", "Ada").set("age", 36).run()?;
//!     let alan = db.create("User").set("name", "Alan").set("age", 41).run()?;
//!     db.link(&ada, "FOLLOWS", &alan).set("since", 2024).run()?;
//!
//!     // Read — it reads like a sentence.
//!     let who_ada_follows = db.find("User").with("name", "Ada").outgoing("FOLLOWS").all()?;
//!     let adults = db.find("User").filter(prop("age").gte(18)).order_by("name").count()?;
//!
//!     // Or drop down to RQL.
//!     let rows = db
//!         .query("SELECT f.name, e.since FROM (u:User)-[e:FOLLOWS]->(f) WHERE u.name = $name")
//!         .bind("name", "Ada")
//!         .fetch()?;
//!     for row in &rows {
//!         let name: String = row.get("f.name")?;
//!         println!("Ada follows {name}");
//!     }
//!     # let _ = (who_ada_follows, adults);
//!     Ok(())
//! }
//! ```
//!
//! With the `embedded` feature, `Ratik::open("./data")` runs the database
//! inside your process instead — same API, no server.

#[cfg(not(any(feature = "remote", feature = "embedded")))]
compile_error!("enable at least one of the `remote` or `embedded` features of the `ratik` crate");

mod cond;
mod error;
mod find;
mod rows;
mod schema;
mod write;

use serde::Serialize;
use serde::de::DeserializeOwned;
use serde_json::{Map, Value as Json};

pub use cond::{Cond, Prop, prop};
pub use error::{Error, Result};
pub use find::{Find, Hops, Update};
pub use rows::{Edge, Node, NodeId, Row, Rows, Stats};
pub use schema::{Metric, TypeSpec, edge, node};
pub use write::{Create, Link};

/// Everything you need in one import: `use ratik::prelude::*;`
pub mod prelude {
    pub use crate::{Cond, Edge, Metric, Node, Ratik, Row, Rows, edge, node, prop};
}

/// The default address of `ratik serve` / `ratik start`.
pub const DEFAULT_URL: &str = "http://127.0.0.1:7285";

#[derive(Debug, Clone)]
enum Backend {
    #[cfg(feature = "remote")]
    Remote {
        agent: ureq::Agent,
        base: String,
        token: Option<String>,
    },
    #[cfg(feature = "embedded")]
    Embedded(std::sync::Arc<ratikdb::Database>),
}

/// A connection to RatikDB — remote (`Ratik::connect`) or embedded
/// (`Ratik::open`). Cheap to clone; share it freely between threads.
#[derive(Debug, Clone)]
pub struct Ratik {
    backend: Backend,
}

impl Ratik {
    /// Connects to a running `ratik` server, e.g. `http://127.0.0.1:7285`.
    ///
    /// # Errors
    /// [`Error::Connection`] if the server does not answer its health check.
    #[cfg(feature = "remote")]
    pub fn connect(url: &str) -> Result<Self> {
        Self::connect_with_token(url, None)
    }

    /// Connects to a server started with `--token`.
    ///
    /// # Errors
    /// See [`Ratik::connect`].
    #[cfg(feature = "remote")]
    pub fn connect_with_token(url: &str, token: Option<&str>) -> Result<Self> {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(std::time::Duration::from_secs(600)))
            .build()
            .into();
        let db = Self {
            backend: Backend::Remote {
                agent,
                base: url.trim_end_matches('/').to_owned(),
                token: token.map(str::to_owned),
            },
        };
        db.http("GET", "/v1/health", None)?;
        Ok(db)
    }

    /// Opens (or creates) an embedded database in a directory.
    ///
    /// # Errors
    /// I/O errors, or the directory being in use by another process.
    #[cfg(feature = "embedded")]
    pub fn open(dir: impl AsRef<std::path::Path>) -> Result<Self> {
        Ok(Self::embedded(ratikdb::Database::open(dir)?))
    }

    /// A throwaway embedded database that lives in memory.
    #[cfg(feature = "embedded")]
    #[must_use]
    pub fn memory() -> Self {
        Self::embedded(ratikdb::Database::in_memory())
    }

    /// Wraps an existing engine handle.
    #[cfg(feature = "embedded")]
    #[must_use]
    pub fn embedded(db: ratikdb::Database) -> Self {
        Self {
            backend: Backend::Embedded(std::sync::Arc::new(db)),
        }
    }

    // ── the DSL ──────────────────────────────────────────────────────────

    /// Finds nodes of a type: `db.find("User").with("name", "Ada").first()?`.
    pub fn find(&self, node_type: &str) -> Find<'_> {
        Find::new(self, node_type)
    }

    /// Creates a node: `db.create("User").set("name", "Ada").run()?`.
    pub fn create(&self, node_type: &str) -> Create<'_> {
        Create::new(self, node_type)
    }

    /// Creates many nodes (from structs or maps) in one atomic statement.
    ///
    /// # Errors
    /// Schema/type/constraint errors — if any item is invalid, none are created.
    pub fn create_many<T: Serialize>(
        &self,
        node_type: &str,
        items: impl IntoIterator<Item = T>,
    ) -> Result<Vec<Node>> {
        write::create_many(self, node_type, items)
    }

    /// Connects two nodes: `db.link(&ada, "FOLLOWS", &alan).run()?`.
    pub fn link(
        &self,
        from: impl Into<NodeId>,
        edge_type: &str,
        to: impl Into<NodeId>,
    ) -> Link<'_> {
        Link::new(self, from.into(), edge_type, to.into())
    }

    /// Removes every `edge_type` edge from `from` to `to`; returns how many.
    ///
    /// # Errors
    /// Schema errors for unknown edge types.
    pub fn unlink(
        &self,
        from: impl Into<NodeId>,
        edge_type: &str,
        to: impl Into<NodeId>,
    ) -> Result<u64> {
        let rows = self
            .query(&format!(
                "DELETE e FROM (a)-[e:{}]->(b) WHERE a.`_id` = $from AND b.`_id` = $to",
                cond::ident(edge_type)
            ))
            .bind("from", from.into().0)
            .bind("to", to.into().0)
            .fetch()?;
        Ok(rows.stats.edges_deleted)
    }

    /// Fetches a node by id.
    ///
    /// # Errors
    /// Connection errors.
    pub fn node(&self, id: impl Into<NodeId>) -> Result<Option<Node>> {
        let rows = self
            .query("SELECT n FROM (n) WHERE n.`_id` = $id")
            .bind("id", id.into().0)
            .fetch()?;
        rows.rows
            .into_iter()
            .next()
            .map(|mut r| Node::from_json(r.swap_remove(0)))
            .transpose()
    }

    /// Declares a node or edge type (a no-op if it already exists).
    ///
    /// # Errors
    /// Schema errors, e.g. an edge referencing an undeclared node type.
    pub fn define(&self, spec: TypeSpec) -> Result<()> {
        self.execute(&spec.to_rql()).map(|_| ())
    }

    /// Declares several types in one transaction.
    ///
    /// # Errors
    /// See [`Ratik::define`]; on error nothing is declared.
    pub fn define_all(&self, specs: impl IntoIterator<Item = TypeSpec>) -> Result<()> {
        let script: Vec<String> = specs.into_iter().map(|s| s.to_rql()).collect();
        self.execute(&script.join(";\n")).map(|_| ())
    }

    /// The current schema as RQL `CREATE` statements.
    ///
    /// # Errors
    /// Connection errors.
    pub fn schema(&self) -> Result<String> {
        Ok(self
            .execute("SHOW SCHEMA")?
            .column::<String>("ddl")?
            .join("\n\n"))
    }

    // ── RQL ──────────────────────────────────────────────────────────────

    /// Prepares an RQL query; add parameters with `.bind(...)`.
    pub fn query(&self, rql: &str) -> Query<'_> {
        Query {
            db: self,
            target: Target::Rql(rql.to_owned()),
            params: Map::new(),
        }
    }

    /// Prepares a call to a stored query (`CREATE QUERY ...`).
    pub fn call(&self, stored_query: &str) -> Query<'_> {
        Query {
            db: self,
            target: Target::Call(stored_query.to_owned()),
            params: Map::new(),
        }
    }

    /// Runs an RQL script with no parameters.
    ///
    /// # Errors
    /// Whatever the database reports.
    pub fn execute(&self, rql: &str) -> Result<Rows> {
        self.run_rql(rql, Map::new())
    }

    pub(crate) fn run_rql(&self, rql: &str, params: Map<String, Json>) -> Result<Rows> {
        self.run(&Target::Rql(rql.to_owned()), params)
    }

    fn run(&self, target: &Target, params: Map<String, Json>) -> Result<Rows> {
        match &self.backend {
            #[cfg(feature = "remote")]
            Backend::Remote { .. } => {
                let (path, body) = match target {
                    Target::Rql(q) => (
                        "/v1/query".to_owned(),
                        serde_json::json!({"query": q, "params": params}),
                    ),
                    Target::Call(name) => (
                        format!("/v1/call/{name}"),
                        serde_json::json!({"params": params}),
                    ),
                };
                let json = self.http("POST", &path, Some(body))?;
                rows::decode(json, "response")
            }
            #[cfg(feature = "embedded")]
            Backend::Embedded(db) => {
                let params: ratikdb::Params = params
                    .into_iter()
                    .map(|(k, v)| (k, ratikdb::Value::from_json(v)))
                    .collect();
                let r = match target {
                    Target::Rql(q) => db.execute_with(q, &params)?,
                    Target::Call(name) => db.call(name, &params)?,
                };
                let s = r.stats;
                Ok(Rows {
                    columns: r.columns,
                    rows: r
                        .rows
                        .iter()
                        .map(|row| row.iter().map(ratikdb::Value::to_json).collect())
                        .collect(),
                    stats: Stats {
                        nodes_created: s.nodes_created,
                        nodes_deleted: s.nodes_deleted,
                        edges_created: s.edges_created,
                        edges_deleted: s.edges_deleted,
                        properties_set: s.properties_set,
                        schema_changes: s.schema_changes,
                    },
                })
            }
        }
    }

    #[cfg(feature = "remote")]
    fn http(&self, method: &str, path: &str, body: Option<Json>) -> Result<Json> {
        #[allow(irrefutable_let_patterns)]
        let Backend::Remote { agent, base, token } = &self.backend else {
            return Err(Error::Usage("not a remote connection".into()));
        };
        let url = format!("{base}{path}");
        let auth = token.as_ref().map(|t| format!("Bearer {t}"));
        let sent = match (method, body) {
            ("GET", _) => {
                let mut req = agent.get(&url);
                if let Some(a) = &auth {
                    req = req.header("Authorization", a);
                }
                req.call()
            }
            (_, body) => {
                let mut req = agent.post(&url);
                if let Some(a) = &auth {
                    req = req.header("Authorization", a);
                }
                req.send_json(body.unwrap_or(Json::Null))
            }
        };
        let mut resp = sent.map_err(|e| Error::Connection(format!("{url}: {e}")))?;
        let status = resp.status().as_u16();
        let json: Json = resp
            .body_mut()
            .with_config()
            .limit(u64::MAX)
            .read_json()
            .map_err(|e| Error::Connection(format!("{url}: invalid response ({e})")))?;
        if (200..300).contains(&status) {
            return Ok(json);
        }
        let err = &json["error"];
        Err(Error::Database {
            code: err["code"].as_str().unwrap_or("http_error").to_owned(),
            message: err["message"]
                .as_str()
                .map_or_else(|| format!("HTTP {status}"), str::to_owned),
        })
    }
}

#[derive(Debug, Clone)]
enum Target {
    Rql(String),
    Call(String),
}

/// An RQL query or stored-query call with bound parameters.
#[derive(Debug, Clone)]
#[must_use = "a query does nothing until .fetch() / .run()"]
pub struct Query<'db> {
    db: &'db Ratik,
    target: Target,
    params: Map<String, Json>,
}

impl Query<'_> {
    /// Binds `$name` to a value.
    pub fn bind(mut self, name: &str, value: impl Into<Json>) -> Self {
        self.params
            .insert(name.trim_start_matches('$').to_owned(), value.into());
        self
    }

    /// Binds every field of a serializable struct or map.
    ///
    /// # Errors
    /// When `value` does not serialize to an object.
    pub fn bind_all(mut self, value: impl Serialize) -> Result<Self> {
        self.params.extend(write::to_props(value)?);
        Ok(self)
    }

    /// Runs the query and returns the result table.
    ///
    /// # Errors
    /// Whatever the database reports.
    pub fn fetch(self) -> Result<Rows> {
        self.db.run(&self.target, self.params)
    }

    /// Runs the query for its side effects and returns what changed.
    ///
    /// # Errors
    /// Whatever the database reports.
    pub fn run(self) -> Result<Stats> {
        Ok(self.fetch()?.stats)
    }

    /// Runs the query and decodes every row into `T`.
    ///
    /// # Errors
    /// Database or decode errors.
    pub fn all_as<T: DeserializeOwned>(self) -> Result<Vec<T>> {
        self.fetch()?.decode()
    }

    /// Runs the query and decodes the first row into `T`.
    ///
    /// # Errors
    /// Database or decode errors.
    pub fn first_as<T: DeserializeOwned>(self) -> Result<Option<T>> {
        self.fetch()?.first().map(|r| r.decode()).transpose()
    }
}
