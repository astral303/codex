use clap::Parser;
use clap::builder::NonEmptyStringValueParser;
use codex_protocol::ThreadId;
use codex_send::SendError;
use codex_send::SendRequest;
use codex_send::SendResponse;
use codex_utils_home_dir::find_codex_home;
use uuid::Uuid;

/// Deliver a message to a running session now: it steers the active turn or
/// starts a new one. `codex queue` waits for the turn to end.
#[derive(Debug, Parser)]
pub(crate) struct SendCommand {
    /// Thread id of the running session.
    #[arg(long, value_name = "THREAD")]
    thread: String,

    /// Message text to deliver.
    #[arg(long, value_name = "TEXT", value_parser = NonEmptyStringValueParser::new())]
    message: String,
}

pub(crate) async fn run_send_command(command: SendCommand) -> anyhow::Result<String> {
    let SendCommand { thread, message } = command;
    let thread_id = ThreadId::from_string(&thread).map_err(|_| {
        anyhow::anyhow!("`codex send` takes a thread id (UUID); `{thread}` is not one")
    })?;
    let codex_home = find_codex_home()?;
    let request = SendRequest {
        id: Uuid::now_v7().to_string(),
        text: message,
    };
    match codex_send::send_message(codex_home.as_path(), thread_id, &request).await {
        Ok(SendResponse::Submitted) => Ok(format!("Delivered message to thread {thread_id}.")),
        Ok(SendResponse::Queued) => Ok(format!(
            "Thread {thread_id} accepted the message and will submit it when it can take input."
        )),
        Ok(SendResponse::Rejected { message }) => {
            anyhow::bail!("thread {thread_id} rejected the message: {message}")
        }
        Err(error @ SendError::SessionNotRunning { .. }) => anyhow::bail!(
            "{error}; start or resume the session, or check `[ipc] enabled` in config.toml"
        ),
        Err(error) => Err(error.into()),
    }
}
