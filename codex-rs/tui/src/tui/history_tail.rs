//! Update visible terminal-history tails without discarding terminal scrollback.

use super::Tui;
use crate::custom_terminal::Terminal as CustomTerminal;
use crate::insert_history::HistoryLineWrapPolicy;
use crate::insert_history::HistoryTailReplacement;
use crate::insert_history::InsertHistoryMode;
use crate::insert_history::insert_history_hyperlink_lines_with_mode_and_wrap_policy;
use crate::insert_history::wrap_history_hyperlink_lines;
use crate::terminal_hyperlinks::HyperlinkLine;
use ratatui::backend::Backend;
use ratatui::layout::Position;
use std::io;
use std::io::Write;

impl Tui {
    #[cfg(test)]
    pub(crate) fn pending_history_lines_for_test(&self) -> Vec<HyperlinkLine> {
        let mut lines = self
            .inline_viewport
            .pending_transcript_replay_lines_for_test();
        lines.extend(
            self.pending_history_lines
                .iter()
                .flat_map(|batch| batch.lines.iter().cloned()),
        );
        lines
    }

    #[cfg(test)]
    pub(crate) fn transcript_replay_is_pending_for_test(&self) -> bool {
        self.inline_viewport.has_pending_transcript_replay()
    }

    #[cfg(all(test, windows))]
    pub(crate) fn tracked_history_source_for_test(&self) -> Option<&[HyperlinkLine]> {
        self.inline_viewport.retained_source_for_test()
    }

    pub(crate) fn replace_visible_history_tail(
        &mut self,
        previous_lines: &[HyperlinkLine],
        replacement: &[HyperlinkLine],
        wrap_policy: HistoryLineWrapPolicy,
    ) -> io::Result<HistoryTailReplacement> {
        if self.inline_viewport.has_pending_transcript_replay() {
            return Ok(HistoryTailReplacement::RequiresTranscriptReflow);
        }
        self.inline_viewport.flush_pending_history_lines(
            &mut self.terminal,
            &mut self.pending_history_lines,
            self.is_zellij,
        )?;
        if let Some(outcome) = self.inline_viewport.replace_visible_history_tail(
            &mut self.terminal,
            previous_lines,
            replacement,
            wrap_policy,
        )? {
            if outcome == HistoryTailReplacement::Replaced {
                self.frame_requester().schedule_frame();
            }
            return Ok(outcome);
        }
        let mode = if self.is_zellij && wrap_policy == HistoryLineWrapPolicy::Terminal {
            InsertHistoryMode::ZellijRaw
        } else {
            InsertHistoryMode::Standard
        };
        let outcome = replace_visible_terminal_history_tail(
            &mut self.terminal,
            previous_lines,
            replacement,
            mode,
            wrap_policy,
        )?;
        if outcome == HistoryTailReplacement::Replaced {
            self.frame_requester().schedule_frame();
        }
        Ok(outcome)
    }
}

fn replace_visible_terminal_history_tail<B>(
    terminal: &mut CustomTerminal<B>,
    previous_lines: &[HyperlinkLine],
    replacement: &[HyperlinkLine],
    mode: InsertHistoryMode,
    wrap_policy: HistoryLineWrapPolicy,
) -> io::Result<HistoryTailReplacement>
where
    B: Backend<Error = io::Error> + Write,
{
    let mut viewport = terminal.viewport_area;
    let wrap_width = usize::from(viewport.width.max(/*other*/ 1));
    let (_, previous_rows) = wrap_history_hyperlink_lines(previous_lines, wrap_width, wrap_policy);
    let Ok(previous_rows) = u16::try_from(previous_rows) else {
        return Ok(HistoryTailReplacement::NotVisible);
    };
    if previous_rows == 0 || previous_rows > viewport.top() {
        return Ok(HistoryTailReplacement::NotVisible);
    }

    viewport.y -= previous_rows;
    terminal.clear_after_position(Position::new(/*x*/ 0, viewport.y))?;
    terminal.set_viewport_area(viewport);
    terminal.invalidate_viewport();
    insert_history_hyperlink_lines_with_mode_and_wrap_policy(
        terminal,
        replacement,
        mode,
        wrap_policy,
    )?;
    Ok(HistoryTailReplacement::Replaced)
}

#[cfg(test)]
#[path = "history_tail_tests.rs"]
mod tests;
