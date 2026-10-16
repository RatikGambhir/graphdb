//! RQL — the Ratik Query Language. SQL you already know, plus graph
//! patterns, vector similarity and full-text search.

pub mod ast;
pub(crate) mod lexer;
pub mod parser;

pub use parser::parse;
