//! Control socket for the bar plugin and `bitchatctl`: newline-delimited
//! JSON over a Unix socket only the current user can reach.
//!
//! ```text
//! → {"id":1,"method":"send","params":{"text":"hi"}}
//! ← {"id":1,"result":true}            or {"id":1,"error":"..."}
//! ← {"event":"message","data":{...}}  (after "subscribe", whose result is the snapshot)
//! ```

use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use anyhow::{Context, Result, bail};
use serde::Deserialize;
use serde_json::{Value, json};
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::mpsc;

use crate::node::Node;
use crate::store::Mode;

const MAX_LINE: usize = 64 * 1024;

pub fn socket_path() -> Result<PathBuf> {
    let dir = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    Ok(PathBuf::from(dir).join("bitchat.sock"))
}

#[derive(Deserialize)]
struct Request {
    #[serde(default)]
    id: u64,
    method: String,
    #[serde(default)]
    params: Value,
}

/// Bind the socket, refusing to start if another daemon answers on it.
pub async fn bind(path: &Path) -> Result<UnixListener> {
    if path.exists() {
        if UnixStream::connect(path).await.is_ok() {
            bail!("bitchatd is already running ({})", path.display());
        }
        std::fs::remove_file(path).ok();
    }
    let listener = UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    Ok(listener)
}

pub async fn serve(node: Arc<Node>, listener: UnixListener) -> Result<()> {
    let uid = unsafe { libc::geteuid() };
    loop {
        let (stream, _) = listener.accept().await?;
        match stream.peer_cred() {
            Ok(cred) if cred.uid() == uid => {}
            _ => {
                tracing::warn!("rejected a connection from another user");
                continue;
            }
        }
        let node = node.clone();
        tokio::spawn(async move {
            if let Err(e) = handle(node, stream).await {
                tracing::debug!("client disconnected: {e}");
            }
        });
    }
}

async fn handle(node: Arc<Node>, stream: UnixStream) -> Result<()> {
    let (read, mut write) = stream.into_split();
    let (tx, mut rx) = mpsc::channel::<String>(256);

    let writer = tokio::spawn(async move {
        while let Some(line) = rx.recv().await {
            if write.write_all(line.as_bytes()).await.is_err() || write.write_all(b"\n").await.is_err() {
                break;
            }
        }
    });

    let mut forwarder: Option<tokio::task::JoinHandle<()>> = None;
    let mut reader = BufReader::new(read);
    let mut line = String::new();
    loop {
        line.clear();
        let n = (&mut reader).take(MAX_LINE as u64 + 1).read_line(&mut line).await?;
        if n == 0 {
            break;
        }
        if line.len() > MAX_LINE {
            bail!("request too long");
        }
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        let req: Request = match serde_json::from_str(trimmed) {
            Ok(r) => r,
            Err(e) => {
                let _ = tx.send(json!({ "id": 0, "error": format!("bad request: {e}") }).to_string()).await;
                continue;
            }
        };

        if req.method == "subscribe" && forwarder.is_none() {
            // Subscribe to events before taking the snapshot so nothing
            // falls between the two.
            let mut events = node.events();
            let snapshot = node.snapshot();
            let _ = tx.send(json!({ "id": req.id, "result": snapshot }).to_string()).await;
            let tx = tx.clone();
            forwarder = Some(tokio::spawn(async move {
                loop {
                    match events.recv().await {
                        Ok(ev) => {
                            if tx.send(ev.to_string()).await.is_err() {
                                break;
                            }
                        }
                        Err(tokio::sync::broadcast::error::RecvError::Lagged(_)) => {
                            // Too slow to keep up: tell the client to resync.
                            let _ = tx.send(json!({ "event": "resync", "data": null }).to_string()).await;
                        }
                        Err(_) => break,
                    }
                }
            }));
            continue;
        }

        let reply = match dispatch(&node, &req.method, &req.params) {
            Ok(result) => json!({ "id": req.id, "result": result }),
            Err(error) => json!({ "id": req.id, "error": error }),
        };
        let _ = tx.send(reply.to_string()).await;
    }

    if let Some(f) = forwarder {
        f.abort();
    }
    drop(tx);
    let _ = writer.await;
    Ok(())
}

fn dispatch(node: &Node, method: &str, params: &Value) -> Result<Value, String> {
    let str_param = |key: &str| {
        params
            .get(key)
            .and_then(Value::as_str)
            .ok_or_else(|| format!("missing string parameter \"{key}\""))
    };
    match method {
        "status" | "subscribe" => Ok(node.snapshot()),
        "send" => node.send_text(str_param("text")?).map(|_| json!(true)),
        "setNickname" => node.set_nickname(str_param("nickname")?).map(|_| json!(true)),
        "setMode" => {
            let mode = Mode::parse(str_param("mode")?).ok_or("mode must be auto, balanced, saver or off")?;
            node.set_mode(mode).map(|_| json!(true))
        }
        "setPersistHistory" => {
            let enabled = params.get("enabled").and_then(Value::as_bool).ok_or("missing boolean \"enabled\"")?;
            node.set_persist_history(enabled).map(|_| json!(true))
        }
        "clearHistory" => node.clear_history().map(|_| json!(true)),
        "ping" => Ok(json!("pong")),
        other => Err(format!("unknown method \"{other}\"")),
    }
}
