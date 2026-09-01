use std::path::PathBuf;
use std::time::Instant;

use codex_config::types::KeybindingSpec;
use codex_config::types::KeybindingsSpec;
use codex_config::types::TuiKeymap;
use crossterm::event::KeyCode;
use crossterm::event::KeyEvent;
use crossterm::event::KeyModifiers;
use pretty_assertions::assert_eq;
use tokio::sync::mpsc::UnboundedReceiver;
use tokio::sync::mpsc::unbounded_channel;

use super::*;
use crate::app_event::AppEvent;
use crate::bottom_pane::AppEventSender;
const UNDO_KEY: KeyCode = KeyCode::F(2);
const REDO_KEY: KeyCode = KeyCode::F(3);
const MINIMUM_NON_ASCII_RETRO_CAPTURE_INPUT: &str = "界\u{3000}界";

fn new_composer() -> (ChatComposer, UnboundedReceiver<AppEvent>) {
    let (tx, rx) = unbounded_channel();
    let mut composer = ChatComposer::new(
        /*has_input_focus*/ true,
        AppEventSender::new(tx),
        /*enhanced_keys_supported*/ false,
        "Ask Codex to do anything".to_string(),
        /*disable_paste_burst*/ true,
    );
    let mut config = TuiKeymap::default();
    config.composer.undo = Some(KeybindingsSpec::One(KeybindingSpec("f2".to_string())));
    config.composer.redo = Some(KeybindingsSpec::One(KeybindingSpec("f3".to_string())));
    let keymap = RuntimeKeymap::from_config(&config).expect("valid undo test bindings");
    composer.set_keymap_bindings(&keymap);
    (composer, rx)
}

fn press(composer: &mut ChatComposer, code: KeyCode) -> bool {
    composer
        .handle_key_event(KeyEvent::new(code, KeyModifiers::NONE))
        .1
}

fn press_at(composer: &mut ChatComposer, code: KeyCode, now: Instant) -> bool {
    composer
        .dispatch_with_undo_history(|composer| {
            composer.handle_input_basic_with_time(KeyEvent::new(code, KeyModifiers::NONE), now)
        })
        .1
}

fn at_history_boundary(mut draft: EditableDraft) -> EditableDraft {
    draft.cursor = draft.content.text.len();
    draft
}

#[test]
fn undo_walks_typing_then_explicit_paste_in_reverse_order() {
    let (mut composer, _rx) = new_composer();
    composer.set_text_content("existing prompt".to_string(), Vec::new(), Vec::new());
    composer.move_cursor_to_end();
    let before_paste = composer.snapshot_draft();

    composer.handle_paste("\npasted line".to_string());
    let after_paste = composer.snapshot_draft();
    press(&mut composer, KeyCode::Char('!'));
    let after_typing = composer.snapshot_draft();

    assert!(press(&mut composer, UNDO_KEY));
    assert_eq!(composer.snapshot_draft(), after_paste);
    assert!(press(&mut composer, UNDO_KEY));
    assert_eq!(composer.snapshot_draft(), before_paste);
    assert!(press(&mut composer, REDO_KEY));
    assert_eq!(composer.snapshot_draft(), after_paste);
    assert!(press(&mut composer, REDO_KEY));
    assert_eq!(composer.snapshot_draft(), after_typing);
}

#[test]
fn key_dispatch_does_not_record_a_nested_edit_twice() {
    let (mut composer, _rx) = new_composer();

    assert!(press(&mut composer, KeyCode::Char('x')));
    assert!(press(&mut composer, UNDO_KEY));
    assert_eq!(composer.current_text(), "");
    assert!(!press(&mut composer, UNDO_KEY));
}

#[test]
fn ctrl_c_undo_restores_the_complete_draft_and_redo_clears_it() {
    let (mut composer, _rx) = new_composer();
    composer.set_text_content("!echo ".to_string(), Vec::new(), Vec::new());
    composer.move_cursor_to_end();
    composer.set_remote_image_urls(vec!["https://example.test/remote.png".to_string()]);
    composer.handle_paste("x".repeat(LARGE_PASTE_CHAR_THRESHOLD + 1));
    composer.attach_image(PathBuf::from("C:/images/local.png"));
    composer.set_current_cursor(/*cursor*/ 2);
    composer.establish_undo_baseline();
    let before_clear = composer.snapshot_draft();

    assert!(composer.clear_for_ctrl_c().is_some());
    let after_clear = composer.snapshot_draft();
    assert!(press(&mut composer, UNDO_KEY));
    assert_eq!(composer.snapshot_draft(), before_clear);
    assert!(press(&mut composer, REDO_KEY));
    assert_eq!(composer.snapshot_draft(), after_clear);
}

#[test]
fn history_up_reuses_ctrl_c_undo_lineage_without_becoming_an_undo_step() {
    let (mut composer, _rx) = new_composer();
    assert!(press(&mut composer, KeyCode::Char('a')));
    let before_second_edit = composer.snapshot_draft();
    assert!(press(&mut composer, KeyCode::Char('b')));
    composer.set_current_cursor(/*cursor*/ 1);
    let before_clear = composer.snapshot_draft();

    assert!(composer.clear_for_ctrl_c().is_some());
    let after_clear = composer.snapshot_draft();
    assert!(press(&mut composer, KeyCode::Up));
    let recalled = at_history_boundary(before_clear);
    assert_eq!(composer.snapshot_draft(), recalled);

    assert!(press(&mut composer, UNDO_KEY));
    assert_eq!(composer.snapshot_draft(), before_second_edit);
    assert!(press(&mut composer, REDO_KEY));
    assert_eq!(composer.snapshot_draft(), recalled);
    assert!(press(&mut composer, REDO_KEY));
    assert_eq!(composer.snapshot_draft(), after_clear);
}

#[test]
fn matching_history_recall_keeps_repeated_navigation_at_the_history_boundary() {
    let (mut composer, _rx) = new_composer();
    composer
        .history
        .record_local_submission(HistoryEntry::new("older prompt".to_string()));
    assert!(press(&mut composer, KeyCode::Char('a')));
    assert!(press(&mut composer, KeyCode::Char('b')));
    composer.set_current_cursor(/*cursor*/ 1);
    assert!(composer.clear_for_ctrl_c().is_some());

    assert!(press(&mut composer, KeyCode::Up));
    assert_eq!(
        (composer.current_text(), composer.cursor()),
        ("ab".into(), 2)
    );
    assert!(press(&mut composer, KeyCode::Up));
    assert_eq!(composer.current_text(), "older prompt");
}

#[test]
fn history_down_reuses_ctrl_c_redo_lineage() {
    let (mut composer, _rx) = new_composer();
    assert!(press(&mut composer, KeyCode::Char('a')));
    let before_second_edit = composer.snapshot_draft();
    assert!(press(&mut composer, KeyCode::Char('b')));
    let before_clear = composer.snapshot_draft();

    assert!(composer.clear_for_ctrl_c().is_some());
    let after_clear = composer.snapshot_draft();
    assert!(press(&mut composer, KeyCode::Up));
    assert_eq!(composer.snapshot_draft(), before_clear);
    assert!(press(&mut composer, KeyCode::Down));
    assert_eq!(composer.snapshot_draft(), after_clear);

    assert!(press(&mut composer, UNDO_KEY));
    assert_eq!(composer.snapshot_draft(), before_clear);
    assert!(press(&mut composer, UNDO_KEY));
    assert_eq!(composer.snapshot_draft(), before_second_edit);
}

#[test]
fn unrelated_history_recall_starts_a_new_undo_baseline() {
    let (mut composer, _rx) = new_composer();
    assert!(press(&mut composer, KeyCode::Char('a')));
    assert!(press(&mut composer, KeyCode::Char('b')));
    assert!(composer.clear_for_ctrl_c().is_some());
    composer
        .history
        .record_local_submission(HistoryEntry::new("unrelated prompt".to_string()));

    assert!(press(&mut composer, KeyCode::Up));
    assert_eq!(composer.current_text(), "unrelated prompt");
    assert!(!press(&mut composer, UNDO_KEY));
}

#[test]
fn equal_text_without_rich_draft_state_does_not_reuse_undo_lineage() {
    let (mut composer, _rx) = new_composer();
    composer.set_text_content("same text".to_string(), Vec::new(), Vec::new());
    composer.set_remote_image_urls(vec!["https://example.test/remote.png".to_string()]);
    composer.move_cursor_to_end();
    assert!(composer.clear_for_ctrl_c().is_some());
    composer
        .history
        .record_local_submission(HistoryEntry::new("same text".to_string()));

    assert!(press(&mut composer, KeyCode::Up));
    assert_eq!(composer.current_text(), "same text");
    assert!(composer.remote_image_urls().is_empty());
    assert!(!press(&mut composer, UNDO_KEY));
}

#[test]
fn async_history_recall_uses_the_same_undo_lineage_transition() {
    let (mut composer, mut rx) = new_composer();
    let thread_id = ThreadId::new();
    composer.set_history_metadata(thread_id, /*log_id*/ 1, /*entry_count*/ 0);
    assert!(press(&mut composer, KeyCode::Char('a')));
    let before_second_edit = composer.snapshot_draft();
    assert!(press(&mut composer, KeyCode::Char('b')));
    composer.set_current_cursor(/*cursor*/ 1);
    let before_clear = composer.snapshot_draft();
    assert!(composer.clear_for_ctrl_c().is_some());

    composer.set_history_metadata(thread_id, /*log_id*/ 2, /*entry_count*/ 1);
    let _ = press(&mut composer, KeyCode::Up);
    let AppEvent::LookupMessageHistoryEntry {
        thread_id: requested_thread_id,
        offset,
        log_id,
    } = rx.try_recv().expect("expected persistent history lookup")
    else {
        panic!("unexpected app event");
    };
    assert_eq!((requested_thread_id, offset, log_id), (thread_id, 0, 2));

    assert!(composer.on_history_entry_response(
        /*log_id*/ 2,
        /*offset*/ 0,
        Some("ab".to_string())
    ));
    assert_eq!(composer.snapshot_draft(), at_history_boundary(before_clear));
    assert!(press(&mut composer, UNDO_KEY));
    assert_eq!(composer.snapshot_draft(), before_second_edit);
}

#[test]
fn cursor_motion_preserves_redo_but_a_divergent_edit_discards_it() {
    let (mut composer, _rx) = new_composer();
    composer.insert_str("ab");
    let edited = composer.snapshot_draft();

    assert!(press(&mut composer, UNDO_KEY));
    assert!(press(&mut composer, KeyCode::Left));
    assert!(press(&mut composer, REDO_KEY));
    assert_eq!(composer.snapshot_draft(), edited);

    assert!(press(&mut composer, UNDO_KEY));
    assert!(press(&mut composer, KeyCode::Char('x')));
    assert!(!press(&mut composer, REDO_KEY));
    assert_eq!(composer.current_text(), "x");
}

#[test]
fn external_edit_is_reversible_but_submission_starts_a_new_history() {
    let (mut composer, _rx) = new_composer();
    composer.set_text_content("before".to_string(), Vec::new(), Vec::new());
    composer.move_cursor_to_end();
    let before_edit = composer.snapshot_draft();

    composer.apply_external_edit("after".to_string());
    assert!(press(&mut composer, UNDO_KEY));
    assert_eq!(composer.snapshot_draft(), before_edit);
    assert!(press(&mut composer, REDO_KEY));
    assert_eq!(composer.current_text(), "after");

    let (result, _) = composer.handle_key_event(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(matches!(result, InputResult::Submitted { .. }));
    assert!(!press(&mut composer, UNDO_KEY));
}

#[test]
fn detected_paste_burst_is_one_undo_step() {
    let (mut composer, _rx) = new_composer();
    composer.set_disable_paste_burst(/*disabled*/ false);
    composer.set_text_content("existing prompt".to_string(), Vec::new(), Vec::new());
    composer.move_cursor_to_end();
    let before_paste = composer.snapshot_draft();
    let now = Instant::now();

    // Non-ASCII whitespace lets the existing detector recognize the minimum-length burst.
    for ch in MINIMUM_NON_ASCII_RETRO_CAPTURE_INPUT.chars() {
        assert!(press_at(&mut composer, KeyCode::Char(ch), now));
    }
    assert!(composer.is_in_paste_burst());
    assert!(composer.handle_paste_burst_flush(now + PasteBurst::recommended_active_flush_delay()));
    let after_paste = composer.snapshot_draft();

    assert!(press(&mut composer, UNDO_KEY));
    assert_eq!(composer.snapshot_draft(), before_paste);
    assert!(!press(&mut composer, UNDO_KEY));
    assert!(press(&mut composer, REDO_KEY));
    assert_eq!(composer.snapshot_draft(), after_paste);
}

#[test]
fn detected_paste_burst_joins_an_active_vim_insert_transaction() {
    let (mut composer, _rx) = new_composer();
    composer.set_disable_paste_burst(/*disabled*/ false);
    composer.set_vim_enabled(/*enabled*/ true);
    assert!(press(&mut composer, KeyCode::Char('i')));
    let now = Instant::now();

    for ch in MINIMUM_NON_ASCII_RETRO_CAPTURE_INPUT.chars() {
        assert!(press_at(&mut composer, KeyCode::Char(ch), now));
    }
    assert!(composer.is_in_paste_burst());
    assert!(composer.handle_paste_burst_flush(now + PasteBurst::recommended_active_flush_delay()));
    assert!(press(&mut composer, KeyCode::Esc));
    let after_paste = composer.snapshot_draft();

    assert!(press(&mut composer, UNDO_KEY));
    assert!(composer.is_empty());
    assert!(!press(&mut composer, UNDO_KEY));
    assert!(press(&mut composer, REDO_KEY));
    assert_eq!(composer.snapshot_draft(), after_paste);
}

#[test]
fn missing_provisional_boundary_falls_back_without_losing_older_history() {
    let (mut composer, _rx) = new_composer();
    composer.set_disable_paste_burst(/*disabled*/ false);
    composer.set_text_content("base".to_string(), Vec::new(), Vec::new());
    composer.move_cursor_to_end();
    let now = Instant::now();
    let mut chars = MINIMUM_NON_ASCII_RETRO_CAPTURE_INPUT.chars();
    let first = chars.next().expect("first provisional character");
    let second = chars.next().expect("second provisional character");
    let third = chars.next().expect("paste-detecting character");

    assert!(press_at(&mut composer, KeyCode::Char(first), now));
    assert!(press_at(&mut composer, KeyCode::Char(second), now));
    let current = composer.snapshot_draft();
    assert!(composer.undo_history.undo(current).is_some());

    assert!(press_at(&mut composer, KeyCode::Char(third), now));
    assert!(!composer.is_in_paste_burst());
    assert_eq!(
        composer.current_text(),
        format!("base{MINIMUM_NON_ASCII_RETRO_CAPTURE_INPUT}")
    );
    assert!(press(&mut composer, UNDO_KEY));
    assert_eq!(composer.current_text(), format!("base{first}{second}"));
    assert!(press(&mut composer, UNDO_KEY));
    assert_eq!(composer.current_text(), "base");
}

#[test]
fn disabled_paste_burst_keeps_plain_keys_as_individual_undo_steps() {
    let (mut composer, _rx) = new_composer();
    let now = Instant::now();

    for ch in MINIMUM_NON_ASCII_RETRO_CAPTURE_INPUT.chars() {
        assert!(press_at(&mut composer, KeyCode::Char(ch), now));
    }
    assert!(!composer.is_in_paste_burst());
    assert_eq!(
        composer.current_text(),
        MINIMUM_NON_ASCII_RETRO_CAPTURE_INPUT
    );

    assert!(press(&mut composer, UNDO_KEY));
    assert_eq!(
        composer.current_text(),
        MINIMUM_NON_ASCII_RETRO_CAPTURE_INPUT
            .chars()
            .take(MINIMUM_NON_ASCII_RETRO_CAPTURE_INPUT.chars().count() - 1)
            .collect::<String>()
    );
    assert!(press(&mut composer, UNDO_KEY));
    assert_eq!(composer.current_text(), "界");
}
