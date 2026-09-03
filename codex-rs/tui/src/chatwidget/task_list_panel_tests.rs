use super::*;
use crate::key_hint::KeyBinding;
use crate::render::renderable::Renderable;
use crossterm::event::KeyCode;
use crossterm::event::KeyModifiers;
use insta::assert_snapshot;
use pretty_assertions::assert_eq;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Color;

/// Needs more than two wrapped lines at the widths the panel tests render.
const CLIPPED_TASK: &str = "A task whose text runs long enough to need more than the two lines the compact panel allows before it clips";

fn task(step: usize, status: StepStatus) -> PlanItemArg {
    PlanItemArg {
        step: format!("Task {step}"),
        status,
    }
}

fn clipped_task_panel() -> TaskListPanel {
    visible_panel(vec![PlanItemArg {
        step: CLIPPED_TASK.to_string(),
        status: StepStatus::InProgress,
    }])
}

fn plan(statuses: Vec<StepStatus>) -> Vec<PlanItemArg> {
    statuses
        .into_iter()
        .enumerate()
        .map(|(index, status)| task(index, status))
        .collect()
}

fn selected_steps(plan: &[PlanItemArg], window: TaskWindow) -> Vec<&str> {
    plan[window.start..window.end]
        .iter()
        .map(|item| item.step.as_str())
        .collect()
}

fn plan_value(plan: &[PlanItemArg]) -> serde_json::Value {
    serde_json::to_value(plan).expect("serialize plan")
}

fn retained_plan_value(panel: &TaskListPanel) -> serde_json::Value {
    panel
        .plan
        .as_ref()
        .map_or_else(|| serde_json::json!([]), |plan| plan_value(&plan.items))
}

fn visible_panel(plan: Vec<PlanItemArg>) -> TaskListPanel {
    let shortcut_hint = KeyBinding::new(KeyCode::Char('p'), KeyModifiers::ALT).into();
    let mut panel = TaskListPanel::new(/*render_enabled*/ true, Some(shortcut_hint));
    panel.replace_plan(plan, TaskListTurnState::Running);
    panel
}

fn render_panel(
    panel: &TaskListPanel,
    width: u16,
    height: u16,
    right_reserved_cols: u16,
) -> String {
    let buffer = render_panel_buffer(panel, width, height, right_reserved_cols);
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

fn render_panel_buffer(
    panel: &TaskListPanel,
    width: u16,
    height: u16,
    right_reserved_cols: u16,
) -> Buffer {
    let area = Rect::new(0, 0, width, height);
    let mut buffer = Buffer::empty(area);
    panel
        .as_renderable(right_reserved_cols)
        .render(area, &mut buffer);
    buffer
}

#[test]
fn compact_window_centers_one_current_task_with_two_rows_on_each_side() {
    let plan = plan(vec![
        StepStatus::Completed,
        StepStatus::Completed,
        StepStatus::Completed,
        StepStatus::InProgress,
        StepStatus::Pending,
        StepStatus::Pending,
        StepStatus::Pending,
    ]);

    let window = select_task_window(&plan, DEFAULT_COMPACT_TASK_ROWS);

    assert_eq!(window, TaskWindow { start: 1, end: 6 });
    assert_eq!(
        selected_steps(&plan, window),
        vec!["Task 1", "Task 2", "Task 3", "Task 4", "Task 5"]
    );
}

#[test]
fn compact_window_uses_one_previous_row_for_multiple_adjacent_current_tasks() {
    let plan = plan(vec![
        StepStatus::Completed,
        StepStatus::Completed,
        StepStatus::InProgress,
        StepStatus::InProgress,
        StepStatus::InProgress,
        StepStatus::Pending,
        StepStatus::Pending,
    ]);

    let window = select_task_window(&plan, DEFAULT_COMPACT_TASK_ROWS);

    assert_eq!(window, TaskWindow { start: 1, end: 6 });
    assert_eq!(
        selected_steps(&plan, window),
        vec!["Task 1", "Task 2", "Task 3", "Task 4", "Task 5"]
    );
}

#[test]
fn compact_window_stays_contiguous_when_current_tasks_are_separated() {
    let plan = plan(vec![
        StepStatus::Completed,
        StepStatus::Completed,
        StepStatus::InProgress,
        StepStatus::Pending,
        StepStatus::Pending,
        StepStatus::Pending,
        StepStatus::InProgress,
    ]);

    let window = select_task_window(&plan, DEFAULT_COMPACT_TASK_ROWS);

    assert_eq!(window, TaskWindow { start: 1, end: 6 });
    assert_eq!(
        selected_steps(&plan, window),
        vec!["Task 1", "Task 2", "Task 3", "Task 4", "Task 5"]
    );
}

#[test]
fn compact_window_anchors_the_first_pending_task_when_none_are_current() {
    let plan = plan(vec![
        StepStatus::Completed,
        StepStatus::Completed,
        StepStatus::Completed,
        StepStatus::Pending,
        StepStatus::Pending,
        StepStatus::Pending,
    ]);

    assert_eq!(
        select_task_window(&plan, DEFAULT_COMPACT_TASK_ROWS),
        TaskWindow { start: 1, end: 6 }
    );
}

#[test]
fn compact_window_shows_the_last_completed_rows_when_every_task_is_done() {
    let plan = plan(vec![StepStatus::Completed; 7]);

    assert_eq!(
        select_task_window(&plan, DEFAULT_COMPACT_TASK_ROWS),
        TaskWindow { start: 2, end: 7 }
    );
}

#[test]
fn compact_window_backfills_at_plan_boundaries() {
    let current_at_start = plan(vec![
        StepStatus::InProgress,
        StepStatus::Pending,
        StepStatus::Pending,
        StepStatus::Pending,
        StepStatus::Pending,
        StepStatus::Pending,
        StepStatus::Pending,
    ]);
    let current_at_end = plan(vec![
        StepStatus::Completed,
        StepStatus::Completed,
        StepStatus::Completed,
        StepStatus::Completed,
        StepStatus::Completed,
        StepStatus::Completed,
        StepStatus::InProgress,
    ]);

    assert_eq!(
        select_task_window(&current_at_start, DEFAULT_COMPACT_TASK_ROWS),
        TaskWindow { start: 0, end: 5 }
    );
    assert_eq!(
        select_task_window(&current_at_end, DEFAULT_COMPACT_TASK_ROWS),
        TaskWindow { start: 2, end: 7 }
    );
}

#[test]
fn compact_window_handles_every_capacity_as_one_contiguous_range() {
    let plan = plan(vec![
        StepStatus::Completed,
        StepStatus::Completed,
        StepStatus::Completed,
        StepStatus::InProgress,
        StepStatus::Pending,
        StepStatus::Pending,
        StepStatus::Pending,
    ]);
    let expected = [
        TaskWindow { start: 0, end: 0 },
        TaskWindow { start: 3, end: 4 },
        TaskWindow { start: 3, end: 5 },
        TaskWindow { start: 3, end: 6 },
        TaskWindow { start: 2, end: 6 },
        TaskWindow { start: 1, end: 6 },
        TaskWindow { start: 1, end: 7 },
        TaskWindow { start: 0, end: 7 },
    ];

    let actual = (0..=plan.len())
        .map(|capacity| select_task_window(&plan, capacity))
        .collect::<Vec<_>>();

    assert_eq!(actual, expected);
    for (capacity, window) in actual.into_iter().enumerate() {
        assert_eq!(window.len(), capacity.min(plan.len()));
        assert!(window.start <= window.end);
        assert!(window.end <= plan.len());
        if capacity > 0 {
            assert!(window.start <= 3 && 3 < window.end);
        }
    }
}

#[test]
fn compact_window_is_empty_for_empty_input_or_zero_capacity() {
    let empty = Vec::new();
    let non_empty = plan(vec![StepStatus::InProgress]);

    assert_eq!(select_task_window(&empty, 5), TaskWindow::default());
    assert_eq!(select_task_window(&non_empty, 0), TaskWindow::default());
}

#[test]
fn panel_retains_an_incomplete_plan_while_rendering_is_disabled() {
    let retained_plan = plan(vec![StepStatus::InProgress, StepStatus::Pending]);
    let mut panel = TaskListPanel::new(/*render_enabled*/ false, None);

    panel.replace_plan(retained_plan.clone(), TaskListTurnState::Running);
    assert!(!panel.is_visible());

    panel.set_render_enabled(true);
    assert!(panel.is_visible());
    assert_eq!(retained_plan_value(&panel), plan_value(&retained_plan));
}

#[test]
fn panel_clears_a_completed_plan_when_its_turn_finishes() {
    let mut panel = TaskListPanel::new(/*render_enabled*/ true, None);
    panel.replace_plan(
        plan(vec![StepStatus::Completed, StepStatus::Completed]),
        TaskListTurnState::Running,
    );

    assert!(panel.is_visible());
    assert!(panel.toggle_expanded());

    panel.on_turn_finished();

    assert!(!panel.is_visible());
    assert_eq!(retained_plan_value(&panel), serde_json::json!([]));
    assert_eq!(panel.presentation, TaskListPresentation::Compact);
}

#[test]
fn panel_immediately_clears_a_completed_idle_update() {
    let mut panel = TaskListPanel::new(/*render_enabled*/ true, None);

    panel.replace_plan(plan(vec![StepStatus::Completed]), TaskListTurnState::Idle);

    assert!(!panel.is_visible());
    assert_eq!(retained_plan_value(&panel), serde_json::json!([]));
}

#[test]
fn panel_keeps_incomplete_work_after_the_turn_finishes() {
    let retained_plan = plan(vec![StepStatus::Completed, StepStatus::Pending]);
    let mut panel = TaskListPanel::new(/*render_enabled*/ true, None);
    panel.replace_plan(retained_plan.clone(), TaskListTurnState::Running);

    panel.on_turn_finished();

    assert!(panel.is_visible());
    assert_eq!(retained_plan_value(&panel), plan_value(&retained_plan));
}

#[test]
fn panel_replaces_the_previous_plan_and_empty_updates_clear_it() {
    let mut panel = TaskListPanel::new(/*render_enabled*/ true, None);
    panel.replace_plan(
        plan(vec![StepStatus::InProgress, StepStatus::Pending]),
        TaskListTurnState::Running,
    );
    assert!(panel.toggle_expanded());

    let replacement = plan(vec![StepStatus::Pending]);
    panel.replace_plan(replacement.clone(), TaskListTurnState::Running);

    assert_eq!(retained_plan_value(&panel), plan_value(&replacement));
    assert_eq!(panel.presentation, TaskListPresentation::Expanded);

    panel.replace_plan(Vec::new(), TaskListTurnState::Running);

    assert_eq!(retained_plan_value(&panel), serde_json::json!([]));
    assert_eq!(panel.presentation, TaskListPresentation::Compact);
}

#[test]
fn panel_bounds_retained_entries_and_task_text_without_losing_source_counts() {
    let mut source_plan = (0..150)
        .map(|index| task(index, StepStatus::Pending))
        .collect::<Vec<_>>();
    source_plan[120] = PlanItemArg {
        step: "x".repeat(MAX_TASK_STEP_GRAPHEMES + 1),
        status: StepStatus::InProgress,
    };
    let mut panel = visible_panel(source_plan);

    let retained = panel.plan.as_ref().expect("retained display plan");
    assert_eq!(retained.items.len(), MAX_TASK_LIST_ENTRIES);
    assert_eq!(retained.source_start, 50);
    assert_eq!(retained.total_items, 150);
    assert_eq!(retained.completed_items, 0);
    let current = retained
        .items
        .iter()
        .find(|item| matches!(&item.status, StepStatus::InProgress))
        .expect("current item remains in the bounded source window");
    assert_eq!(current.step.chars().count(), MAX_TASK_STEP_GRAPHEMES);
    assert!(current.step.ends_with("..."));

    assert!(panel.toggle_expanded());
    let height = panel.desired_height(/*width*/ 400);
    let rendered = render_panel(
        &panel, /*width*/ 400, height, /*right_reserved_cols*/ 0,
    );
    assert!(rendered.starts_with("Tasks 0/150 · 50 hidden"));
}

#[test]
fn removing_the_last_shortcut_collapses_an_expanded_panel() {
    let mut panel = visible_panel(plan(vec![StepStatus::InProgress, StepStatus::Pending]));
    assert!(panel.toggle_expanded());

    panel.set_shortcut_hint(None);

    assert_eq!(panel.presentation, TaskListPresentation::Compact);
    assert_eq!(panel.shortcut_hint, None);
}

#[test]
fn fully_reserved_width_consumes_no_panel_height() {
    let panel = visible_panel(plan(vec![StepStatus::InProgress]));

    assert_eq!(
        panel
            .as_renderable(/*right_reserved_cols*/ 20)
            .desired_height(/*width*/ 20),
        0
    );
}

#[test]
fn task_statuses_render_markers_and_text_with_distinct_styles() {
    let panel = visible_panel(plan(vec![
        StepStatus::Completed,
        StepStatus::InProgress,
        StepStatus::Pending,
    ]));
    let buffer = render_panel_buffer(
        &panel, /*width*/ 40, /*height*/ 4, /*right_reserved_cols*/ 0,
    );

    let actual = [1, 2, 3].map(|row| (buffer[(3, row)].style(), buffer[(5, row)].style()));
    let base_style = Style::default()
        .fg(Color::Reset)
        .bg(Color::Reset)
        .underline_color(Color::Reset);
    let expected = [
        (base_style.dim(), base_style.crossed_out().dim()),
        (base_style, base_style.cyan().bold()),
        (base_style, base_style.dim()),
    ];
    assert_eq!(actual, expected);
}

#[test]
fn compact_panel_renders_five_rows_and_both_directional_gutters() {
    let panel = visible_panel(plan(vec![
        StepStatus::Completed,
        StepStatus::Completed,
        StepStatus::Completed,
        StepStatus::InProgress,
        StepStatus::Pending,
        StepStatus::Pending,
        StepStatus::Pending,
    ]));

    assert_snapshot!(
        "task_list_panel_compact_five_rows",
        render_panel(
            &panel, /*width*/ 48, /*height*/ 6, /*right_reserved_cols*/ 0
        )
    );
}

#[test]
fn compact_panel_keeps_a_continuous_multi_current_window() {
    let panel = visible_panel(plan(vec![
        StepStatus::Completed,
        StepStatus::Completed,
        StepStatus::InProgress,
        StepStatus::InProgress,
        StepStatus::Pending,
        StepStatus::Pending,
        StepStatus::InProgress,
    ]));

    assert_snapshot!(
        "task_list_panel_compact_multi_current",
        render_panel(
            &panel, /*width*/ 48, /*height*/ 6, /*right_reserved_cols*/ 0
        )
    );
}

#[test]
fn compact_panel_uses_the_combined_gutter_when_only_one_task_fits() {
    let panel = visible_panel(plan(vec![
        StepStatus::Completed,
        StepStatus::Completed,
        StepStatus::Completed,
        StepStatus::InProgress,
        StepStatus::Pending,
        StepStatus::Pending,
        StepStatus::Pending,
    ]));

    assert_snapshot!(
        "task_list_panel_one_row_both_directions",
        render_panel(
            &panel, /*width*/ 32, /*height*/ 2, /*right_reserved_cols*/ 0
        )
    );
}

#[test]
fn compact_panel_wraps_a_task_to_two_lines_and_clips_the_remainder() {
    let panel = clipped_task_panel();

    let rendered = render_panel(
        &panel, /*width*/ 40, /*height*/ 6, /*right_reserved_cols*/ 0,
    );
    let task_lines = rendered
        .lines()
        .skip(1)
        .filter(|line| !line.is_empty())
        .collect::<Vec<_>>();

    assert_eq!(task_lines.len(), COMPACT_TASK_LINE_LIMIT, "{rendered}");
    assert!(task_lines[1].ends_with('…'), "{rendered}");
}

#[test]
fn heading_omits_the_shortcut_when_every_task_fits() {
    let panel = visible_panel(plan(vec![StepStatus::InProgress, StepStatus::Pending]));

    let rendered = render_panel(
        &panel, /*width*/ 40, /*height*/ 3, /*right_reserved_cols*/ 0,
    );

    assert_eq!(rendered.lines().next(), Some("Tasks 0/2"));
}

#[test]
fn heading_offers_the_shortcut_when_a_task_is_clipped() {
    let panel = clipped_task_panel();

    let rendered = render_panel(
        &panel, /*width*/ 40, /*height*/ 6, /*right_reserved_cols*/ 0,
    );

    assert!(
        rendered
            .lines()
            .next()
            .is_some_and(|heading| heading.ends_with("expand")),
        "{rendered}"
    );
}

#[test]
fn heading_offers_the_shortcut_when_tasks_are_hidden() {
    let panel = visible_panel(plan(vec![
        StepStatus::Completed,
        StepStatus::Completed,
        StepStatus::InProgress,
        StepStatus::Pending,
        StepStatus::Pending,
        StepStatus::Pending,
    ]));

    let rendered = render_panel(
        &panel, /*width*/ 40, /*height*/ 6, /*right_reserved_cols*/ 0,
    );

    assert!(
        rendered
            .lines()
            .next()
            .is_some_and(|heading| heading.ends_with("expand")),
        "{rendered}"
    );
}

#[test]
fn compact_panel_truncates_rows_and_reserves_right_columns() {
    let panel = visible_panel(vec![PlanItemArg {
        step: "A task with text that cannot fit".to_string(),
        status: StepStatus::InProgress,
    }]);

    assert_snapshot!(
        "task_list_panel_compact_narrow_reserved",
        render_panel(
            &panel, /*width*/ 20, /*height*/ 2, /*right_reserved_cols*/ 4
        )
    );
}

#[test]
fn expanded_panel_wraps_every_task_and_updates_the_heading_action() {
    let mut panel = visible_panel(vec![
        PlanItemArg {
            step: "Inspect https://example.com/path before editing".to_string(),
            status: StepStatus::Completed,
        },
        PlanItemArg {
            step: "Implement the persistent checklist above the composer".to_string(),
            status: StepStatus::InProgress,
        },
        PlanItemArg {
            step: "Verify narrow and constrained layouts".to_string(),
            status: StepStatus::Pending,
        },
    ]);
    assert!(panel.toggle_expanded());
    let height = panel.desired_height(/*width*/ 34);

    assert_snapshot!(
        "task_list_panel_expanded_wrapped",
        render_panel(
            &panel, /*width*/ 34, height, /*right_reserved_cols*/ 0
        )
    );
}

#[test]
fn expanded_panel_preserves_the_anchor_when_height_is_constrained() {
    let mut panel = visible_panel(vec![
        task(0, StepStatus::Completed),
        task(1, StepStatus::Completed),
        task(2, StepStatus::Completed),
        PlanItemArg {
            step: "Current task wraps across several constrained rows".to_string(),
            status: StepStatus::InProgress,
        },
        task(4, StepStatus::Pending),
        task(5, StepStatus::Pending),
    ]);
    assert!(panel.toggle_expanded());

    assert_snapshot!(
        "task_list_panel_expanded_constrained",
        render_panel(
            &panel, /*width*/ 24, /*height*/ 3, /*right_reserved_cols*/ 0
        )
    );
}

#[test]
fn completed_panel_remains_visible_while_its_turn_is_running() {
    let panel = visible_panel(plan(vec![StepStatus::Completed, StepStatus::Completed]));

    assert_snapshot!(
        "task_list_panel_completed_running",
        render_panel(
            &panel, /*width*/ 40, /*height*/ 3, /*right_reserved_cols*/ 0
        )
    );
}
