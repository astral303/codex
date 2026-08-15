//! Source-backed history placement for inline terminal viewports.

mod append;

pub(crate) use append::append_history_hyperlink_lines_at_placement;
pub(crate) use append::replace_history_tail_at_placement;

use std::io;
use std::io::Write;

use crate::custom_terminal::Terminal;
use crate::terminal_hyperlinks::HyperlinkLine;
use crate::terminal_hyperlinks::mark_buffer_hyperlinks;
use crate::terminal_hyperlinks::visible_lines_ref;
use crossterm::cursor::MoveTo;
use crossterm::queue;
use ratatui::backend::Backend;
use ratatui::buffer::Buffer;
use ratatui::layout::Rect;
use ratatui::text::Text;
use ratatui::widgets::Paragraph;
use ratatui::widgets::Widget;
use ratatui::widgets::Wrap;

/// Tracks the source-backed history tail that can be covered by an inline viewport.
///
/// While placement tracking is active, `visible_rows` is authoritative. The terminal keeps a
/// synchronized copy only to support generic history insertion and to seed a future placement
/// after tracking is reset.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InlineHistoryPlacement {
    history_bottom: u16,
    visible_rows: u16,
    covered_rows: u16,
    viewport_growth_start: u16,
    retained_lines: Vec<HyperlinkLine>,
}

impl InlineHistoryPlacement {
    pub(crate) fn new(history_bottom: u16, visible_rows: u16) -> Self {
        Self {
            history_bottom,
            visible_rows,
            covered_rows: 0,
            viewport_growth_start: history_bottom.saturating_sub(visible_rows),
            retained_lines: Vec::new(),
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

    pub(crate) fn has_covered_rows(&self) -> bool {
        self.covered_rows > 0
    }

    pub(crate) fn covered_rows(&self) -> u16 {
        self.covered_rows
    }

    #[cfg(test)]
    pub(crate) fn retained_lines(&self) -> &[HyperlinkLine] {
        &self.retained_lines
    }

    /// Record a full-screen scroll and retire retained rows that entered scrollback.
    fn record_terminal_scroll(&mut self, rows: u16) {
        let retained_rows_scrolled = rows
            .saturating_sub(self.screen_start())
            .min(self.retained_screen_rows());
        let visible_rows_scrolled = retained_rows_scrolled.min(self.visible_rows);
        let covered_rows_scrolled = retained_rows_scrolled.saturating_sub(visible_rows_scrolled);

        self.history_bottom = self.history_bottom.saturating_sub(rows);
        self.viewport_growth_start = self.viewport_growth_start.saturating_sub(rows);
        self.visible_rows = self.visible_rows.saturating_sub(visible_rows_scrolled);
        self.covered_rows = self.covered_rows.saturating_sub(covered_rows_scrolled);
    }

    pub(crate) fn screen_start(&self) -> u16 {
        self.history_bottom
            .saturating_sub(self.retained_screen_rows())
    }

    fn record_covered_rows_exposed(&mut self, viewport_top: u16) {
        let retained_rows = self.retained_screen_rows().min(viewport_top);
        self.history_bottom = viewport_top;
        self.visible_rows = retained_rows;
        self.covered_rows = 0;
        self.viewport_growth_start = viewport_top.saturating_sub(retained_rows);
    }

    fn record_gap_append(
        &mut self,
        appended_rows: u16,
        lines: &[HyperlinkLine],
        wrap_width: usize,
    ) {
        let history_bottom = self.history_bottom.saturating_add(appended_rows);
        self.record_history_append(history_bottom, appended_rows, lines, wrap_width);
    }

    fn record_scrolling_append(
        &mut self,
        viewport_top: u16,
        appended_rows: u16,
        lines: &[HyperlinkLine],
        wrap_width: usize,
    ) {
        self.record_history_append(viewport_top, appended_rows, lines, wrap_width);
        self.viewport_growth_start = self.history_bottom.saturating_sub(self.visible_rows);
    }

    fn record_history_append(
        &mut self,
        history_bottom: u16,
        appended_rows: u16,
        lines: &[HyperlinkLine],
        wrap_width: usize,
    ) {
        self.history_bottom = history_bottom;
        self.visible_rows = self
            .visible_rows
            .saturating_add(appended_rows)
            .min(history_bottom);
        self.covered_rows = 0;
        self.retain_visible_history_suffix(lines, wrap_width);
    }

    fn retain_visible_history_suffix(&mut self, lines: &[HyperlinkLine], wrap_width: usize) {
        self.retained_lines.extend_from_slice(lines);
        let required_rows = usize::from(self.retained_screen_rows());
        let mut retained_rows = retained_history_row_count(&self.retained_lines, wrap_width);
        let mut remove_count = 0usize;

        while let Some(line) = self.retained_lines.get(remove_count) {
            let line_rows = physical_row_count(line, wrap_width);
            if retained_rows.saturating_sub(line_rows) < required_rows {
                break;
            }
            retained_rows = retained_rows.saturating_sub(line_rows);
            remove_count += 1;
        }
        self.retained_lines.drain(..remove_count);
    }

    fn has_complete_retained_source(&self, wrap_width: usize) -> bool {
        retained_history_row_count(&self.retained_lines, wrap_width)
            >= usize::from(self.retained_screen_rows())
    }

    fn retained_tail_matches(&self, lines: &[HyperlinkLine]) -> bool {
        self.retained_lines.ends_with(lines)
    }

    fn record_visible_history_tail_removal(&mut self, line_count: usize, rows: u16) {
        debug_assert_eq!(self.covered_rows, 0);
        debug_assert!(line_count <= self.retained_lines.len());
        debug_assert!(rows <= self.visible_rows);
        self.retained_lines
            .truncate(self.retained_lines.len() - line_count);
        self.history_bottom -= rows;
        self.visible_rows -= rows;
    }

    fn update_for_viewport(&mut self, viewport_top: u16) -> bool {
        let retained_rows = self.retained_screen_rows();
        let history_start = self.history_bottom.saturating_sub(retained_rows);
        let visible_end = self.history_bottom.min(viewport_top);
        let next_visible_rows = visible_end.saturating_sub(history_start).min(retained_rows);
        let needs_repaint = next_visible_rows != self.visible_rows;

        self.visible_rows = next_visible_rows;
        self.covered_rows = retained_rows.saturating_sub(next_visible_rows);
        needs_repaint
    }

    fn retained_screen_rows(&self) -> u16 {
        self.visible_rows.saturating_add(self.covered_rows)
    }
}

/// Update placement for a viewport boundary and synchronize the terminal's visibility cache.
pub(crate) fn update_inline_history_for_viewport<B>(
    terminal: &mut Terminal<B>,
    placement: &mut InlineHistoryPlacement,
    viewport_top: u16,
) -> bool
where
    B: Backend<Error = io::Error> + Write,
{
    let needs_repaint = placement.update_for_viewport(viewport_top);
    sync_terminal_visible_history_rows(terminal, placement);
    needs_repaint
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

/// Repaint the complete visible source-backed tail at its current history boundary.
pub(crate) fn repaint_inline_history_tail<B>(
    terminal: &mut Terminal<B>,
    placement: &InlineHistoryPlacement,
) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    repaint_inline_history_rows(terminal, placement, placement.visible_rows)
}

/// Repaint retained history into a blank prefix owned by the viewport.
pub(crate) fn repaint_inline_history_with_covered_rows<B>(
    terminal: &mut Terminal<B>,
    placement: &InlineHistoryPlacement,
    covered_rows: u16,
) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    let repaint_rows = placement
        .visible_rows
        .saturating_add(covered_rows.min(placement.covered_rows));
    repaint_inline_history_rows(terminal, placement, repaint_rows)
}

fn repaint_inline_history_rows<B>(
    terminal: &mut Terminal<B>,
    placement: &InlineHistoryPlacement,
    repaint_rows: u16,
) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    let width = terminal.viewport_area.width.max(1);
    let retained_rows = usize::from(placement.retained_screen_rows());
    let repaint_rows = usize::from(repaint_rows);
    if repaint_rows == 0 || placement.retained_lines.is_empty() {
        return Ok(());
    }

    let cached_rows = retained_history_row_count(&placement.retained_lines, usize::from(width));
    if cached_rows < retained_rows {
        return Ok(());
    }

    let scroll_rows = cached_rows - repaint_rows;
    let history_start = placement.screen_start();
    let area = Rect::new(
        /*x*/ 0,
        history_start,
        width,
        u16::try_from(repaint_rows).unwrap_or(u16::MAX),
    );
    let mut buffer = Buffer::empty(area);
    Paragraph::new(Text::from(visible_lines_ref(&placement.retained_lines)))
        .wrap(Wrap { trim: false })
        .scroll((u16::try_from(scroll_rows).unwrap_or(u16::MAX), /*x*/ 0))
        .render(area, &mut buffer);
    mark_buffer_hyperlinks(&mut buffer, area, &placement.retained_lines, scroll_rows);

    let last_cursor_pos = terminal.last_known_cursor_pos;
    terminal.paint_buffer(&buffer)?;
    queue!(
        terminal.backend_mut(),
        MoveTo(last_cursor_pos.x, last_cursor_pos.y)
    )?;
    std::io::Write::flush(terminal.backend_mut())
}

fn retained_history_row_count(lines: &[HyperlinkLine], wrap_width: usize) -> usize {
    lines
        .iter()
        .map(|line| physical_row_count(line, wrap_width))
        .sum()
}

fn physical_row_count(line: &HyperlinkLine, wrap_width: usize) -> usize {
    line.width().max(1).div_ceil(wrap_width)
}

fn sync_terminal_visible_history_rows<B>(
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
    use crate::terminal_hyperlinks::plain_hyperlink_lines;
    use crate::test_backend::VT100Backend;
    use crossterm::terminal::Clear;
    use crossterm::terminal::ClearType;
    use pretty_assertions::assert_eq;
    use ratatui::layout::Position;
    use ratatui::text::Line;

    #[test]
    fn viewport_and_terminal_scroll_events_preserve_placement_geometry() {
        let mut placement =
            InlineHistoryPlacement::new(/*history_bottom*/ 10, /*visible_rows*/ 4);
        placement.allow_viewport_growth_to(/*row*/ 3);

        assert!(placement.update_for_viewport(/*viewport_top*/ 8));
        assert_eq!(
            (
                placement.history_bottom(),
                placement.visible_rows(),
                placement.covered_rows(),
                placement.viewport_growth_start(),
                placement.screen_start(),
            ),
            (10, 2, 2, 3, 6)
        );
        assert!(placement.has_covered_rows());

        placement.record_terminal_scroll(/*rows*/ 7);

        assert_eq!(
            (
                placement.history_bottom(),
                placement.visible_rows(),
                placement.covered_rows(),
                placement.viewport_growth_start(),
                placement.screen_start(),
            ),
            (3, 1, 2, 0, 0)
        );
    }

    #[test]
    fn repainting_through_boundary_fills_rows_covered_by_viewport_growth() {
        let width = 20;
        let height = 10;
        let backend = VT100Backend::new(width, height);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(
            /*x*/ 0, /*y*/ 7, width, /*height*/ 3,
        ));
        let history = plain_hyperlink_lines(vec![
            Line::from("HISTORY-1"),
            Line::from("HISTORY-2"),
            Line::from("HISTORY-3"),
            Line::from("HISTORY-4"),
        ]);
        let mut placement = InlineHistoryPlacement {
            history_bottom: 7,
            visible_rows: 4,
            covered_rows: 0,
            viewport_growth_start: 3,
            retained_lines: history,
        };
        repaint_inline_history_tail(&mut terminal, &placement).expect("paint visible history");

        terminal.set_viewport_area(Rect::new(
            /*x*/ 0, /*y*/ 5, width, /*height*/ 5,
        ));
        assert!(update_inline_history_for_viewport(
            &mut terminal,
            &mut placement,
            /*viewport_top*/ 5,
        ));
        assert_eq!(terminal.visible_history_rows(), 2);
        for row in 5..7 {
            queue!(
                terminal.backend_mut(),
                MoveTo(/*x*/ 0, row),
                Clear(ClearType::CurrentLine)
            )
            .expect("clear covered row");
        }
        terminal
            .backend_mut()
            .set_cursor_position(Position::new(/*x*/ 0, /*y*/ 7))
            .expect("position composer marker");
        write!(terminal.backend_mut(), "COMPOSER").expect("write composer marker");

        repaint_inline_history_with_covered_rows(
            &mut terminal,
            &placement,
            placement.covered_rows(),
        )
        .expect("restore covered history");

        assert_eq!(
            terminal
                .backend()
                .vt100()
                .screen()
                .rows(/*start column*/ 0, width)
                .skip(3)
                .take(5)
                .map(|row| row.trim_end().to_string())
                .collect::<Vec<_>>(),
            [
                "HISTORY-1",
                "HISTORY-2",
                "HISTORY-3",
                "HISTORY-4",
                "COMPOSER",
            ]
        );
        assert_eq!((placement.visible_rows, placement.covered_rows), (2, 2));
    }

    #[test]
    fn repainting_history_clears_stale_cells_between_words() {
        let width = 20;
        let height = 4;
        let backend = VT100Backend::new(width, height);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(Rect::new(
            /*x*/ 0, /*y*/ 3, width, /*height*/ 1,
        ));
        terminal
            .backend_mut()
            .set_cursor_position(Position::new(/*x*/ 0, /*y*/ 2))
            .expect("position stale row");
        write!(terminal.backend_mut(), "XXXXXXXXXXXXXXXXXXXX").expect("write stale row");
        let history = plain_hyperlink_lines(vec![Line::from("TIP: A B")]);
        let placement = InlineHistoryPlacement {
            history_bottom: 3,
            visible_rows: 1,
            covered_rows: 0,
            viewport_growth_start: 2,
            retained_lines: history,
        };

        repaint_inline_history_tail(&mut terminal, &placement).expect("repaint history");

        let row = terminal
            .backend()
            .vt100()
            .screen()
            .rows(/*start column*/ 0, width)
            .nth(2)
            .expect("history row");
        assert_eq!(row.trim_end(), "TIP: A B");
    }
}
