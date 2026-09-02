use super::*;
use crate::bottom_pane::BottomPaneView;
use crate::render::renderable::Renderable;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use insta::assert_snapshot;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::unbounded_channel;

fn view_with_events(
    keep_in_progress_tasks_visible: bool,
) -> (MultiSelectPicker, UnboundedReceiver<AppEvent>) {
    let (tx, rx) = unbounded_channel();
    let view = tasks_settings_view(
        keep_in_progress_tasks_visible,
        AppEventSender::new(tx),
        crate::keymap::RuntimeKeymap::defaults().list,
    );
    (view, rx)
}

fn render_view(view: &MultiSelectPicker, width: u16) -> String {
    let height = view.desired_height(width);
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    view.render(area, &mut buffer);
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

#[test]
fn tasks_settings_view_snapshot() {
    let (view, _rx) = view_with_events(/*keep_in_progress_tasks_visible*/ false);

    assert_snapshot!("tasks_settings_view", render_view(&view, /*width*/ 100));
}

#[test]
fn confirming_the_tasks_setting_emits_the_selected_value() {
    let (mut view, mut rx) = view_with_events(/*keep_in_progress_tasks_visible*/ false);

    view.handle_key_event(KeyEvent::from(KeyCode::Char(' ')));
    view.handle_key_event(KeyEvent::from(KeyCode::Enter));

    loop {
        match rx.try_recv().expect("tasks setting event") {
            AppEvent::TaskListSettingsUpdated {
                keep_in_progress_tasks_visible,
            } => {
                assert!(keep_in_progress_tasks_visible);
                break;
            }
            _ => continue,
        }
    }
}

#[test]
fn cancelling_the_tasks_setting_emits_no_update() {
    let (mut view, mut rx) = view_with_events(/*keep_in_progress_tasks_visible*/ true);

    view.handle_key_event(KeyEvent::from(KeyCode::Esc));

    while let Ok(event) = rx.try_recv() {
        assert!(!matches!(event, AppEvent::TaskListSettingsUpdated { .. }));
    }
}
