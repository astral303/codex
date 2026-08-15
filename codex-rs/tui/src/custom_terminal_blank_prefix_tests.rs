use super::Frame;
use super::Terminal;
use crate::test_backend::VT100Backend;
use pretty_assertions::assert_eq;
use ratatui::layout::Rect;
use ratatui::style::Color;
use ratatui::style::Modifier;
use ratatui::style::Style;

fn completed_frame_blank_prefix(render: impl FnOnce(&mut Frame<'_>)) -> u16 {
    let area = Rect::new(
        /*x*/ 0, /*y*/ 0, /*width*/ 8, /*height*/ 4,
    );
    let mut terminal =
        Terminal::with_options(VT100Backend::new(area.width, area.height)).expect("test terminal");
    terminal.set_viewport_area(area);
    terminal.draw(render).expect("draw completed frame");
    terminal.rendered_viewport_blank_prefix_rows(area.height)
}

#[test]
fn blank_prefix_reads_the_completed_frame() {
    let blank_rows = completed_frame_blank_prefix(|frame| {
        frame
            .buffer_mut()
            .set_string(/*x*/ 0, /*y*/ 2, "composer", Style::default());
    });

    assert_eq!(blank_rows, 2);
}

#[test]
fn blank_prefix_requires_unmodified_spaces_with_reset_backgrounds() {
    let empty_symbol = completed_frame_blank_prefix(|frame| {
        frame.buffer_mut()[(0, 0)].set_symbol("");
    });
    let colored_background = completed_frame_blank_prefix(|frame| {
        frame.buffer_mut()[(0, 0)].set_style(Style::default().bg(Color::Blue));
    });
    let modified_space = completed_frame_blank_prefix(|frame| {
        frame.buffer_mut()[(0, 0)].set_style(Style::default().add_modifier(Modifier::REVERSED));
    });

    assert_eq!(
        (empty_symbol, colored_background, modified_space),
        (0, 0, 0)
    );
}
