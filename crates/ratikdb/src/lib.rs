//! # RatikDB
//!
//! A strictly-typed graph database with built-in vector search and BM25
//! full-text search, queried with **RQL** — SQL you already know, plus graph
//! patterns.
//!
//! ```
//! use ratikdb::{Database, Params, Value};
//!
//! let db = Database::in_memory();
//! db.execute(
//!     "CREATE NODE Person (name STRING NOT NULL UNIQUE, born INT);
//!      CREATE EDGE KNOWS FROM Person TO Person (since INT);",
//! ).unwrap();
//! db.execute(
//!     "INSERT (:Person {name: 'Ada', born: 1815})-[:KNOWS {since: 1833}]->(:Person {name: 'Charles', born: 1791})",
//! ).unwrap();
//!
//! let mut params = Params::new();
//! params.insert("name".into(), Value::from("Ada"));
//! let r = db
//!     .execute_with("SELECT friend.name FROM (p:Person)-[:KNOWS]->(friend) WHERE p.name = $name", &params)
//!     .unwrap();
//! assert_eq!(r.rows, vec![vec![Value::from("Charles")]]);
//! ```
//!
//! Everything must be declared before it is used: inserting or querying an
//! undeclared type or property is an error, with a suggestion when it looks
//! like a typo.

mod db;
mod error;
mod exec;
mod graph;
mod index;
pub mod rql;
pub mod schema;
mod storage;
mod value;

pub use db::{Database, DbStats, Options};
pub use error::{Error, Result};
pub use exec::{Params, QueryResult, Stats};
pub use graph::StoredQuery;
pub use storage::SyncMode;
pub use value::{ElementId, Value};
