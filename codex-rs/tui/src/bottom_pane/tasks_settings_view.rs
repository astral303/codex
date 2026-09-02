//! Settings view for the persistent task list.

use crate::app_event::AppEvent;
use crate::app_event_sender::AppEventSender;
use crate::bottom_pane::multi_select_picker::MultiSelectItem;
use crate::bottom_pane::multi_select_picker::MultiSelectPicker;
use crate::keymap::ListKeymap;

const KEEP_IN_PROGRESS_TASKS_VISIBLE_ID: &str = "keep-in-progress-tasks-visible";
const KEEP_IN_PROGRESS_TASKS_VISIBLE_LABEL: &str = "Keep in-progress tasks visible";

pub(crate) fn tasks_settings_view(
    keep_in_progress_tasks_visible: bool,
    app_event_tx: AppEventSender,
    list_keymap: ListKeymap,
) -> MultiSelectPicker {
    MultiSelectPicker::builder(
        "Tasks".to_string(),
        Some("Configure the task list shown above the prompt.".to_string()),
        app_event_tx,
    )
    .list_keymap(list_keymap)
    // Keep the required full label visible even though it exceeds the picker's default limit.
    .item_name_truncate_len(KEEP_IN_PROGRESS_TASKS_VISIBLE_LABEL.len())
    .items(vec![MultiSelectItem {
        id: KEEP_IN_PROGRESS_TASKS_VISIBLE_ID.to_string(),
        name: KEEP_IN_PROGRESS_TASKS_VISIBLE_LABEL.to_string(),
        description: Some("Pin the latest unfinished plan above the prompt.".to_string()),
        enabled: keep_in_progress_tasks_visible,
        orderable: false,
        section_break_after: false,
    }])
    .on_confirm(|ids, app_event_tx| {
        app_event_tx.send(AppEvent::TaskListSettingsUpdated {
            keep_in_progress_tasks_visible: ids
                .iter()
                .any(|id| id == KEEP_IN_PROGRESS_TASKS_VISIBLE_ID),
        });
    })
    .build()
}

#[cfg(test)]
#[path = "tasks_settings_view_tests.rs"]
mod tests;
