//! `thclaws shell preview <path>` — local dev server for iterating on
//! shells. Serves the shell at `http://localhost:<port>/`, ships a
//! mock agent over the same WebSocket the real bridge uses, and
//! hot-reloads on filesystem changes via SSE.
//!
//! dev-plan/39 Tier 2.

use std::net::SocketAddr;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::Duration;

use axum::{
    extract::{
        ws::{Message, WebSocket, WebSocketUpgrade},
        Path as AxumPath, State,
    },
    http::StatusCode,
    response::{sse::Event, IntoResponse, Sse},
    routing::get,
    Router,
};
use futures::Stream;
use serde_json::{json, Value};
use tokio::sync::broadcast;

#[derive(Clone)]
struct PreviewState {
    root: Arc<PathBuf>,
    mock: Arc<Value>,
    reload: broadcast::Sender<()>,
}

pub async fn run_preview(path: &Path, port: u16) -> Result<(), String> {
    let root = path
        .canonicalize()
        .map_err(|e| format!("canonicalize {}: {e}", path.display()))?;
    let manifest_path = root.join("shell.json");
    if !manifest_path.exists() {
        return Err(format!("missing shell.json at {}", manifest_path.display()));
    }

    let mock_path = root.join("mock.json");
    let mock: Value = if mock_path.exists() {
        let raw =
            std::fs::read_to_string(&mock_path).map_err(|e| format!("read mock.json: {e}"))?;
        serde_json::from_str(&raw).unwrap_or_else(|_| json!({}))
    } else {
        json!({})
    };

    let (reload_tx, _reload_rx) = broadcast::channel::<()>(16);
    let state = PreviewState {
        root: Arc::new(root.clone()),
        mock: Arc::new(mock),
        reload: reload_tx.clone(),
    };

    // Filesystem watcher → broadcast a reload signal on any edit
    // (debounced). Runs for the duration of the preview server.
    spawn_watcher(&root, reload_tx.clone())?;

    let app = Router::new()
        .route("/", get(serve_index))
        .route("/__ws", get(ws_handler))
        .route("/__reload", get(sse_reload))
        .route("/__bridge.js", get(bridge_runtime))
        .route("/{*rel}", get(serve_asset))
        .with_state(state);

    let bind: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let listener = tokio::net::TcpListener::bind(&bind)
        .await
        .map_err(|e| format!("bind {bind}: {e}"))?;
    let resolved = listener.local_addr().unwrap_or(bind);
    eprintln!(
        "\x1b[36m[shell preview] open http://{} (hot-reload on save)\x1b[0m",
        resolved
    );
    axum::serve(listener, app)
        .await
        .map_err(|e| format!("serve: {e}"))?;
    Ok(())
}

fn spawn_watcher(root: &Path, reload: broadcast::Sender<()>) -> Result<(), String> {
    use notify_debouncer_mini::new_debouncer;
    let root_buf = root.to_path_buf();
    std::thread::spawn(move || {
        let (tx, rx) = std::sync::mpsc::channel();
        let mut debouncer = match new_debouncer(Duration::from_millis(300), tx) {
            Ok(d) => d,
            Err(e) => {
                eprintln!("[shell preview] watcher init failed: {e}");
                return;
            }
        };
        if let Err(e) = debouncer.watcher().watch(
            &root_buf,
            notify_debouncer_mini::notify::RecursiveMode::Recursive,
        ) {
            eprintln!("[shell preview] watch failed: {e}");
            return;
        }
        while let Ok(_events) = rx.recv() {
            let _ = reload.send(());
        }
    });
    Ok(())
}

async fn serve_index(State(state): State<PreviewState>) -> impl IntoResponse {
    let manifest = match read_manifest(&state.root) {
        Ok(m) => m,
        Err(e) => return (StatusCode::INTERNAL_SERVER_ERROR, e).into_response(),
    };
    let entry = state.root.join(&manifest.entry);
    let mut html = match std::fs::read_to_string(&entry) {
        Ok(s) => s,
        Err(e) => {
            return (
                StatusCode::INTERNAL_SERVER_ERROR,
                format!("read {}: {e}", entry.display()),
            )
                .into_response();
        }
    };
    // Inject bridge + hot-reload client into the shell's <head>.
    // `__thclaws_shell_ws_url` is a PATH, not a URL: gui-shell-bridge.js
    // prefixes `proto//host` itself in connect(), and fileUrl() strips the
    // trailing `/__ws` to derive the asset prefix. Passing a full `ws://…`
    // here produced `ws://host` + `ws://host/__ws` and every shell that
    // called run/callTool died on an invalid WebSocket URL in preview.
    // `--serve` always passed a path (server.rs: `{prefix}/__ws`).
    let bootstrap = format!(
        "<script>window.__thclaws_shell_mode='ws';\
window.__thclaws_shell_id={};\
window.__thclaws_shell_session_id='preview';\
window.__thclaws_shell_ws_url='/__ws';</script>\
<script src='/__bridge.js'></script>\
<script>(()=>{{const ev=new EventSource('/__reload');ev.onmessage=e=>{{if(e.data==='reload')location.reload();}};}})();</script>",
        serde_json::to_string(&manifest.id).unwrap_or_else(|_| "\"preview\"".into()),
    );
    html = html.replacen("</head>", &format!("{bootstrap}\n</head>"), 1);
    (
        StatusCode::OK,
        [("content-type", "text/html; charset=utf-8")],
        html,
    )
        .into_response()
}

async fn bridge_runtime() -> impl IntoResponse {
    (
        StatusCode::OK,
        [("content-type", "application/javascript; charset=utf-8")],
        super::BRIDGE_RUNTIME,
    )
}

async fn serve_asset(
    State(state): State<PreviewState>,
    AxumPath(rel): AxumPath<String>,
) -> impl IntoResponse {
    // Path-traversal guard: normalize + ensure under root.
    let candidate = state.root.join(&rel);
    let canonical = candidate.canonicalize().ok();
    if let Some(c) = canonical {
        if !c.starts_with(&*state.root) {
            return (StatusCode::FORBIDDEN, "path escape").into_response();
        }
        match std::fs::read(&c) {
            Ok(bytes) => {
                let ct = mime_for(&c).unwrap_or("application/octet-stream");
                (StatusCode::OK, [("content-type", ct)], bytes).into_response()
            }
            Err(_) => (StatusCode::NOT_FOUND, "not found").into_response(),
        }
    } else {
        (StatusCode::NOT_FOUND, "not found").into_response()
    }
}

fn mime_for(p: &Path) -> Option<&'static str> {
    let ext = p.extension().and_then(|s| s.to_str())?.to_ascii_lowercase();
    Some(match ext.as_str() {
        "html" | "htm" => "text/html; charset=utf-8",
        "css" => "text/css; charset=utf-8",
        "js" | "mjs" => "application/javascript; charset=utf-8",
        "json" => "application/json; charset=utf-8",
        "png" => "image/png",
        "jpg" | "jpeg" => "image/jpeg",
        "svg" => "image/svg+xml",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "ico" => "image/x-icon",
        "woff" => "font/woff",
        "woff2" => "font/woff2",
        _ => return None,
    })
}

async fn sse_reload(
    State(state): State<PreviewState>,
) -> Sse<impl Stream<Item = Result<Event, std::convert::Infallible>>> {
    use futures::StreamExt;
    let rx = state.reload.subscribe();
    let stream = tokio_stream::wrappers::BroadcastStream::new(rx).filter_map(|r| async move {
        match r {
            Ok(_) => Some(Ok(Event::default().data("reload"))),
            Err(_) => None,
        }
    });
    Sse::new(stream).keep_alive(
        axum::response::sse::KeepAlive::new()
            .interval(Duration::from_secs(15))
            .text("keepalive"),
    )
}

async fn ws_handler(ws: WebSocketUpgrade, State(state): State<PreviewState>) -> impl IntoResponse {
    ws.on_upgrade(move |sock| handle_ws(sock, state))
}

async fn handle_ws(mut socket: WebSocket, state: PreviewState) {
    // Send a "ready" event so the bridge unblocks.
    let _ = socket
        .send(Message::Text(
            json!({"event":"ready","payload":{}}).to_string().into(),
        ))
        .await;

    while let Some(Ok(msg)) = futures::StreamExt::next(&mut socket).await {
        let Message::Text(text) = msg else { continue };
        let req: Value = match serde_json::from_str(text.as_str()) {
            Ok(v) => v,
            Err(_) => continue,
        };
        let method = req.get("method").and_then(|v| v.as_str()).unwrap_or("");
        // Inbound key is `requestId` (what the bridge sends); the reply key is
        // `replyTo` (what it waits on). They are deliberately different.
        let req_id = req.get("requestId").and_then(|v| v.as_u64());
        match method {
            "run" => {
                let prompt = req
                    .get("params")
                    .and_then(|p| p.get("prompt"))
                    .and_then(|v| v.as_str())
                    .unwrap_or("");
                // Acknowledge with a runId so thclaws.run()'s promise resolves.
                if let Some(id) = req_id {
                    let _ = socket
                        .send(Message::Text(
                            json!({"replyTo": id, "result": {"runId": "mock-run-1"}})
                                .to_string()
                                .into(),
                        ))
                        .await;
                }
                // Stream the mock reply as text deltas, then done.
                let reply = pick_reply(&state.mock, prompt);
                for chunk in chunk_text(&reply, 24) {
                    let _ = socket
                        .send(Message::Text(
                            json!({"event":"text","payload":{"delta": chunk}})
                                .to_string()
                                .into(),
                        ))
                        .await;
                    tokio::time::sleep(Duration::from_millis(50)).await;
                }
                let _ = socket
                    .send(Message::Text(
                        json!({"event":"done","payload":{}}).to_string().into(),
                    ))
                    .await;
            }
            "cancel" => {
                if let Some(id) = req_id {
                    let _ = socket
                        .send(Message::Text(
                            json!({"replyTo": id, "result": null}).to_string().into(),
                        ))
                        .await;
                }
            }
            "storage.get" => {
                if let Some(id) = req_id {
                    // Mock storage is per-process volatile; return null.
                    let _ = socket
                        .send(Message::Text(
                            json!({"replyTo": id, "result": null}).to_string().into(),
                        ))
                        .await;
                }
            }
            "storage.set" | "storage.delete" => {
                if let Some(id) = req_id {
                    let _ = socket
                        .send(Message::Text(
                            json!({"replyTo": id, "result": null}).to_string().into(),
                        ))
                        .await;
                }
            }
            _ => {
                if let Some(id) = req_id {
                    let _ = socket
                        .send(Message::Text(
                            json!({
                                "replyTo": id,
                                "error": format!("preview mock doesn't implement '{method}'")
                            })
                            .to_string()
                            .into(),
                        ))
                        .await;
                }
            }
        }
    }
}

fn pick_reply(mock: &Value, prompt: &str) -> String {
    if let Some(rules) = mock.get("rules").and_then(|v| v.as_array()) {
        for r in rules {
            if let (Some(pat), Some(reply)) = (
                r.get("match").and_then(|v| v.as_str()),
                r.get("reply").and_then(|v| v.as_str()),
            ) {
                if let Ok(re) = regex::Regex::new(pat) {
                    if re.is_match(prompt) {
                        return reply.to_string();
                    }
                }
            }
        }
    }
    mock.get("default_response")
        .and_then(|v| v.as_str())
        .map(str::to_string)
        .unwrap_or_else(|| {
            "[mock reply] Edit `mock.json` to script different responses per prompt regex."
                .to_string()
        })
}

fn chunk_text(s: &str, n: usize) -> Vec<String> {
    s.chars()
        .collect::<Vec<_>>()
        .chunks(n)
        .map(|c| c.iter().collect())
        .collect()
}

fn read_manifest(root: &Path) -> Result<super::manifest::ShellManifest, String> {
    let p = root.join("shell.json");
    let raw = std::fs::read_to_string(&p).map_err(|e| format!("read {}: {e}", p.display()))?;
    serde_json::from_str(&raw).map_err(|e| format!("parse {}: {e}", p.display()))
}
