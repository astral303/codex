use super::App;
use super::AppEvent;
use super::AppServerSession;
use super::Result;
use super::TuiEvent;
use super::make_chatwidget_manual_with_sender;
use super::make_test_app;
use super::make_test_app_with_channels;
use super::start_config_write_test_app_server;
use crate::chatwidget::ChatWidget;
use crate::keymap::RuntimeKeymap;
use crate::render::renderable::Renderable;
use codex_app_server_protocol::ServerNotification;
use codex_app_server_protocol::TurnPlanStep;
use codex_app_server_protocol::TurnPlanStepStatus;
use codex_app_server_protocol::TurnPlanUpdatedNotification;
use codex_config::ConfigLayerStack;
use codex_config::types::KeybindingSpec;
use codex_config::types::KeybindingsSpec;
use codex_config::types::TuiKeymap;
use codex_utils_absolute_path::test_support::PathExt;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use tempfile::tempdir;
use toml::Value as TomlValue;

fn plan_update(step: &str) -> ServerNotification {
    ServerNotification::TurnPlanUpdated(TurnPlanUpdatedNotification {
        thread_id: "thread-1".to_string(),
        turn_id: "turn-1".to_string(),
        explanation: None,
        plan: vec![TurnPlanStep {
            step: step.to_string(),
            status: TurnPlanStepStatus::InProgress,
        }],
    })
}

fn render_chat_widget(chat_widget: &ChatWidget) -> String {
    let area = Rect::new(0, 0, /*width*/ 80, /*height*/ 24);
    let mut buffer = Buffer::empty(area);
    chat_widget.as_renderable().render(area, &mut buffer);
    buffer
        .content
        .chunks(usize::from(area.width))
        .map(|row| {
            row.iter()
                .map(ratatui::buffer::Cell::symbol)
                .collect::<String>()
                .trim_end()
                .to_string()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

fn task_heading(rendered: &str) -> Option<&str> {
    rendered
        .lines()
        .find(|line| line.trim_start().starts_with("Tasks "))
}

fn apply_task_list_binding(app: &mut App, binding: KeybindingsSpec) -> Result<()> {
    let mut keymap = TuiKeymap::default();
    keymap.global.toggle_task_list = Some(binding);
    let runtime =
        RuntimeKeymap::from_config(&keymap).map_err(|error| color_eyre::eyre::eyre!(error))?;
    app.chat_widget.apply_keymap_update(keymap, &runtime);
    app.keymap = runtime;
    Ok(())
}

async fn press(
    app: &mut App,
    tui: &mut crate::tui::Tui,
    app_server: &mut AppServerSession,
    key_event: KeyEvent,
) -> Result<()> {
    app.handle_tui_event(tui, app_server, TuiEvent::Key(key_event))
        .await
        .map(|_| ())
}

#[tokio::test]
async fn task_list_setting_persists_and_updates_both_runtime_configs() -> Result<()> {
    let (mut app, _events, _op_rx) = make_test_app_with_channels().await;
    let codex_home = tempdir()?;
    app.config.codex_home = codex_home.path().abs();
    app.config.config_layer_stack = ConfigLayerStack::default();
    let config_path = codex_home.path().join("config.toml");
    std::fs::write(&config_path, "[tui]\nraw_output_mode = true\n")?;
    let mut app_server = start_config_write_test_app_server(&app).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;

    app.handle_event(
        &mut tui,
        &mut app_server,
        AppEvent::TaskListSettingsUpdated {
            keep_in_progress_tasks_visible: true,
        },
    )
    .await?;
    app.chat_widget
        .handle_server_notification(plan_update("Persisted task"), /*replay_kind*/ None);

    assert!(app.config.tui_keep_in_progress_tasks_visible);
    assert!(
        app.chat_widget
            .config_ref()
            .tui_keep_in_progress_tasks_visible
    );
    assert!(render_chat_widget(&app.chat_widget).contains("Persisted task"));
    let actual: TomlValue = toml::from_str(&std::fs::read_to_string(config_path)?)?;
    let expected: TomlValue =
        toml::from_str("[tui]\nraw_output_mode = true\nkeep_in_progress_tasks_visible = true\n")?;
    assert_eq!(actual, expected);

    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn failed_task_list_setting_write_keeps_runtime_state_and_reports_error() -> Result<()> {
    let (mut app, mut events, _op_rx) = make_test_app_with_channels().await;
    let mut app_server = start_config_write_test_app_server(&app).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;
    while events.try_recv().is_ok() {}
    let temp_dir = tempdir()?;
    let blocked_home = temp_dir.path().join("not-a-directory");
    std::fs::write(&blocked_home, "block config directory creation")?;
    app.config.codex_home = blocked_home.abs();
    app.config.config_layer_stack = ConfigLayerStack::default();

    app.handle_event(
        &mut tui,
        &mut app_server,
        AppEvent::TaskListSettingsUpdated {
            keep_in_progress_tasks_visible: true,
        },
    )
    .await?;

    assert!(!app.config.tui_keep_in_progress_tasks_visible);
    assert!(
        !app.chat_widget
            .config_ref()
            .tui_keep_in_progress_tasks_visible
    );
    let error = loop {
        match events.try_recv() {
            Ok(AppEvent::InsertHistoryCell(cell)) => {
                let text = cell
                    .display_lines(/*width*/ 120)
                    .into_iter()
                    .map(|line| line.to_string())
                    .collect::<Vec<_>>()
                    .join("\n");
                if text.contains("Failed to save task list setting") {
                    break text;
                }
            }
            Ok(_) => continue,
            Err(err) => panic!("expected task-list persistence error, got {err}"),
        }
    };
    assert!(error.contains("not-a-directory"));

    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn task_list_shortcut_routes_default_remapped_unbound_and_modal_states() -> Result<()> {
    let mut app = make_test_app().await;
    app.chat_widget
        .set_keep_in_progress_tasks_visible(/*enabled*/ true);
    app.chat_widget
        .handle_server_notification(plan_update("Routed task"), /*replay_kind*/ None);
    app.chat_widget.insert_str("keep this draft");
    let draft = app.chat_widget.composer_text_with_pending();
    let mut app_server = start_config_write_test_app_server(&app).await?;
    let mut tui = crate::tui::test_support::make_test_tui()?;

    let rendered = render_chat_widget(&app.chat_widget);
    assert!(task_heading(&rendered).is_some_and(|heading| heading.ends_with("expand")));
    press(
        &mut app,
        &mut tui,
        &mut app_server,
        KeyEvent::new(KeyCode::Char('p'), KeyModifiers::ALT),
    )
    .await?;
    let rendered = render_chat_widget(&app.chat_widget);
    assert!(task_heading(&rendered).is_some_and(|heading| heading.ends_with("collapse")));

    apply_task_list_binding(
        &mut app,
        KeybindingsSpec::One(KeybindingSpec("f12".to_string())),
    )?;
    press(
        &mut app,
        &mut tui,
        &mut app_server,
        KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE),
    )
    .await?;
    let rendered = render_chat_widget(&app.chat_widget);
    assert_eq!(task_heading(&rendered), Some("Tasks 0/1 · f12 expand"));

    let keymap = app.keymap.clone();
    app.chat_widget.open_keymap_debug(&keymap);
    assert!(task_heading(&render_chat_widget(&app.chat_widget)).is_none());
    press(
        &mut app,
        &mut tui,
        &mut app_server,
        KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE),
    )
    .await?;
    press(
        &mut app,
        &mut tui,
        &mut app_server,
        KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL),
    )
    .await?;
    let rendered = render_chat_widget(&app.chat_widget);
    assert_eq!(task_heading(&rendered), Some("Tasks 0/1 · f12 expand"));

    apply_task_list_binding(&mut app, KeybindingsSpec::Many(Vec::new()))?;
    press(
        &mut app,
        &mut tui,
        &mut app_server,
        KeyEvent::new(KeyCode::F(12), KeyModifiers::NONE),
    )
    .await?;
    let rendered = render_chat_widget(&app.chat_widget);
    assert_eq!(task_heading(&rendered), Some("Tasks 0/1"));
    assert_eq!(app.chat_widget.composer_text_with_pending(), draft);

    app_server.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn replacing_the_chat_widget_cannot_leak_the_previous_thread_plan() {
    let mut app = make_test_app().await;
    app.chat_widget
        .set_keep_in_progress_tasks_visible(/*enabled*/ true);
    app.chat_widget
        .handle_server_notification(plan_update("Old thread task"), /*replay_kind*/ None);
    assert!(render_chat_widget(&app.chat_widget).contains("Old thread task"));

    let (mut replacement, _event_tx, _events, _operations) =
        make_chatwidget_manual_with_sender().await;
    replacement.set_keep_in_progress_tasks_visible(/*enabled*/ true);
    replacement.handle_server_notification(
        plan_update("Replacement thread task"),
        /*replay_kind*/ None,
    );

    app.replace_chat_widget(replacement);

    let rendered = render_chat_widget(&app.chat_widget);
    assert!(rendered.contains("Replacement thread task"));
    assert!(!rendered.contains("Old thread task"));
}
