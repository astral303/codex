use super::*;
use crate::chatwidget::UserMessage;

#[tokio::test]
async fn starting_request_rolls_back_only_its_own_reservation() {
    let (mut app, _app_event_rx, _op_rx) = make_test_app_with_channels().await;

    super::super::thread_routing::dispatch_starting_request(&mut app.chat_widget, async {
        Ok::<(), &'static str>(())
    })
    .await
    .expect("request accepted");
    assert!(app.chat_widget.user_turn_pending_start_for_test());
    assert!(app.chat_widget.is_task_running_for_test());

    let failed_with_older_reservation =
        super::super::thread_routing::dispatch_starting_request(&mut app.chat_widget, async {
            Err::<(), _>("rejected")
        })
        .await;
    std::assert_eq!(failed_with_older_reservation, Err("rejected"));
    assert!(app.chat_widget.user_turn_pending_start_for_test());
    assert!(app.chat_widget.is_task_running_for_test());

    app.chat_widget.clear_user_turn_pending_start();
    let failed_with_new_reservation =
        super::super::thread_routing::dispatch_starting_request(&mut app.chat_widget, async {
            Err::<(), _>("rejected")
        })
        .await;
    std::assert_eq!(failed_with_new_reservation, Err("rejected"));
    assert!(!app.chat_widget.user_turn_pending_start_for_test());
    assert!(!app.chat_widget.is_task_running_for_test());
}

#[tokio::test]
async fn turn_start_failure_is_shown_without_exiting() -> Result<()> {
    let (mut app, mut app_event_rx, mut op_rx) = make_test_app_with_channels().await;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    let mut app_server = Box::pin(crate::start_embedded_app_server_for_picker(&app.config)).await?;
    let thread_id = ThreadId::from_string("123e4567-e89b-12d3-a456-426614174000")?;
    app.active_thread_id = Some(thread_id);
    app.chat_widget
        .handle_thread_session(test_thread_session(thread_id, app.config.cwd.to_path_buf()));
    while app_event_rx.try_recv().is_ok() {}

    app.chat_widget
        .restore_user_message_to_composer(UserMessage::from("hello"));
    app.chat_widget
        .handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    let op = next_user_turn_op(&mut op_rx);
    app.chat_widget.clear_user_turn_pending_start();
    assert!(!app.chat_widget.user_turn_pending_start_for_test());
    while let Ok(event) = app_event_rx.try_recv() {
        app.handle_event(&mut tui, &mut app_server, event).await?;
    }

    let control = app
        .handle_event(&mut tui, &mut app_server, AppEvent::CodexOp(op))
        .await?;

    assert!(matches!(control, AppRunControl::Continue));
    assert!(!app.chat_widget.user_turn_pending_start_for_test());
    assert!(!app.chat_widget.is_task_running_for_test());
    let error_cell = std::iter::from_fn(|| app_event_rx.try_recv().ok())
        .find_map(|event| match event {
            AppEvent::InsertHistoryCell(cell) => Some(cell),
            _ => None,
        })
        .expect("turn/start failure should be added to history");
    let transcript = app
        .transcript_cells
        .iter()
        .map(|cell| lines_to_single_string(&cell.display_lines(/*width*/ 80)))
        .chain(std::iter::once(lines_to_single_string(
            &error_cell.display_lines(/*width*/ 80),
        )))
        .collect::<Vec<_>>()
        .join("\n");
    insta::assert_snapshot!(transcript, @r"
    › hello

    ■ Failed to start turn: turn/start failed in TUI: turn/start failed: thread not found: 123e4567-e89b-12d3-a456-426614174000 (code -32600)
    ");

    app_server.shutdown().await?;
    Ok(())
}
