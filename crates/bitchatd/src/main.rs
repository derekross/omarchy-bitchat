//! bitchatd: a bitchat BLE mesh node for the Omarchy bar.
//!
//! Runs as a systemd user service. Owns the Bluetooth side (via BlueZ) and
//! the mesh; the bar plugin talks to it over `$XDG_RUNTIME_DIR/bitchat.sock`.

mod ble;
mod ipc;
mod mesh;
mod node;
mod power;
mod store;

use std::time::Duration;

use anyhow::Result;
use tokio::signal::unix::{SignalKind, signal};

use crate::mesh::{Mesh, Target};
use crate::node::Node;
use crate::store::Store;

#[tokio::main]
async fn main() -> Result<()> {
    tracing_subscriber::fmt()
        .with_env_filter(
            tracing_subscriber::EnvFilter::try_from_env("BITCHAT_LOG")
                .unwrap_or_else(|_| "bitchatd=info".into()),
        )
        .without_time() // journald adds its own
        .with_ansi(std::io::IsTerminal::is_terminal(&std::io::stdout()))
        .init();

    if std::env::args().any(|a| a == "--version" || a == "-V") {
        println!("bitchatd {}", env!("CARGO_PKG_VERSION"));
        return Ok(());
    }

    let store = Store::from_env()?;
    let (identity, nickname) = store.load_identity()?;
    let settings = store.load_settings();
    let history = if settings.persist_history { store.load_history() } else { Vec::new() };
    tracing::info!(
        "peer {} ({nickname}), fingerprint {}",
        identity.peer_id(),
        identity.fingerprint()
    );

    let socket = ipc::socket_path()?;
    let listener = ipc::bind(&socket).await?;

    let mesh = Mesh::new(identity, nickname, history);
    let (node, out_rx) = Node::new(mesh, store, settings);

    tokio::spawn(node::ticker(node.clone()));
    tokio::spawn(ble::supervise(node.clone(), out_rx));
    let server = tokio::spawn(ipc::serve(node.clone(), listener));

    let mut term = signal(SignalKind::terminate())?;
    let mut int = signal(SignalKind::interrupt())?;
    tokio::select! {
        _ = term.recv() => {}
        _ = int.recv() => {}
        res = server => {
            tracing::error!("control socket stopped: {res:?}");
        }
    }

    // Say goodbye so peers drop us now rather than after a timeout.
    node.send_raw(node.leave_packet(), Target::All);
    tokio::time::sleep(Duration::from_millis(400)).await;
    let _ = std::fs::remove_file(&socket);
    tracing::info!("stopped");
    Ok(())
}
