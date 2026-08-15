//! Platform-specific inline viewport resize mechanics.

use std::io;
use std::io::Write;

use crate::custom_terminal::Terminal;
use ratatui::backend::Backend;
use ratatui::layout::Position;
use ratatui::layout::Size;

#[derive(Debug, Default)]
pub(super) struct InlineViewportState {
    #[cfg(windows)]
    windows: WindowsInlineViewportState,
}

impl InlineViewportState {
    /// Resize the inline viewport for transcript reflow.
    pub(super) fn update_for_resize_reflow<B>(
        &mut self,
        terminal: &mut Terminal<B>,
        height: u16,
        screen_size: Size,
    ) -> io::Result<bool>
    where
        B: Backend<Error = io::Error> + Write,
    {
        #[cfg(windows)]
        {
            self.windows
                .update_for_resize_reflow(terminal, height, screen_size)
        }
        #[cfg(not(windows))]
        {
            update_non_windows_for_resize_reflow(terminal, height, screen_size)
        }
    }
}

#[cfg(any(windows, test))]
#[derive(Debug, Default)]
pub(super) struct WindowsInlineViewportState {
    /// Earliest row the initial viewport may grow into without scrolling launch output.
    viewport_growth_start: Option<u16>,
}

#[cfg(any(windows, test))]
impl WindowsInlineViewportState {
    /// Resize the Windows inline viewport without scrolling transcript rows a second time.
    pub(super) fn update_for_resize_reflow<B>(
        &mut self,
        terminal: &mut Terminal<B>,
        height: u16,
        screen_size: Size,
    ) -> io::Result<bool>
    where
        B: Backend<Error = io::Error> + Write,
    {
        let terminal_height_grew = screen_size.height > terminal.last_known_screen_size.height;
        let terminal_size_changed = screen_size != terminal.last_known_screen_size;
        let previous_area = terminal.viewport_area;
        let viewport_was_bottom_aligned =
            previous_area.bottom() == terminal.last_known_screen_size.height;
        let first_viewport_reservation = previous_area.height == 0;

        let mut area = previous_area;
        area.height = height.min(screen_size.height);
        area.width = screen_size.width;
        let viewport_height_shrank = area.height < previous_area.height;

        if first_viewport_reservation {
            if area.bottom() > screen_size.height {
                terminal
                    .backend_mut()
                    .append_lines(area.height.saturating_sub(1))?;
            }
            area.y = screen_size.height - area.height;
        } else if area.bottom() > screen_size.height
            || (viewport_was_bottom_aligned && (terminal_height_grew || viewport_height_shrank))
        {
            area.y = screen_size.height - area.height;
        }

        if let Some(viewport_growth_start) = self.viewport_growth_start.as_mut() {
            let max_safe_height = screen_size.height.saturating_sub(*viewport_growth_start);
            if area.height > max_safe_height {
                let missing_rows = area.height - max_safe_height;
                if area.height < screen_size.height {
                    terminal.backend_mut().set_cursor_position(Position::new(
                        /*x*/ 0,
                        screen_size.height.saturating_sub(1),
                    ))?;
                    terminal.backend_mut().append_lines(missing_rows)?;
                    *viewport_growth_start = viewport_growth_start.saturating_sub(missing_rows);
                } else {
                    area.height = max_safe_height;
                    area.y = screen_size.height - area.height;
                }
            }
        }

        if terminal_size_changed {
            self.viewport_growth_start = None;
        }

        let needs_full_repaint = area != previous_area;
        if needs_full_repaint {
            let clear_y = if first_viewport_reservation {
                area.y
            } else {
                previous_area.y.min(area.y)
            };
            terminal.set_viewport_area(area);
            terminal.clear_after_position(Position::new(/*x*/ 0, clear_y))?;
        }

        if self.viewport_growth_start.is_none() {
            let mut viewport_growth_start = area.top();
            if first_viewport_reservation && previous_area.y <= area.y {
                viewport_growth_start = previous_area.y;
            }
            self.viewport_growth_start = Some(viewport_growth_start);
        }

        Ok(needs_full_repaint)
    }
}

/// Resize the non-Windows inline viewport without scrolling transcript rows during shrink reflow.
#[cfg(not(windows))]
fn update_non_windows_for_resize_reflow<B>(
    terminal: &mut Terminal<B>,
    height: u16,
    screen_size: Size,
) -> io::Result<bool>
where
    B: Backend<Error = io::Error> + Write,
{
    let terminal_height_shrank = screen_size.height < terminal.last_known_screen_size.height;
    let terminal_height_grew = screen_size.height > terminal.last_known_screen_size.height;
    let viewport_was_bottom_aligned =
        terminal.viewport_area.bottom() == terminal.last_known_screen_size.height;
    let previous_area = terminal.viewport_area;

    let mut area = previous_area;
    area.height = height.min(screen_size.height);
    area.width = screen_size.width;

    if area.bottom() > screen_size.height {
        let scroll_by = area.bottom() - screen_size.height;
        if !terminal_height_shrank {
            terminal
                .backend_mut()
                .scroll_region_up(0..area.top(), scroll_by)?;
        }
        area.y = screen_size.height - area.height;
    } else if terminal_height_grew && viewport_was_bottom_aligned {
        area.y = screen_size.height - area.height;
    }

    let needs_full_repaint = area != previous_area;
    if needs_full_repaint {
        let clear_position = Position::new(/*x*/ 0, previous_area.y.min(area.y));
        terminal.set_viewport_area(area);
        terminal.clear_after_position(clear_position)?;
    }

    Ok(needs_full_repaint)
}

#[cfg(test)]
#[path = "inline_viewport_tests.rs"]
mod tests;
