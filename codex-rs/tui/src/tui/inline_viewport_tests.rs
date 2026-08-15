use std::io::Write as _;

use super::WindowsInlineViewportState;
use crate::custom_terminal::Terminal as CustomTerminal;
use crate::test_backend::VT100Backend;
use pretty_assertions::assert_eq;
use ratatui::layout::Position;
use ratatui::layout::Rect;
use ratatui::layout::Size;

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
        ),
        (Rect::new(0, 6, width, 4), "WRAPPED-LAUNCH-END", &[1][..])
    );
}
