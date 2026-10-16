//! The application: one entity owns the connection session and every
//! workspace resource, so a reconnect or disconnect invalidates pending
//! query, inspector, schema and graph requests together (each request carries
//! the generation and its own request id, and stale replies are dropped).

mod dialog;
mod graph_workspace;
mod inspector;
mod query;
mod results_view;
mod schema_view;
mod shell;
mod splash;

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Instant;

use gpui::{
    App, Context, Entity, FocusHandle, Focusable, IntoElement, KeyBinding, PromptLevel, Render,
    SharedString, Window, actions, div, prelude::*,
};
use serde_json::{Map, Value};

use crate::client::{Client, ClientError, ConnectionSettings, Executed};
use crate::graph::{GraphEvent, GraphView};
use crate::results::{
    ElementKind, EntityDetail, Execution, GraphBuilder, GraphData, build_execution, edge_detail,
    node_detail,
};
use crate::rql::{self, Analysis};
use crate::schema::Schema;
use crate::settings::{self, Settings};
use crate::theme::{self, Mode};
use crate::ui::{Select, TextField, TextFieldEvent, TextFieldOptions};

pub use dialog::ConnectionDialog;
pub use splash::Splash;

actions!(
    atlas,
    [
        Quit,
        ToggleTheme,
        ShowQuery,
        ShowSchema,
        ShowGraph,
        OpenConnection,
        RunQuery,
        ContinueSplash,
    ]
);

/// Global key bindings.
#[must_use]
pub fn key_bindings() -> Vec<KeyBinding> {
    vec![
        KeyBinding::new("cmd-q", Quit, None),
        KeyBinding::new("cmd-shift-l", ToggleTheme, None),
        KeyBinding::new("cmd-1", ShowQuery, None),
        KeyBinding::new("cmd-2", ShowSchema, None),
        KeyBinding::new("cmd-3", ShowGraph, None),
        KeyBinding::new("cmd-k", OpenConnection, None),
        KeyBinding::new("cmd-enter", RunQuery, Some("Atlas")),
        KeyBinding::new("enter", ContinueSplash, Some("Splash")),
    ]
}

/// The three primary surfaces.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum View {
    Query,
    Schema,
    Graph,
}

/// Connection state shown in the header and status strip.
#[derive(Clone, Debug, PartialEq)]
pub enum Status {
    Disconnected,
    Checking,
    Connected { ms: f64, version: String },
    Failed(String),
}

impl Status {
    fn is_connected(&self) -> bool {
        matches!(self, Self::Connected { .. })
    }
}

/// The output tabs under the editor.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum OutputTab {
    Results,
    Wire,
}

/// A failed run.
#[derive(Clone, Debug, PartialEq)]
pub struct RunError {
    pub message: String,
    pub detail: Option<String>,
}

/// The editor's current run.
#[derive(Clone, Debug, Default)]
pub struct RunState {
    pub running: bool,
    pub result: Option<Execution>,
    pub error: Option<RunError>,
}

/// One typed parameter row.
pub struct ParamRow {
    pub id: usize,
    pub name: Entity<TextField>,
    pub ty: Entity<Select>,
    pub value: Entity<TextField>,
}

/// Parameter types offered in the editor.
pub const PARAM_TYPES: &[&str] = &["String", "Integer", "Float", "Boolean", "Null", "JSON"];

/// Schema categories.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SchemaCategory {
    Nodes,
    Edges,
    Vectors,
    Queries,
}

const HISTORY_LIMIT: usize = 12;

/// The application root view.
pub struct Atlas {
    focus_handle: FocusHandle,
    splash: Option<Entity<Splash>>,
    pending_editor_focus: bool,

    view: View,
    status: Status,
    connection: Option<ConnectionSettings>,
    client: Option<Client>,
    generation: u64,
    dialog: Option<Entity<ConnectionDialog>>,

    editor: Entity<TextField>,
    params: Vec<ParamRow>,
    next_param_id: usize,
    params_open: bool,
    output_tab: OutputTab,
    run: RunState,
    run_id: u64,
    history: Vec<String>,
    table_sort: Option<(usize, bool)>,

    schema: Option<Schema>,
    schema_error: Option<String>,
    schema_loading: bool,
    schema_id: u64,
    schema_category: SchemaCategory,
    schema_search: Entity<TextField>,
    schema_expanded: HashSet<String>,

    detail: Option<EntityDetail>,
    detail_loading: bool,
    detail_error: Option<String>,
    detail_id: u64,
    selected: Option<(ElementKind, u64)>,

    graph: Option<Arc<GraphData>>,
    graph_loading: bool,
    graph_error: Option<String>,
    graph_source: (String, Map<String, Value>),
    graph_attempted: bool,
    graph_id: u64,
    graph_view: Entity<GraphView>,
    graph_ms: Option<f64>,

    started: Instant,
    /// Launch actions applied once the first connection succeeds.
    launch_inspect: Option<(ElementKind, u64)>,
    launch_run: Option<String>,
}

impl Focusable for Atlas {
    fn focus_handle(&self, _: &App) -> FocusHandle {
        self.focus_handle.clone()
    }
}

/// Runs a script. A read-only script with several statements runs statement
/// by statement so every statement's elements reach the graph (the server
/// returns only the last statement's rows); anything that writes is sent as
/// one request, which the server executes as one atomic transaction.
fn run_script(
    client: &Client,
    source: &str,
    params: &Map<String, Value>,
) -> Result<Vec<Executed>, ClientError> {
    let analysis = Analysis::of(source);
    if !analysis.has_write() && analysis.statements.len() > 1 {
        analysis
            .statements
            .iter()
            .map(|s| client.query(&s.source, params))
            .collect()
    } else {
        client.query(source, params).map(|e| vec![e])
    }
}

/// `line` of a `syntax error at L:C` message.
fn error_line(message: &str) -> Option<usize> {
    let rest = message.split("at ").nth(1)?;
    let line: String = rest.chars().take_while(char::is_ascii_digit).collect();
    line.parse().ok()
}

impl Atlas {
    /// Builds the app, restoring saved settings and reconnecting in the
    /// background when a connection was saved.
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let saved = settings::load();
        let editor =
            cx.new(|cx| TextField::new(TextFieldOptions::editor(), rql::GRAPH_SNAPSHOT, cx));
        cx.subscribe_in(&editor, window, |this, _, event, window, cx| match event {
            TextFieldEvent::Changed => {
                this.editor.update(cx, |e, _| e.error_line = None);
                cx.notify();
            }
            TextFieldEvent::Submit => this.run_query(window, cx),
            TextFieldEvent::Cancel => {}
        })
        .detach();
        let schema_search =
            cx.new(|cx| TextField::new(TextFieldOptions::input("Search node types…"), "", cx));
        cx.subscribe(&schema_search, |_, _, _: &TextFieldEvent, cx| cx.notify())
            .detach();
        let graph_view = cx.new(GraphView::new);
        cx.subscribe(&graph_view, |this, _, event: &GraphEvent, cx| match event {
            GraphEvent::Select(kind, id) => this.inspect(*kind, *id, cx),
            GraphEvent::Expand(id) => {
                let id = *id;
                this.editor
                    .update(cx, |e, cx| e.set_text(rql::neighbourhood_query(id), cx));
                this.view = View::Query;
                this.pending_editor_focus = true;
                cx.notify();
            }
            GraphEvent::Clear => {
                this.selected = None;
                this.graph_view.update(cx, |g, cx| g.set_selected(None, cx));
                cx.notify();
            }
        })
        .detach();

        let splash = cx.new(|cx| Splash::new(window, cx));
        cx.subscribe(&splash, |this, _, _: &splash::SplashDone, cx| {
            this.splash = None;
            this.pending_editor_focus = true;
            cx.notify();
        })
        .detach();

        // Launch options, useful for demos and screenshots:
        // RATIK_ATLAS_SKIP_SPLASH=1, RATIK_ATLAS_VIEW=query|schema|graph,
        // RATIK_ATLAS_URL=http://host:port (overrides the saved connection).
        let skip_splash = std::env::var_os("RATIK_ATLAS_SKIP_SPLASH").is_some();
        let start_view = match std::env::var("RATIK_ATLAS_VIEW").as_deref() {
            Ok("schema") => View::Schema,
            Ok("graph") => View::Graph,
            _ => View::Query,
        };
        let connection = std::env::var("RATIK_ATLAS_URL")
            .ok()
            .map(|url| ConnectionSettings {
                url,
                ..ConnectionSettings::default()
            })
            .or(saved.connection);
        // RATIK_ATLAS_INSPECT=node:ID|edge:ID and RATIK_ATLAS_RUN=<RQL> act
        // once after connecting (RUN skips the destructive-statement prompt).
        let launch_inspect = std::env::var("RATIK_ATLAS_INSPECT").ok().and_then(|v| {
            let (kind, id) = v.split_once(':')?;
            let kind = if kind == "edge" {
                ElementKind::Edge
            } else {
                ElementKind::Node
            };
            Some((kind, id.parse().ok()?))
        });
        let launch_run = std::env::var("RATIK_ATLAS_RUN").ok();
        let mut atlas = Self {
            focus_handle: cx.focus_handle(),
            splash: (!skip_splash).then_some(splash),
            pending_editor_focus: skip_splash,
            view: start_view,
            status: Status::Disconnected,
            connection: connection.clone(),
            client: None,
            generation: 0,
            dialog: None,
            editor,
            params: Vec::new(),
            next_param_id: 0,
            params_open: false,
            output_tab: OutputTab::Results,
            run: RunState::default(),
            run_id: 0,
            history: Vec::new(),
            table_sort: None,
            schema: None,
            schema_error: None,
            schema_loading: false,
            schema_id: 0,
            schema_category: SchemaCategory::Nodes,
            schema_search,
            schema_expanded: HashSet::new(),
            detail: None,
            detail_loading: false,
            detail_error: None,
            detail_id: 0,
            selected: None,
            graph: None,
            graph_loading: false,
            graph_error: None,
            graph_source: (rql::GRAPH_SNAPSHOT.to_owned(), Map::new()),
            graph_attempted: false,
            graph_id: 0,
            graph_view,
            graph_ms: None,
            started: Instant::now(),
            launch_inspect,
            launch_run,
        };
        if let Some(connection) = connection {
            atlas.reconnect(connection, cx);
        }
        atlas
    }

    // ---- connection ---------------------------------------------------------

    fn reset_session(&mut self, cx: &mut Context<Self>) {
        self.generation += 1;
        self.run_id += 1;
        self.detail_id += 1;
        self.schema_id += 1;
        self.graph_id += 1;
        self.run = RunState::default();
        self.schema = None;
        self.schema_loading = false;
        self.schema_error = None;
        self.detail = None;
        self.detail_loading = false;
        self.detail_error = None;
        self.selected = None;
        self.graph_loading = false;
        self.graph_error = None;
        self.graph_attempted = false;
        self.graph_source = (rql::GRAPH_SNAPSHOT.to_owned(), Map::new());
        self.set_graph(None, cx);
    }

    /// Verifies a candidate before saving it, so a bad URL never becomes the
    /// saved one. Reports the outcome to the dialog, if one is open.
    pub fn connect(&mut self, settings: ConnectionSettings, cx: &mut Context<Self>) {
        self.reset_session(cx);
        self.status = Status::Checking;
        let generation = self.generation;
        let candidate = Client::new(settings.clone());
        cx.notify();
        cx.spawn(async move |this, cx| {
            let probe = candidate.clone();
            let health = cx
                .background_executor()
                .spawn(async move { probe.health() })
                .await;
            this.update(cx, |this, cx| {
                if generation != this.generation {
                    return;
                }
                match health {
                    Ok(health) => {
                        this.status = Status::Connected {
                            ms: health.duration_ms,
                            version: health.version,
                        };
                        this.connection = Some(settings.clone());
                        this.client = Some(candidate);
                        this.persist(cx);
                        if let Some(dialog) = this.dialog.take() {
                            dialog.update(cx, |d, cx| d.finish(cx));
                        }
                        this.refresh_schema(cx);
                        this.maybe_load_graph(cx);
                        if let Some(source) = this.launch_run.take() {
                            this.editor
                                .update(cx, |e, cx| e.set_text(source.clone(), cx));
                            let params = this.param_values(cx).unwrap_or_default();
                            this.execute(source, params, cx);
                        }
                        if let Some((kind, id)) = this.launch_inspect.take() {
                            this.inspect(kind, id, cx);
                        }
                    }
                    Err(error) => {
                        let message = error.to_string();
                        this.status = Status::Failed(message.clone());
                        if let Some(dialog) = &this.dialog {
                            dialog.update(cx, |d, cx| d.report(Err(message), cx));
                        }
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Silent reconnect at launch with saved settings.
    fn reconnect(&mut self, settings: ConnectionSettings, cx: &mut Context<Self>) {
        self.connect(settings, cx);
    }

    /// Ends the session; the saved settings stay for the next connect.
    pub fn disconnect(&mut self, cx: &mut Context<Self>) {
        self.reset_session(cx);
        self.client = None;
        self.status = Status::Disconnected;
        self.view = View::Query;
        self.dialog = None;
        cx.notify();
    }

    fn persist(&self, cx: &App) {
        let settings = Settings {
            connection: self.connection.clone(),
            theme: Some(theme::theme(cx).mode.name().to_owned()),
        };
        if let Err(error) = settings::save(&settings) {
            eprintln!("ratik-atlas: could not save settings: {error}");
        }
    }

    /// Opens the connection dialog.
    pub fn open_connection(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.dialog.is_some() {
            return;
        }
        let connection = self.connection.clone();
        let connected = self.status.is_connected();
        let dialog = cx.new(|cx| ConnectionDialog::new(connection, connected, window, cx));
        cx.subscribe(
            &dialog,
            |this, dialog, event: &dialog::DialogEvent, cx| match event {
                dialog::DialogEvent::Close => {
                    this.dialog = None;
                    cx.notify();
                }
                dialog::DialogEvent::Connect(settings) => this.connect(settings.clone(), cx),
                dialog::DialogEvent::Test(settings) => {
                    let client = Client::new(settings.clone());
                    let dialog = dialog.downgrade();
                    cx.spawn(async move |_, cx| {
                        let health = cx
                            .background_executor()
                            .spawn(async move { client.health() })
                            .await;
                        dialog
                            .update(cx, |d, cx| {
                                d.report(
                                    health
                                        .map(|h| {
                                            format!(
                                                "Reached RatikDB v{} in {:.0} ms.",
                                                h.version, h.duration_ms
                                            )
                                        })
                                        .map_err(|e| e.to_string()),
                                    cx,
                                );
                            })
                            .ok();
                    })
                    .detach();
                }
                dialog::DialogEvent::Disconnect => this.disconnect(cx),
            },
        )
        .detach();
        self.dialog = Some(dialog);
        cx.notify();
    }

    // ---- navigation ---------------------------------------------------------

    /// Switches the primary surface.
    pub fn set_view(&mut self, view: View, cx: &mut Context<Self>) {
        self.view = view;
        if view == View::Query {
            self.pending_editor_focus = true;
        }
        self.maybe_load_graph(cx);
        cx.notify();
    }

    /// Loads text into the editor and shows it.
    pub fn use_query(&mut self, query: impl Into<String>, cx: &mut Context<Self>) {
        let query = query.into();
        self.editor.update(cx, |e, cx| e.set_text(query, cx));
        self.set_view(View::Query, cx);
    }

    fn toggle_theme(&mut self, cx: &mut Context<Self>) {
        let mode = theme::theme(cx).mode.toggled();
        theme::set_mode(cx, mode);
        self.persist(cx);
        cx.notify();
    }

    // ---- parameters ---------------------------------------------------------

    fn add_param(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let id = self.next_param_id;
        self.next_param_id += 1;
        let name = cx.new(|cx| TextField::new(TextFieldOptions::input("name").mono(true), "", cx));
        let value =
            cx.new(|cx| TextField::new(TextFieldOptions::input("value").mono(true), "", cx));
        let ty = cx.new(|cx| {
            Select::new(
                PARAM_TYPES.iter().map(|s| SharedString::from(*s)).collect(),
                0,
                cx,
            )
        });
        for field in [&name, &value] {
            cx.subscribe(field, |_, _, _: &TextFieldEvent, cx| cx.notify())
                .detach();
        }
        cx.subscribe(&ty, |_, _, _: &crate::ui::SelectEvent, cx| cx.notify())
            .detach();
        name.update(cx, |f, _| f.focus(window));
        self.params.push(ParamRow {
            id,
            name,
            ty,
            value,
        });
        self.params_open = true;
        cx.notify();
    }

    fn remove_param(&mut self, id: usize, cx: &mut Context<Self>) {
        self.params.retain(|p| p.id != id);
        cx.notify();
    }

    /// Converts the parameter rows to JSON values. RQL owns the typing; this
    /// only parses what the form says.
    fn param_values(&self, cx: &App) -> Result<Map<String, Value>, String> {
        let mut values = Map::new();
        for row in &self.params {
            let name = row
                .name
                .read(cx)
                .text()
                .trim()
                .trim_start_matches('$')
                .to_owned();
            if name.is_empty() && row.value.read(cx).is_empty() {
                continue;
            }
            let valid = name
                .chars()
                .next()
                .is_some_and(|c| c.is_ascii_alphabetic() || c == '_')
                && name.chars().all(|c| c.is_ascii_alphanumeric() || c == '_');
            if !valid {
                return Err("Parameter names use ASCII letters, digits and underscores, and cannot start with a digit.".to_owned());
            }
            if values.contains_key(&name) {
                return Err(format!("Duplicate parameter ${name}"));
            }
            let raw = row.value.read(cx).text().to_owned();
            let ty = PARAM_TYPES[row.ty.read(cx).selected];
            let value = match ty {
                "Integer" => Value::from(
                    raw.trim()
                        .parse::<i64>()
                        .map_err(|_| format!("${name} requires a 64-bit integer."))?,
                ),
                "Float" => {
                    let f: f64 = raw
                        .trim()
                        .parse()
                        .map_err(|_| format!("${name} requires a finite float."))?;
                    serde_json::Number::from_f64(f)
                        .map(Value::Number)
                        .ok_or_else(|| format!("${name} requires a finite float."))?
                }
                "Boolean" => match raw.trim().to_ascii_lowercase().as_str() {
                    "true" | "1" | "yes" => Value::Bool(true),
                    "false" | "0" | "no" => Value::Bool(false),
                    _ => return Err(format!("${name} requires true or false.")),
                },
                "Null" => Value::Null,
                "JSON" => serde_json::from_str(&raw)
                    .map_err(|e| format!("${name} is not valid JSON: {e}"))?,
                _ => Value::String(raw),
            };
            values.insert(name, value);
        }
        Ok(values)
    }

    // ---- running ------------------------------------------------------------

    /// Runs the editor's script. Destructive statements ask first.
    pub fn run_query(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.run.running {
            return;
        }
        let source = self.editor.read(cx).text().to_owned();
        let analysis = Analysis::of(&source);
        if analysis.is_empty() {
            self.run.error = Some(RunError {
                message: "Type a query first.".to_owned(),
                detail: None,
            });
            cx.notify();
            return;
        }
        if self.client.is_none() || !self.status.is_connected() {
            self.run = RunState {
                running: false,
                result: None,
                error: Some(RunError {
                    message: "Connect to a RatikDB server before running a query.".to_owned(),
                    detail: None,
                }),
            };
            self.output_tab = OutputTab::Results;
            cx.notify();
            return;
        }
        let params = match self.param_values(cx) {
            Ok(params) => params,
            Err(message) => {
                self.run.error = Some(RunError {
                    message,
                    detail: None,
                });
                cx.notify();
                return;
            }
        };
        self.run_confirmed(source, params, &analysis, window, cx);
    }

    /// Asks before statements that remove data or schema, then executes.
    fn run_confirmed(
        &mut self,
        source: String,
        params: Map<String, Value>,
        analysis: &Analysis,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let destructive: Vec<&str> = analysis
            .statements
            .iter()
            .filter(|s| matches!(s.keyword.as_str(), "DELETE" | "DROP"))
            .map(|s| s.keyword.as_str())
            .collect();
        if destructive.is_empty() {
            self.execute(source, params, cx);
            return;
        }
        let what = if destructive.contains(&"DROP") {
            "This script drops schema objects. Dropping a type with CASCADE also deletes its data."
        } else {
            "This script deletes data. Deleting a node also removes its edges."
        };
        let answer = window.prompt(
            PromptLevel::Warning,
            "Run this mutation?",
            Some(&format!("{what} The whole script runs as one transaction.")),
            &["Run mutation", "Cancel"],
            cx,
        );
        cx.spawn(async move |this, cx| {
            if answer.await == Ok(0) {
                this.update(cx, |this, cx| this.execute(source, params, cx))
                    .ok();
            }
        })
        .detach();
    }

    /// Sends a script to the server.
    fn execute(&mut self, source: String, params: Map<String, Value>, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        // A manual query supersedes any automatic graph snapshot in flight.
        self.graph_id += 1;
        self.graph_loading = false;
        self.run_id += 1;
        let run_id = self.run_id;
        let generation = self.generation;
        self.run = RunState {
            running: true,
            result: None,
            error: None,
        };
        self.output_tab = OutputTab::Results;
        cx.notify();
        let endpoint = client.settings().query_endpoint();
        cx.spawn(async move |this, cx| {
            let (task_source, task_params) = (source.clone(), params.clone());
            let outcome = cx
                .background_executor()
                .spawn(async move { run_script(&client, &task_source, &task_params) })
                .await;
            this.update(cx, |this, cx| {
                this.finish_run(run_id, generation, &source, &params, &endpoint, outcome, cx)
            })
            .ok();
        })
        .detach();
    }

    #[allow(clippy::too_many_arguments)]
    fn finish_run(
        &mut self,
        run_id: u64,
        generation: u64,
        source: &str,
        params: &Map<String, Value>,
        endpoint: &str,
        outcome: Result<Vec<Executed>, ClientError>,
        cx: &mut Context<Self>,
    ) {
        if run_id != self.run_id || generation != self.generation {
            return;
        }
        match outcome {
            Ok(executed) => {
                let analysis = Analysis::of(source);
                let execution = build_execution(source, params, endpoint, &analysis, &executed);
                if let Status::Connected { ms, .. } = &mut self.status {
                    *ms = execution.duration_ms;
                }
                let changed = execution.is_write || !execution.stats.is_empty();
                if execution.pure_graph
                    && let Some(graph) = &execution.graph
                {
                    self.graph_source = (source.to_owned(), params.clone());
                    self.graph_attempted = true;
                    self.graph_error = None;
                    self.graph_ms = Some(execution.duration_ms);
                    self.set_graph(Some(Arc::new(graph.clone())), cx);
                    self.view = View::Graph;
                }
                self.history.retain(|h| h != source);
                self.history.insert(0, source.to_owned());
                self.history.truncate(HISTORY_LIMIT);
                self.table_sort = None;
                self.run = RunState {
                    running: false,
                    result: Some(execution),
                    error: None,
                };
                if changed {
                    self.after_mutation(cx);
                }
            }
            Err(error) => {
                if error.is_connection_failure() {
                    self.status = Status::Failed(error.to_string());
                }
                let message = error.to_string();
                let line = error_line(&message);
                self.editor.update(cx, |e, _| e.error_line = line);
                self.run = RunState {
                    running: false,
                    result: None,
                    error: Some(RunError {
                        detail: Some(error.headline()),
                        message,
                    }),
                };
            }
        }
        cx.notify();
    }

    /// After a commit, refresh schema, the drawn graph and the inspector. A
    /// refresh failure leaves the receipt and the previous graph visible.
    fn after_mutation(&mut self, cx: &mut Context<Self>) {
        self.refresh_schema(cx);
        if self.graph.is_some() || self.graph_attempted {
            self.load_graph(cx);
        }
        if let Some((kind, id)) = self.selected {
            self.inspect(kind, id, cx);
        }
    }

    /// Deletes a node or edge from the inspector, after confirmation.
    pub fn delete_entity(
        &mut self,
        kind: ElementKind,
        id: u64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let source = match kind {
            ElementKind::Node => format!("DELETE n FROM (n) WHERE id(n) = {id};"),
            ElementKind::Edge => format!("DELETE e FROM (a)-[e]->(b) WHERE id(e) = {id};"),
        };
        let analysis = Analysis::of(&source);
        self.selected = None;
        self.detail = None;
        self.graph_view.update(cx, |g, cx| g.set_selected(None, cx));
        self.editor
            .update(cx, |e, cx| e.set_text(source.clone(), cx));
        self.run_confirmed(source, Map::new(), &analysis, window, cx);
    }

    // ---- schema -------------------------------------------------------------

    /// Re-reads the catalog.
    pub fn refresh_schema(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.schema_id += 1;
        let id = self.schema_id;
        self.schema_loading = true;
        self.schema_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    client
                        .schema()
                        .map_err(|e| e.to_string())
                        .and_then(Schema::from_json)
                })
                .await;
            this.update(cx, |this, cx| {
                if id != this.schema_id {
                    return;
                }
                this.schema_loading = false;
                match result {
                    Ok(schema) => this.schema = Some(schema),
                    Err(error) => this.schema_error = Some(error),
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    // ---- graph --------------------------------------------------------------

    fn set_graph(&mut self, graph: Option<Arc<GraphData>>, cx: &mut Context<Self>) {
        self.graph = graph.clone();
        let selected = self.selected;
        self.graph_view.update(cx, |g, cx| {
            g.set_graph(graph, cx);
            g.set_selected(selected, cx);
        });
    }

    /// Entering Graph while connected loads the current snapshot once.
    fn maybe_load_graph(&mut self, cx: &mut Context<Self>) {
        if self.view == View::Graph
            && self.status.is_connected()
            && self.graph.is_none()
            && !self.graph_loading
            && !self.graph_attempted
        {
            self.load_graph(cx);
        }
    }

    /// Re-runs the graph source query without touching the editor.
    pub fn load_graph(&mut self, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.graph_id += 1;
        let id = self.graph_id;
        self.graph_attempted = true;
        self.graph_loading = true;
        self.graph_error = None;
        cx.notify();
        let (source, params) = self.graph_source.clone();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let executed = run_script(&client, &source, &params)?;
                    let mut builder = GraphBuilder::default();
                    let mut ms = 0.;
                    for e in &executed {
                        builder.add_response(&e.response);
                        ms += e.duration_ms;
                    }
                    Ok::<_, ClientError>((builder.build(), ms))
                })
                .await;
            this.update(cx, |this, cx| {
                if id != this.graph_id {
                    return;
                }
                this.graph_loading = false;
                match result {
                    Ok((graph, ms)) => {
                        this.graph_ms = Some(ms);
                        if let Status::Connected { ms: status_ms, .. } = &mut this.status {
                            *status_ms = ms;
                        }
                        this.set_graph(Some(Arc::new(graph)), cx);
                    }
                    Err(error) => {
                        if error.is_connection_failure() {
                            this.status = Status::Failed(error.to_string());
                        }
                        this.graph_error = Some(error.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Draws the elements of the last result on the canvas.
    pub fn open_result_in_graph(&mut self, cx: &mut Context<Self>) {
        let Some(execution) = &self.run.result else {
            return;
        };
        if let Some(graph) = execution.graph.clone() {
            self.graph_source = (execution.source.clone(), execution.params.clone());
            self.graph_attempted = true;
            self.graph_error = None;
            self.set_graph(Some(Arc::new(graph)), cx);
            self.set_view(View::Graph, cx);
        }
    }

    // ---- inspector ----------------------------------------------------------

    /// Loads one node or edge with everything attached to it.
    pub fn inspect(&mut self, kind: ElementKind, id: u64, cx: &mut Context<Self>) {
        let Some(client) = self.client.clone() else {
            return;
        };
        self.detail_id += 1;
        let request = self.detail_id;
        let generation = self.generation;
        self.selected = Some((kind, id));
        self.graph_view
            .update(cx, |g, cx| g.set_selected(Some((kind, id)), cx));
        self.detail_loading = true;
        self.detail_error = None;
        cx.notify();
        cx.spawn(async move |this, cx| {
            let result = cx
                .background_executor()
                .spawn(async move {
                    let none = Map::new();
                    match kind {
                        ElementKind::Node => {
                            let node = client.query(&rql::node_query(id), &none)?;
                            let edges = client.query(&rql::incident_edges_query(id, 50), &none)?;
                            Ok::<_, ClientError>(node_detail(&node.response, &edges.response))
                        }
                        ElementKind::Edge => {
                            let edge = client.query(&rql::edge_query(id), &none)?;
                            Ok(edge_detail(&edge.response))
                        }
                    }
                })
                .await;
            this.update(cx, |this, cx| {
                if request != this.detail_id || generation != this.generation {
                    return;
                }
                this.detail_loading = false;
                match result {
                    Ok(Some(detail)) => this.detail = Some(detail),
                    Ok(None) => {
                        this.detail = None;
                        this.detail_error =
                            Some(format!("{} {id} no longer exists.", kind.title()));
                    }
                    Err(error) => {
                        this.detail = None;
                        this.detail_error = Some(error.to_string());
                    }
                }
                cx.notify();
            })
            .ok();
        })
        .detach();
    }

    /// Loads a neighbourhood query for a node into the editor.
    pub fn expand_node(&mut self, id: u64, cx: &mut Context<Self>) {
        self.use_query(rql::neighbourhood_query(id), cx);
    }

    fn uptime(&self) -> u64 {
        self.started.elapsed().as_secs()
    }
}

impl Render for Atlas {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(splash) = &self.splash {
            return div().size_full().child(splash.clone()).into_any_element();
        }
        if self.pending_editor_focus && self.view == View::Query && self.dialog.is_none() {
            self.pending_editor_focus = false;
            self.editor.update(cx, |e, _| e.focus(window));
        }
        self.render_shell(window, cx).into_any_element()
    }
}

/// Applies the saved theme before the first frame.
pub fn initial_mode() -> Mode {
    let env = std::env::var("RATIK_ATLAS_THEME").ok();
    match env.as_deref().or(settings::load().theme.as_deref()) {
        Some("light") => Mode::Light,
        _ => Mode::Dark,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn reads_the_line_of_a_syntax_error() {
        assert_eq!(error_line("syntax error at 3:14: expected ')'"), Some(3));
        assert_eq!(error_line("schema error: unknown type"), None);
    }
}
