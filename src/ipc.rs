//! Hook to TUI messages over the Unix socket: one connection per request,
//! one JSON line each way.

use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
use std::path::{Path, PathBuf};
use std::time::Duration;

use anyhow::{Context, Result, bail};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};
use tokio::net::{UnixListener, UnixStream};
use tokio::sync::{mpsc, oneshot};

/// What a hook asks the TUI.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum HookRequest {
    PreTool {
        session_id: String,
        transcript_path: String,
        cwd: String,
        tool_name: String,
        tool_use_id: String,
        tool_input: Value,
    },
    PostTool {
        session_id: String,
        cwd: String,
        tool_name: String,
        tool_input: Value,
    },
    Permission {
        session_id: String,
        cwd: String,
        tool_name: String,
        tool_input: Value,
        /// `$TMUX_PANE` of the hook, so the card can jump to the window.
        pane: Option<String>,
    },
}

/// The answer to a waiting hook.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum HookReply {
    Allow,
    Deny { reason: String },
    NoDecision,
}

/// A request from a hook. `reply` closes when the hook goes away.
#[derive(Debug)]
pub struct IpcEvent {
    pub req_id: u64,
    pub req: HookRequest,
    pub reply: oneshot::Sender<HookReply>,
}

/// Binds and serves the socket until the receiver of `tx` goes away.
pub async fn serve(socket: PathBuf, tx: mpsc::Sender<IpcEvent>) -> Result<()> {
    let listener = listen(&socket)?;
    accept_loop(listener, tx).await;
    Ok(())
}

/// Binds the socket with mode 0600. A socket file nothing answers on is
/// removed first; one that answers means another `aq` is live.
pub fn listen(socket: &Path) -> Result<UnixListener> {
    if let Some(dir) = socket.parent()
        && dir == crate::config::fallback_socket_dir()
    {
        ensure_private_dir(dir)?;
    }
    if std::fs::symlink_metadata(socket).is_ok() {
        if std::os::unix::net::UnixStream::connect(socket).is_ok() {
            bail!("aq is already running (socket {})", socket.display());
        }
        std::fs::remove_file(socket)
            .with_context(|| format!("removing stale socket {}", socket.display()))?;
    }
    let listener =
        UnixListener::bind(socket).with_context(|| format!("binding {}", socket.display()))?;
    std::fs::set_permissions(socket, std::fs::Permissions::from_mode(0o600))
        .with_context(|| format!("setting the mode of {}", socket.display()))?;
    Ok(listener)
}

/// Creates `dir` with mode 0700, or checks that the existing one is ours and
/// makes it private.
pub fn ensure_private_dir(dir: &Path) -> Result<()> {
    match std::fs::DirBuilder::new().mode(0o700).create(dir) {
        Ok(()) => return Ok(()),
        Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e).with_context(|| format!("creating {}", dir.display())),
    }
    let meta =
        std::fs::symlink_metadata(dir).with_context(|| format!("reading {}", dir.display()))?;
    if !meta.is_dir() || meta.uid() != crate::config::current_uid() {
        bail!("{} is not a directory owned by this user", dir.display());
    }
    std::fs::set_permissions(dir, std::fs::Permissions::from_mode(0o700))
        .with_context(|| format!("setting the mode of {}", dir.display()))
}

/// Accepts hooks until the receiver of `tx` goes away.
pub async fn accept_loop(listener: UnixListener, tx: mpsc::Sender<IpcEvent>) {
    let mut next_id = 0u64;
    loop {
        let accepted = tokio::select! {
            _ = tx.closed() => return,
            accepted = listener.accept() => accepted,
        };
        match accepted {
            Ok((stream, _)) => {
                next_id += 1;
                tokio::spawn(handle(stream, next_id, tx.clone()));
            }
            // Out of file descriptors and the like: back off and retry.
            Err(_) => tokio::time::sleep(Duration::from_millis(100)).await,
        }
    }
}

/// Reads one request, waits for its reply and writes it back. If the hook
/// disconnects first, the reply receiver is dropped so the sender sees it.
async fn handle(stream: UnixStream, req_id: u64, tx: mpsc::Sender<IpcEvent>) {
    let (rd, mut wr) = stream.into_split();
    let mut rd = BufReader::new(rd);
    let mut line = String::new();
    match rd.read_line(&mut line).await {
        Ok(n) if n > 0 => {}
        _ => return,
    }
    let Ok(req) = serde_json::from_str::<HookRequest>(&line) else {
        return;
    };
    let (reply_tx, reply_rx) = oneshot::channel();
    let event = IpcEvent {
        req_id,
        req,
        reply: reply_tx,
    };
    if tx.send(event).await.is_err() {
        return;
    }
    let mut byte = [0u8; 1];
    tokio::select! {
        reply = reply_rx => {
            // A dropped sender closes the connection with no answer.
            if let Ok(reply) = reply
                && let Ok(mut out) = serde_json::to_string(&reply)
            {
                out.push('\n');
                let _ = wr.write_all(out.as_bytes()).await;
            }
        }
        // The hook sends nothing more, so any read means it is gone.
        _ = rd.read(&mut byte) => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn mode(d: &Path) -> u32 {
        std::fs::metadata(d).unwrap().permissions().mode() & 0o777
    }

    #[test]
    fn private_dir_is_created_0700_and_tightened() {
        let base = tempfile::tempdir().unwrap();
        let dir = base.path().join("aq-test");
        ensure_private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
        std::fs::set_permissions(&dir, std::fs::Permissions::from_mode(0o755)).unwrap();
        ensure_private_dir(&dir).unwrap();
        assert_eq!(mode(&dir), 0o700);
    }

    #[test]
    fn private_dir_refuses_a_file() {
        let base = tempfile::tempdir().unwrap();
        let file = base.path().join("aq-test");
        std::fs::write(&file, "").unwrap();
        assert!(ensure_private_dir(&file).is_err());
    }
}
