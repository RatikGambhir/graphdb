//! Search structures: BM25 scoring helpers and the HNSW vector index.
//!
//! B-tree property indexes and the full-text postings live on disk, in the
//! graph store's tables (see `graph.rs`).

pub(crate) mod text;
pub(crate) mod vector;
