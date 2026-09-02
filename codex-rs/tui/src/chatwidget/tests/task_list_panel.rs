use super::*;
use crate::render::renderable::Renderable;
use crate::slash_command::SlashCommand;
use codex_config::types::TuiKeymap;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use std::time::Instant;

fn task(step: &str, status: StepStatus) -> PlanItemArg {
    PlanItemArg {
        step: step.to_string(),
        status,
    }
}

fn update(explanation: Option<&str>, plan: Vec<PlanItemArg>) -> UpdatePlanArgs {
    UpdatePlanArgs {
        explanation: explanation.map(str::to_string),
        plan,
    }
}

fn task_keymap(binding: &str) -> TuiKeymap {
    toml::from_str(&format!("[global]\ntoggle_task_list = {binding}\n"))
        .expect("parse task-list keymap")
}

async fn running_chat() -> (
    ChatWidget,
    tokio::sync::mpsc::UnboundedReceiver<AppEvent>,
    tokio::sync::mpsc::UnboundedReceiver<Op>,
) {
    let (mut chat, event_rx, op_rx) = make_chatwidget_manual(/*model_override*/ None).await;
    chat.set_keep_in_progress_tasks_visible(/*enabled*/ true);
    chat.turn_lifecycle.start(Instant::now());
    (chat, event_rx, op_rx)
}

#[tokio::test]
async fn constructor_honors_enabled_task_list_config() {
    let (event_tx, _event_rx) = unbounded_channel::<AppEvent>();
    let mut config = test_config().await;
    config.tui_keep_in_progress_tasks_visible = true;
    let resolved_model = get_model_offline_for_tests(config.model.as_deref());
    let session_telemetry = test_session_telemetry(&config, resolved_model.as_str());
    let init = ChatWidgetInit {
        config: config.clone(),
        frame_requester: FrameRequester::test_dummy(),
        app_event_tx: AppEventSender::new(event_tx),
        workspace_command_runner: None,
        initial_user_message: None,
        enhanced_keys_supported: false,
        has_chatgpt_account: false,
        has_codex_backend_auth: false,
        model_catalog: test_model_catalog(&config),
        feedback: codex_feedback::CodexFeedback::new(),
        is_first_run: true,
        status_account_display: None,
        runtime_model_provider_base_url: None,
        initial_plan_type: None,
        model: Some(resolved_model),
        startup_tooltip_override: None,
        status_line_invalid_items_warned: Arc::new(AtomicBool::new(false)),
        terminal_title_invalid_items_warned: Arc::new(AtomicBool::new(false)),
        session_telemetry,
    };
    let mut chat = ChatWidget::new_with_app_event(init);

    chat.on_plan_update(update(
        /*explanation*/ None,
        vec![task("Configured task", StepStatus::InProgress)],
    ));

    assert!(chat.task_list_panel.is_visible());
}

fn render_task_panel(chat: &ChatWidget, width: u16) -> String {
    let renderable = chat
        .task_list_panel
        .as_renderable(/*right_reserved_cols*/ 0);
    let height = renderable.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    renderable.render(area, &mut buffer);
    (0..height)
        .map(|row| {
            (0..width)
                .map(|column| buffer[(column, row)].symbol())
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

#[tokio::test]
async fn plan_update_retains_the_panel_and_preserves_the_full_transcript_entry() {
    let (mut chat, mut event_rx, _op_rx) = running_chat().await;
    chat.on_plan_update(update(
        Some("Adapting plan"),
        vec![
            task("Explore codebase", StepStatus::Completed),
            task("Implement feature", StepStatus::InProgress),
            task("Write tests", StepStatus::Pending),
        ],
    ));

    assert!(chat.task_list_panel.is_visible());
    assert_snapshot!(
        "task_list_chatwidget_retained_panel",
        render_task_panel(&chat, /*width*/ 48)
    );

    let cells = drain_insert_history(&mut event_rx);
    let transcript = lines_to_single_string(cells.last().expect("plan update history cell"));
    assert_snapshot!("task_list_plan_update_transcript", transcript);
}

#[tokio::test]
async fn panel_text_cap_does_not_truncate_the_transcript_entry() {
    let (mut chat, mut event_rx, _op_rx) = running_chat().await;
    let full_step = format!("{} transcript tail", "x".repeat(300));

    chat.on_plan_update(update(
        /*explanation*/ None,
        vec![task(&full_step, StepStatus::InProgress)],
    ));

    assert!(chat.toggle_task_list());
    assert!(!render_task_panel(&chat, /*width*/ 400).contains("transcript tail"));
    let cells = drain_insert_history(&mut event_rx);
    let transcript = lines_to_single_string(cells.last().expect("plan update history cell"));
    assert!(transcript.contains("transcript tail"));
}

#[tokio::test]
async fn successful_turn_completion_clears_a_completed_panel_only() {
    let (mut chat, _event_rx, _op_rx) = running_chat().await;
    chat.on_plan_update(update(
        /*explanation*/ None,
        vec![
            task("Implement feature", StepStatus::Completed),
            task("Write tests", StepStatus::Completed),
        ],
    ));
    assert!(chat.task_list_panel.is_visible());

    chat.on_task_complete(
        /*last_agent_message*/ None, /*duration_ms*/ None, /*from_replay*/ false,
    );

    assert!(!chat.task_list_panel.is_visible());
    assert_eq!(chat.transcript.last_plan_progress, Some((2, 2)));
}

#[tokio::test]
async fn interrupted_or_failed_turn_clears_a_completed_panel() {
    let (mut chat, _event_rx, _op_rx) = running_chat().await;
    chat.on_plan_update(update(
        /*explanation*/ None,
        vec![task("Finish work", StepStatus::Completed)],
    ));
    assert!(chat.task_list_panel.is_visible());

    chat.finalize_turn();

    assert!(!chat.task_list_panel.is_visible());
    assert_eq!(chat.transcript.last_plan_progress, Some((1, 1)));
}

#[tokio::test]
async fn incomplete_panel_survives_turn_completion_and_the_next_turn_start() {
    let (mut chat, _event_rx, _op_rx) = running_chat().await;
    chat.on_plan_update(update(
        /*explanation*/ None,
        vec![
            task("Finished work", StepStatus::Completed),
            task("Remaining work", StepStatus::Pending),
        ],
    ));

    chat.on_task_complete(
        /*last_agent_message*/ None, /*duration_ms*/ None, /*from_replay*/ false,
    );
    assert!(chat.task_list_panel.is_visible());

    chat.turn_lifecycle.start(Instant::now());
    assert!(chat.task_list_panel.is_visible());
    assert!(render_task_panel(&chat, /*width*/ 48).contains("Remaining work"));
}

#[tokio::test]
async fn later_and_empty_updates_replace_then_clear_the_panel() {
    let (mut chat, _event_rx, _op_rx) = running_chat().await;
    chat.on_plan_update(update(
        /*explanation*/ None,
        vec![task("Old task", StepStatus::InProgress)],
    ));

    chat.on_plan_update(update(
        /*explanation*/ None,
        vec![task("Replacement task", StepStatus::Pending)],
    ));
    let replacement = render_task_panel(&chat, /*width*/ 48);
    assert!(replacement.contains("Replacement task"));
    assert!(!replacement.contains("Old task"));

    chat.on_plan_update(update(/*explanation*/ None, Vec::new()));

    assert!(!chat.task_list_panel.is_visible());
    assert_eq!(chat.transcript.last_plan_progress, None);
}

#[tokio::test]
async fn task_list_toggle_changes_only_the_visible_panel_presentation() {
    let (mut chat, _event_rx, _op_rx) = running_chat().await;
    chat.bottom_pane
        .set_composer_text("keep this draft".to_string(), Vec::new(), Vec::new());
    chat.on_plan_update(update(
        /*explanation*/ None,
        vec![
            task("Current work", StepStatus::InProgress),
            task("Next work", StepStatus::Pending),
        ],
    ));
    let draft = chat.composer_text_with_pending();
    assert!(render_task_panel(&chat, /*width*/ 48).contains("expand"));

    assert!(chat.toggle_task_list());

    assert!(render_task_panel(&chat, /*width*/ 48).contains("collapse"));
    assert_eq!(chat.composer_text_with_pending(), draft);

    chat.set_keep_in_progress_tasks_visible(/*enabled*/ false);
    assert!(!chat.toggle_task_list());
    assert_eq!(chat.composer_text_with_pending(), draft);
}

#[tokio::test]
async fn unavailable_task_list_shortcut_leaves_the_composer_unchanged() {
    let (mut chat, _event_rx, _op_rx) = running_chat().await;
    chat.bottom_pane
        .set_composer_text("keep this draft".to_string(), Vec::new(), Vec::new());
    let draft = chat.composer_text_with_pending();

    assert!(!chat.toggle_task_list());
    chat.handle_key_event(KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT));

    assert_eq!(chat.composer_text_with_pending(), draft);
}

#[tokio::test]
async fn tasks_command_opens_during_a_running_turn_without_discarding_the_plan() {
    let (mut chat, _event_rx, _op_rx) = running_chat().await;
    chat.on_plan_update(update(
        /*explanation*/ None,
        vec![task("Current work", StepStatus::InProgress)],
    ));

    chat.dispatch_command_with_args(SlashCommand::Tasks, String::new(), Vec::new());

    assert!(!chat.bottom_pane.no_modal_or_popup_active());
    assert!(chat.task_list_panel.is_visible());
}

#[tokio::test]
async fn committed_keymap_edits_refresh_or_remove_the_panel_hint() {
    let (mut chat, _event_rx, _op_rx) = running_chat().await;
    chat.on_plan_update(update(
        /*explanation*/ None,
        vec![task("Current work", StepStatus::InProgress)],
    ));

    let keymap = task_keymap("\"f12\"");
    let runtime = RuntimeKeymap::from_config(&keymap).expect("remapped keymap");
    chat.apply_keymap_update(keymap, &runtime);
    assert!(
        render_task_panel(&chat, /*width*/ 48)
            .lines()
            .next()
            .expect("task heading")
            .contains("f12 expand")
    );
    assert!(chat.toggle_task_list());
    assert!(render_task_panel(&chat, /*width*/ 48).contains("f12 collapse"));

    let keymap = task_keymap("[]");
    let runtime = RuntimeKeymap::from_config(&keymap).expect("unbound keymap");
    chat.apply_keymap_update(keymap, &runtime);
    assert_eq!(
        render_task_panel(&chat, /*width*/ 48)
            .lines()
            .next()
            .expect("task heading"),
        "Tasks 0/1"
    );

    let keymap = task_keymap("\"f12\"");
    let runtime = RuntimeKeymap::from_config(&keymap).expect("rebound keymap");
    chat.apply_keymap_update(keymap, &runtime);
    assert!(render_task_panel(&chat, /*width*/ 48).contains("f12 expand"));
}
