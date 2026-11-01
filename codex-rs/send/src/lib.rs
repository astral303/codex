//! Live message delivery into a running Codex TUI session.
//!
//! Each TUI session listens on `$CODEX_HOME/ipc/<thread-id>.sock`. A sender
//! connects, writes one JSON line holding the message, and reads one JSON line
//! back. The socket is private to the OS user; the TUI routes an accepted
//! message through its ordinary user-input path.

use std::future::Future;
use std::io;
use std::path::Path;
use std::path::PathBuf;
use std::time::Duration;

use codex_protocol::ThreadId;
use codex_uds::UnixListener;
use codex_uds::UnixStream;
use serde::Deserialize;
use serde::Serialize;
use tokio::io::AsyncBufReadExt;
use tokio::io::AsyncReadExt;
use tokio::io::AsyncWriteExt;
use tokio::io::BufReader;

/// Subdirectory of `$CODEX_HOME` holding one socket per live session.
pub const IPC_DIR_NAME: &str = "ipc";

/// Longest request line a session accepts, in bytes.
pub const MAX_REQUEST_BYTES: usize = 1024 * 1024;

const LIVENESS_PROBE_TIMEOUT: Duration = Duration::from_millis(500);
/// A connected sender writes its request at once; a silent connection must
/// not hold the one-at-a-time accept loop.
const REQUEST_READ_TIMEOUT: Duration = Duration::from_secs(5);
/// The reply waits on the recipient's event loop, which may be mid-render.
const RESPONSE_TIMEOUT: Duration = Duration::from_secs(30);
const MAX_CONSECUTIVE_ACCEPT_FAILURES: u32 = 10;

pub fn ipc_directory(codex_home: &Path) -> PathBuf {
    codex_home.join(IPC_DIR_NAME)
}

pub fn session_socket_path(codex_home: &Path, thread_id: ThreadId) -> PathBuf {
    ipc_directory(codex_home).join(format!("{thread_id}.sock"))
}

/// One message for the session. `id` lets the session acknowledge a repeated
/// delivery of the same message without submitting it twice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct SendRequest {
    pub id: String,
    pub text: String,
}

/// The session's answer. `Submitted` means the text entered the turn pipeline
/// as if typed and submitted with Enter. `Queued` means the session held it
/// for the next eligible turn. Neither claims the model has read it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "result", rename_all = "camelCase")]
pub enum SendResponse {
    Submitted,
    Queued,
    Rejected { message: String },
}

#[derive(Debug, thiserror::Error)]
pub enum SendError {
    #[error("no live session is listening for thread {thread_id} (expected {})", path.display())]
    SessionNotRunning { thread_id: ThreadId, path: PathBuf },

    #[error(
        "the session endpoint for thread {thread_id} is not accepting connections ({source}); the session has likely exited"
    )]
    EndpointDead {
        thread_id: ThreadId,
        source: io::Error,
    },

    #[error("failed to exchange the message with the session: {0}")]
    Transport(#[from] io::Error),

    #[error("the session closed the connection without a response")]
    NoResponse,

    #[error("the session did not respond within {} seconds", RESPONSE_TIMEOUT.as_secs())]
    ResponseTimeout,

    #[error("the session returned an unreadable response: {0}")]
    InvalidResponse(#[from] serde_json::Error),
}

/// Delivers `text` to the session that owns `thread_id`.
pub async fn send_message(
    codex_home: &Path,
    thread_id: ThreadId,
    request: &SendRequest,
) -> Result<SendResponse, SendError> {
    let path = session_socket_path(codex_home, thread_id);
    if !tokio::fs::try_exists(&path).await? {
        return Err(SendError::SessionNotRunning { thread_id, path });
    }
    let stream = match UnixStream::connect(&path).await {
        Ok(stream) => stream,
        Err(source) if is_dead_endpoint_error(&source) => {
            return Err(SendError::EndpointDead { thread_id, source });
        }
        Err(source) => return Err(SendError::Transport(source)),
    };
    #[cfg(windows)]
    stream.ensure_non_elevated_peer()?;

    let mut stream = BufReader::new(stream);
    let mut line = serde_json::to_string(request).map_err(io::Error::other)?;
    line.push('\n');
    stream.write_all(line.as_bytes()).await?;
    stream.flush().await?;

    let mut response_line = String::new();
    let bytes_read = tokio::time::timeout(RESPONSE_TIMEOUT, stream.read_line(&mut response_line))
        .await
        .map_err(|_| SendError::ResponseTimeout)??;
    if bytes_read == 0 {
        return Err(SendError::NoResponse);
    }
    Ok(serde_json::from_str(response_line.trim_end())?)
}

fn is_dead_endpoint_error(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
    )
}

#[derive(Debug, thiserror::Error)]
pub enum BindError {
    #[error("another running session already owns thread {thread_id} at {}", path.display())]
    AlreadyOwned { thread_id: ThreadId, path: PathBuf },

    #[error("the session endpoint path {} exceeds the platform's Unix socket path limit; shorten CODEX_HOME or disable [ipc]", path.display())]
    PathTooLong { path: PathBuf },

    #[error("failed to prepare the session endpoint at {}: {source}", path.display())]
    Io { path: PathBuf, source: io::Error },
}

/// The listening side of one session's socket. Dropping it removes the socket
/// file; keep it alive for as long as the session owns the thread.
pub struct SessionEndpoint {
    listener: UnixListener,
    path: PathBuf,
}

/// Binds the socket for `thread_id`. A socket that still answers a connection
/// belongs to another live session and is not replaced; a socket that does not
/// answer is stale and is replaced.
pub async fn bind_session_endpoint(
    codex_home: &Path,
    thread_id: ThreadId,
) -> Result<SessionEndpoint, BindError> {
    let path = session_socket_path(codex_home, thread_id);
    let io_error = |source: io::Error| BindError::Io {
        path: path.clone(),
        source,
    };
    codex_uds::prepare_private_socket_directory(ipc_directory(codex_home))
        .await
        .map_err(io_error)?;

    if tokio::fs::try_exists(&path).await.map_err(io_error)? {
        let probe = tokio::time::timeout(LIVENESS_PROBE_TIMEOUT, UnixStream::connect(&path)).await;
        if matches!(probe, Ok(Ok(_))) {
            return Err(BindError::AlreadyOwned { thread_id, path });
        }
        if codex_uds::is_stale_socket_path(&path)
            .await
            .map_err(io_error)?
        {
            tokio::fs::remove_file(&path).await.map_err(io_error)?;
        } else {
            return Err(io_error(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "path exists and is not a socket",
            )));
        }
    }

    let listener = match UnixListener::bind(&path).await {
        Ok(listener) => listener,
        Err(source) if source.kind() == io::ErrorKind::InvalidInput => {
            return Err(BindError::PathTooLong { path });
        }
        Err(source) => return Err(io_error(source)),
    };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        tokio::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o600))
            .await
            .map_err(io_error)?;
    }
    Ok(SessionEndpoint { listener, path })
}

impl SessionEndpoint {
    /// Accepts connections until the future is dropped or accepting fails
    /// repeatedly. Connections are served one at a time so concurrent senders
    /// are submitted in arrival order.
    pub async fn serve<F, Fut>(mut self, mut handle_request: F)
    where
        F: FnMut(SendRequest) -> Fut,
        Fut: Future<Output = SendResponse>,
    {
        let mut consecutive_accept_failures = 0;
        loop {
            let stream = match self.listener.accept().await {
                Ok(stream) => {
                    consecutive_accept_failures = 0;
                    stream
                }
                Err(error) => {
                    consecutive_accept_failures += 1;
                    if consecutive_accept_failures >= MAX_CONSECUTIVE_ACCEPT_FAILURES {
                        tracing::warn!(
                            "session endpoint {} stopped after repeated accept failures: {error}",
                            self.path.display()
                        );
                        return;
                    }
                    tracing::warn!("session endpoint accept failed: {error}");
                    tokio::time::sleep(Duration::from_millis(100)).await;
                    continue;
                }
            };
            if let Err(error) = serve_connection(stream, &mut handle_request).await {
                tracing::warn!("session endpoint connection failed: {error}");
            }
        }
    }
}

impl Drop for SessionEndpoint {
    fn drop(&mut self) {
        if let Err(error) = std::fs::remove_file(&self.path)
            && error.kind() != io::ErrorKind::NotFound
        {
            tracing::warn!(
                "failed to remove session endpoint {}: {error}",
                self.path.display()
            );
        }
    }
}

async fn serve_connection<F, Fut>(stream: UnixStream, handle_request: &mut F) -> io::Result<()>
where
    F: FnMut(SendRequest) -> Fut,
    Fut: Future<Output = SendResponse>,
{
    #[cfg(windows)]
    stream.ensure_non_elevated_peer()?;
    let mut stream = BufReader::new(stream);
    let mut line = String::new();
    let read = tokio::time::timeout(
        REQUEST_READ_TIMEOUT,
        (&mut stream)
            .take(MAX_REQUEST_BYTES as u64 + 1)
            .read_line(&mut line),
    )
    .await;
    let response = match read {
        Err(_elapsed) => SendResponse::Rejected {
            message: format!(
                "no request received within {} seconds",
                REQUEST_READ_TIMEOUT.as_secs()
            ),
        },
        Ok(Ok(0)) => return Ok(()),
        Ok(Err(error)) => return Err(error),
        Ok(Ok(_)) if line.len() > MAX_REQUEST_BYTES => SendResponse::Rejected {
            message: format!("message exceeds {MAX_REQUEST_BYTES} bytes"),
        },
        Ok(Ok(_)) => match serde_json::from_str::<SendRequest>(line.trim_end()) {
            Ok(request) if request.text.trim().is_empty() => SendResponse::Rejected {
                message: "message text must not be empty".to_string(),
            },
            Ok(request) => handle_request(request).await,
            Err(error) => SendResponse::Rejected {
                message: format!("invalid request: {error}"),
            },
        },
    };
    let mut response_line = serde_json::to_string(&response)?;
    response_line.push('\n');
    stream.write_all(response_line.as_bytes()).await?;
    stream.flush().await?;
    stream.shutdown().await
}
#[cfg(test)]
#[path = "lib_tests.rs"]
mod tests;
