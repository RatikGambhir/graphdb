//! HTTP transport to a `ratik` server. Blocking calls, meant to run on the
//! background executor; the app never talks to the network from the UI thread.

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::{Map, Value, json};

/// How to reach a server. Persisted between launches.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct ConnectionSettings {
    /// Origin such as `http://127.0.0.1:7285`, or a gateway path.
    pub url: String,
    /// Bearer token for servers started with `--token`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub token: Option<String>,
    /// Request timeout.
    #[serde(default = "default_timeout_ms")]
    pub timeout_ms: u64,
}

fn default_timeout_ms() -> u64 {
    30_000
}

impl Default for ConnectionSettings {
    fn default() -> Self {
        Self {
            url: "http://127.0.0.1:7285".to_owned(),
            token: None,
            timeout_ms: default_timeout_ms(),
        }
    }
}

impl ConnectionSettings {
    /// Host portion for compact display, e.g. `127.0.0.1:7285`.
    #[must_use]
    pub fn short_host(&self) -> String {
        let trimmed = self
            .url
            .trim_start_matches("http://")
            .trim_start_matches("https://");
        trimmed.trim_end_matches('/').to_owned()
    }

    /// The query endpoint.
    #[must_use]
    pub fn query_endpoint(&self) -> String {
        format!("{}/v1/query", self.url.trim_end_matches('/'))
    }
}

/// What can go wrong talking to the server.
#[derive(Debug, Clone, thiserror::Error)]
pub enum ClientError {
    /// No HTTP response at all.
    #[error("{0}")]
    Connection(String),
    /// The server answered with a structured error.
    #[error("{message}")]
    Api {
        status: u16,
        code: String,
        message: String,
    },
    /// The response could not be understood.
    #[error("unexpected response: {0}")]
    Decode(String),
}

impl ClientError {
    /// `true` when the failure means the connection itself is gone.
    #[must_use]
    pub fn is_connection_failure(&self) -> bool {
        matches!(self, Self::Connection(_))
            || matches!(self, Self::Api { status, .. } if *status == 401)
    }

    /// A one-line label, the structured code where available.
    #[must_use]
    pub fn headline(&self) -> String {
        match self {
            Self::Api { code, .. } => code.replace('_', " "),
            Self::Connection(_) => "connection failed".to_owned(),
            Self::Decode(_) => "unreadable response".to_owned(),
        }
    }
}

/// `GET /v1/health`.
#[derive(Clone, Debug, PartialEq)]
pub struct Health {
    pub version: String,
    pub duration_ms: f64,
}

/// Counters a statement reports.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Stats {
    #[serde(default)]
    pub nodes_created: u64,
    #[serde(default)]
    pub nodes_deleted: u64,
    #[serde(default)]
    pub edges_created: u64,
    #[serde(default)]
    pub edges_deleted: u64,
    #[serde(default)]
    pub properties_set: u64,
    #[serde(default)]
    pub schema_changes: u64,
}

impl Stats {
    /// `true` when nothing changed.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == Self::default()
    }

    /// Sums counters from a later statement.
    pub fn add(&mut self, other: &Self) {
        self.nodes_created += other.nodes_created;
        self.nodes_deleted += other.nodes_deleted;
        self.edges_created += other.edges_created;
        self.edges_deleted += other.edges_deleted;
        self.properties_set += other.properties_set;
        self.schema_changes += other.schema_changes;
    }
}

/// `POST /v1/query` decoded.
#[derive(Clone, Debug, PartialEq, Deserialize)]
pub struct QueryResponse {
    #[serde(default)]
    pub columns: Vec<String>,
    #[serde(default)]
    pub rows: Vec<Vec<Value>>,
    #[serde(default)]
    pub stats: Stats,
    #[serde(default)]
    pub time_ms: f64,
}

/// A response with the raw JSON kept for the wire-format view.
#[derive(Clone, Debug, PartialEq)]
pub struct Executed {
    pub response: QueryResponse,
    pub raw: Value,
    /// Round-trip wall time as seen by the app.
    pub duration_ms: f64,
}

/// A blocking HTTP client. Cheap to clone and safe to move to a worker.
#[derive(Clone, Debug)]
pub struct Client {
    agent: ureq::Agent,
    settings: Arc<ConnectionSettings>,
}

impl Client {
    /// A client for the given settings.
    #[must_use]
    pub fn new(settings: ConnectionSettings) -> Self {
        let agent: ureq::Agent = ureq::Agent::config_builder()
            .http_status_as_error(false)
            .timeout_global(Some(Duration::from_millis(settings.timeout_ms.max(1000))))
            .build()
            .into();
        Self {
            agent,
            settings: Arc::new(settings),
        }
    }

    /// The settings this client was built from.
    #[must_use]
    pub fn settings(&self) -> &ConnectionSettings {
        &self.settings
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.settings.url.trim_end_matches('/'), path)
    }

    fn decode(status: u16, body: &str) -> Result<Value, ClientError> {
        let json: Value = serde_json::from_str(body).map_err(|e| {
            if status >= 400 {
                ClientError::Api {
                    status,
                    code: format!("http_{status}"),
                    message: format!("HTTP {status}: {}", truncate(body, 200)),
                }
            } else {
                ClientError::Decode(format!("{e} ({})", truncate(body, 120)))
            }
        })?;
        if let Some(error) = json.get("error") {
            let code = error
                .get("code")
                .and_then(Value::as_str)
                .unwrap_or("error")
                .to_owned();
            let message = error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("unknown error")
                .to_owned();
            return Err(ClientError::Api {
                status,
                code,
                message,
            });
        }
        if status >= 400 {
            return Err(ClientError::Api {
                status,
                code: format!("http_{status}"),
                message: format!("HTTP {status}"),
            });
        }
        Ok(json)
    }

    fn request(
        &self,
        method: &str,
        path: &str,
        body: Option<&Value>,
    ) -> Result<Value, ClientError> {
        let url = self.url(path);
        let token = self.settings.token.as_deref();
        let result = match (method, body) {
            ("POST", Some(body)) => {
                let mut req = self
                    .agent
                    .post(&url)
                    .header("content-type", "application/json");
                if let Some(token) = token {
                    req = req.header("authorization", &format!("Bearer {token}"));
                }
                req.send_json(body)
            }
            _ => {
                let mut req = self.agent.get(&url);
                if let Some(token) = token {
                    req = req.header("authorization", &format!("Bearer {token}"));
                }
                req.call()
            }
        };
        let mut response =
            result.map_err(|e| ClientError::Connection(describe_transport_error(&e, &url)))?;
        let status = response.status().as_u16();
        let body = response
            .body_mut()
            .with_config()
            .limit(64 * 1024 * 1024)
            .read_to_string()
            .map_err(|e| ClientError::Decode(e.to_string()))?;
        Self::decode(status, &body)
    }

    /// Liveness check.
    pub fn health(&self) -> Result<Health, ClientError> {
        let started = Instant::now();
        let json = self.request("GET", "/v1/health", None)?;
        let version = json
            .get("version")
            .and_then(Value::as_str)
            .unwrap_or("?")
            .to_owned();
        if json.get("name").and_then(Value::as_str) != Some("RatikDB") {
            return Err(ClientError::Decode(
                "the endpoint answered, but it is not a RatikDB server".to_owned(),
            ));
        }
        Ok(Health {
            version,
            duration_ms: started.elapsed().as_secs_f64() * 1000.,
        })
    }

    /// Runs RQL with named parameters.
    pub fn query(&self, rql: &str, params: &Map<String, Value>) -> Result<Executed, ClientError> {
        let started = Instant::now();
        let body = json!({ "query": rql, "params": params });
        let raw = self.request("POST", "/v1/query", Some(&body))?;
        let response: QueryResponse =
            serde_json::from_value(raw.clone()).map_err(|e| ClientError::Decode(e.to_string()))?;
        Ok(Executed {
            response,
            raw,
            duration_ms: started.elapsed().as_secs_f64() * 1000.,
        })
    }

    /// `GET /v1/schema`.
    pub fn schema(&self) -> Result<Value, ClientError> {
        self.request("GET", "/v1/schema", None)
    }
}

fn describe_transport_error(error: &ureq::Error, url: &str) -> String {
    let text = error.to_string();
    if text.contains("Connection refused") || text.contains("connection refused") {
        format!("nothing is listening at {url} (connection refused)")
    } else if text.contains("timed out") || text.contains("timeout") {
        format!("the request to {url} timed out")
    } else {
        text
    }
}

fn truncate(text: &str, max: usize) -> String {
    if text.chars().count() <= max {
        text.trim().to_owned()
    } else {
        let cut: String = text.chars().take(max).collect();
        format!("{}…", cut.trim())
    }
}

/// The request body as the server sees it, pretty-printed.
#[must_use]
pub fn wire_request(rql: &str, params: &Map<String, Value>) -> String {
    serde_json::to_string_pretty(&json!({ "query": rql, "params": params })).unwrap_or_default()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn decodes_structured_errors() {
        let err = Client::decode(400, r#"{"error":{"code":"syntax_error","message":"bad"}}"#)
            .unwrap_err();
        match err {
            ClientError::Api {
                status,
                code,
                message,
            } => {
                assert_eq!(
                    (status, code.as_str(), message.as_str()),
                    (400, "syntax_error", "bad")
                );
            }
            other => panic!("unexpected {other:?}"),
        }
    }

    #[test]
    fn short_host_strips_scheme() {
        let s = ConnectionSettings {
            url: "https://db.example.com/helix/".into(),
            ..Default::default()
        };
        assert_eq!(s.short_host(), "db.example.com/helix");
        assert_eq!(s.query_endpoint(), "https://db.example.com/helix/v1/query");
    }
}
