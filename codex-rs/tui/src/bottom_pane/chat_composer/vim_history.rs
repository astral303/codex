//! Vim transaction boundaries for the shared composer undo history.
//!
//! The composer owns rich draft state that the textarea cannot restore by itself. Vim editing
//! therefore contributes complete draft snapshots to the same bounded history as ordinary editor
//! actions, while this module owns only the pending transaction needed to group a complete Vim
//! command or insert session into one step.

use super::super::textarea::VimPersistentState;
use super::ChatComposer;
use super::EditableDraft;
use crate::key_hint::KeyBindingListExt;
use crossterm::event::KeyEvent;

#[derive(Debug, Default)]
pub(super) struct VimEditTransaction {
    pending: Option<EditableDraft>,
}

impl VimEditTransaction {
    pub(super) fn is_active(&self) -> bool {
        self.pending.is_some()
    }
}

impl ChatComposer {
    /// Apply the Vim normal-mode undo binding to the shared composer history.
    pub(super) fn handle_vim_history_key(&mut self, event: KeyEvent) -> bool {
        if !self.draft.textarea.is_vim_normal_mode()
            || self.draft.textarea.is_vim_operator_pending()
            || self.popups.active()
            || !self.vim_normal_keymap.undo.is_pressed(event)
        {
            return false;
        }

        self.undo_edit();
        true
    }

    /// Snapshot only keys that can begin or complete a Vim edit transaction.
    pub(super) fn begin_vim_edit(&mut self, event: KeyEvent) {
        if !self.draft.textarea.is_vim_enabled()
            || self.vim_edit_transaction.is_active()
            || self.draft.textarea.is_vim_operator_pending()
        {
            return;
        }

        if self.draft.textarea.is_vim_normal_mode()
            && (self.vim_normal_keymap.move_left.is_pressed(event)
                || self.vim_normal_keymap.move_right.is_pressed(event)
                || self.vim_normal_keymap.move_up.is_pressed(event)
                || self.vim_normal_keymap.move_down.is_pressed(event)
                || self.vim_normal_keymap.move_word_forward.is_pressed(event)
                || self.vim_normal_keymap.move_word_backward.is_pressed(event)
                || self.vim_normal_keymap.move_word_end.is_pressed(event)
                || self.vim_normal_keymap.move_line_start.is_pressed(event)
                || self.vim_normal_keymap.move_line_end.is_pressed(event)
                || self.vim_normal_keymap.find_forward.is_pressed(event)
                || self.vim_normal_keymap.find_backward.is_pressed(event)
                || self.vim_normal_keymap.jump_top.is_pressed(event)
                || self.vim_normal_keymap.jump_bottom.is_pressed(event)
                || self.draft.textarea.wants_vim_search_key(event)
                || self.vim_normal_keymap.yank_line.is_pressed(event)
                || self.vim_normal_keymap.start_yank_operator.is_pressed(event)
                || self.vim_normal_keymap.cancel_operator.is_pressed(event))
        {
            return;
        }

        self.begin_vim_edit_transaction();
    }

    /// Start one standalone draft edit without splitting an active Vim command.
    pub(super) fn begin_direct_vim_edit(&mut self) -> bool {
        if !self.draft.textarea.is_vim_enabled()
            || self.draft.textarea.is_vim_operator_pending()
            || self.vim_edit_transaction.is_active()
            || self.history_search.is_some()
        {
            return false;
        }

        self.begin_vim_edit_transaction();
        if !self.vim_edit_transaction.is_active() {
            return false;
        }
        let mut vim_state = VimPersistentState::default();
        self.draft
            .textarea
            .swap_vim_persistent_state(&mut vim_state);
        vim_state.commands.last_change.clear();
        self.draft
            .textarea
            .swap_vim_persistent_state(&mut vim_state);
        true
    }

    fn begin_vim_edit_transaction(&mut self) {
        self.vim_edit_transaction.pending = Some(self.snapshot_draft());
    }

    /// Commit a complete normal-mode command or one insert-mode session.
    pub(super) fn finish_vim_edit(&mut self) {
        if !self.draft.textarea.is_vim_normal_mode()
            || self.draft.textarea.is_vim_operator_pending()
        {
            return;
        }

        self.commit_pending_vim_edit();
    }

    pub(super) fn commit_pending_vim_edit(&mut self) {
        let Some(before_edit) = self.vim_edit_transaction.pending.take() else {
            return;
        };
        self.record_edit_since(before_edit);
    }
}

#[cfg(test)]
#[path = "vim_history_tests.rs"]
mod tests;
