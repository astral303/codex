use super::*;
use crate::app::test_support::make_test_app;
use codex_send::SendRequest;
use codex_send::session_socket_path;
use codex_utils_absolute_path::test_support::PathExt;
use pretty_assertions::assert_eq;
use std::path::Path;
use std::time::Duration;

/// Unix socket paths are limited to about 104 bytes on macOS, and the default
/// temp directory there is already most of that.
fn short_codex_home() -> tempfile::TempDir {
    let builder = tempfile::Builder::new();
    #[cfg(unix)]
    let dir = builder.tempdir_in("/tmp");
    #[cfg(not(unix))]
    let dir = builder.tempdir();
    dir.expect("temp dir")
}

async fn wait_for_socket(path: &Path, expected: bool) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(5);
    while tokio::fs::try_exists(path).await.expect("probe socket") != expected {
        assert!(
            tokio::time::Instant::now() < deadline,
            "timed out waiting for socket {} to {}",
            path.display(),
            if expected { "appear" } else { "disappear" }
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test]
async fn endpoint_moves_with_the_primary_thread_and_is_removed_on_reset() {
    let mut app = make_test_app().await;
    let codex_home = short_codex_home();
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    app.config.ipc_enabled = true;
    let first = ThreadId::new();
    let second = ThreadId::new();
    let first_socket = session_socket_path(codex_home.path(), first);
    let second_socket = session_socket_path(codex_home.path(), second);

    app.bind_send_endpoint(first);
    wait_for_socket(&first_socket, /*expected*/ true).await;

    app.bind_send_endpoint(second);
    wait_for_socket(&second_socket, /*expected*/ true).await;
    wait_for_socket(&first_socket, /*expected*/ false).await;

    // The test app has no event loop, so the reply channel closes unanswered.
    let response = codex_send::send_message(
        codex_home.path(),
        second,
        &SendRequest {
            id: "r1".to_string(),
            text: "hello".to_string(),
        },
    )
    .await
    .expect("round trip over the socket");
    assert_eq!(
        response,
        codex_send::SendResponse::Rejected {
            message: "the session did not accept the message".to_string(),
        }
    );

    app.reset_thread_event_state().await;
    assert!(app.send_endpoint.is_none());
    wait_for_socket(&second_socket, /*expected*/ false).await;
}

#[tokio::test]
async fn rebinding_the_same_thread_waits_for_the_previous_cleanup() {
    let mut app = make_test_app().await;
    let codex_home = short_codex_home();
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    app.config.ipc_enabled = true;
    let thread_id = ThreadId::new();
    let socket = session_socket_path(codex_home.path(), thread_id);

    app.bind_send_endpoint(thread_id);
    wait_for_socket(&socket, /*expected*/ true).await;
    app.reset_thread_event_state().await;
    app.bind_send_endpoint(thread_id);

    wait_for_socket(&socket, /*expected*/ true).await;
    // The aborted first task must not remove the socket the second task owns.
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(tokio::fs::try_exists(&socket).await.expect("probe socket"));
}

#[tokio::test]
async fn endpoint_is_not_bound_when_ipc_is_disabled() {
    let mut app = make_test_app().await;
    let codex_home = short_codex_home();
    app.config.codex_home = codex_home.path().to_path_buf().abs();
    app.config.ipc_enabled = false;

    app.bind_send_endpoint(ThreadId::new());

    assert!(app.send_endpoint.is_none());
    assert!(!codex_send::ipc_directory(codex_home.path()).exists());
}
