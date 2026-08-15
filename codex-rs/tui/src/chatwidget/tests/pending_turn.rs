use super::*;

#[tokio::test]
async fn submitted_turn_reserves_working_rows_before_turn_started() {
    let (mut chat, mut rx, mut op_rx) =
        make_chatwidget_manual(/*model_override*/ Some("gpt-5")).await;
    chat.thread_id = Some(ThreadId::new());
    drain_insert_history(&mut rx);

    chat.submit_user_message(UserMessage::from(
        "A submitted prompt that wraps across rows.",
    ));

    assert_matches!(next_submit_op(&mut op_rx), Op::UserTurn { .. });
    assert!(chat.input_queue.user_turn_pending_start);
    assert!(chat.bottom_pane.is_task_running());
    let pending_height = chat.desired_height(/*width*/ 24);

    chat.on_task_started();

    assert_eq!(chat.desired_height(/*width*/ 24), pending_height);
}

#[tokio::test]
async fn accepted_direct_start_commands_reserve_pending_rows() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    let commands = [
        AppCommand::compact(),
        AppCommand::review(ReviewTarget::Custom {
            instructions: "review this".to_string(),
        }),
        AppCommand::run_user_shell_command("echo ready".to_string()),
    ];

    for command in commands {
        assert!(chat.submit_op(command));
        assert!(chat.input_queue.user_turn_pending_start);
        assert!(chat.bottom_pane.is_task_running());
        chat.clear_user_turn_pending_start();
    }
}

#[tokio::test]
async fn rejected_direct_start_does_not_reserve_pending_rows() {
    let (mut chat, _rx, op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    drop(op_rx);

    assert!(!chat.submit_op(AppCommand::review(ReviewTarget::Custom {
        instructions: "review this".to_string(),
    })));
    assert!(!chat.input_queue.user_turn_pending_start);
    assert!(!chat.bottom_pane.is_task_running());
}

#[tokio::test]
async fn rejected_app_event_enqueue_does_not_reserve_pending_rows() {
    let (mut chat, app_event_rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.codex_op_target = CodexOpTarget::AppEvent;
    drop(app_event_rx);

    assert!(!chat.submit_op(AppCommand::compact()));
    assert!(!chat.input_queue.user_turn_pending_start);
    assert!(!chat.bottom_pane.is_task_running());
}

#[tokio::test]
async fn failed_attempt_does_not_release_an_older_pending_reservation() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    assert!(chat.reserve_user_turn_pending_start());
    chat.input_queue.submit_pending_steers_after_interrupt = true;
    let second_attempt_created_reservation = chat.reserve_user_turn_pending_start();
    assert!(!second_attempt_created_reservation);

    chat.rollback_user_turn_pending_start(second_attempt_created_reservation);
    chat.handle_turn_start_rejection("request rejected".to_string());

    assert!(chat.input_queue.user_turn_pending_start);
    assert!(chat.input_queue.submit_pending_steers_after_interrupt);
    assert!(chat.bottom_pane.is_task_running());
}

#[tokio::test]
async fn canceled_safety_retry_releases_pending_turn_reservation() {
    let (mut chat, _rx, _op_rx) = make_chatwidget_manual(/*model_override*/ None).await;

    chat.prepare_safety_buffered_retry_submission(UserMessage::from("retry this prompt"));
    assert!(chat.input_queue.user_turn_pending_start);
    assert!(chat.bottom_pane.is_task_running());

    chat.cancel_safety_buffered_retry_submission();

    assert!(!chat.input_queue.user_turn_pending_start);
    assert!(!chat.bottom_pane.is_task_running());
}
