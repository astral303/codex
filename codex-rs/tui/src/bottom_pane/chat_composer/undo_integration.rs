//! Composer-level coordination for shared undo and redo history.
//!
//! Storage and byte bounds live in [`super::undo`]. This module owns the transitions that must
//! keep rich drafts, Vim transactions, history recall, and detected paste bursts on one timeline.

use std::time::Instant;

use super::super::chat_composer_history::HistoryDirection;
use super::super::chat_composer_history::HistoryNavigation;
use super::super::paste_burst::RetroGrab;
use super::super::textarea::VimPersistentState;
use super::ChatComposer;
use super::InputResult;
use super::undo::EditableDraft;
use super::undo::EditableDraftContent;
use super::vim_history::VimEditTransaction;

impl ChatComposer {
    pub(super) fn snapshot_draft(&self) -> EditableDraft {
        EditableDraft {
            content: EditableDraftContent {
                text: self.current_text(),
                text_elements: self.current_text_elements(),
                local_image_paths: self.attachments.local_image_paths(),
                remote_image_urls: self.attachments.remote_image_urls(),
                mention_bindings: self.snapshot_mention_bindings(),
                pending_pastes: self.draft.pending_pastes.clone(),
            },
            cursor: self.current_cursor(),
        }
    }

    pub(super) fn restore_draft(&mut self, draft: EditableDraft) {
        let EditableDraft { content, cursor } = draft;
        let EditableDraftContent {
            text,
            text_elements,
            local_image_paths,
            remote_image_urls,
            mention_bindings,
            pending_pastes,
        } = content;
        self.replace_remote_image_urls(remote_image_urls);
        self.replace_text_content_with_mention_bindings(
            text,
            text_elements,
            local_image_paths,
            mention_bindings,
        );
        self.replace_pending_pastes(pending_pastes);
        self.set_current_cursor(cursor);
        self.sync_popups();
    }

    pub(super) fn record_edit_since(&mut self, before_edit: EditableDraft) {
        if self.vim_edit_transaction.is_active() {
            return;
        }
        let after_edit = self.snapshot_draft();
        if before_edit.has_same_content(&after_edit) {
            return;
        }
        self.undo_history.record(before_edit);
    }

    pub(super) fn establish_undo_baseline(&mut self) {
        self.vim_edit_transaction = VimEditTransaction::default();
        self.undo_history.clear();
    }

    pub(super) fn undo_edit(&mut self) -> bool {
        self.finish_pending_vim_edit_for_history_action();
        let current = self.snapshot_draft();
        let Some(draft) = self.undo_history.undo(current) else {
            return false;
        };
        self.restore_draft_preserving_vim_state(draft);
        self.draft.textarea.enter_vim_normal_mode();
        true
    }

    pub(super) fn redo_edit(&mut self) -> bool {
        self.finish_pending_vim_edit_for_history_action();
        let current = self.snapshot_draft();
        let Some(draft) = self.undo_history.redo(current) else {
            return false;
        };
        self.restore_draft_preserving_vim_state(draft);
        self.draft.textarea.enter_vim_normal_mode();
        true
    }

    pub(super) fn finish_pending_vim_edit_for_history_action(&mut self) {
        if self.vim_edit_transaction.is_active() {
            self.draft.textarea.finish_vim_insert_session();
            self.commit_pending_vim_edit();
        }
    }

    fn restore_draft_preserving_vim_state(&mut self, draft: EditableDraft) {
        let mut vim_state = VimPersistentState::default();
        self.draft
            .textarea
            .swap_vim_persistent_state(&mut vim_state);
        self.restore_draft(draft);
        self.draft
            .textarea
            .swap_vim_persistent_state(&mut vim_state);
    }

    pub(super) fn apply_history_navigation(&mut self, navigation: HistoryNavigation) {
        let current = self.snapshot_draft();
        self.apply_history_entry(navigation.entry);
        let recalled = self.snapshot_draft();
        let adjacent_draft = match navigation.direction {
            HistoryDirection::Older => self.undo_history.undo(current),
            HistoryDirection::Newer => self.undo_history.redo(current),
        };
        match adjacent_draft {
            Some(draft) if draft.has_same_content(&recalled) => {
                self.restore_draft_preserving_vim_state(draft);
                self.move_cursor_to_history_entry_end();
            }
            Some(_) | None => self.establish_undo_baseline(),
        }
    }

    pub(super) fn dispatch_with_undo_history(
        &mut self,
        dispatch: impl FnOnce(&mut Self) -> (InputResult, bool),
    ) -> (InputResult, bool) {
        let before_edit = self.snapshot_draft();
        let history_epoch_before_dispatch = self.undo_history.mutation_epoch();
        let result = dispatch(self);
        self.reset_vim_mode_after_successful_dispatch(&result.0);
        // Direct paste and Vim handlers may already have recorded the same user action.
        let nested_handler_mutated_history =
            self.undo_history.mutation_epoch() != history_epoch_before_dispatch;
        if matches!(
            &result.0,
            InputResult::Submitted { .. }
                | InputResult::Queued { .. }
                | InputResult::Command(_)
                | InputResult::ServiceTierCommand(_)
                | InputResult::CommandWithArgs(_, _, _)
        ) {
            self.establish_undo_baseline();
        } else if !nested_handler_mutated_history {
            self.record_edit_since(before_edit);
        }
        self.sync_popups();
        result
    }

    pub(super) fn move_retro_capture_to_paste_buffer(
        &mut self,
        grab: RetroGrab,
        end_byte: usize,
        next_char: char,
        now: Instant,
    ) {
        let vim_transaction_owns_edit = self.vim_edit_transaction.is_active();
        let draft_before_retro_capture =
            (!vim_transaction_owns_edit).then(|| self.snapshot_draft());
        let provisional_edit_count = grab.grabbed.chars().count();
        self.draft
            .textarea
            .replace_range(grab.start_byte..end_byte, "");

        if let Some(draft_before_retro_capture) = draft_before_retro_capture {
            let expected_before_provisional_edits = self.snapshot_draft();
            if !self.undo_history.try_discard_provisional_edits(
                provisional_edit_count,
                &expected_before_provisional_edits,
            ) {
                self.restore_draft(draft_before_retro_capture);
                self.draft.paste_burst.clear_after_explicit_paste();
                self.insert_str_without_history(&next_char.to_string());
                return;
            }
        }

        self.draft.paste_burst.append_char_to_buffer(next_char, now);
    }
}
