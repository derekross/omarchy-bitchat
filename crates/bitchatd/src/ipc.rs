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
    // $XDG_RUNTIME_DIR/bitchat/: under systemd that's the unit's
    // RuntimeDirectory, the only part of /run/user its sandbox can see.
    let dir = std::env::var_os("XDG_RUNTIME_DIR").context("XDG_RUNTIME_DIR is not set")?;
    Ok(PathBuf::from(dir).join("bitchat").join("bitchat.sock"))
}

#[derive(Deserialize)]
struct Request {
    #[serde(default)]
    id: u64,
    method: String,
    #[serde(default)]
    params: Value,
}

/// The bound socket plus the lock that makes it ours. Drop it last.
pub struct Bound {
    listener: UnixListener,
    owner: Owner,
}

/// What stays behind after the listener is handed to the server: the lock
/// and the identity of the socket file.
pub struct Owner {
    _lock: std::fs::File,
    path: PathBuf,
    ino: u64,
}

impl Bound {
    pub fn split(self) -> (UnixListener, Owner) {
        (self.listener, self.owner)
    }
}

impl Owner {
    /// Remove the socket file, but only if it's still the one we bound.
    pub fn remove(&self) {
        use std::os::unix::fs::MetadataExt;
        if std::fs::symlink_metadata(&self.path).is_ok_and(|m| m.ino() == self.ino) {
            let _ = std::fs::remove_file(&self.path);
        }
    }
}

/// Bind the socket. An exclusive lock on `bitchat.lock` next to it decides
/// who owns the name, so two daemons starting at once can't unlink each
/// other's socket; a stale socket from a crash is then safe to replace.
pub async fn bind(path: &Path) -> Result<Bound> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt, OpenOptionsExt};
    use std::os::unix::io::AsRawFd;

    // The folder must be ours alone (systemd makes it 0700; outside systemd
    // we do).
    let dir = path.parent().context("socket path has no folder")?;
    if let Err(e) = std::fs::create_dir(dir)
        && e.kind() != std::io::ErrorKind::AlreadyExists
    {
        return Err(anyhow::Error::from(e).context(format!("creating {}", dir.display())));
    }
    let meta = std::fs::symlink_metadata(dir)?;
    if !meta.is_dir() || meta.uid() != unsafe { libc::geteuid() } {
        bail!("{} must be a folder you own, not a link", dir.display());
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))?;

    let lock_path = path.with_file_name("bitchat.lock");
    let lock = std::fs::OpenOptions::new()
        .create(true)
        .truncate(false)
        .write(true)
        .mode(0o600)
        .custom_flags(libc::O_NOFOLLOW)
        .open(&lock_path)
        .with_context(|| format!("opening {}", lock_path.display()))?;
    if unsafe { libc::flock(lock.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } != 0 {
        bail!("bitchatd is already running ({})", path.display());
    }
    match std::fs::symlink_metadata(path) {
        Ok(m) if m.file_type().is_socket() => std::fs::remove_file(path)?,
        Ok(_) => bail!("{} exists and isn't a socket; not touching it", path.display()),
        Err(_) => {}
    }
    let listener = UnixListener::bind(path).with_context(|| format!("binding {}", path.display()))?;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600))?;
    let ino = std::fs::symlink_metadata(path)?.ino();
    Ok(Bound { listener, owner: Owner { _lock: lock, path: path.to_owned(), ino } })
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
        "forgetPeer" => node.forget_peer(str_param("peerId")?).map(|f| json!(f)),
        "ping" => Ok(json!("pong")),
        other => Err(format!("unknown method \"{other}\"")),
    }
}
