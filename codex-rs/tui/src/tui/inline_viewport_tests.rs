use std::io::Write as _;

use super::WindowsInlineViewportState;
use crate::custom_terminal::Terminal as CustomTerminal;
use crate::insert_history::HistoryLineWrapPolicy;
use crate::insert_history::InlineHistoryPlacement;
use crate::insert_history::update_inline_history_for_viewport;
use crate::terminal_hyperlinks::plain_hyperlink_lines;
use crate::test_backend::VT100Backend;
use pretty_assertions::assert_eq;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::layout::Size;
use ratatui::text::Line;

type TestTerminal = CustomTerminal<VT100Backend>;

fn terminal(width: u16, height: u16, viewport: Rect) -> TestTerminal {
    let backend = VT100Backend::new(width, height);
    let mut terminal =
        CustomTerminal::with_options_and_cursor_position(backend, Position::new(/*x*/ 0, /*y*/ 0))
            .expect("terminal");
    terminal.set_viewport_area(viewport);
    terminal
}

#[test]
fn flush_order_tracks_viewport_direction_and_covered_rows() {
    let mut state = WindowsInlineViewportState {
        placement: Some(InlineHistoryPlacement::new(
            /*history_bottom*/ 17, /*visible_rows*/ 5,
        )),
    };
    assert!(state.pending_history_precedes_resize(/*requested_top*/ 15, /*current_top*/ 17,));
    assert!(!state.pending_history_precedes_resize(/*requested_top*/ 25, /*current_top*/ 17,));

    let mut terminal = terminal(
        /*width*/ 20,
        /*height*/ 30,
        Rect::new(
            /*x*/ 0, /*y*/ 17, /*width*/ 20, /*height*/ 13,
        ),
    );
    update_inline_history_for_viewport(
        &mut terminal,
        state.placement.as_mut().expect("placement"),
        /*viewport_top*/ 16,
    );
    assert!(!state.pending_history_precedes_resize(/*requested_top*/ 15, /*current_top*/ 17,));
    state.reset();
    assert!(state.placement.is_none());
}

#[test]
fn repeated_viewport_growth_reuses_the_existing_history_gap() {
    let width = 80;
    let height = 30;
    let mut terminal = terminal(
        width,
        height,
        Rect::new(/*x*/ 0, /*y*/ 23, width, /*height*/ 7),
    );
    let mut state = WindowsInlineViewportState {
        placement: Some(InlineHistoryPlacement::new(
            /*history_bottom*/ 23, /*visible_rows*/ 20,
        )),
    };

    state
        .update_for_resize_reflow(&mut terminal, /*height*/ 5, Size::new(width, height))
        .expect("shrink viewport");
    state
        .update_for_resize_reflow(&mut terminal, /*height*/ 7, Size::new(width, height))
        .expect("grow viewport into existing gap");

    assert_eq!(terminal.viewport_area, Rect::new(0, 23, width, 7));
    assert_eq!(
        state.placement,
        Some(InlineHistoryPlacement::new(
            /*history_bottom*/ 23, /*visible_rows*/ 20,
        ))
    );
}

#[test]
fn viewport_growth_and_shrink_cover_then_restore_history() {
    let width = 80;
    let height = 30;
    let mut terminal = terminal(
        width,
        height,
        Rect::new(/*x*/ 0, /*y*/ 25, width, /*height*/ 5),
    );
    let mut state = WindowsInlineViewportState {
        placement: Some(InlineHistoryPlacement::new(
            /*history_bottom*/ 24, /*visible_rows*/ 20,
        )),
    };

    state
        .update_for_resize_reflow(&mut terminal, /*height*/ 8, Size::new(width, height))
        .expect("grow viewport");

    assert_eq!(
        (
            terminal.viewport_area,
            terminal.visible_history_rows(),
            state.placement.as_ref().expect("placement").visible_rows(),
        ),
        (Rect::new(0, 22, width, 8), 18, 18)
    );
    assert!(
        state
            .placement
            .as_ref()
            .expect("placement")
            .has_covered_rows()
    );

    state
        .update_for_resize_reflow(&mut terminal, /*height*/ 5, Size::new(width, height))
        .expect("shrink viewport");

    assert_eq!(
        (
            terminal.viewport_area,
            terminal.visible_history_rows(),
            state.placement,
        ),
        (
            Rect::new(0, 25, width, 5),
            20,
            Some(InlineHistoryPlacement::new(
                /*history_bottom*/ 24, /*visible_rows*/ 20,
            )),
        )
    );
}

#[test]
fn initial_viewport_reservation_appends_a_complete_bottom_gap() {
    let width = 20;
    let height = 8;
    let backend = VT100Backend::new(width, height);
    let mut terminal =
        CustomTerminal::with_options_and_cursor_position(backend, Position::new(/*x*/ 0, /*y*/ 5))
            .expect("terminal");
    let mut state = WindowsInlineViewportState::default();

    state
        .update_for_resize_reflow(&mut terminal, /*height*/ 5, Size::new(width, height))
        .expect("reserve first viewport");

    assert_eq!(terminal.viewport_area, Rect::new(0, 3, width, 5));
    assert_eq!(terminal.backend().append_lines_calls(), &[4]);
    assert_eq!(
        state.placement,
        Some(InlineHistoryPlacement::new(
            /*history_bottom*/ 3, /*visible_rows*/ 0,
        ))
    );
}

#[test]
fn initial_viewport_that_fits_is_still_bottom_aligned() {
    let width = 20;
    let height = 10;
    let backend = VT100Backend::new(width, height);
    let mut terminal =
        CustomTerminal::with_options_and_cursor_position(backend, Position::new(/*x*/ 0, /*y*/ 1))
            .expect("terminal");
    let mut state = WindowsInlineViewportState::default();

    state
        .update_for_resize_reflow(&mut terminal, /*height*/ 4, Size::new(width, height))
        .expect("reserve first viewport");

    assert_eq!(
        (
            terminal.viewport_area,
            terminal.backend().append_lines_calls(),
        ),
        (Rect::new(0, 6, width, 4), &[] as &[u16])
    );
}

#[test]
fn growing_first_viewport_preserves_the_launch_row() {
    let width = 20;
    let height = 10;
    let mut backend = VT100Backend::new(width, height);
    write!(backend, "\x1b[7;1HWRAPPED-LAUNCH-END").expect("prefill terminal");
    let mut terminal =
        CustomTerminal::with_options_and_cursor_position(backend, Position::new(/*x*/ 0, /*y*/ 7))
            .expect("terminal");
    let mut state = WindowsInlineViewportState::default();

    state
        .update_for_resize_reflow(&mut terminal, /*height*/ 3, Size::new(width, height))
        .expect("reserve first viewport");
    state
        .update_for_resize_reflow(&mut terminal, /*height*/ 4, Size::new(width, height))
        .expect("grow first viewport");

    let rows = terminal
        .backend()
        .vt100()
        .screen()
        .rows(/*start column*/ 0, width)
        .collect::<Vec<_>>();
    assert_eq!(
        (
            terminal.viewport_area,
            rows[5].trim_end(),
            terminal.backend().append_lines_calls(),
            state.placement,
        ),
        (
            Rect::new(0, 6, width, 4),
            "WRAPPED-LAUNCH-END",
            &[1][..],
            Some(InlineHistoryPlacement::new(
                /*history_bottom*/ 6, /*visible_rows*/ 0,
            )),
        )
    );
}

#[test]
fn first_history_batch_uses_the_shrunken_viewport_boundary() {
    let width = 20;
    let height = 30;
    let mut terminal = terminal(
        width,
        height,
        Rect::new(/*x*/ 0, /*y*/ 18, width, /*height*/ 12),
    );
    let mut state = WindowsInlineViewportState {
        placement: Some(InlineHistoryPlacement::new(
            /*history_bottom*/ 18, /*visible_rows*/ 0,
        )),
    };

    state
        .update_for_resize_reflow(&mut terminal, /*height*/ 7, Size::new(width, height))
        .expect("shrink viewport");
    let history = plain_hyperlink_lines(
        (1..=10)
            .map(|index| Line::from(format!("WELCOME-{index:02}")))
            .collect(),
    );
    state
        .append_standard_history(&mut terminal, &history, HistoryLineWrapPolicy::PreWrap)
        .expect("append history");

    let rows = terminal
        .backend()
        .vt100()
        .screen()
        .rows(/*start column*/ 0, width)
        .collect::<Vec<_>>();
    assert_eq!(
        rows[13..23]
            .iter()
            .map(|row| row.trim_end())
            .collect::<Vec<_>>(),
        (1..=10)
            .map(|index| format!("WELCOME-{index:02}"))
            .collect::<Vec<_>>()
    );
    assert_eq!(terminal.viewport_area, Rect::new(0, 23, width, 7));
}
