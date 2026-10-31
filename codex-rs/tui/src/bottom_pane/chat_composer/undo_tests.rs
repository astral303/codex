use super::*;
use pretty_assertions::assert_eq;

fn draft(text: &str) -> ComposerDraft {
    draft_at(text, text.len())
}

fn draft_at(text: &str, cursor: usize) -> ComposerDraft {
    ComposerDraft {
        content: ComposerDraftContent {
            text: text.to_string(),
            text_elements: Vec::new(),
            local_image_paths: Vec::new(),
            remote_image_urls: Vec::new(),
            mention_bindings: Vec::new(),
            mention_ranges: Vec::new(),
            pending_pastes: Vec::new(),
        },
        cursor,
    }
}

/// Types each text in turn, one keystroke per entry, at the given offsets from `start`.
fn type_keystrokes(
    history: &mut ComposerUndoHistory,
    start: Instant,
    keystrokes: &[(&str, &str, u64)],
) {
    for (before, after, offset_ms) in keystrokes {
        history.record_keystroke(
            draft(before),
            &draft(after),
            start + Duration::from_millis(*offset_ms),
        );
    }
}

#[test]
fn continuous_typing_within_a_second_is_one_undo_step() {
    let mut history = ComposerUndoHistory::default();
    type_keystrokes(
        &mut history,
        Instant::now(),
        &[("", "a", 0), ("a", "ab", 300), ("ab", "abc", 600)],
    );

    assert_eq!(history.undo(draft("abc")), Some(draft("")));
    assert_eq!(history.undo(draft("")), None);
}

#[test]
fn a_one_second_pause_starts_a_new_typing_step() {
    let mut history = ComposerUndoHistory::default();
    type_keystrokes(
        &mut history,
        Instant::now(),
        &[("", "a", 0), ("a", "ab", 1000)],
    );

    assert_eq!(history.undo(draft("ab")), Some(draft("a")));
    assert_eq!(history.undo(draft("a")), Some(draft("")));
}

#[test]
fn one_second_of_continuous_typing_closes_the_step() {
    let mut history = ComposerUndoHistory::default();
    type_keystrokes(
        &mut history,
        Instant::now(),
        &[
            ("", "a", 0),
            ("a", "ab", 300),
            ("ab", "abc", 600),
            ("abc", "abcd", 900),
            ("abcd", "abcde", 1200),
        ],
    );

    assert_eq!(history.undo(draft("abcde")), Some(draft("abcd")));
    assert_eq!(history.undo(draft("abcd")), Some(draft("")));
}

#[test]
fn switching_between_typing_and_deleting_starts_a_new_step() {
    let mut history = ComposerUndoHistory::default();
    type_keystrokes(
        &mut history,
        Instant::now(),
        &[
            ("", "a", 0),
            ("a", "ab", 100),
            ("ab", "a", 200),
            ("a", "", 300),
        ],
    );

    assert_eq!(history.undo(draft("")), Some(draft("ab")));
    assert_eq!(history.undo(draft("ab")), Some(draft("")));
}

#[test]
fn typing_after_a_cursor_move_starts_a_new_step() {
    let mut history = ComposerUndoHistory::default();
    let start = Instant::now();
    history.record_keystroke(draft(""), &draft("a"), start);
    history.record_keystroke(
        draft_at("a", 0),
        &draft_at("ba", 1),
        start + Duration::from_millis(100),
    );

    assert_eq!(history.undo(draft_at("ba", 1)), Some(draft_at("a", 0)));
    assert_eq!(history.undo(draft_at("a", 0)), Some(draft("")));
}

#[test]
fn a_multi_character_edit_is_its_own_step() {
    let mut history = ComposerUndoHistory::default();
    type_keystrokes(
        &mut history,
        Instant::now(),
        &[
            ("", "a", 0),
            ("a", "a word", 100),
            ("a word", "a words", 200),
        ],
    );

    assert_eq!(history.undo(draft("a words")), Some(draft("a word")));
    assert_eq!(history.undo(draft("a word")), Some(draft("a")));
    assert_eq!(history.undo(draft("a")), Some(draft("")));
}

#[test]
fn undo_and_redo_close_the_typing_step() {
    let mut history = ComposerUndoHistory::default();
    let start = Instant::now();
    history.record_keystroke(draft(""), &draft("a"), start);
    assert_eq!(history.undo(draft("a")), Some(draft("")));
    assert_eq!(history.redo(draft("")), Some(draft("a")));

    history.record_keystroke(draft("a"), &draft("ab"), start + Duration::from_millis(100));

    assert_eq!(history.undo(draft("ab")), Some(draft("a")));
}

#[test]
fn discarding_provisional_edits_drops_a_typing_step_that_holds_only_them() {
    let mut history = ComposerUndoHistory::default();
    history.record(draft(""));
    type_keystrokes(
        &mut history,
        Instant::now(),
        &[("stable", "stable界", 0), ("stable界", "stable界界", 0)],
    );

    assert!(history.try_discard_provisional_edits(
        /*provisional_edit_count*/ 2,
        &draft("stable"),
        &draft("stable界界"),
    ));
    history.record(draft("stable"));

    assert_eq!(history.undo(draft("stable界界")), Some(draft("stable")));
    assert_eq!(history.undo(draft("stable")), Some(draft("")));
}

#[test]
fn discarding_provisional_edits_keeps_earlier_typing_in_the_step() {
    let mut history = ComposerUndoHistory::default();
    type_keystrokes(
        &mut history,
        Instant::now(),
        &[("", "a", 0), ("a", "a界", 0), ("a界", "a界界", 0)],
    );

    assert!(history.try_discard_provisional_edits(
        /*provisional_edit_count*/ 2,
        &draft("a"),
        &draft("a界界"),
    ));
    history.record(draft("a"));

    assert_eq!(history.undo(draft("a界界")), Some(draft("a")));
    assert_eq!(history.undo(draft("a")), Some(draft("")));
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
    let mut history = ComposerUndoHistory::with_limits(
        /*max_entries*/ 2,
        /*max_retained_bytes*/ usize::MAX,
    );
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
    let mut history = ComposerUndoHistory::with_limits(/*max_entries*/ 10, limit);
    history.record(draft("old"));

    assert!(history.record(newest.clone()));

    assert_eq!(history.undo(draft("current")), Some(newest));
    assert_eq!(history.undo(draft("newest")), None);
}

#[test]
fn oversized_draft_clears_older_history() {
    let small = draft("small");
    let mut history =
        ComposerUndoHistory::with_limits(/*max_entries*/ 10, small.estimated_retained_bytes());
    history.record(small);

    assert!(!history.record(draft("a draft that cannot fit")));
    assert_eq!(history.undo(draft("current")), None);
}

#[test]
#[should_panic(expected = "composer undo history retained-byte accounting underflow")]
fn retained_byte_underflow_is_not_hidden() {
    let mut history = ComposerUndoHistory::default();
    history.record(draft("before"));
    history.retained_bytes = 0;

    let _ = history.undo(draft("after"));
}

#[test]
fn discarding_provisional_edits_preserves_earlier_history() {
    let mut history = ComposerUndoHistory::default();
    history.record(draft(""));
    history.record(draft("stable"));
    history.record(draft("stable界"));

    assert!(history.try_discard_provisional_edits(
        /*provisional_edit_count*/ 2,
        &draft("stable"),
        &draft("stable界界"),
    ));
    history.record(draft("stable"));

    assert_eq!(history.undo(draft("stable界界")), Some(draft("stable")));
    assert_eq!(history.undo(draft("stable")), Some(draft("")));
}

#[test]
fn invalid_provisional_edit_boundary_preserves_history() {
    let mut history = ComposerUndoHistory::default();
    history.record(draft(""));
    history.record(draft("stable"));
    history.record(draft("stable界"));
    let mutation_epoch = history.mutation_epoch();

    assert!(!history.try_discard_provisional_edits(
        /*provisional_edit_count*/ 4,
        &draft("stable"),
        &draft("stable界界"),
    ));
    assert!(!history.try_discard_provisional_edits(
        /*provisional_edit_count*/ 2,
        &draft("wrong baseline"),
        &draft("stable界界"),
    ));
    assert_eq!(history.mutation_epoch(), mutation_epoch);
    assert_eq!(history.undo(draft("stable界界")), Some(draft("stable界")));
    assert_eq!(history.undo(draft("stable界")), Some(draft("stable")));
    assert_eq!(history.undo(draft("stable")), Some(draft("")));
}
