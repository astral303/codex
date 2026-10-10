//! The per-session socket that `codex send` delivers messages through.
//!
//! The app binds one endpoint for its primary thread and replaces it when the
//! primary thread changes. Dropping the task removes the socket file, so a
//! sender sees `no live session is listening` instead of a dead endpoint.

use std::path::PathBuf;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;
use std::sync::atomic::Ordering;

use codex_protocol::ThreadId;
use codex_send::SendResponse;
use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use super::App;
use crate::app_event::AppEvent;

pub(crate) struct SendEndpointTask {
    thread_id: ThreadId,
    socket_path: PathBuf,
    /// Set once the task owns the socket. A task that lost the bind to another
    /// live session, or to a non-socket file, must not remove that path.
    bound: Arc<AtomicBool>,
    task: Option<JoinHandle<()>>,
}

impl Drop for SendEndpointTask {
    fn drop(&mut self) {
        if let Some(task) = self.task.take() {
            task.abort();
        }
        if !self.bound.load(Ordering::Acquire) {
            return;
        }
        // The aborted task drops its listener later; remove the file now so a
        // sender that races the shutdown sees a missing endpoint.
        if let Err(error) = std::fs::remove_file(&self.socket_path)
            && error.kind() != std::io::ErrorKind::NotFound
        {
            tracing::warn!(
                "failed to remove session endpoint {}: {error}",
                self.socket_path.display()
            );
        }
    }
}

impl App {
    pub(super) fn bind_send_endpoint(&mut self, thread_id: ThreadId) {
        if !self.config.ipc_enabled {
            return;
        }
        if self
            .send_endpoint
            .as_ref()
            .is_some_and(|endpoint| endpoint.thread_id == thread_id)
        {
            return;
        }
        // The previous task removes its socket when it is dropped, which happens
        // on its next poll. Binding before that would race a same-path cleanup.
        let previous = self.send_endpoint.take().and_then(|mut endpoint| {
            let task = endpoint.task.take()?;
            task.abort();
            Some(task)
        });

        let codex_home = self.config.codex_home.to_path_buf();
        let socket_path = codex_send::session_socket_path(&codex_home, thread_id);
        let app_event_tx = self.app_event_tx.clone();
        let bound = Arc::new(AtomicBool::new(false));
        let bound_by_task = Arc::clone(&bound);
        let task = tokio::spawn(async move {
            if let Some(previous) = previous {
                let _ = previous.await;
            }
            let endpoint = match codex_send::bind_session_endpoint(&codex_home, thread_id).await {
                Ok(endpoint) => endpoint,
                Err(error) => {
                    tracing::warn!("codex send is unavailable for this session: {error}");
                    return;
                }
            };
            bound_by_task.store(true, Ordering::Release);
            endpoint
                .serve(move |request| {
                    let app_event_tx = app_event_tx.clone();
                    async move {
                        let (reply_tx, reply_rx) = oneshot::channel();
                        app_event_tx.send(AppEvent::SendMessageReceived {
                            request,
                            reply: reply_tx,
                        });
                        reply_rx.await.unwrap_or_else(|_| SendResponse::Rejected {
                            message: "the session did not accept the message".to_string(),
                        })
                    }
                })
                .await;
        });
        self.send_endpoint = Some(SendEndpointTask {
            thread_id,
            socket_path,
            bound,
            task: Some(task),
        });
    }
}

#[cfg(test)]
#[path = "send_endpoint_tests.rs"]
mod tests;
