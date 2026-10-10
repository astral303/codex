use super::*;
use pretty_assertions::assert_eq;
use std::sync::Arc;
use tokio::sync::Mutex;

/// Unix socket paths are limited to about 104 bytes on macOS, and the default
/// temp directory there is already most of that.
fn short_tempdir() -> tempfile::TempDir {
    let builder = tempfile::Builder::new();
    #[cfg(unix)]
    let dir = builder.tempdir_in("/tmp");
    #[cfg(not(unix))]
    let dir = builder.tempdir();
    dir.expect("temp dir")
}

fn thread_id() -> ThreadId {
    ThreadId::from_string("01a05483-980f-7f13-80a8-a6efb95dd407").expect("valid thread id")
}

fn request(text: &str) -> SendRequest {
    SendRequest {
        id: "req-1".to_string(),
        text: text.to_string(),
    }
}

/// Serves `endpoint` in a background task and records every accepted request.
fn serve_recording(
    endpoint: SessionEndpoint,
) -> (tokio::task::JoinHandle<()>, Arc<Mutex<Vec<SendRequest>>>) {
    let received = Arc::new(Mutex::new(Vec::new()));
    let recorder = Arc::clone(&received);
    let task = tokio::spawn(endpoint.serve(move |request| {
        let recorder = Arc::clone(&recorder);
        async move {
            recorder.lock().await.push(request);
            SendResponse::Submitted
        }
    }));
    (task, received)
}

#[tokio::test]
async fn round_trip_preserves_multiline_text_and_reports_submission() {
    let codex_home = short_tempdir();
    let endpoint = bind_session_endpoint(codex_home.path(), thread_id())
        .await
        .expect("bind");
    let (task, received) = serve_recording(endpoint);

    let text = "Ghidra is available.\n\nYou can acquire the lock.\n";
    let response = send_message(codex_home.path(), thread_id(), &request(text))
        .await
        .expect("send");

    assert_eq!(response, SendResponse::Submitted);
    assert_eq!(*received.lock().await, vec![request(text)]);
    task.abort();
}

#[tokio::test]
async fn missing_endpoint_reports_session_not_running() {
    let codex_home = short_tempdir();

    let error = send_message(codex_home.path(), thread_id(), &request("hello"))
        .await
        .expect_err("no listener");

    assert!(
        matches!(error, SendError::SessionNotRunning { thread_id: id, .. } if id == thread_id()),
        "{error:?}"
    );
}

#[tokio::test]
async fn invalid_request_line_is_rejected_without_closing_the_listener() {
    let codex_home = short_tempdir();
    let endpoint = bind_session_endpoint(codex_home.path(), thread_id())
        .await
        .expect("bind");
    let path = session_socket_path(codex_home.path(), thread_id());
    let (task, received) = serve_recording(endpoint);

    let mut stream = BufReader::new(UnixStream::connect(&path).await.expect("connect"));
    stream.write_all(b"not json\n").await.expect("write");
    let mut line = String::new();
    stream.read_line(&mut line).await.expect("read");
    let response: SendResponse = serde_json::from_str(line.trim_end()).expect("parse");
    assert!(
        matches!(response, SendResponse::Rejected { .. }),
        "{response:?}"
    );

    let response = send_message(codex_home.path(), thread_id(), &request("hello"))
        .await
        .expect("send after rejection");
    assert_eq!(response, SendResponse::Submitted);
    assert_eq!(*received.lock().await, vec![request("hello")]);
    task.abort();
}

#[tokio::test]
async fn live_endpoint_is_not_replaced_by_a_second_session() {
    let codex_home = short_tempdir();
    let endpoint = bind_session_endpoint(codex_home.path(), thread_id())
        .await
        .expect("bind");
    let (task, _received) = serve_recording(endpoint);

    let error = bind_session_endpoint(codex_home.path(), thread_id())
        .await
        .err()
        .expect("second bind must fail");

    assert!(matches!(error, BindError::AlreadyOwned { .. }), "{error:?}");
    task.abort();
}

#[tokio::test]
async fn stale_endpoint_is_replaced_and_dropping_removes_the_socket() {
    let codex_home = short_tempdir();
    let path = session_socket_path(codex_home.path(), thread_id());

    // A closed listener leaves its socket file behind with nobody answering.
    tokio::fs::create_dir_all(ipc_directory(codex_home.path()))
        .await
        .expect("ipc dir");
    drop(UnixListener::bind(&path).await.expect("stale listener"));
    assert!(tokio::fs::try_exists(&path).await.expect("exists"));

    let endpoint = bind_session_endpoint(codex_home.path(), thread_id())
        .await
        .expect("stale socket is replaced");
    drop(endpoint);

    assert!(!tokio::fs::try_exists(&path).await.expect("exists"));
}

#[tokio::test]
async fn silent_connection_does_not_block_later_senders() {
    let codex_home = short_tempdir();
    let endpoint = bind_session_endpoint(codex_home.path(), thread_id())
        .await
        .expect("bind");
    let path = session_socket_path(codex_home.path(), thread_id());
    let (task, received) = serve_recording(endpoint);

    let _silent = UnixStream::connect(&path)
        .await
        .expect("connect without writing");
    let response = tokio::time::timeout(
        REQUEST_READ_TIMEOUT + Duration::from_secs(5),
        send_message(
            codex_home.path(),
            thread_id(),
            &request("after the silent one"),
        ),
    )
    .await
    .expect("the silent connection times out and the next sender is served")
    .expect("send");

    assert_eq!(response, SendResponse::Submitted);
    assert_eq!(
        *received.lock().await,
        vec![request("after the silent one")]
    );
    task.abort();
}
