//! Platform-specific placement and history insertion for an inline viewport.

use std::io;
use std::io::Write;

use crate::custom_terminal::Terminal;
use crate::insert_history::HistoryLineWrapPolicy;
#[cfg(any(windows, test))]
use crate::insert_history::InlineHistoryPlacement;
use crate::insert_history::InsertHistoryMode;
#[cfg(any(windows, test))]
use crate::insert_history::append_history_hyperlink_lines_at_placement;
use crate::insert_history::insert_history_hyperlink_lines_with_mode_and_wrap_policy;
#[cfg(any(windows, test))]
use crate::insert_history::record_inline_history_terminal_scroll;
#[cfg(any(windows, test))]
use crate::insert_history::sync_terminal_visible_history_rows;
use crate::terminal_hyperlinks::HyperlinkLine;
use ratatui::backend::Backend;
use ratatui::layout::Position;
use ratatui::layout::Size;

#[derive(Debug, Default)]
pub(super) struct InlineViewportState {
    #[cfg(windows)]
    windows: WindowsInlineViewportState,
}

impl InlineViewportState {
    pub(super) fn reset(&mut self) {
        #[cfg(windows)]
        self.windows.reset();
    }

    pub(super) fn append_standard_history<B>(
        &mut self,
        terminal: &mut Terminal<B>,
        lines: &[HyperlinkLine],
        wrap_policy: HistoryLineWrapPolicy,
    ) -> io::Result<()>
    where
        B: Backend<Error = io::Error> + Write,
    {
        #[cfg(windows)]
        {
            self.windows
                .append_standard_history(terminal, lines, wrap_policy)
        }
        #[cfg(not(windows))]
        {
            insert_history_hyperlink_lines_with_mode_and_wrap_policy(
                terminal,
                lines,
                InsertHistoryMode::Standard,
                wrap_policy,
            )
        }
    }

    #[cfg(windows)]
    pub(super) fn pending_history_precedes_resize(
        &self,
        requested_top: u16,
        current_top: u16,
    ) -> bool {
        self.windows
            .pending_history_precedes_resize(requested_top, current_top)
    }

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
    placement: Option<InlineHistoryPlacement>,
}

#[cfg(any(windows, test))]
impl WindowsInlineViewportState {
    pub(super) fn reset(&mut self) {
        self.placement = None;
    }

    pub(super) fn pending_history_precedes_resize(
        &self,
        requested_top: u16,
        current_top: u16,
    ) -> bool {
        self.placement.is_some() && requested_top <= current_top
    }

    pub(super) fn append_standard_history<B>(
        &mut self,
        terminal: &mut Terminal<B>,
        lines: &[HyperlinkLine],
        wrap_policy: HistoryLineWrapPolicy,
    ) -> io::Result<()>
    where
        B: Backend<Error = io::Error> + Write,
    {
        if let Some(placement) = self.placement.as_mut() {
            append_history_hyperlink_lines_at_placement(terminal, lines, wrap_policy, placement)
        } else {
            insert_history_hyperlink_lines_with_mode_and_wrap_policy(
                terminal,
                lines,
                InsertHistoryMode::Standard,
                wrap_policy,
            )
        }
    }

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

        if let Some(placement) = self.placement.as_mut() {
            let max_safe_height = screen_size
                .height
                .saturating_sub(placement.viewport_growth_start());
            if area.height > max_safe_height {
                let missing_rows = area.height - max_safe_height;
                if area.height < screen_size.height && placement.visible_rows() == 0 {
                    terminal.backend_mut().set_cursor_position(Position::new(
                        /*x*/ 0,
                        screen_size.height.saturating_sub(1),
                    ))?;
                    terminal.backend_mut().append_lines(missing_rows)?;
                    record_inline_history_terminal_scroll(terminal, placement, missing_rows);
                } else {
                    area.height = max_safe_height;
                    area.y = screen_size.height - area.height;
                }
            }
        }

        if terminal_size_changed {
            self.placement = None;
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

        if self.placement.is_none() {
            let mut placement = InlineHistoryPlacement::new(
                area.top(),
                terminal.visible_history_rows().min(area.top()),
            );
            if first_viewport_reservation && previous_area.y <= area.y {
                placement.allow_viewport_growth_to(previous_area.y);
            }
            sync_terminal_visible_history_rows(terminal, &placement);
            self.placement = Some(placement);
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
