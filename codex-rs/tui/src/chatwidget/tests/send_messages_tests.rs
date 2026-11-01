use super::*;
use codex_send::SendRequest;
use codex_send::SendResponse;
use pretty_assertions::assert_eq;

fn request(id: &str, text: &str) -> SendRequest {
    SendRequest {
        id: id.to_string(),
        text: text.to_string(),
    }
}

fn plain_text_turn(text: &str) -> Vec<UserInput> {
    vec![UserInput::Text {
        text: text.to_string(),
        text_elements: Vec::new(),
    }]
}

#[tokio::test]
async fn sent_message_submits_a_turn_when_the_session_is_idle() {
    let (mut chat, mut rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    let text = "Ghidra is available.\n\nYou can acquire the lock.";

    let response = chat.submit_sent_message(request("r1", text));

    assert_eq!(response, SendResponse::Submitted);
    let Op::UserTurn { items, .. } = next_submit_op(&mut op_rx) else {
        panic!("expected a user turn");
    };
    assert_eq!(items, plain_text_turn(text));
    assert!(
        std::iter::from_fn(|| rx.try_recv().ok())
            .any(|event| matches!(event, AppEvent::FollowTranscript))
    );
}

#[tokio::test]
async fn sent_message_steers_a_running_turn_instead_of_waiting() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());
    chat.on_task_started();

    let response = chat.submit_sent_message(request("r1", "status?"));

    assert_eq!(response, SendResponse::Submitted);
    let Op::UserTurn { items, .. } = next_submit_op(&mut op_rx) else {
        panic!("expected a user turn");
    };
    assert_eq!(items, plain_text_turn("status?"));
    assert!(chat.input_queue.queued_user_messages.is_empty());
}

#[tokio::test]
async fn sent_message_is_queued_until_the_session_is_configured() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    let response = chat.submit_sent_message(request("r1", "hello"));

    assert_eq!(response, SendResponse::Queued);
    assert_eq!(
        chat.input_queue
            .queued_user_messages
            .iter()
            .map(|queued| queued.user_message.text.clone())
            .collect::<Vec<_>>(),
        vec!["hello".to_string()]
    );
    assert!(op_rx.try_recv().is_err());
}

#[tokio::test]
async fn repeated_request_id_is_acknowledged_without_a_second_submission() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.thread_id = Some(ThreadId::new());

    let first = chat.submit_sent_message(request("r1", "once"));
    let second = chat.submit_sent_message(request("r1", "once"));

    assert_eq!(
        (first, second),
        (SendResponse::Submitted, SendResponse::Submitted)
    );
    assert_matches!(next_submit_op(&mut op_rx), Op::UserTurn { .. });
    assert!(op_rx.try_recv().is_err());
}

#[tokio::test]
async fn sent_message_delivers_shell_escapes_and_slash_commands_as_plain_text() {
    for text in ["!rm -rf scratch", "/compact", "/quit now"] {
        let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
        chat.thread_id = Some(ThreadId::new());

        let response = chat.submit_sent_message(request("r1", text));

        assert_eq!(response, SendResponse::Submitted, "{text}");
        let Op::UserTurn { items, .. } = next_submit_op(&mut op_rx) else {
            panic!("expected `{text}` to be a plain user turn");
        };
        assert_eq!(items, plain_text_turn(text));
        assert!(op_rx.try_recv().is_err(), "{text}");
    }
}

#[tokio::test]
async fn sent_message_leaves_pending_questions_in_place() {
    let pending_question = codex_protocol::items::AsyncUserInputQuestion {
        title: "Which way?".into(),
        options: None,
    };

    // Submitted while idle, and steered while a turn runs.
    for running_turn in [false, true] {
        let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
        chat.thread_id = Some(ThreadId::new());
        chat.add_async_questions("question", std::slice::from_ref(&pending_question));
        if running_turn {
            chat.on_task_started();
        }

        let response = chat.submit_sent_message(request("r1", "status?"));

        assert_eq!(response, SendResponse::Submitted, "{running_turn}");
        assert_matches!(next_submit_op(&mut op_rx), Op::UserTurn { .. });
        assert_eq!(
            chat.bottom_pane.question_editor().unanswered_count(),
            1,
            "{running_turn}"
        );
    }

    // Queued before the session is configured, then drained.
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.add_async_questions("question", std::slice::from_ref(&pending_question));

    let response = chat.submit_sent_message(request("r1", "status?"));

    assert_eq!(response, SendResponse::Queued);
    assert_eq!(chat.bottom_pane.question_editor().unanswered_count(), 1);
    chat.thread_id = Some(ThreadId::new());
    chat.maybe_send_next_queued_input();
    assert_matches!(next_submit_op(&mut op_rx), Op::UserTurn { .. });
    assert_eq!(chat.bottom_pane.question_editor().unanswered_count(), 1);
}

#[tokio::test]
async fn queued_sent_message_stays_literal_when_the_queue_drains() {
    let (mut chat, _rx, mut op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    let response = chat.submit_sent_message(request("r1", "!rm -rf scratch"));

    assert_eq!(response, SendResponse::Queued);
    let queued = &chat.input_queue.queued_user_messages;
    assert_eq!(queued.len(), 1);
    assert!(matches!(queued[0].action, QueuedInputAction::Literal));

    chat.thread_id = Some(ThreadId::new());
    chat.maybe_send_next_queued_input();
    let Op::UserTurn { items, .. } = next_submit_op(&mut op_rx) else {
        panic!("expected the drained message to be a plain user turn, not a shell command");
    };
    assert_eq!(items, plain_text_turn("!rm -rf scratch"));
}
