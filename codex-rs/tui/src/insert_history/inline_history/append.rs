//! Appends history at an inline placement boundary.

use std::io;
use std::io::Write;

use super::InlineHistoryPlacement;
use super::sync_terminal_visible_history_rows;
use crate::custom_terminal::Terminal;
use crate::insert_history::HistoryLineWrapPolicy;
use crate::insert_history::InsertHistoryMode;
use crate::insert_history::insert_history_hyperlink_lines_with_mode_and_wrap_policy;
use crate::insert_history::wrap_history_hyperlink_lines;
use crate::insert_history::write_history_line;
use crate::terminal_hyperlinks::HyperlinkLine;
use crossterm::cursor::MoveTo;
use crossterm::queue;
use crossterm::style::Attribute;
use crossterm::style::Color;
use crossterm::style::SetAttribute;
use crossterm::style::SetBackgroundColor;
use crossterm::style::SetForegroundColor;
use crossterm::terminal::Clear;
use crossterm::terminal::ClearType;
use ratatui::backend::Backend;

/// Append history into rows exposed by a viewport shrink before scrolling the terminal.
///
/// New rows consume the gap between history and the viewport first. Only overflow moves older
/// visible rows into scrollback, and no write crosses into the viewport.
pub(crate) fn append_history_hyperlink_lines_at_placement<B>(
    terminal: &mut Terminal<B>,
    lines: &[HyperlinkLine],
    wrap_policy: HistoryLineWrapPolicy,
    placement: &mut InlineHistoryPlacement,
) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    let viewport_top = terminal.viewport_area.top();
    let wrap_width = usize::from(terminal.viewport_area.width.max(1));
    let (prepared_lines, prepared_rows) =
        wrap_history_hyperlink_lines(lines, wrap_width, wrap_policy);
    let appended_rows = u16::try_from(prepared_rows).unwrap_or(u16::MAX);
    if appended_rows == 0 {
        return Ok(());
    }

    let history_bottom = placement.history_bottom.min(viewport_top);
    let available_rows = viewport_top.saturating_sub(history_bottom);
    if appended_rows > available_rows {
        let mode = if history_bottom > 0 && history_bottom < viewport_top {
            InsertHistoryMode::StandardAtHistoryBoundary { history_bottom }
        } else {
            InsertHistoryMode::Standard
        };
        insert_history_hyperlink_lines_with_mode_and_wrap_policy(
            terminal,
            lines,
            mode,
            wrap_policy,
        )?;
        placement.record_scrolling_append(terminal.viewport_area.top(), appended_rows);
        sync_terminal_visible_history_rows(terminal, placement);
        return Ok(());
    }

    paint_history_into_gap(terminal, history_bottom, &prepared_lines, wrap_width)?;
    placement.record_gap_append(appended_rows);
    sync_terminal_visible_history_rows(terminal, placement);
    Ok(())
}

fn paint_history_into_gap<B>(
    terminal: &mut Terminal<B>,
    history_bottom: u16,
    lines: &[HyperlinkLine],
    wrap_width: usize,
) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    let last_cursor_pos = terminal.last_known_cursor_pos;
    let mut row = history_bottom;
    let writer = terminal.backend_mut();
    for line in lines {
        queue!(writer, MoveTo(/*x*/ 0, row), Clear(ClearType::CurrentLine))?;
        write_history_line(writer, line, wrap_width)?;
        let line_rows = u16::try_from(physical_row_count(line, wrap_width)).unwrap_or(u16::MAX);
        row = row.saturating_add(line_rows);
    }
    queue!(
        writer,
        MoveTo(last_cursor_pos.x, last_cursor_pos.y),
        SetForegroundColor(Color::Reset),
        SetBackgroundColor(Color::Reset),
        SetAttribute(Attribute::Reset),
    )?;
    std::io::Write::flush(writer)
}

fn physical_row_count(line: &HyperlinkLine, wrap_width: usize) -> usize {
    line.width().max(1).div_ceil(wrap_width)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::terminal_hyperlinks::plain_hyperlink_lines;
    use crate::test_backend::VT100Backend;
    use pretty_assertions::assert_eq;
    use ratatui::layout::Position;
    use ratatui::layout::Rect;
    use ratatui::text::Line;

    type TestTerminal = Terminal<VT100Backend>;

    fn terminal(width: u16, height: u16, viewport: Rect) -> TestTerminal {
        let backend = VT100Backend::new(width, height);
        let mut terminal = Terminal::with_options(backend).expect("terminal");
        terminal.set_viewport_area(viewport);
        terminal
    }

    fn write_markers(terminal: &mut TestTerminal, markers: &[(u16, &str)]) {
        for &(row, marker) in markers {
            terminal
                .backend_mut()
                .set_cursor_position(Position::new(/*x*/ 0, row))
                .expect("position marker");
            write!(terminal.backend_mut(), "{marker}").expect("write marker");
        }
    }

    fn history(labels: &[&str]) -> Vec<HyperlinkLine> {
        plain_hyperlink_lines(
            labels
                .iter()
                .map(|label| Line::from((*label).to_owned()))
                .collect(),
        )
    }

    fn append(
        terminal: &mut TestTerminal,
        placement: &mut InlineHistoryPlacement,
        lines: &[HyperlinkLine],
    ) {
        append_history_hyperlink_lines_at_placement(
            terminal,
            lines,
            HistoryLineWrapPolicy::PreWrap,
            placement,
        )
        .expect("append history");
    }

    fn screen_rows(terminal: &TestTerminal, width: u16) -> Vec<String> {
        terminal
            .backend()
            .vt100()
            .screen()
            .rows(/*start column*/ 0, width)
            .map(|row| row.trim_end().to_string())
            .collect()
    }

    #[test]
    fn viewport_shrink_capacity_accepts_history_without_scrolling() {
        let mut terminal = terminal(20, 8, Rect::new(/*x*/ 0, /*y*/ 7, 20, /*height*/ 1));
        write_markers(&mut terminal, &[(7, "VIEWPORT")]);
        let mut placement =
            InlineHistoryPlacement::new(/*history_bottom*/ 4, /*visible_rows*/ 4);
        let pending = history(&["PENDING-1", "PENDING-2", "PENDING-3"]);

        append(&mut terminal, &mut placement, &pending);

        assert_eq!(
            &screen_rows(&terminal, 20)[4..],
            ["PENDING-1", "PENDING-2", "PENDING-3", "VIEWPORT"]
        );
        assert_eq!((placement.history_bottom, placement.visible_rows), (7, 7));
        assert_eq!(terminal.visible_history_rows(), 7);
    }

    #[test]
    fn first_and_subsequent_history_appends_preserve_the_viewport() {
        let mut terminal = terminal(20, 8, Rect::new(/*x*/ 0, /*y*/ 5, 20, /*height*/ 3));
        write_markers(
            &mut terminal,
            &[(5, "STATUS"), (6, "COMPOSER"), (7, "FOOTER")],
        );
        let mut placement =
            InlineHistoryPlacement::new(/*history_bottom*/ 5, /*visible_rows*/ 0);

        append(
            &mut terminal,
            &mut placement,
            &history(&["HISTORY-1", "HISTORY-2", "HISTORY-3"]),
        );
        append(
            &mut terminal,
            &mut placement,
            &history(&["HISTORY-4", "HISTORY-5"]),
        );

        assert_eq!(
            &screen_rows(&terminal, 20)[5..],
            ["STATUS", "COMPOSER", "FOOTER"]
        );
        assert_eq!((placement.history_bottom, placement.visible_rows), (5, 5));
        assert_eq!(terminal.visible_history_rows(), 5);
    }

    #[test]
    fn history_overflow_scrolls_only_rows_that_do_not_fit_before_viewport() {
        let mut terminal = terminal(20, 8, Rect::new(/*x*/ 0, /*y*/ 5, 20, /*height*/ 3));
        write_markers(
            &mut terminal,
            &[(5, "STATUS"), (6, "MESSAGE"), (7, "COMPOSER")],
        );
        let mut placement =
            InlineHistoryPlacement::new(/*history_bottom*/ 4, /*visible_rows*/ 4);

        append(
            &mut terminal,
            &mut placement,
            &history(&["HISTORY-1", "HISTORY-2", "HISTORY-3"]),
        );

        assert_eq!(
            &screen_rows(&terminal, 20)[5..],
            ["STATUS", "MESSAGE", "COMPOSER"]
        );
        assert_eq!((placement.history_bottom, placement.visible_rows), (5, 5));
        assert_eq!(terminal.visible_history_rows(), 5);
    }

    #[test]
    fn oversized_history_consumes_exposed_rows_before_scrolling() {
        let mut terminal = terminal(20, 8, Rect::new(/*x*/ 0, /*y*/ 5, 20, /*height*/ 3));
        write_markers(
            &mut terminal,
            &[(5, "STATUS"), (6, "COMPOSER"), (7, "FOOTER")],
        );
        let mut placement =
            InlineHistoryPlacement::new(/*history_bottom*/ 4, /*visible_rows*/ 4);
        let lines = plain_hyperlink_lines(
            (1..=8)
                .map(|index| Line::from(format!("NEW-{index}")))
                .collect(),
        );
        terminal.backend_mut().clear_written();

        append(&mut terminal, &mut placement, &lines);

        let history_boundary_cursor = b"\x1b[4;1H";
        assert!(
            terminal
                .backend()
                .written()
                .windows(history_boundary_cursor.len())
                .any(|bytes| bytes == history_boundary_cursor)
        );
        assert_eq!(
            &screen_rows(&terminal, 20)[5..],
            ["STATUS", "COMPOSER", "FOOTER"]
        );
    }

    #[test]
    fn wrapped_history_stops_before_the_first_viewport_row() {
        let mut terminal = terminal(10, 8, Rect::new(/*x*/ 0, /*y*/ 7, 10, /*height*/ 1));
        write_markers(&mut terminal, &[(7, "VIEWPORT")]);
        let mut placement =
            InlineHistoryPlacement::new(/*history_bottom*/ 4, /*visible_rows*/ 4);

        append(
            &mut terminal,
            &mut placement,
            &history(&["PROMPT-START-ab-PROMPT-END"]),
        );

        let rows = screen_rows(&terminal, 10);
        assert!(rows[..7].join("").contains("PROMPT-START"));
        assert!(rows[..7].join("").contains("PROMPT-END"));
        assert_eq!(
            (rows[7].as_str(), placement.history_bottom),
            ("VIEWPORT", 7)
        );
    }
}
