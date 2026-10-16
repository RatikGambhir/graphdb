/// Result alias for the SDK.
pub type Result<T, E = Error> = std::result::Result<T, E>;

/// Errors returned by the SDK.
#[derive(Debug, Clone, thiserror::Error)]
#[non_exhaustive]
pub enum Error {
    /// The database rejected the query (syntax, schema, type, constraint...).
    #[error("{message}")]
    Database {
        /// Stable error code, e.g. `schema_error` or `constraint_violation`.
        code: String,
        /// Human-readable explanation.
        message: String,
    },
    /// The server could not be reached or answered with something unexpected.
    #[error("connection error: {0}")]
    Connection(String),
    /// A value could not be converted into the requested Rust type.
    #[error("could not decode {what}: {reason}")]
    Decode {
        /// What was being decoded.
        what: String,
        /// Why it failed.
        reason: String,
    },
    /// `.one()` found nothing.
    #[error("no matching {0} found")]
    NotFound(String),
    /// The SDK was used incorrectly (e.g. an empty builder).
    #[error("{0}")]
    Usage(String),
}

impl Error {
    /// The database error code, if this came from the database.
    #[must_use]
    pub fn code(&self) -> Option<&str> {
        match self {
            Self::Database { code, .. } => Some(code),
            _ => None,
        }
    }
}

#[cfg(feature = "embedded")]
impl From<ratikdb::Error> for Error {
    fn from(e: ratikdb::Error) -> Self {
        Self::Database { code: e.code().to_owned(), message: e.to_string() }
    }
}
