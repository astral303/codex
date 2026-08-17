//! Platform-specific source-backed placement and history insertion for an inline viewport.

use std::io;
use std::io::Write;

use crate::custom_terminal::Terminal;
use crate::insert_history::HistoryLineWrapPolicy;
use crate::insert_history::HistoryTailReplacement;
#[cfg(any(windows, test))]
use crate::insert_history::InlineHistoryPlacement;
use crate::insert_history::InsertHistoryMode;
use crate::insert_history::PreparedHistoryLines;
#[cfg(any(windows, test))]
use crate::insert_history::append_history_hyperlink_lines_at_placement;
#[cfg(any(windows, test))]
use crate::insert_history::append_prepared_history_hyperlink_lines_at_placement;
use crate::insert_history::insert_history_hyperlink_lines_with_mode_and_wrap_policy;
use crate::insert_history::insert_prepared_history_hyperlink_lines_with_mode;
use crate::insert_history::prepare_history_hyperlink_lines;
#[cfg(any(windows, test))]
use crate::insert_history::record_inline_history_terminal_scroll;
#[cfg(any(windows, test))]
use crate::insert_history::repaint_inline_history_tail;
#[cfg(any(windows, test))]
use crate::insert_history::replace_history_tail_at_placement;
#[cfg(any(windows, test))]
use crate::insert_history::update_inline_history_for_viewport;
use crate::terminal_hyperlinks::HyperlinkLine;
use ratatui::backend::Backend;
use ratatui::layout::Position;
use ratatui::layout::Size;

use super::covered_history::CoveredHistoryPolicy;
#[cfg(any(windows, test))]
use super::covered_history::reconcile_after_draw;

#[derive(Debug)]
pub(super) struct PendingHistoryLines {
    pub(super) lines: Vec<HyperlinkLine>,
    pub(super) wrap_policy: HistoryLineWrapPolicy,
}

#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub(super) enum TranscriptReplayTarget {
    AlternateScreen,
    InlineViewport,
}

#[derive(Debug)]
struct PendingTranscriptReplay {
    target: TranscriptReplayTarget,
    history: Vec<PendingHistoryLines>,
}

#[derive(Debug)]
pub(super) struct PreparedTranscriptReplay {
    history: Vec<PreparedHistoryBatch>,
}

#[derive(Debug)]
struct PreparedHistoryBatch {
    lines: PreparedHistoryLines,
    mode: InsertHistoryMode,
}

impl PendingTranscriptReplay {
    fn new(
        target: TranscriptReplayTarget,
        lines: Vec<HyperlinkLine>,
        wrap_policy: HistoryLineWrapPolicy,
    ) -> Self {
        let mut replay = Self {
            target,
            history: Vec::new(),
        };
        replay.append(lines, wrap_policy);
        replay
    }

    fn append(&mut self, lines: Vec<HyperlinkLine>, wrap_policy: HistoryLineWrapPolicy) {
        if lines.is_empty() {
            return;
        }
        if let Some(last) = self.history.last_mut()
            && last.wrap_policy == wrap_policy
        {
            last.lines.extend(lines);
        } else {
            self.history
                .push(PendingHistoryLines { lines, wrap_policy });
        }
    }

    fn prepare(&self, wrap_width: usize, is_zellij: bool) -> PreparedTranscriptReplay {
        let history = self
            .history
            .iter()
            .map(|batch| {
                let mode = if is_zellij && batch.wrap_policy == HistoryLineWrapPolicy::Terminal {
                    InsertHistoryMode::ZellijRaw
                } else {
                    InsertHistoryMode::Standard
                };
                PreparedHistoryBatch {
                    lines: prepare_history_hyperlink_lines(
                        &batch.lines,
                        wrap_width,
                        batch.wrap_policy,
                    ),
                    mode,
                }
            })
            .collect();
        PreparedTranscriptReplay { history }
    }
}

#[derive(Debug, Default)]
pub(super) struct InlineViewportState {
    pending_transcript_replay: Option<PendingTranscriptReplay>,
    #[cfg(windows)]
    windows: WindowsInlineViewportState,
}

impl InlineViewportState {
    pub(super) fn reset(&mut self) {
        self.pending_transcript_replay = None;
        self.reset_platform_state();
    }

    fn reset_platform_state(&mut self) {
        #[cfg(windows)]
        self.windows.reset();
    }

    pub(super) fn queue_transcript_replay(
        &mut self,
        target: TranscriptReplayTarget,
        lines: Vec<HyperlinkLine>,
        wrap_policy: HistoryLineWrapPolicy,
    ) {
        self.pending_transcript_replay =
            Some(PendingTranscriptReplay::new(target, lines, wrap_policy));
    }

    pub(super) fn has_pending_transcript_replay(&self) -> bool {
        self.pending_transcript_replay.is_some()
    }

    pub(super) fn append_to_pending_transcript_replay(
        &mut self,
        lines: Vec<HyperlinkLine>,
        wrap_policy: HistoryLineWrapPolicy,
    ) {
        self.pending_transcript_replay
            .as_mut()
            .expect("transcript replay must be pending before appending rows")
            .append(lines, wrap_policy);
    }

    pub(super) fn prepare_pending_transcript_replay(
        &self,
        wrap_width: u16,
        is_zellij: bool,
    ) -> Option<PreparedTranscriptReplay> {
        self.pending_transcript_replay
            .as_ref()
            .map(|replay| replay.prepare(usize::from(wrap_width.max(1)), is_zellij))
    }

    pub(super) fn begin_transcript_replay<B>(
        &mut self,
        terminal: &mut Terminal<B>,
    ) -> io::Result<()>
    where
        B: Backend<Error = io::Error> + Write,
    {
        let Some(target) = self
            .pending_transcript_replay
            .as_ref()
            .map(|replay| replay.target)
        else {
            return Ok(());
        };

        self.reset_platform_state();
        self.anchor_viewport_for_transcript_replay(terminal);
        match target {
            TranscriptReplayTarget::AlternateScreen => terminal.clear_visible_screen(),
            TranscriptReplayTarget::InlineViewport => {
                terminal.clear_scrollback_and_visible_screen_ansi()
            }
        }
    }

    fn anchor_viewport_for_transcript_replay<B>(&self, terminal: &mut Terminal<B>)
    where
        B: Backend<Error = io::Error> + Write,
    {
        let mut area = terminal.viewport_area;
        area.y = 0;
        terminal.set_viewport_area(area);
    }

    pub(super) fn write_prepared_transcript_replay<B>(
        &mut self,
        terminal: &mut Terminal<B>,
        prepared: Option<&PreparedTranscriptReplay>,
    ) -> io::Result<()>
    where
        B: Backend<Error = io::Error> + Write,
    {
        let Some(replay) = prepared else {
            return Ok(());
        };
        self.write_prepared_history_batches(terminal, &replay.history)
    }

    pub(super) fn complete_transcript_replay(&mut self) {
        self.pending_transcript_replay = None;
    }

    pub(super) fn flush_pending_history_lines<B>(
        &mut self,
        terminal: &mut Terminal<B>,
        pending_history: &mut Vec<PendingHistoryLines>,
        is_zellij: bool,
    ) -> io::Result<()>
    where
        B: Backend<Error = io::Error> + Write,
    {
        self.write_history_batches(terminal, pending_history, is_zellij)?;
        pending_history.clear();
        Ok(())
    }

    fn write_history_batches<B>(
        &mut self,
        terminal: &mut Terminal<B>,
        batches: &[PendingHistoryLines],
        is_zellij: bool,
    ) -> io::Result<()>
    where
        B: Backend<Error = io::Error> + Write,
    {
        for batch in batches {
            let mode = if is_zellij && batch.wrap_policy == HistoryLineWrapPolicy::Terminal {
                InsertHistoryMode::ZellijRaw
            } else {
                InsertHistoryMode::Standard
            };
            if mode == InsertHistoryMode::Standard {
                self.append_standard_history(terminal, &batch.lines, batch.wrap_policy)?;
            } else {
                insert_history_hyperlink_lines_with_mode_and_wrap_policy(
                    terminal,
                    &batch.lines,
                    mode,
                    batch.wrap_policy,
                )?;
            }
        }
        Ok(())
    }

    fn write_prepared_history_batches<B>(
        &mut self,
        terminal: &mut Terminal<B>,
        batches: &[PreparedHistoryBatch],
    ) -> io::Result<()>
    where
        B: Backend<Error = io::Error> + Write,
    {
        for batch in batches {
            if batch.mode == InsertHistoryMode::Standard {
                #[cfg(windows)]
                self.windows
                    .append_prepared_standard_history(terminal, &batch.lines)?;
                #[cfg(not(windows))]
                insert_prepared_history_hyperlink_lines_with_mode(
                    terminal,
                    &batch.lines,
                    batch.mode,
                )?;
            } else {
                insert_prepared_history_hyperlink_lines_with_mode(
                    terminal,
                    &batch.lines,
                    batch.mode,
                )?;
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(super) fn pending_transcript_replay_lines_for_test(&self) -> Vec<HyperlinkLine> {
        self.pending_transcript_replay
            .iter()
            .flat_map(|replay| replay.history.iter())
            .flat_map(|batch| batch.lines.iter().cloned())
            .collect()
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

    pub(super) fn replace_visible_history_tail<B>(
        &mut self,
        terminal: &mut Terminal<B>,
        previous_lines: &[HyperlinkLine],
        replacement: &[HyperlinkLine],
        wrap_policy: HistoryLineWrapPolicy,
    ) -> io::Result<Option<HistoryTailReplacement>>
    where
        B: Backend<Error = io::Error> + Write,
    {
        #[cfg(windows)]
        {
            self.windows.replace_visible_history_tail(
                terminal,
                previous_lines,
                replacement,
                wrap_policy,
            )
        }
        #[cfg(not(windows))]
        {
            let _ = (terminal, previous_lines, replacement, wrap_policy);
            Ok(None)
        }
    }

    #[cfg(all(test, windows))]
    pub(super) fn retained_source_for_test(&self) -> Option<&[HyperlinkLine]> {
        self.windows.retained_source_for_test()
    }

    pub(super) fn pending_history_precedes_resize(
        &self,
        viewport_height: u16,
        screen_size: Size,
        current_top: u16,
    ) -> bool {
        if self.has_pending_transcript_replay() {
            return false;
        }
        #[cfg(windows)]
        {
            let requested_top = screen_size
                .height
                .saturating_sub(viewport_height.min(screen_size.height));
            self.windows
                .pending_history_precedes_resize(requested_top, current_top)
        }
        #[cfg(not(windows))]
        {
            let _ = (viewport_height, screen_size, current_top);
            false
        }
    }

    /// Clear the viewport if this platform's history insertion bypasses ratatui's diff state.
    pub(super) fn clear_viewport_after_history_flush<B>(
        &self,
        terminal: &mut Terminal<B>,
    ) -> io::Result<bool>
    where
        B: Backend<Error = io::Error> + Write,
    {
        #[cfg(windows)]
        {
            terminal.clear()?;
            Ok(true)
        }
        #[cfg(not(windows))]
        {
            let _ = terminal;
            Ok(false)
        }
    }

    pub(super) fn reconcile_after_draw<B>(
        &mut self,
        terminal: &mut Terminal<B>,
        policy: CoveredHistoryPolicy,
    ) -> io::Result<()>
    where
        B: Backend<Error = io::Error> + Write,
    {
        #[cfg(windows)]
        {
            self.windows.reconcile_after_draw(terminal, policy)
        }
        #[cfg(not(windows))]
        {
            let _ = (terminal, policy);
            Ok(())
        }
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

    #[cfg(test)]
    pub(super) fn retained_source_for_test(&self) -> Option<&[HyperlinkLine]> {
        self.placement
            .as_ref()
            .map(InlineHistoryPlacement::retained_lines)
    }

    pub(super) fn pending_history_precedes_resize(
        &self,
        requested_top: u16,
        current_top: u16,
    ) -> bool {
        self.placement
            .as_ref()
            .is_some_and(|placement| !placement.has_covered_rows() && requested_top <= current_top)
    }

    pub(super) fn replace_visible_history_tail<B>(
        &mut self,
        terminal: &mut Terminal<B>,
        previous_lines: &[HyperlinkLine],
        replacement: &[HyperlinkLine],
        wrap_policy: HistoryLineWrapPolicy,
    ) -> io::Result<Option<HistoryTailReplacement>>
    where
        B: Backend<Error = io::Error> + Write,
    {
        let Some(placement) = self.placement.as_mut() else {
            return Ok(None);
        };
        replace_history_tail_at_placement(
            terminal,
            previous_lines,
            replacement,
            wrap_policy,
            placement,
        )
        .map(Some)
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

    fn append_prepared_standard_history<B>(
        &mut self,
        terminal: &mut Terminal<B>,
        lines: &PreparedHistoryLines,
    ) -> io::Result<()>
    where
        B: Backend<Error = io::Error> + Write,
    {
        if let Some(placement) = self.placement.as_mut() {
            append_prepared_history_hyperlink_lines_at_placement(terminal, lines, placement)
        } else {
            insert_prepared_history_hyperlink_lines_with_mode(
                terminal,
                lines,
                InsertHistoryMode::Standard,
            )
        }
    }

    pub(super) fn reconcile_after_draw<B>(
        &mut self,
        terminal: &mut Terminal<B>,
        policy: CoveredHistoryPolicy,
    ) -> io::Result<()>
    where
        B: Backend<Error = io::Error> + Write,
    {
        if let Some(placement) = self.placement.as_mut()
            && placement.has_covered_rows()
        {
            reconcile_after_draw(terminal, placement, policy)?;
        }
        Ok(())
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
                if area.height < screen_size.height
                    && placement.visible_rows() == 0
                    && !placement.has_covered_rows()
                {
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

        let needs_history_repaint = self.placement.as_mut().is_some_and(|placement| {
            update_inline_history_for_viewport(terminal, placement, area.top())
        });

        if needs_history_repaint && let Some(placement) = self.placement.as_ref() {
            repaint_inline_history_tail(terminal, placement)?;
        }

        if self.placement.is_none() {
            let mut placement = InlineHistoryPlacement::new(
                area.top(),
                terminal.visible_history_rows().min(area.top()),
            );
            if first_viewport_reservation && previous_area.y <= area.y {
                placement.allow_viewport_growth_to(previous_area.y);
            }
            update_inline_history_for_viewport(terminal, &mut placement, area.top());
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
