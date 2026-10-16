//! Error type shared by every part of the engine.

/// Convenient alias used throughout RatikDB.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Everything that can go wrong inside RatikDB.
///
/// Messages are written for humans: they say what was wrong and, where
/// possible, how to fix it.
#[derive(Debug, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The query text could not be parsed.
    #[error("syntax error at {line}:{col}: {msg}")]
    Syntax {
        /// 1-based line of the offending token.
        line: u32,
        /// 1-based column of the offending token.
        col: u32,
        /// What went wrong.
        msg: String,
    },
    /// The query referenced something the schema does not declare, or a DDL
    /// statement would leave the schema inconsistent.
    #[error("schema error: {0}")]
    Schema(String),
    /// A value had the wrong type for where it was used.
    #[error("type error: {0}")]
    Type(String),
    /// A UNIQUE / NOT NULL / endpoint constraint was violated.
    #[error("constraint violation: {0}")]
    Constraint(String),
    /// The query is well formed but cannot be executed.
    #[error("query error: {0}")]
    Query(String),
    /// A referenced stored query, element or file does not exist.
    #[error("not found: {0}")]
    NotFound(String),
    /// Underlying file-system failure.
    #[error("i/o error: {0}")]
    Io(#[from] std::io::Error),
    /// On-disk data could not be decoded.
    #[error("storage corruption: {0}")]
    Corrupt(String),
    /// Another process already holds the data directory.
    #[error("data directory is locked by another RatikDB process: {0}")]
    Locked(String),
}

impl Error {
    /// Stable, machine-readable error code (used by the HTTP API and SDKs).
    #[must_use]
    pub fn code(&self) -> &'static str {
        match self {
            Self::Syntax { .. } => "syntax_error",
            Self::Schema(_) => "schema_error",
            Self::Type(_) => "type_error",
            Self::Constraint(_) => "constraint_violation",
            Self::Query(_) => "query_error",
            Self::NotFound(_) => "not_found",
            Self::Io(_) => "io_error",
            Self::Corrupt(_) => "corrupt",
            Self::Locked(_) => "locked",
        }
    }

    pub(crate) fn schema(msg: impl Into<String>) -> Self {
        Self::Schema(msg.into())
    }

    pub(crate) fn ty(msg: impl Into<String>) -> Self {
        Self::Type(msg.into())
    }

    pub(crate) fn query(msg: impl Into<String>) -> Self {
        Self::Query(msg.into())
    }

    pub(crate) fn constraint(msg: impl Into<String>) -> Self {
        Self::Constraint(msg.into())
    }
}

/// Returns the candidate closest to `name` (by edit distance) when it is
/// close enough to plausibly be a typo.
pub(crate) fn suggest<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> Option<&'a str> {
    let lower = name.to_ascii_lowercase();
    candidates
        .into_iter()
        .map(|c| (edit_distance(&lower, &c.to_ascii_lowercase()), c))
        .filter(|(d, c)| *d <= (c.len().max(name.len()) / 3).max(1))
        .min_by_key(|(d, _)| *d)
        .map(|(_, c)| c)
}

/// Formats a ` — did you mean 'x'?` hint, or nothing.
pub(crate) fn hint<'a>(name: &str, candidates: impl IntoIterator<Item = &'a str>) -> String {
    suggest(name, candidates).map_or_else(String::new, |s| format!(" — did you mean '{s}'?"))
}

fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut prev: Vec<usize> = (0..=b.len()).collect();
    let mut cur = vec![0; b.len() + 1];
    for (i, ca) in a.chars().enumerate() {
        cur[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let cost = usize::from(ca != *cb);
            cur[j + 1] = (prev[j] + cost).min(prev[j + 1] + 1).min(cur[j] + 1);
        }
        std::mem::swap(&mut prev, &mut cur);
    }
    prev[b.len()]
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn suggests_close_names_only() {
        assert_eq!(suggest("agee", ["age", "name"]), Some("age"));
        assert_eq!(suggest("Usr", ["User", "Post"]), Some("User"));
        assert_eq!(suggest("zzzzzz", ["age", "name"]), None);
    }
}
