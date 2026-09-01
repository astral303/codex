//! Composer-level coordination for shared undo and redo history.
//!
//! Storage and byte bounds live in [`super::undo`]. This module owns the transitions that must
//! keep rich drafts, Vim transactions, history recall, and detected paste bursts on one timeline.

use std::time::Instant;

use super::super::chat_composer_history::HistoryNavigation;
use super::super::chat_composer_history::HistorySearchDirection;
use super::super::paste_burst::RetroGrab;
use super::super::textarea::VimPersistentState;
use super::ChatComposer;
use super::InputResult;
use super::draft_state::ComposerMentionBinding;
use super::undo::ComposerDraft;
use super::undo::ComposerDraftContent;
use super::vim_history::VimEditTransaction;

impl ChatComposer {
    pub(in crate::bottom_pane) fn snapshot_draft(&self) -> ComposerDraft {
        let (mention_bindings, mention_ranges) = self.snapshot_mention_bindings_with_ranges();
        ComposerDraft {
            content: ComposerDraftContent {
                text: self.current_text(),
                text_elements: self.current_text_elements(),
                local_image_paths: self.attachments.local_image_paths(),
                remote_image_urls: self.attachments.remote_image_urls(),
                mention_bindings,
                mention_ranges,
                pending_pastes: self.draft.pending_pastes.clone(),
            },
            cursor: self.current_cursor(),
        }
    }

    pub(in crate::bottom_pane) fn restore_draft(&mut self, draft: ComposerDraft) {
        let ComposerDraft { content, cursor } = draft;
        let ComposerDraftContent {
            text,
            text_elements,
            local_image_paths,
            remote_image_urls,
            mention_bindings,
            mention_ranges,
            pending_pastes,
        } = content;
        self.replace_remote_image_urls(remote_image_urls);
        self.replace_text_content_with_mention_bindings(
            text,
            text_elements,
            local_image_paths,
            Vec::new(),
        );
        for (binding, range) in mention_bindings.into_iter().zip(mention_ranges) {
            if let Some(id) = self.draft.textarea.element_id_for_exact_range(range) {
                self.draft.mention_bindings.insert(
                    id,
                    ComposerMentionBinding {
                        sigil: binding.sigil,
                        mention: binding.mention,
                        path: binding.path,
                    },
                );
            }
        }
        self.replace_pending_pastes(pending_pastes);
        self.set_current_cursor(cursor);
        self.sync_popups();
    }

    pub(super) fn record_edit_since(&mut self, before_edit: ComposerDraft) {
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

    fn restore_draft_preserving_vim_state(&mut self, draft: ComposerDraft) {
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
            HistorySearchDirection::Older => self.undo_history.undo(current),
            HistorySearchDirection::Newer => self.undo_history.redo(current),
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
        next_char: char,
        now: Instant,
    ) {
        let vim_transaction_owns_edit = self.vim_edit_transaction.is_active();
        let draft_before_retro_capture =
            (!vim_transaction_owns_edit).then(|| self.snapshot_draft());
        let provisional_edit_count = grab.grabbed.chars().count();
        // Vim replace mode restores overwritten characters rather than deleting them, so
        // retraction fails when the burst does not line up with the recorded replace steps.
        if !grab.grabbed.is_empty() && !self.draft.textarea.retract_paste_burst(grab.start_byte) {
            self.draft.paste_burst.clear_after_explicit_paste();
            self.insert_str_at_target_without_history(
                self.draft.textarea.edit_target(),
                &next_char.to_string(),
            );
            return;
        }

        if let Some(draft_before_retro_capture) = draft_before_retro_capture {
            let expected_before_provisional_edits = self.snapshot_draft();
            if !self.undo_history.try_discard_provisional_edits(
                provisional_edit_count,
                &expected_before_provisional_edits,
            ) {
                self.restore_draft(draft_before_retro_capture);
                self.draft.paste_burst.clear_after_explicit_paste();
                self.insert_str_at_target_without_history(
                    self.draft.textarea.edit_target(),
                    &next_char.to_string(),
                );
                return;
            }
        }

        self.draft.paste_burst.append_char_to_buffer(next_char, now);
    }
}
