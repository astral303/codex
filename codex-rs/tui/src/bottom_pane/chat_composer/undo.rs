//! Bounded undo and redo history for complete composer drafts.

use std::collections::VecDeque;
use std::mem::size_of;
use std::ops::Range;
use std::path::PathBuf;
use std::time::Duration;
use std::time::Instant;

use codex_protocol::user_input::TextElement;

use crate::bottom_pane::MentionBinding;

const MAX_HISTORY_ENTRIES: usize = 1000;
const MAX_RETAINED_BYTES: usize = 32 * 1024 * 1024;
pub(super) const TYPING_STEP_PAUSE: Duration = Duration::from_secs(1);
/// Limits how much continuous typing one undo removes.
const TYPING_STEP_MAX_SPAN: Duration = Duration::from_secs(1);

#[derive(Clone, Debug, Default, PartialEq)]
pub(in crate::bottom_pane) struct ComposerDraft {
    pub(in crate::bottom_pane) content: ComposerDraftContent,
    pub(super) cursor: usize,
}

impl ComposerDraft {
    pub(super) fn has_same_content(&self, other: &Self) -> bool {
        self.content == other.content
    }

    fn estimated_retained_bytes(&self) -> usize {
        size_of::<Self>() + self.content.estimated_retained_bytes()
    }
}

#[derive(Clone, Debug, Default, PartialEq)]
pub(in crate::bottom_pane) struct ComposerDraftContent {
    pub(in crate::bottom_pane) text: String,
    pub(super) text_elements: Vec<TextElement>,
    pub(super) local_image_paths: Vec<PathBuf>,
    pub(super) remote_image_urls: Vec<String>,
    pub(super) mention_bindings: Vec<MentionBinding>,
    pub(super) mention_ranges: Vec<Range<usize>>,
    pub(super) pending_pastes: Vec<(String, String)>,
}

impl ComposerDraftContent {
    /// Compares everything except the text; element and mention ranges may shift with the text.
    fn has_same_non_text_content(&self, other: &Self) -> bool {
        self.text_elements.len() == other.text_elements.len()
            && self.local_image_paths == other.local_image_paths
            && self.remote_image_urls == other.remote_image_urls
            && self.mention_bindings == other.mention_bindings
            && self.mention_ranges.len() == other.mention_ranges.len()
            && self.pending_pastes == other.pending_pastes
    }

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
                .sum::<usize>()
            + self.mention_ranges.len() * size_of::<Range<usize>>();
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

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum TypingEdit {
    Insert,
    Delete,
}

impl TypingEdit {
    /// Classifies a one-character insertion or deletion at the cursor. Any other edit is its own
    /// undo step.
    fn between(before: &ComposerDraft, after: &ComposerDraft) -> Option<Self> {
        if !before.content.has_same_non_text_content(&after.content) {
            return None;
        }
        let before_text = before.content.text.as_str();
        let after_text = after.content.text.as_str();
        if after_text.len() > before_text.len() {
            // Typing inserts the character at the cursor and moves the cursor past it.
            let inserted = before.cursor..after.cursor;
            let is_typed = after.cursor > before.cursor
                && after_text.len() - before_text.len() == inserted.len()
                && is_one_char(after_text.get(inserted)?)
                && after_text.get(..before.cursor)? == before_text.get(..before.cursor)?
                && after_text.get(after.cursor..)? == before_text.get(before.cursor..)?;
            is_typed.then_some(Self::Insert)
        } else {
            // Backspace removes the character before the cursor; Delete removes the one after it.
            let removed_len = before_text.len() - after_text.len();
            let removed = after.cursor..after.cursor + removed_len;
            let is_deleted = (removed.end == before.cursor || removed.start == before.cursor)
                && is_one_char(before_text.get(removed.clone())?)
                && after_text.get(..removed.start)? == before_text.get(..removed.start)?
                && after_text.get(removed.start..)? == before_text.get(removed.end..)?;
            is_deleted.then_some(Self::Delete)
        }
    }
}

fn is_one_char(text: &str) -> bool {
    let mut chars = text.chars();
    chars.next().is_some() && chars.next().is_none()
}

/// The newest undo step while it can still absorb the next typed character or deletion.
#[derive(Clone, Debug)]
struct OpenTypingStep {
    edit: TypingEdit,
    started_at: Instant,
    last_edit_at: Instant,
    edit_count: usize,
    /// The next keystroke extends the step only if it starts from this draft.
    draft_after: ComposerDraft,
    /// Any other history change after this epoch closes the step.
    mutation_epoch: u64,
}

impl OpenTypingStep {
    fn accepts(
        &self,
        edit: TypingEdit,
        before_edit: &ComposerDraft,
        edited_at: Instant,
        mutation_epoch: u64,
    ) -> bool {
        self.edit == edit
            && self.mutation_epoch == mutation_epoch
            && &self.draft_after == before_edit
            && edited_at.saturating_duration_since(self.last_edit_at) < TYPING_STEP_PAUSE
            && edited_at.saturating_duration_since(self.started_at) < TYPING_STEP_MAX_SPAN
    }
}

#[derive(Clone, Debug)]
struct StoredDraft {
    retained_bytes: usize,
    draft: ComposerDraft,
}

impl StoredDraft {
    fn new(draft: ComposerDraft) -> Self {
        Self {
            retained_bytes: draft.estimated_retained_bytes(),
            draft,
        }
    }
}

#[derive(Clone, Debug)]
pub(super) struct ComposerUndoHistory {
    undo: VecDeque<StoredDraft>,
    redo: VecDeque<StoredDraft>,
    retained_bytes: usize,
    max_entries: usize,
    max_retained_bytes: usize,
    /// Changes whenever an operation mutates either history stack.
    mutation_epoch: u64,
    open_typing_step: Option<OpenTypingStep>,
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
            open_typing_step: None,
        }
    }

    pub(super) fn mutation_epoch(&self) -> u64 {
        self.mutation_epoch
    }

    #[cfg(test)]
    pub(super) fn redo_depth(&self) -> usize {
        self.redo.len()
    }

    /// Record the state immediately before one content-changing user action.
    pub(super) fn record(&mut self, before_edit: ComposerDraft) -> bool {
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

    /// Record one key-driven edit. A typed character or one-character deletion that continues
    /// the open typing step joins it; any other edit records its own step.
    pub(super) fn record_keystroke(
        &mut self,
        before_edit: ComposerDraft,
        after_edit: &ComposerDraft,
        edited_at: Instant,
    ) -> bool {
        let Some(edit) = TypingEdit::between(&before_edit, after_edit) else {
            return self.record(before_edit);
        };
        let mutation_epoch = self.mutation_epoch;
        let extends_open_step = self
            .open_typing_step
            .as_ref()
            .is_some_and(|step| step.accepts(edit, &before_edit, edited_at, mutation_epoch));
        if extends_open_step {
            // Advance the epoch so the outer key dispatcher sees the keystroke as recorded.
            self.advance_mutation_epoch();
            let mutation_epoch = self.mutation_epoch;
            if let Some(step) = self.open_typing_step.as_mut() {
                step.last_edit_at = edited_at;
                step.edit_count += 1;
                step.draft_after = after_edit.clone();
                step.mutation_epoch = mutation_epoch;
            }
            return true;
        }
        if !self.record(before_edit) {
            return false;
        }
        self.open_typing_step = Some(OpenTypingStep {
            edit,
            started_at: edited_at,
            last_edit_at: edited_at,
            edit_count: 1,
            draft_after: after_edit.clone(),
            mutation_epoch: self.mutation_epoch,
        });
        true
    }

    pub(super) fn undo(&mut self, current: ComposerDraft) -> Option<ComposerDraft> {
        let target = self.pop_undo()?;
        self.push_redo_if_it_fits(current);
        self.trim_to_limits();
        self.advance_mutation_epoch();
        Some(target.draft)
    }

    pub(super) fn redo(&mut self, current: ComposerDraft) -> Option<ComposerDraft> {
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

    /// Replace this history with one saved earlier. The saved typing step is closed.
    pub(super) fn restore_saved(&mut self, saved: ComposerUndoHistory) {
        // Callers detect history changes by epoch, so the restored history takes a new epoch.
        let mutation_epoch = self.mutation_epoch.wrapping_add(1);
        *self = saved;
        self.mutation_epoch = mutation_epoch;
        self.open_typing_step = None;
    }

    pub(super) fn try_discard_provisional_edits(
        &mut self,
        provisional_edit_count: usize,
        expected_before_provisional_edits: &ComposerDraft,
        draft_with_provisional_edits: &ComposerDraft,
    ) -> bool {
        let mutation_epoch = self.mutation_epoch;
        let open_step_edit_count = self
            .open_typing_step
            .as_ref()
            .filter(|step| {
                step.edit == TypingEdit::Insert
                    && step.mutation_epoch == mutation_epoch
                    && &step.draft_after == draft_with_provisional_edits
            })
            .map(|step| step.edit_count);
        if let Some(open_step_edit_count) =
            open_step_edit_count.filter(|count| *count >= provisional_edit_count)
        {
            // The open typing step holds every provisional edit. Keep its entry when it also
            // holds earlier typing, because that entry precedes the earlier typing.
            if open_step_edit_count == provisional_edit_count {
                if self.undo.back().map(|stored| &stored.draft)
                    != Some(expected_before_provisional_edits)
                {
                    return false;
                }
                self.pop_undo();
            }
            self.open_typing_step = None;
            self.advance_mutation_epoch();
            return true;
        }

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

    fn push_undo_if_it_fits(&mut self, draft: ComposerDraft) {
        let draft = StoredDraft::new(draft);
        if draft.retained_bytes > self.max_retained_bytes {
            self.clear_undo();
            return;
        }
        self.retained_bytes += draft.retained_bytes;
        self.undo.push_back(draft);
    }

    fn push_redo_if_it_fits(&mut self, draft: ComposerDraft) {
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
