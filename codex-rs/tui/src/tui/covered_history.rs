//! Post-draw reconciliation for history covered by the Windows inline viewport.

#[cfg(any(windows, test))]
use std::io;
#[cfg(any(windows, test))]
use std::io::Write;

#[cfg(any(windows, test))]
use crate::custom_terminal::Terminal;
#[cfg(any(windows, test))]
use crate::insert_history::InlineHistoryPlacement;
#[cfg(any(windows, test))]
use crate::insert_history::record_inline_history_terminal_scroll;
#[cfg(any(windows, test))]
use crate::insert_history::repaint_inline_history_with_covered_rows;
#[cfg(any(windows, test))]
use crate::insert_history::update_inline_history_for_viewport;
#[cfg(any(windows, test))]
use ratatui::backend::Backend;
#[cfg(any(windows, test))]
use ratatui::layout::Position;

/// Controls what happens when a completed frame conflicts with retained history.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(crate) enum CoveredHistoryPolicy {
    MoveConflictsToScrollback,
    RestoreAfterViewportShrinks,
}

#[cfg(any(windows, test))]
pub(super) fn reconcile_after_draw<B>(
    terminal: &mut Terminal<B>,
    placement: &mut InlineHistoryPlacement,
    policy: CoveredHistoryPolicy,
) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    let mut blank_rows = terminal.rendered_viewport_blank_prefix_rows(placement.covered_rows());
    let conflicting_rows = placement.covered_rows().saturating_sub(blank_rows);
    let scroll_rows = match policy {
        CoveredHistoryPolicy::MoveConflictsToScrollback => conflicting_rows,
        CoveredHistoryPolicy::RestoreAfterViewportShrinks => {
            let rows_before_retained_history = placement.screen_start();
            conflicting_rows.min(rows_before_retained_history)
        }
    };

    if scroll_rows > 0 {
        // Repaint source-backed rows before moving the terminal document. The completed frame may
        // have overwritten them, and scrolling may move the oldest retained rows into scrollback.
        repaint_inline_history_with_covered_rows(terminal, placement, placement.covered_rows())?;
        let screen_height = terminal.size()?.height;
        terminal
            .backend_mut()
            .set_cursor_position(Position::new(/*x*/ 0, screen_height.saturating_sub(1)))?;
        terminal.backend_mut().append_lines(scroll_rows)?;
        record_inline_history_terminal_scroll(terminal, placement, scroll_rows);
        let viewport_top = terminal.viewport_area.top();
        update_inline_history_for_viewport(terminal, placement, viewport_top);
        terminal.repaint_rendered_viewport()?;
        blank_rows = terminal.rendered_viewport_blank_prefix_rows(placement.covered_rows());
    }

    if placement.has_covered_rows() {
        repaint_inline_history_with_covered_rows(terminal, placement, blank_rows)?;
    }
    Ok(())
}
