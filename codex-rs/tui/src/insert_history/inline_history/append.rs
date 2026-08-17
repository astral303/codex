//! Appends source-backed history at an inline placement boundary.

use std::io;
use std::io::Write;

use super::InlineHistoryPlacement;
use super::physical_row_count;
use super::repaint_inline_history_with_covered_rows;
use super::sync_terminal_visible_history_rows;
use crate::custom_terminal::Terminal;
use crate::insert_history::HistoryLineWrapPolicy;
use crate::insert_history::HistoryTailReplacement;
use crate::insert_history::InsertHistoryMode;
use crate::insert_history::PreparedHistoryLines;
use crate::insert_history::insert_prepared_history_hyperlink_lines_with_mode;
use crate::insert_history::prepare_history_hyperlink_lines;
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
    let wrap_width = usize::from(terminal.viewport_area.width.max(1));
    let prepared = prepare_history_hyperlink_lines(lines, wrap_width, wrap_policy);
    append_prepared_history_hyperlink_lines_at_placement(terminal, &prepared, placement)
}

pub(crate) fn append_prepared_history_hyperlink_lines_at_placement<B>(
    terminal: &mut Terminal<B>,
    prepared: &PreparedHistoryLines,
    placement: &mut InlineHistoryPlacement,
) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    let viewport_top = terminal.viewport_area.top();
    debug_assert_eq!(
        prepared.wrap_width,
        usize::from(terminal.viewport_area.width.max(1))
    );
    let appended_rows = u16::try_from(prepared.row_count).unwrap_or(u16::MAX);
    if appended_rows == 0 {
        return Ok(());
    }

    expose_covered_rows_before_append(terminal, placement)?;

    let history_bottom = placement.history_bottom.min(viewport_top);
    let available_rows = viewport_top.saturating_sub(history_bottom);
    if appended_rows > available_rows {
        let mode = if history_bottom > 0 && history_bottom < viewport_top {
            InsertHistoryMode::StandardAtHistoryBoundary { history_bottom }
        } else {
            InsertHistoryMode::Standard
        };
        insert_prepared_history_hyperlink_lines_with_mode(terminal, prepared, mode)?;
        placement.record_scrolling_append(
            terminal.viewport_area.top(),
            appended_rows,
            &prepared.lines,
            prepared.wrap_width,
        );
        sync_terminal_visible_history_rows(terminal, placement);
        return Ok(());
    }

    paint_history_into_gap(
        terminal,
        history_bottom,
        &prepared.lines,
        prepared.wrap_width,
    )?;
    placement.record_gap_append(appended_rows, &prepared.lines, prepared.wrap_width);
    sync_terminal_visible_history_rows(terminal, placement);
    Ok(())
}

/// Replace a retained history suffix through the same placement boundary used by appends.
pub(crate) fn replace_history_tail_at_placement<B>(
    terminal: &mut Terminal<B>,
    previous_lines: &[HyperlinkLine],
    replacement: &[HyperlinkLine],
    wrap_policy: HistoryLineWrapPolicy,
    placement: &mut InlineHistoryPlacement,
) -> io::Result<HistoryTailReplacement>
where
    B: Backend<Error = io::Error> + Write,
{
    let viewport_top = terminal.viewport_area.top();
    let wrap_width = usize::from(terminal.viewport_area.width.max(1));
    let (prepared_previous, previous_rows) =
        wrap_history_hyperlink_lines(previous_lines, wrap_width, wrap_policy);
    let Ok(previous_rows) = u16::try_from(previous_rows) else {
        return Ok(HistoryTailReplacement::RequiresTranscriptReflow);
    };
    let exposable_rows = placement.retained_screen_rows().min(viewport_top);
    if !placement.has_complete_retained_source(wrap_width)
        || !placement.retained_tail_matches(&prepared_previous)
        || previous_rows > exposable_rows
    {
        return Ok(HistoryTailReplacement::RequiresTranscriptReflow);
    }

    expose_covered_rows_before_append(terminal, placement)?;
    let tail_start = placement.history_bottom.saturating_sub(previous_rows);
    clear_history_rows(terminal, tail_start, previous_rows)?;
    placement.record_visible_history_tail_removal(prepared_previous.len(), previous_rows);
    sync_terminal_visible_history_rows(terminal, placement);
    append_history_hyperlink_lines_at_placement(terminal, replacement, wrap_policy, placement)?;
    Ok(HistoryTailReplacement::Replaced)
}

fn clear_history_rows<B>(terminal: &mut Terminal<B>, start: u16, rows: u16) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    let last_cursor_pos = terminal.last_known_cursor_pos;
    let end = start.saturating_add(rows);
    let writer = terminal.backend_mut();
    for row in start..end {
        queue!(writer, MoveTo(/*x*/ 0, row), Clear(ClearType::CurrentLine))?;
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

fn expose_covered_rows_before_append<B>(
    terminal: &mut Terminal<B>,
    placement: &mut InlineHistoryPlacement,
) -> io::Result<()>
where
    B: Backend<Error = io::Error> + Write,
{
    let viewport_top = terminal.viewport_area.top();
    let scroll_rows = placement
        .history_bottom
        .saturating_sub(viewport_top)
        .min(placement.covered_rows);
    if scroll_rows == 0 {
        return Ok(());
    }

    repaint_inline_history_with_covered_rows(terminal, placement, placement.covered_rows)?;
    let screen_height = terminal.backend().size()?.height;
    queue!(
        terminal.backend_mut(),
        MoveTo(/*x*/ 0, screen_height.saturating_sub(1))
    )?;
    terminal.backend_mut().append_lines(scroll_rows)?;

    placement.record_covered_rows_exposed(viewport_top);
    sync_terminal_visible_history_rows(terminal, placement);
    terminal.invalidate_viewport();
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::super::repaint_inline_history_tail;
    use super::super::update_inline_history_for_viewport;
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

    fn replace_tail(
        terminal: &mut TestTerminal,
        placement: &mut InlineHistoryPlacement,
        previous: &[HyperlinkLine],
        replacement: &[HyperlinkLine],
    ) -> HistoryTailReplacement {
        replace_history_tail_at_placement(
            terminal,
            previous,
            replacement,
            HistoryLineWrapPolicy::PreWrap,
            placement,
        )
        .expect("replace history tail")
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
        assert_eq!(placement.retained_lines, pending);
    }

    #[test]
    fn replacing_history_tail_keeps_source_geometry_and_cache_in_sync() {
        let mut terminal = terminal(20, 10, Rect::new(/*x*/ 0, /*y*/ 7, 20, /*height*/ 3));
        write_markers(
            &mut terminal,
            &[(7, "STATUS"), (8, "COMPOSER"), (9, "FOOTER")],
        );
        let mut placement =
            InlineHistoryPlacement::new(/*history_bottom*/ 3, /*visible_rows*/ 0);
        let initial = history(&["HISTORY-1", "HISTORY-2", "HISTORY-3", "OLD-TAIL"]);
        append(&mut terminal, &mut placement, &initial);

        let expanded_tail = history(&["EXPANDED-1", "EXPANDED-2"]);
        assert_eq!(
            replace_tail(
                &mut terminal,
                &mut placement,
                &history(&["OLD-TAIL"]),
                &expanded_tail,
            ),
            HistoryTailReplacement::Replaced
        );
        assert_eq!(
            (
                placement.history_bottom,
                placement.visible_rows,
                placement.covered_rows,
                terminal.visible_history_rows(),
            ),
            (7, 5, 0, 5)
        );
        assert_eq!(
            placement.retained_lines,
            history(&[
                "HISTORY-1",
                "HISTORY-2",
                "HISTORY-3",
                "EXPANDED-1",
                "EXPANDED-2",
            ])
        );

        let final_tail = history(&["FINAL-TAIL"]);
        assert_eq!(
            replace_tail(&mut terminal, &mut placement, &expanded_tail, &final_tail,),
            HistoryTailReplacement::Replaced
        );
        assert_eq!(
            (
                placement.history_bottom,
                placement.visible_rows,
                placement.covered_rows,
                terminal.visible_history_rows(),
            ),
            (6, 4, 0, 4)
        );
        assert_eq!(
            placement.retained_lines,
            history(&["HISTORY-1", "HISTORY-2", "HISTORY-3", "FINAL-TAIL"])
        );
        assert_eq!(
            &screen_rows(&terminal, 20)[6..],
            ["", "STATUS", "COMPOSER", "FOOTER"]
        );
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
    fn appending_history_exposes_a_covered_tail_through_scrollback() {
        let mut terminal = terminal(20, 10, Rect::new(/*x*/ 0, /*y*/ 7, 20, /*height*/ 3));
        let mut placement =
            InlineHistoryPlacement::new(/*history_bottom*/ 7, /*visible_rows*/ 0);
        append(
            &mut terminal,
            &mut placement,
            &history(&["HISTORY-1", "HISTORY-2", "HISTORY-3", "HISTORY-4"]),
        );
        terminal.set_viewport_area(Rect::new(/*x*/ 0, /*y*/ 5, 20, /*height*/ 5));
        assert!(update_inline_history_for_viewport(
            &mut terminal,
            &mut placement,
            5
        ));
        repaint_inline_history_tail(&mut terminal, &placement).expect("repaint tail");

        append(
            &mut terminal,
            &mut placement,
            &history(&["PENDING-1", "PENDING-2"]),
        );

        assert_eq!(
            &screen_rows(&terminal, 20)[..5],
            [
                "HISTORY-2",
                "HISTORY-3",
                "HISTORY-4",
                "PENDING-1",
                "PENDING-2"
            ]
        );
        assert_eq!(terminal.backend().append_lines_calls(), &[2]);
        assert_eq!(
            (
                placement.history_bottom,
                placement.visible_rows,
                placement.covered_rows
            ),
            (5, 5, 0)
        );
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
