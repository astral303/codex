use super::*;
use pretty_assertions::assert_eq;

fn draft(text: &str) -> EditableDraft {
    EditableDraft {
        content: EditableDraftContent {
            text: text.to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
            mention_bindings: Vec::new(),
            pending_pastes: Vec::new(),
        },
        cursor: text.len(),
    }
}

#[test]
fn undo_and_redo_walk_drafts_in_reverse_order() {
    let mut history = ComposerUndoHistory::default();
    history.record(draft(""));
    history.record(draft("a"));

    assert_eq!(history.undo(draft("ab")), Some(draft("a")));
    assert_eq!(history.undo(draft("a")), Some(draft("")));
    assert_eq!(history.redo(draft("")), Some(draft("a")));
    assert_eq!(history.redo(draft("a")), Some(draft("ab")));
}

#[test]
fn divergent_edit_discards_redo_history() {
    let mut history = ComposerUndoHistory::default();
    history.record(draft(""));
    assert_eq!(history.undo(draft("a")), Some(draft("")));

    history.record(draft(""));

    assert_eq!(history.redo(draft("b")), None);
}

#[test]
fn entry_limit_evicts_oldest_drafts_first() {
    let mut history = ComposerUndoHistory::with_limits(2, usize::MAX);
    history.record(draft(""));
    history.record(draft("a"));
    history.record(draft("ab"));

    assert_eq!(history.undo(draft("abc")), Some(draft("ab")));
    assert_eq!(history.undo(draft("ab")), Some(draft("a")));
    assert_eq!(history.undo(draft("a")), None);
}

#[test]
fn byte_limit_keeps_the_newest_draft_that_fits() {
    let newest = draft("newest");
    let limit = newest.estimated_retained_bytes();
    let mut history = ComposerUndoHistory::with_limits(10, limit);
    history.record(draft("old"));

    assert!(history.record(newest.clone()));

    assert_eq!(history.undo(draft("current")), Some(newest));
    assert_eq!(history.undo(draft("newest")), None);
}

#[test]
fn oversized_draft_clears_older_history() {
    let small = draft("small");
    let mut history = ComposerUndoHistory::with_limits(10, small.estimated_retained_bytes());
    history.record(small);

    assert!(!history.record(draft("a draft that cannot fit")));
    assert_eq!(history.undo(draft("current")), None);
}
