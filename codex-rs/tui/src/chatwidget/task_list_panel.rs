//! Persistent rendering for the latest structured task list.

use codex_protocol::plan_tool::PlanItemArg;
use codex_protocol::plan_tool::StepStatus;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::style::Style;
use ratatui::style::Styled;
use ratatui::style::Stylize;
use ratatui::text::Line;
use ratatui::text::Span;
use ratatui::widgets::Clear;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;

use crate::key_hint::ShortcutHint;
use crate::line_truncation::truncate_line_with_ellipsis_if_overflow;
use crate::render::line_utils::push_owned_lines;
use crate::render::renderable::Renderable;
use crate::text_formatting::truncate_text;
use crate::wrapping::RtOptions;
use crate::wrapping::adaptive_wrap_line;

pub(crate) const DEFAULT_COMPACT_TASK_ROWS: usize = 5;

const MAX_TASK_LIST_ENTRIES: usize = 100;
const MAX_TASK_STEP_GRAPHEMES: usize = 300;
const CONTINUATION_GUTTER_WIDTH: usize = 3;
const STATUS_MARKER_WIDTH: usize = 2;
const TASK_PREFIX_WIDTH: usize = CONTINUATION_GUTTER_WIDTH + STATUS_MARKER_WIDTH;
const FOOTER_TOP_PADDING: u16 = 1;
const HIDDEN_BEFORE_GUTTER: &str = "⇈  ";
const HIDDEN_AFTER_GUTTER: &str = "⇊  ";
const HIDDEN_BOTH_GUTTER: &str = "⇈⇊ ";
const EMPTY_GUTTER: &str = "   ";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TaskListPresentation {
    Compact,
    Expanded,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum TaskListTurnState {
    Running,
    Idle,
}

#[derive(Debug)]
pub(super) struct TaskListPanel {
    render_enabled: bool,
    plan: Option<DisplayPlan>,
    presentation: TaskListPresentation,
    shortcut_hint: Option<ShortcutHint>,
}

#[derive(Debug)]
struct DisplayPlan {
    items: Vec<PlanItemArg>,
    source_start: usize,
    total_items: usize,
    completed_items: usize,
}

impl DisplayPlan {
    fn from_source(plan: Vec<PlanItemArg>) -> Option<Self> {
        if plan.is_empty() {
            return None;
        }

        let total_items = plan.len();
        let completed_items = plan.iter().filter(|item| task_is_completed(item)).count();
        let retained_window = select_task_window(&plan, MAX_TASK_LIST_ENTRIES);
        let source_start = retained_window.start;
        let mut items = plan
            .into_iter()
            .skip(retained_window.start)
            .take(retained_window.len())
            .collect::<Vec<_>>();
        for item in &mut items {
            item.step = truncate_text(&item.step, MAX_TASK_STEP_GRAPHEMES);
        }

        Some(Self {
            items,
            source_start,
            total_items,
            completed_items,
        })
    }

    fn is_complete(&self) -> bool {
        self.completed_items == self.total_items
    }

    fn source_window(&self, retained_window: TaskWindow) -> TaskWindow {
        TaskWindow {
            start: self.source_start.saturating_add(retained_window.start),
            end: self.source_start.saturating_add(retained_window.end),
        }
    }
}

impl TaskListPanel {
    pub(super) fn new(render_enabled: bool, shortcut_hint: Option<ShortcutHint>) -> Self {
        Self {
            render_enabled,
            plan: None,
            presentation: TaskListPresentation::Compact,
            shortcut_hint,
        }
    }

    pub(super) fn set_render_enabled(&mut self, render_enabled: bool) {
        self.render_enabled = render_enabled;
    }

    pub(super) fn set_shortcut_hint(&mut self, shortcut_hint: Option<ShortcutHint>) {
        if shortcut_hint.is_none() {
            self.presentation = TaskListPresentation::Compact;
        }
        self.shortcut_hint = shortcut_hint;
    }

    pub(super) fn replace_plan(&mut self, plan: Vec<PlanItemArg>, turn_state: TaskListTurnState) {
        let Some(plan) = DisplayPlan::from_source(plan) else {
            self.clear();
            return;
        };
        if turn_state == TaskListTurnState::Idle && plan.is_complete() {
            self.clear();
            return;
        }

        self.plan = Some(plan);
    }

    pub(super) fn on_turn_finished(&mut self) {
        if self.plan.as_ref().is_some_and(DisplayPlan::is_complete) {
            self.clear();
        }
    }

    pub(super) fn toggle_expanded(&mut self) -> bool {
        if !self.is_visible() {
            return false;
        }

        self.presentation = match self.presentation {
            TaskListPresentation::Compact => TaskListPresentation::Expanded,
            TaskListPresentation::Expanded => TaskListPresentation::Compact,
        };
        true
    }

    pub(super) fn is_visible(&self) -> bool {
        self.render_enabled && self.plan.is_some()
    }

    #[cfg(test)]
    pub(super) fn as_renderable(&self, right_reserved_cols: u16) -> impl Renderable + '_ {
        TaskListPanelRenderable {
            panel: self,
            right_reserved_cols,
            top_padding: 0,
        }
    }

    pub(super) fn as_footer_renderable(&self, right_reserved_cols: u16) -> impl Renderable + '_ {
        TaskListPanelRenderable {
            panel: self,
            right_reserved_cols,
            top_padding: FOOTER_TOP_PADDING,
        }
    }

    fn clear(&mut self) {
        self.plan = None;
        self.presentation = TaskListPresentation::Compact;
    }

    fn desired_height(&self, width: u16) -> u16 {
        let Some(plan) = self.plan.as_ref().filter(|_| self.render_enabled) else {
            return 0;
        };
        if width == 0 {
            return 0;
        }

        let task_rows = match self.presentation {
            TaskListPresentation::Compact => plan.items.len().min(DEFAULT_COMPACT_TASK_ROWS),
            TaskListPresentation::Expanded => plan
                .items
                .iter()
                .map(|item| expanded_task_lines(item, EMPTY_GUTTER, width).len())
                .sum(),
        };
        u16::try_from(task_rows.saturating_add(1)).unwrap_or(u16::MAX)
    }

    fn render_lines(&self, width: u16, height: u16) -> Vec<Line<'static>> {
        let Some(plan) = self.plan.as_ref().filter(|_| self.render_enabled) else {
            return Vec::new();
        };
        if width == 0 || height == 0 {
            return Vec::new();
        }

        let task_row_capacity = usize::from(height.saturating_sub(1));
        let window = match self.presentation {
            TaskListPresentation::Compact => select_task_window(
                &plan.items,
                task_row_capacity.min(DEFAULT_COMPACT_TASK_ROWS),
            ),
            TaskListPresentation::Expanded => {
                select_expanded_task_window(&plan.items, task_row_capacity, width)
            }
        };
        let source_window = plan.source_window(window);
        let hidden_count = source_window.hidden_count(plan.total_items);
        let mut lines = vec![self.heading(plan, hidden_count, width)];

        for index in window.start..window.end {
            let source_index = plan.source_start.saturating_add(index);
            let gutter = continuation_gutter(source_window, source_index, plan.total_items);
            match self.presentation {
                TaskListPresentation::Compact => {
                    lines.push(compact_task_line(&plan.items[index], gutter, width));
                }
                TaskListPresentation::Expanded => {
                    lines.extend(expanded_task_lines(&plan.items[index], gutter, width));
                }
            }
        }
        lines.truncate(usize::from(height));
        lines
    }

    fn heading(&self, plan: &DisplayPlan, hidden_count: usize, width: u16) -> Line<'static> {
        let mut spans = vec![
            "Tasks ".bold(),
            format!("{}/{}", plan.completed_items, plan.total_items).into(),
        ];
        if hidden_count > 0 {
            spans.push(format!(" · {hidden_count} hidden").dim());
        }
        let mut heading: Line<'static> = spans.into();
        if let Some(shortcut_hint) = self.shortcut_hint {
            let action = match self.presentation {
                TaskListPresentation::Compact => "expand",
                TaskListPresentation::Expanded => "collapse",
            };
            let shortcut = format!(" · {} {action}", shortcut_hint.display_label()).dim();
            if heading.width().saturating_add(shortcut.width()) <= usize::from(width) {
                heading.spans.push(shortcut);
            }
        }
        truncate_line_with_ellipsis_if_overflow(heading, usize::from(width))
    }
}

struct TaskListPanelRenderable<'a> {
    panel: &'a TaskListPanel,
    right_reserved_cols: u16,
    top_padding: u16,
}

impl Renderable for TaskListPanelRenderable<'_> {
    fn render(&self, area: Rect, buf: &mut Buffer) {
        if area.width == 0 || area.height == 0 {
            return;
        }

        let content_area = Rect::new(
            area.x,
            area.y.saturating_add(self.top_padding),
            area.width.saturating_sub(self.right_reserved_cols),
            area.height.saturating_sub(self.top_padding),
        );
        if content_area.width == 0 || content_area.height == 0 {
            return;
        }

        Clear.render(content_area, buf);
        Paragraph::new(
            self.panel
                .render_lines(content_area.width, content_area.height),
        )
        .render(content_area, buf);
    }

    fn desired_height(&self, width: u16) -> u16 {
        let content_width = width.saturating_sub(self.right_reserved_cols);
        if content_width == 0 {
            return 0;
        }
        let content_height = self.panel.desired_height(content_width);
        if content_height == 0 {
            0
        } else {
            content_height.saturating_add(self.top_padding)
        }
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
struct TaskWindow {
    start: usize,
    end: usize,
}

impl TaskWindow {
    fn len(self) -> usize {
        self.end.saturating_sub(self.start)
    }

    fn hidden_count(self, total: usize) -> usize {
        self.start.saturating_add(total.saturating_sub(self.end))
    }
}

fn select_task_window(plan: &[PlanItemArg], capacity: usize) -> TaskWindow {
    let capacity = capacity.min(plan.len());
    if capacity == 0 {
        return TaskWindow::default();
    }

    let in_progress_count = plan
        .iter()
        .filter(|item| matches!(&item.status, StepStatus::InProgress))
        .count();
    let anchor = plan
        .iter()
        .position(|item| matches!(&item.status, StepStatus::InProgress))
        .or_else(|| {
            plan.iter()
                .position(|item| matches!(&item.status, StepStatus::Pending))
        })
        .unwrap_or(plan.len() - 1);
    let (target_before, target_after) = if in_progress_count > 1 {
        (1, 3)
    } else {
        (2, 2)
    };
    let after = target_after.min(capacity.saturating_sub(1));
    let before = target_before.min(capacity.saturating_sub(after + 1));
    let mut start = anchor.saturating_sub(before);
    let mut end = (anchor + after + 1).min(plan.len());

    let missing = capacity.saturating_sub(end - start);
    end = (end + missing).min(plan.len());
    let missing = capacity.saturating_sub(end - start);
    start = start.saturating_sub(missing);

    TaskWindow { start, end }
}

fn select_expanded_task_window(
    plan: &[PlanItemArg],
    row_capacity: usize,
    width: u16,
) -> TaskWindow {
    let mut task_capacity = row_capacity.min(plan.len());
    loop {
        let window = select_task_window(plan, task_capacity);
        let rendered_rows = (window.start..window.end)
            .map(|index| expanded_task_lines(&plan[index], EMPTY_GUTTER, width).len())
            .sum::<usize>();
        if rendered_rows <= row_capacity || task_capacity <= 1 {
            return window;
        }
        task_capacity -= 1;
    }
}

fn continuation_gutter(window: TaskWindow, index: usize, total: usize) -> &'static str {
    let hidden_before = window.start > 0;
    let hidden_after = window.end < total;
    if window.len() == 1 && hidden_before && hidden_after {
        HIDDEN_BOTH_GUTTER
    } else if index == window.start && hidden_before {
        HIDDEN_BEFORE_GUTTER
    } else if index + 1 == window.end && hidden_after {
        HIDDEN_AFTER_GUTTER
    } else {
        EMPTY_GUTTER
    }
}

fn compact_task_line(item: &PlanItemArg, gutter: &str, width: u16) -> Line<'static> {
    let (marker, style) = task_marker_and_style(&item.status);
    truncate_line_with_ellipsis_if_overflow(
        vec![
            gutter.to_string().dim(),
            marker.set_style(style),
            item.step.clone().set_style(style),
        ]
        .into(),
        usize::from(width),
    )
}

fn expanded_task_lines(item: &PlanItemArg, gutter: &str, width: u16) -> Vec<Line<'static>> {
    let (marker, style) = task_marker_and_style(&item.status);
    let prefix = Line::from(vec![gutter.to_string().dim(), marker.set_style(style)]);
    let continuation = Line::from(" ".repeat(TASK_PREFIX_WIDTH));
    let step = Line::from(Span::from(item.step.clone()).set_style(style));
    let wrapped = adaptive_wrap_line(
        &step,
        RtOptions::new(usize::from(width.max(1)))
            .initial_indent(prefix)
            .subsequent_indent(continuation),
    );
    let mut lines = Vec::new();
    push_owned_lines(&wrapped, &mut lines);
    lines
}

fn task_marker_and_style(status: &StepStatus) -> (&'static str, Style) {
    match status {
        StepStatus::Completed => ("✔ ", Style::default().crossed_out().dim()),
        StepStatus::InProgress => ("□ ", Style::default().cyan().bold()),
        StepStatus::Pending => ("□ ", Style::default().dim()),
    }
}

fn task_is_completed(item: &PlanItemArg) -> bool {
    matches!(&item.status, StepStatus::Completed)
}

impl super::ChatWidget {
    pub(crate) fn set_keep_in_progress_tasks_visible(&mut self, enabled: bool) {
        self.config.tui_keep_in_progress_tasks_visible = enabled;
        self.task_list_panel.set_render_enabled(enabled);
        self.request_redraw();
    }

    pub(crate) fn toggle_task_list(&mut self) -> bool {
        let toggled = self.task_list_panel.toggle_expanded();
        if toggled {
            self.request_redraw();
        }
        toggled
    }
}

#[cfg(test)]
#[path = "task_list_panel_tests.rs"]
mod tests;
