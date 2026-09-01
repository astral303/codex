//! Bounded undo and redo history for complete composer drafts.

use std::collections::VecDeque;
use std::mem::size_of;
use std::path::PathBuf;

use codex_protocol::user_input::TextElement;

use crate::bottom_pane::MentionBinding;

const MAX_HISTORY_ENTRIES: usize = 100;
const MAX_RETAINED_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, PartialEq)]
pub(super) struct EditableDraft {
    pub(super) content: EditableDraftContent,
    pub(super) cursor: usize,
}

impl EditableDraft {
    pub(super) fn has_same_content(&self, other: &Self) -> bool {
        self.content == other.content
    }

    fn estimated_retained_bytes(&self) -> usize {
        size_of::<Self>() + self.content.estimated_retained_bytes()
    }
}

#[derive(Clone, Debug, PartialEq)]
pub(super) struct EditableDraftContent {
    pub(super) text: String,
    pub(super) text_elements: Vec<TextElement>,
    pub(super) local_image_paths: Vec<PathBuf>,
    pub(super) remote_image_urls: Vec<String>,
    pub(super) mention_bindings: Vec<MentionBinding>,
    pub(super) pending_pastes: Vec<(String, String)>,
}

impl EditableDraftContent {
    fn estimated_retained_bytes(&self) -> usize {
        let text_element_bytes = self.text_elements.len() * size_of::<TextElement>()
            + self
                .text_elements
                .iter()
                .filter_map(|element| element.placeholder(&self.text))
                .map(str::len)
                .sum::<usize>();
        let local_image_bytes = self.local_image_paths.len() * size_of::<PathBuf>()
            + self
                .local_image_paths
                .iter()
                .map(|path| path.as_os_str().as_encoded_bytes().len())
                .sum::<usize>();
        let remote_image_bytes = self.remote_image_urls.len() * size_of::<String>()
            + self
                .remote_image_urls
                .iter()
                .map(String::len)
                .sum::<usize>();
        let mention_binding_bytes = self.mention_bindings.len() * size_of::<MentionBinding>()
            + self
                .mention_bindings
                .iter()
                .map(|binding| binding.mention.len() + binding.path.len())
                .sum::<usize>();
        let pending_paste_bytes = self.pending_pastes.len() * size_of::<(String, String)>()
            + self
                .pending_pastes
                .iter()
                .map(|(placeholder, pasted)| placeholder.len() + pasted.len())
                .sum::<usize>();

        self.text.len()
            + text_element_bytes
            + local_image_bytes
            + remote_image_bytes
            + mention_binding_bytes
            + pending_paste_bytes
    }
}

#[derive(Debug)]
struct StoredDraft {
    retained_bytes: usize,
    draft: EditableDraft,
}

impl StoredDraft {
    fn new(draft: EditableDraft) -> Self {
        Self {
            retained_bytes: draft.estimated_retained_bytes(),
            draft,
        }
    }
}

#[derive(Debug)]
pub(super) struct ComposerUndoHistory {
    undo: VecDeque<StoredDraft>,
    redo: VecDeque<StoredDraft>,
    retained_bytes: usize,
    max_entries: usize,
    max_retained_bytes: usize,
    /// Changes whenever an operation mutates either history stack.
    mutation_epoch: u64,
}

impl Default for ComposerUndoHistory {
    fn default() -> Self {
        Self::with_limits(MAX_HISTORY_ENTRIES, MAX_RETAINED_BYTES)
    }
}

impl ComposerUndoHistory {
    fn with_limits(max_entries: usize, max_retained_bytes: usize) -> Self {
        Self {
            undo: VecDeque::new(),
            redo: VecDeque::new(),
            retained_bytes: 0,
            max_entries,
            max_retained_bytes,
            mutation_epoch: 0,
        }
    }

    pub(super) fn mutation_epoch(&self) -> u64 {
        self.mutation_epoch
    }

    /// Record the state immediately before one content-changing user action.
    pub(super) fn record(&mut self, before_edit: EditableDraft) -> bool {
        self.clear_redo();
        let before_edit = StoredDraft::new(before_edit);
        if self.max_entries == 0 || before_edit.retained_bytes > self.max_retained_bytes {
            self.clear_stacks();
            self.advance_mutation_epoch();
            return false;
        }

        self.retained_bytes += before_edit.retained_bytes;
        self.undo.push_back(before_edit);
        self.trim_to_limits();
        self.advance_mutation_epoch();
        true
    }

    pub(super) fn undo(&mut self, current: EditableDraft) -> Option<EditableDraft> {
        let target = self.pop_undo()?;
        self.push_redo_if_it_fits(current);
        self.trim_to_limits();
        self.advance_mutation_epoch();
        Some(target.draft)
    }

    pub(super) fn redo(&mut self, current: EditableDraft) -> Option<EditableDraft> {
        let target = self.pop_redo()?;
        self.push_undo_if_it_fits(current);
        self.trim_to_limits();
        self.advance_mutation_epoch();
        Some(target.draft)
    }

    pub(super) fn clear(&mut self) {
        self.clear_stacks();
        self.advance_mutation_epoch();
    }

    pub(super) fn try_discard_provisional_edits(
        &mut self,
        provisional_edit_count: usize,
        expected_before_provisional_edits: &EditableDraft,
    ) -> bool {
        let Some(first_provisional_edit_index) =
            self.undo.len().checked_sub(provisional_edit_count)
        else {
            return false;
        };
        let Some(stored_before_provisional_edits) = self.undo.get(first_provisional_edit_index)
        else {
            return false;
        };
        if &stored_before_provisional_edits.draft != expected_before_provisional_edits {
            return false;
        }

        for _ in 0..provisional_edit_count {
            assert!(
                self.pop_undo().is_some(),
                "validated provisional composer edit must exist"
            );
        }
        self.advance_mutation_epoch();
        true
    }

    fn push_undo_if_it_fits(&mut self, draft: EditableDraft) {
        let draft = StoredDraft::new(draft);
        if draft.retained_bytes > self.max_retained_bytes {
            self.clear_undo();
            return;
        }
        self.retained_bytes += draft.retained_bytes;
        self.undo.push_back(draft);
    }

    fn push_redo_if_it_fits(&mut self, draft: EditableDraft) {
        let draft = StoredDraft::new(draft);
        if draft.retained_bytes > self.max_retained_bytes {
            self.clear_redo();
            return;
        }
        self.retained_bytes += draft.retained_bytes;
        self.redo.push_back(draft);
    }

    fn pop_undo(&mut self) -> Option<StoredDraft> {
        let draft = self.undo.pop_back()?;
        self.subtract_retained_bytes(draft.retained_bytes);
        Some(draft)
    }

    fn pop_redo(&mut self) -> Option<StoredDraft> {
        let draft = self.redo.pop_back()?;
        self.subtract_retained_bytes(draft.retained_bytes);
        Some(draft)
    }

    fn trim_to_limits(&mut self) {
        while self.undo.len() + self.redo.len() > self.max_entries
            || self.retained_bytes > self.max_retained_bytes
        {
            let removed = if self.redo.len() > 1 {
                self.redo.pop_front()
            } else if self.undo.len() > 1 {
                self.undo.pop_front()
            } else if !self.redo.is_empty() {
                self.redo.pop_front()
            } else {
                self.undo.pop_front()
            };
            let Some(removed) = removed else {
                break;
            };
            self.subtract_retained_bytes(removed.retained_bytes);
        }
    }

    fn clear_stacks(&mut self) {
        let removed_bytes = self
            .undo
            .iter()
            .chain(&self.redo)
            .map(|draft| draft.retained_bytes)
            .sum();
        self.subtract_retained_bytes(removed_bytes);
        assert_eq!(
            self.retained_bytes, 0,
            "composer undo history retained-byte accounting mismatch"
        );
        self.undo.clear();
        self.redo.clear();
    }

    fn clear_undo(&mut self) {
        let removed_bytes = self.undo.iter().map(|draft| draft.retained_bytes).sum();
        self.subtract_retained_bytes(removed_bytes);
        self.undo.clear();
    }

    fn clear_redo(&mut self) {
        let removed_bytes = self.redo.iter().map(|draft| draft.retained_bytes).sum();
        self.subtract_retained_bytes(removed_bytes);
        self.redo.clear();
    }

    fn subtract_retained_bytes(&mut self, removed_bytes: usize) {
        assert!(
            self.retained_bytes >= removed_bytes,
            "composer undo history retained-byte accounting underflow"
        );
        self.retained_bytes -= removed_bytes;
    }

    fn advance_mutation_epoch(&mut self) {
        self.mutation_epoch = self.mutation_epoch.wrapping_add(1);
    }
}

#[cfg(test)]
#[path = "undo_tests.rs"]
mod tests;
