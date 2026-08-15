//! Placement geometry for history appended above an inline terminal viewport.

mod append;

pub(crate) use append::append_history_hyperlink_lines_at_placement;

use std::io;
use std::io::Write;

use crate::custom_terminal::Terminal;
use ratatui::backend::Backend;

/// Tracks the visible history boundary used by placement-aware appends.
///
/// While placement tracking is active, `visible_rows` is authoritative. The terminal keeps a
/// synchronized copy only to support generic history insertion and to seed a future placement
/// after tracking is reset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InlineHistoryPlacement {
    history_bottom: u16,
    visible_rows: u16,
    viewport_growth_start: u16,
}

impl InlineHistoryPlacement {
    pub(crate) fn new(history_bottom: u16, visible_rows: u16) -> Self {
        Self {
            history_bottom,
            visible_rows,
            viewport_growth_start: history_bottom.saturating_sub(visible_rows),
        }
    }

    #[cfg(test)]
    pub(crate) fn history_bottom(&self) -> u16 {
        self.history_bottom
    }

    pub(crate) fn visible_rows(&self) -> u16 {
        self.visible_rows
    }

    pub(crate) fn allow_viewport_growth_to(&mut self, row: u16) {
        self.viewport_growth_start = self.viewport_growth_start.min(row);
    }

    pub(crate) fn viewport_growth_start(&self) -> u16 {
        self.viewport_growth_start
    }

    fn record_terminal_scroll(&mut self, rows: u16) {
        let visible_rows_scrolled = rows
            .saturating_sub(self.screen_start())
            .min(self.visible_rows);
        self.history_bottom = self.history_bottom.saturating_sub(rows);
        self.viewport_growth_start = self.viewport_growth_start.saturating_sub(rows);
        self.visible_rows = self.visible_rows.saturating_sub(visible_rows_scrolled);
    }

    fn screen_start(&self) -> u16 {
        self.history_bottom.saturating_sub(self.visible_rows)
    }

    fn record_gap_append(&mut self, appended_rows: u16) {
        self.history_bottom = self.history_bottom.saturating_add(appended_rows);
        self.visible_rows = self
            .visible_rows
            .saturating_add(appended_rows)
            .min(self.history_bottom);
    }

    fn record_scrolling_append(&mut self, viewport_top: u16, appended_rows: u16) {
        self.history_bottom = viewport_top;
        self.visible_rows = self
            .visible_rows
            .saturating_add(appended_rows)
            .min(viewport_top);
        self.viewport_growth_start = self.history_bottom.saturating_sub(self.visible_rows);
    }
}

/// Record a full-screen scroll and synchronize the terminal's visibility cache.
pub(crate) fn record_inline_history_terminal_scroll<B>(
    terminal: &mut Terminal<B>,
    placement: &mut InlineHistoryPlacement,
    rows: u16,
) where
    B: Backend<Error = io::Error> + Write,
{
    placement.record_terminal_scroll(rows);
    sync_terminal_visible_history_rows(terminal, placement);
}

pub(crate) fn sync_terminal_visible_history_rows<B>(
    terminal: &mut Terminal<B>,
    placement: &InlineHistoryPlacement,
) where
    B: Backend<Error = io::Error> + Write,
{
    terminal.set_visible_history_rows(placement.visible_rows);
}

#[cfg(test)]
mod tests {
    use super::*;
    use pretty_assertions::assert_eq;

    #[test]
    fn terminal_scroll_preserves_visible_placement_geometry() {
        let mut placement =
            InlineHistoryPlacement::new(/*history_bottom*/ 10, /*visible_rows*/ 4);
        placement.allow_viewport_growth_to(/*row*/ 3);

        placement.record_terminal_scroll(/*rows*/ 7);

        assert_eq!(
            (
                placement.history_bottom(),
                placement.visible_rows(),
                placement.viewport_growth_start(),
                placement.screen_start(),
            ),
            (3, 3, 0, 0)
        );
    }
}
