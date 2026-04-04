//! Local live-reload server for rendered freql process graphs.

use std::collections::BTreeSet;
use std::convert::Infallible;
use std::future::Future;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::process::Command;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use axum::extract::State;
use axum::http::{HeaderMap, HeaderValue, StatusCode, header};
use axum::response::Html;
use axum::response::sse::{Event, KeepAlive, Sse};
use axum::routing::{get, post};
use axum::{Json, Router};
use indexmap::IndexMap;
use notify::{RecommendedWatcher, RecursiveMode, Watcher};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::sync::{mpsc, oneshot, watch};
use tokio_stream::StreamExt;
use tokio_stream::wrappers::WatchStream;

use crate::render::{RenderOptions, graph_document};
use crate::{Compiler, CompilerOptions, Completeness, ConfigOverride};

const RELOAD_DEBOUNCE: Duration = Duration::from_millis(125);

/// Configuration for the local graph server.
#[derive(Clone, Debug)]
pub struct ServeOptions {
    /// Interface or hostname to bind. The default is loopback-only.
    pub host: String,
    /// TCP port. Zero asks the operating system for an available port.
    pub port: u16,
    /// Open the resulting URL in the platform's default browser.
    pub open: bool,
    /// Options used to select and render the process graph.
    pub render: RenderOptions,
}

impl Default for ServeOptions {
    fn default() -> Self {
        Self {
            host: "127.0.0.1".to_owned(),
            port: 0,
            open: true,
            render: RenderOptions::default(),
        }
    }
}

#[derive(Clone)]
struct AppState {
    updates: watch::Sender<Arc<String>>,
    shutdown: watch::Sender<bool>,
    override_tx: mpsc::UnboundedSender<OverrideCommand>,
}

#[derive(Debug, Deserialize)]
struct OverrideRequest {
    #[serde(default)]
    path: Option<String>,
    /// `null` removes the override at `path`.
    #[serde(default)]
    value: Option<String>,
    #[serde(default)]
    reset_all: bool,
}

struct OverrideCommand {
    request: OverrideRequest,
    response: oneshot::Sender<Arc<String>>,
}

struct CompilationUpdate {
    payload: Arc<String>,
    source_paths: BTreeSet<PathBuf>,
    /// The most recent successfully rendered graph, excluding transient
    /// server-error metadata.
    graph: Option<Value>,
}

struct WatcherContext {
    entry: PathBuf,
    compiler_options: CompilerOptions,
    render_options: RenderOptions,
    updates: watch::Sender<Arc<String>>,
    last_graph: Option<Value>,
    active_overrides: IndexMap<String, String>,
    override_rx: mpsc::UnboundedReceiver<OverrideCommand>,
}

/// Compile `entry` and serve its process graph until the process is interrupted.
///
/// Compilation errors after startup are sent to connected browsers. They do not
/// terminate the server, which lets an editor save a temporarily invalid file.
pub fn serve(
    entry: &Path,
    compiler_options: CompilerOptions,
    options: ServeOptions,
) -> Result<(), String> {
    let entry = absolute_path(entry)?;
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .enable_all()
        .build()
        .map_err(|error| format!("cannot start async runtime: {error}"))?;
    runtime.block_on(serve_async(entry, compiler_options, options))
}

async fn serve_async(
    entry: PathBuf,
    mut compiler_options: CompilerOptions,
    options: ServeOptions,
) -> Result<(), String> {
    let mut active_overrides = IndexMap::new();
    for value in &compiler_options.overrides {
        active_overrides.insert(value.path.clone(), value.value.clone());
    }
    compiler_options.overrides = override_values(&active_overrides);
    let initial = compile_update(&entry, &compiler_options, &options.render, None);
    let (updates, _) = watch::channel(initial.payload);
    let (shutdown, _) = watch::channel(false);
    let (override_tx, override_rx) = mpsc::unbounded_channel();
    let state = AppState {
        updates: updates.clone(),
        shutdown: shutdown.clone(),
        override_tx,
    };

    let app = Router::new()
        .route("/", get(index))
        .route("/events", get(events))
        .route("/api/graph", get(current_graph))
        .route("/api/overrides", post(update_overrides))
        .with_state(state);

    let listener = tokio::net::TcpListener::bind((options.host.as_str(), options.port))
        .await
        .map_err(|error| {
            format!(
                "cannot bind graph server to {}:{}: {error}",
                options.host, options.port
            )
        })?;
    let address = listener
        .local_addr()
        .map_err(|error| format!("cannot determine graph server address: {error}"))?;
    let url = server_url(address);

    start_watcher(
        initial.source_paths,
        WatcherContext {
            entry: entry.clone(),
            compiler_options,
            render_options: options.render.clone(),
            updates,
            last_graph: initial.graph,
            active_overrides,
            override_rx,
        },
    )?;

    println!("freql serve: {url}");
    if options.open
        && let Err(error) = open_browser(&url)
    {
        eprintln!("freql serve: could not open browser: {error}");
    }

    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal(shutdown))
        .await
        .map_err(|error| format!("graph server failed: {error}"))
}

async fn update_overrides(
    State(state): State<AppState>,
    Json(request): Json<OverrideRequest>,
) -> (StatusCode, HeaderMap, String) {
    if !request.reset_all && request.path.as_deref().is_none_or(str::is_empty) {
        return json_response(
            StatusCode::BAD_REQUEST,
            json!({ "server_error": "override request requires a process-qualified path" })
                .to_string(),
        );
    }
    let (response, receiver) = oneshot::channel();
    if state
        .override_tx
        .send(OverrideCommand { request, response })
        .is_err()
    {
        return json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({ "server_error": "live compiler is no longer running" }).to_string(),
        );
    }
    match receiver.await {
        Ok(payload) => json_response(StatusCode::OK, (*payload).clone()),
        Err(_) => json_response(
            StatusCode::SERVICE_UNAVAILABLE,
            json!({ "server_error": "live compiler stopped before applying the override" })
                .to_string(),
        ),
    }
}

fn json_response(status: StatusCode, body: String) -> (StatusCode, HeaderMap, String) {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (status, headers, body)
}

async fn index(State(state): State<AppState>) -> (HeaderMap, Html<String>) {
    let mut headers = HeaderMap::new();
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    // Embed the current canonical payload rather than the startup payload. A
    // new tab therefore renders the last good graph immediately even if the
    // source is currently unreadable; SSE then keeps it current.
    let payload = state.updates.borrow().clone();
    (
        headers,
        Html(crate::render::html::live_html_with_initial(&payload)),
    )
}

async fn current_graph(State(state): State<AppState>) -> (HeaderMap, String) {
    let mut headers = HeaderMap::new();
    headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_static("application/json; charset=utf-8"),
    );
    headers.insert(header::CACHE_CONTROL, HeaderValue::from_static("no-store"));
    (headers, state.updates.borrow().to_string())
}

async fn events(
    State(state): State<AppState>,
) -> Sse<impl tokio_stream::Stream<Item = Result<Event, Infallible>>> {
    // WatchStream emits the current value first, so every EventSource receives
    // a complete initial graph even when it connects after several rebuilds.
    let updates = WatchStream::new(state.updates.subscribe())
        .map(|payload| Ok(Event::default().data((*payload).clone())));
    // Axum's graceful shutdown waits for response bodies. Explicitly end this
    // otherwise-infinite stream so Ctrl-C cannot be held open by a browser's
    // EventSource connection.
    let stream = ShutdownStream::new(updates, state.shutdown.subscribe());
    Sse::new(stream).keep_alive(
        KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keep-alive"),
    )
}

fn start_watcher(source_paths: BTreeSet<PathBuf>, context: WatcherContext) -> Result<(), String> {
    let WatcherContext {
        entry,
        compiler_options,
        render_options,
        updates,
        mut last_graph,
        mut active_overrides,
        mut override_rx,
    } = context;
    let (change_tx, mut change_rx) = mpsc::unbounded_channel();
    let mut watcher =
        notify::recommended_watcher(move |result: notify::Result<notify::Event>| match result {
            Ok(event) if event.paths.iter().any(|path| is_req_path(path)) => {
                let _ = change_tx.send(());
            }
            Ok(_) => {}
            Err(error) => eprintln!("freql serve: watch error: {error}"),
        })
        .map_err(|error| format!("cannot create file watcher: {error}"))?;

    // Recursively watching the entry directory also catches newly-created
    // imports which did not exist during a failed compilation. Successfully
    // loaded imports outside that directory are watched explicitly below.
    let root = entry.parent().unwrap_or_else(|| Path::new("."));
    watcher
        .watch(root, RecursiveMode::Recursive)
        .map_err(|error| format!("cannot watch {}: {error}", root.display()))?;

    let mut watched = BTreeSet::new();
    watched.insert(root.to_path_buf());
    add_source_watches(&mut watcher, &mut watched, source_paths);

    // The task owns the native watcher, keeping its callback alive while also
    // allowing the watched import set to grow after successful recompiles.
    tokio::spawn(async move {
        let mut watcher = watcher;
        loop {
            let response = tokio::select! {
                changed = change_rx.recv() => {
                    if changed.is_none() { break; }
                    tokio::time::sleep(RELOAD_DEBOUNCE).await;
                    while change_rx.try_recv().is_ok() {}
                    None
                }
                command = override_rx.recv() => {
                    let Some(command) = command else { break; };
                    if command.request.reset_all {
                        active_overrides.clear();
                    } else if let Some(path) = command.request.path {
                        match command.request.value {
                            Some(value) => { active_overrides.insert(path, value); }
                            None => { active_overrides.shift_remove(&path); }
                        }
                    }
                    Some(command.response)
                }
            };

            let mut effective_options = compiler_options.clone();
            effective_options.overrides = override_values(&active_overrides);
            let update = compile_update(
                &entry,
                &effective_options,
                &render_options,
                last_graph.as_ref(),
            );
            add_source_watches(&mut watcher, &mut watched, update.source_paths);
            last_graph = update.graph;
            let payload = update.payload;
            updates.send_replace(payload.clone());
            if let Some(response) = response {
                let _ = response.send(payload);
            }
        }
    });

    Ok(())
}

fn override_values(values: &IndexMap<String, String>) -> Vec<ConfigOverride> {
    values
        .iter()
        .map(|(path, value)| ConfigOverride {
            path: path.clone(),
            value: value.clone(),
        })
        .collect()
}

fn add_source_watches(
    watcher: &mut RecommendedWatcher,
    watched: &mut BTreeSet<PathBuf>,
    source_paths: BTreeSet<PathBuf>,
) {
    for path in source_paths {
        // Watch the containing directory rather than the inode. Editors often
        // save by atomically replacing a file, which can invalidate a direct
        // file watch after the first save.
        let directory = path.parent().unwrap_or(&path).to_path_buf();
        if watched.insert(directory.clone())
            && let Err(error) = watcher.watch(&directory, RecursiveMode::NonRecursive)
        {
            eprintln!("freql serve: cannot watch {}: {error}", directory.display());
        }
    }
}

fn compile_update(
    entry: &Path,
    compiler_options: &CompilerOptions,
    render_options: &RenderOptions,
    last_graph: Option<&Value>,
) -> CompilationUpdate {
    let compiler = Compiler::new(compiler_options.clone());
    match compiler.check_path(entry) {
        Ok(compilation) => {
            let source_paths = local_source_paths(entry, compilation.source_text.keys());
            let invalid = compilation.report.status == Completeness::Invalid;
            match graph_document(&compilation, render_options) {
                Ok(graph) => match serde_json::to_value(&graph) {
                    Ok(graph) => {
                        let (payload, graph) = accept_graph(graph, invalid, last_graph);
                        CompilationUpdate {
                            payload: Arc::new(payload.to_string()),
                            source_paths,
                            graph,
                        }
                    }
                    Err(error) => error_update(
                        format!("cannot serialize rendered graph: {error}"),
                        source_paths,
                        last_graph,
                    ),
                },
                Err(error) => error_update(
                    format!("cannot render graph: {error}"),
                    source_paths,
                    last_graph,
                ),
            }
        }
        Err(error) => error_update(
            format!("cannot compile {}: {error}", entry.display()),
            BTreeSet::new(),
            last_graph,
        ),
    }
}

/// Produce the client payload and next last-known-good graph state.
///
/// Invalid compilations carry useful current diagnostics but their topology can
/// be an arbitrary parser-recovery fragment. Retain topology from the last
/// complete or partial compilation while refreshing report metadata.
fn accept_graph(
    current: Value,
    invalid: bool,
    last_graph: Option<&Value>,
) -> (Value, Option<Value>) {
    if !invalid {
        return (current.clone(), Some(current));
    }

    let Some(Value::Object(last_fields)) = last_graph else {
        // With no trustworthy predecessor, the invalid compilation's
        // best-effort graph is still more useful than an empty page.
        return (current, None);
    };
    let Value::Object(current_fields) = current else {
        return (Value::Object(last_fields.clone()), last_graph.cloned());
    };

    let mut payload = last_fields.clone();
    for field in [
        "schema_version",
        "entry",
        "status",
        "report_scope",
        "diagnostics",
        "obligations",
        "constraints",
        "controls",
        "overrides",
    ] {
        if let Some(value) = current_fields.get(field) {
            payload.insert(field.to_owned(), value.clone());
        }
    }
    payload.insert("stale_graph".to_owned(), Value::Bool(true));
    (Value::Object(payload), last_graph.cloned())
}

fn error_update(
    message: String,
    source_paths: BTreeSet<PathBuf>,
    last_graph: Option<&Value>,
) -> CompilationUpdate {
    CompilationUpdate {
        payload: Arc::new(error_payload(message, last_graph).to_string()),
        source_paths,
        graph: last_graph.cloned(),
    }
}

fn error_payload(message: String, last_graph: Option<&Value>) -> Value {
    let mut payload = last_graph.cloned().unwrap_or_else(|| json!({}));
    if let Value::Object(fields) = &mut payload {
        fields.insert("server_error".to_owned(), Value::String(message));
        payload
    } else {
        // GraphDocument is an object in schema v1. Keep this defensive fallback
        // useful should a future renderer temporarily violate that contract.
        json!({ "server_error": message })
    }
}

fn local_source_paths<'a>(
    entry: &Path,
    display_paths: impl Iterator<Item = &'a String>,
) -> BTreeSet<PathBuf> {
    let root = entry.parent().unwrap_or_else(|| Path::new("."));
    display_paths
        .filter(|path| !path.starts_with('<'))
        .map(PathBuf::from)
        .map(|path| {
            if path.is_absolute() {
                path
            } else {
                root.join(path)
            }
        })
        .filter(|path| is_req_path(path))
        .collect()
}

fn is_req_path(path: &Path) -> bool {
    path.extension()
        .is_some_and(|extension| extension.eq_ignore_ascii_case("req"))
}

fn absolute_path(path: &Path) -> Result<PathBuf, String> {
    if path.is_absolute() {
        Ok(path.to_path_buf())
    } else {
        std::env::current_dir()
            .map(|directory| directory.join(path))
            .map_err(|error| format!("cannot determine current directory: {error}"))
    }
}

fn server_url(address: SocketAddr) -> String {
    match address {
        SocketAddr::V4(address) => format!("http://{}:{}", address.ip(), address.port()),
        SocketAddr::V6(address) => format!("http://[{}]:{}", address.ip(), address.port()),
    }
}

fn open_browser(url: &str) -> Result<(), String> {
    #[cfg(target_os = "macos")]
    let mut command = {
        let mut command = Command::new("open");
        command.arg(url);
        command
    };
    #[cfg(target_os = "windows")]
    let mut command = {
        let mut command = Command::new("cmd");
        command.args(["/C", "start", "", url]);
        command
    };
    #[cfg(all(unix, not(target_os = "macos")))]
    let mut command = {
        let mut command = Command::new("xdg-open");
        command.arg(url);
        command
    };
    #[cfg(not(any(unix, target_os = "windows")))]
    return Err("opening a browser is not supported on this platform".to_owned());

    command
        .spawn()
        .map(|_| ())
        .map_err(|error| error.to_string())
}

async fn wait_for_shutdown(mut shutdown: watch::Receiver<bool>) {
    if *shutdown.borrow() {
        return;
    }
    while shutdown.changed().await.is_ok() {
        if *shutdown.borrow() {
            return;
        }
    }
}

struct ShutdownStream<S> {
    inner: Pin<Box<S>>,
    shutdown: Pin<Box<dyn Future<Output = ()> + Send>>,
}

impl<S> ShutdownStream<S> {
    fn new(inner: S, shutdown: watch::Receiver<bool>) -> Self {
        Self {
            inner: Box::pin(inner),
            shutdown: Box::pin(wait_for_shutdown(shutdown)),
        }
    }
}

impl<S: tokio_stream::Stream> tokio_stream::Stream for ShutdownStream<S> {
    type Item = S::Item;

    fn poll_next(mut self: Pin<&mut Self>, context: &mut Context<'_>) -> Poll<Option<Self::Item>> {
        if self.shutdown.as_mut().poll(context).is_ready() {
            Poll::Ready(None)
        } else {
            self.inner.as_mut().poll_next(context)
        }
    }
}

async fn shutdown_signal(shutdown: watch::Sender<bool>) {
    if let Err(error) = tokio::signal::ctrl_c().await {
        eprintln!("freql serve: cannot install Ctrl-C handler: {error}");
    }
    shutdown.send_replace(true);
}

#[cfg(test)]
mod tests {
    use std::net::{IpAddr, Ipv4Addr, Ipv6Addr};

    use super::*;

    #[test]
    fn serve_defaults_are_safe_for_local_development() {
        let options = ServeOptions::default();
        assert_eq!(options.host, "127.0.0.1");
        assert_eq!(options.port, 0);
        assert!(options.open);
        assert!(options.render.process.is_none());
    }

    #[test]
    fn req_path_filter_is_case_insensitive() {
        assert!(is_req_path(Path::new("factory.req")));
        assert!(is_req_path(Path::new("factory.REQ")));
        assert!(!is_req_path(Path::new("factory.req~")));
        assert!(!is_req_path(Path::new("README.md")));
    }

    #[test]
    fn server_urls_bracket_ipv6_addresses() {
        let ipv4 = SocketAddr::new(IpAddr::V4(Ipv4Addr::LOCALHOST), 3210);
        assert_eq!(server_url(ipv4), "http://127.0.0.1:3210");

        let ipv6 = SocketAddr::new(IpAddr::V6(Ipv6Addr::LOCALHOST), 3210);
        assert_eq!(server_url(ipv6), "http://[::1]:3210");
    }

    #[test]
    fn local_paths_ignore_embedded_standard_library_modules() {
        let entry = Path::new("/workspace/main.req");
        let displays = [
            "main.req".to_owned(),
            "parts/cut.req".to_owned(),
            "<std/process>".to_owned(),
        ];
        let paths = local_source_paths(entry, displays.iter());
        assert_eq!(
            paths,
            BTreeSet::from([
                PathBuf::from("/workspace/main.req"),
                PathBuf::from("/workspace/parts/cut.req"),
            ])
        );
    }

    #[test]
    fn reload_error_retains_last_successful_graph_fields() {
        let graph = json!({
            "schema_version": 1,
            "status": "complete",
            "nodes": [{ "id": "Factory.cut" }],
            "edges": []
        });
        let failed = error_payload("source disappeared".to_owned(), Some(&graph));

        assert_eq!(failed["nodes"], graph["nodes"]);
        assert_eq!(failed["edges"], graph["edges"]);
        assert_eq!(failed["server_error"], "source disappeared");
        assert!(graph.get("server_error").is_none());
    }

    #[test]
    fn initial_error_has_no_invented_graph() {
        let failed = error_payload("entry is unreadable".to_owned(), None);
        assert_eq!(failed, json!({ "server_error": "entry is unreadable" }));
    }

    #[test]
    fn invalid_compilation_retains_good_topology_and_refreshes_findings() {
        let good = json!({
            "schema_version": 1,
            "entry": "main.req",
            "status": "complete",
            "report_scope": "compilation",
            "selected_process": null,
            "roots": ["Factory"],
            "nodes": [{ "id": "Factory.cut" }],
            "edges": [{ "source": "Factory.cut", "target": "Factory.wrap" }],
            "diagnostics": [],
            "obligations": [],
            "constraints": [],
            "controls": [{ "path": "Factory.rate", "value": "10" }],
            "overrides": {},
            "monitored_metrics": [{ "path": "Factory.cost", "value": "1 USD" }]
        });
        let invalid = json!({
            "schema_version": 1,
            "entry": "main.req",
            "status": "invalid",
            "report_scope": "compilation",
            "selected_process": null,
            "roots": [],
            "nodes": [],
            "edges": [],
            "diagnostics": [{ "code": "P0001" }],
            "obligations": [{ "id": "open" }],
            "constraints": [{ "id": "residual" }],
            "controls": [{ "path": "Factory.rate", "value": "not yet valid" }],
            "overrides": { "Factory.rate": "not yet valid" },
            "monitored_metrics": []
        });

        let (_, last_good) = accept_graph(good.clone(), false, None);
        let (payload, retained) = accept_graph(invalid.clone(), true, last_good.as_ref());

        assert_eq!(payload["nodes"], good["nodes"]);
        assert_eq!(payload["edges"], good["edges"]);
        assert_eq!(payload["status"], "invalid");
        assert_eq!(payload["diagnostics"], invalid["diagnostics"]);
        assert_eq!(payload["obligations"], invalid["obligations"]);
        assert_eq!(payload["constraints"], invalid["constraints"]);
        assert_eq!(payload["controls"], invalid["controls"]);
        assert_eq!(payload["overrides"], invalid["overrides"]);
        assert_eq!(payload["monitored_metrics"], good["monitored_metrics"]);
        assert_eq!(payload["stale_graph"], true);
        assert_eq!(retained, Some(good));
    }

    #[test]
    fn initial_invalid_is_best_effort_and_recovery_clears_stale_flag() {
        let invalid = json!({
            "status": "invalid",
            "nodes": [{ "id": "Recovered.fragment" }],
            "edges": []
        });
        let (initial, last_good) = accept_graph(invalid.clone(), true, None);
        assert_eq!(initial, invalid);
        assert!(initial.get("stale_graph").is_none());
        assert!(last_good.is_none());

        let recovered = json!({
            "status": "partial",
            "nodes": [{ "id": "Factory" }],
            "edges": []
        });
        let (payload, last_good) = accept_graph(recovered.clone(), false, None);
        assert_eq!(payload, recovered);
        assert!(payload.get("stale_graph").is_none());
        assert_eq!(last_good, Some(recovered));
    }

    #[test]
    fn shutdown_observer_releases_live_streams() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            let (_updates, receiver) = watch::channel(Arc::new("graph".to_owned()));
            let (shutdown, shutdown_receiver) = watch::channel(false);
            let mut stream = ShutdownStream::new(WatchStream::new(receiver), shutdown_receiver);

            // Consume the initial SSE-equivalent value, then ensure the stream
            // itself closes when the process-wide shutdown signal is raised.
            assert!(stream.next().await.is_some());
            shutdown.send_replace(true);
            let next = tokio::time::timeout(Duration::from_millis(100), stream.next())
                .await
                .expect("live stream should finish promptly");
            assert!(next.is_none());
        });
    }
}
