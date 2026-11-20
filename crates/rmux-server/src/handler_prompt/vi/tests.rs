use super::super::super::scripting_support::QueueExecutionContext;
use super::super::super::{DetachedRequesterAuthority, RequesterOrigin};
use super::super::events::process_prompt_event;
use super::super::{
    ClientPromptState, CommandPromptPlan, PromptAction, PromptCompletion, PromptField,
    PromptFinalizeKind, PromptHistoryStore, PromptInputEvent, PromptType, PROMPT_FLAG_INCREMENTAL,
    PROMPT_FLAG_KEY, PROMPT_FLAG_NUMERIC, PROMPT_FLAG_SINGLE,
};
use super::{translate, PromptEditMode, StatusKeys, ViTranslation, ViWordMotion};

/// The default `word-separators`, as both tmux 3.7b and RMUX report it.
const DEFAULT_WORD_SEPARATORS: &str = "!\"#$%&'()*+,-./:;<=>?@[\\]^`{|}~";

/// One measured vi word-motion stimulus: the payload the prompt opens with, the
/// keys sent after Escape, the effective `word-separators` and the value the
/// pinned tmux 3.7b really produced. The marker `Z` is inserted with `i` where
/// the motion left the cursor, so the value encodes the cursor position.
///
/// Measured before this correction by `sk1/r2/oracle_words.py`
/// against the pinned tmux 3.7b; never derived from RMUX.
struct ViCell {
    name: &'static str,
    payload: &'static str,
    keys: &'static str,
    separators: &'static str,
    measured: &'static str,
}

fn command_prompt(input: &str, flags: u8) -> ClientPromptState {
    let plan = CommandPromptPlan {
        origin: RequesterOrigin::new(1, DetachedRequesterAuthority::Denied),
        target_client: None,
        context: QueueExecutionContext::without_caller_cwd(),
        fields: vec![PromptField {
            prompt: ":".to_owned(),
            input: input.to_owned(),
        }],
        template: "%%".to_owned(),
        flags,
        prompt_type: PromptType::Command,
        background: false,
        format_values: Vec::new(),
    };
    ClientPromptState::new_command(plan, PromptCompletion::Background)
}

fn feed(
    prompt: &mut ClientPromptState,
    history: &mut PromptHistoryStore,
    keys: &[PromptInputEvent],
    status_keys: StatusKeys,
) -> PromptAction {
    feed_with_separators(prompt, history, keys, status_keys, DEFAULT_WORD_SEPARATORS)
}

fn feed_with_separators(
    prompt: &mut ClientPromptState,
    history: &mut PromptHistoryStore,
    keys: &[PromptInputEvent],
    status_keys: StatusKeys,
    separators: &str,
) -> PromptAction {
    let mut action = PromptAction::none();
    for key in keys {
        action = process_prompt_event(prompt, key.clone(), history, separators, 100, status_keys);
    }
    action
}

fn chars(text: &str) -> Vec<PromptInputEvent> {
    text.chars().map(PromptInputEvent::Char).collect()
}

/// Mirrors `handler_pane/attached_input.rs:305-359`: a run of printable text is
/// offered to the batched fast path first, and as soon as the prompt refuses it
/// the very same bytes are replayed as individual prompt events.
fn feed_text_like_attached_input(
    prompt: &mut ClientPromptState,
    history: &mut PromptHistoryStore,
    text: &str,
    status_keys: StatusKeys,
) -> bool {
    feed_text_like_attached_input_with_separators(
        prompt,
        history,
        text,
        status_keys,
        DEFAULT_WORD_SEPARATORS,
    )
}

fn feed_text_like_attached_input_with_separators(
    prompt: &mut ClientPromptState,
    history: &mut PromptHistoryStore,
    text: &str,
    status_keys: StatusKeys,
    separators: &str,
) -> bool {
    if prompt.insert_batched_text(text, status_keys) {
        return true;
    }
    feed_with_separators(prompt, history, &chars(text), status_keys, separators);
    false
}

fn responses(action: PromptAction) -> Vec<String> {
    match action.finalize.expect("prompt finalized") {
        PromptFinalizeKind::Command { responses } => responses,
        other => panic!("expected a command response, got {other:?}"),
    }
}

#[test]
fn status_keys_recognises_only_vi() {
    assert_eq!(StatusKeys::from_option_value(Some("vi")), StatusKeys::Vi);
    assert_eq!(
        StatusKeys::from_option_value(Some("emacs")),
        StatusKeys::Emacs
    );
    // The option table only stores the two choices; anything else keeps the
    // default editing rather than guessing.
    assert_eq!(StatusKeys::from_option_value(Some("VI")), StatusKeys::Emacs);
    assert_eq!(StatusKeys::from_option_value(None), StatusKeys::Emacs);
    assert_eq!(StatusKeys::default(), StatusKeys::Emacs);
    assert_eq!(PromptEditMode::default(), PromptEditMode::Entry);
}

#[test]
fn entry_mode_passes_through_the_source_key_set() {
    let passthrough = [
        PromptInputEvent::Ctrl('a'),
        PromptInputEvent::Ctrl('c'),
        PromptInputEvent::Ctrl('e'),
        PromptInputEvent::Ctrl('g'),
        PromptInputEvent::Ctrl('h'),
        PromptInputEvent::Ctrl('k'),
        PromptInputEvent::Ctrl('n'),
        PromptInputEvent::Ctrl('p'),
        PromptInputEvent::Ctrl('t'),
        PromptInputEvent::Ctrl('u'),
        PromptInputEvent::Ctrl('v'),
        PromptInputEvent::Ctrl('w'),
        PromptInputEvent::Ctrl('y'),
        PromptInputEvent::Tab,
        PromptInputEvent::Enter,
        PromptInputEvent::Backspace,
        PromptInputEvent::Delete,
        PromptInputEvent::Down,
        PromptInputEvent::End,
        PromptInputEvent::Home,
        PromptInputEvent::Left,
        PromptInputEvent::Right,
        PromptInputEvent::Up,
        PromptInputEvent::KeyName("C-Left".to_owned()),
        PromptInputEvent::KeyName("C-Right".to_owned()),
    ];
    for event in passthrough {
        let mut mode = PromptEditMode::Entry;
        assert_eq!(
            translate(&mut mode, event.clone()),
            ViTranslation::Event(event.clone()),
            "{event:?}"
        );
        assert_eq!(mode, PromptEditMode::Entry, "{event:?}");
    }
}

#[test]
fn entry_mode_appends_every_other_key() {
    for event in [
        PromptInputEvent::Char('x'),
        PromptInputEvent::Char('é'),
        PromptInputEvent::Ctrl('b'),
        PromptInputEvent::Ctrl('f'),
        PromptInputEvent::Ctrl('r'),
        PromptInputEvent::KeyName("F1".to_owned()),
    ] {
        let mut mode = PromptEditMode::Entry;
        assert_eq!(
            translate(&mut mode, event.clone()),
            ViTranslation::Insert(event.clone()),
            "{event:?}"
        );
        assert_eq!(mode, PromptEditMode::Entry, "{event:?}");
    }
}

#[test]
fn escape_switches_entry_to_command_mode() {
    for event in [PromptInputEvent::Escape, PromptInputEvent::Ctrl('[')] {
        let mut mode = PromptEditMode::Entry;
        assert_eq!(
            translate(&mut mode, event.clone()),
            ViTranslation::EnterCommandMode,
            "{event:?}"
        );
        assert_eq!(mode, PromptEditMode::Command, "{event:?}");
        // In command mode the same key is a no-op that keeps the mode.
        assert_eq!(translate(&mut mode, event.clone()), ViTranslation::Consumed);
        assert_eq!(mode, PromptEditMode::Command, "{event:?}");
    }
}

#[test]
fn command_mode_maps_the_source_table() {
    let table = [
        (PromptInputEvent::Backspace, PromptInputEvent::Left),
        (PromptInputEvent::Char('$'), PromptInputEvent::End),
        (PromptInputEvent::Char('0'), PromptInputEvent::Home),
        (PromptInputEvent::Char('^'), PromptInputEvent::Home),
        (PromptInputEvent::Char('D'), PromptInputEvent::Ctrl('k')),
        (PromptInputEvent::Char('X'), PromptInputEvent::Backspace),
        (PromptInputEvent::Char('d'), PromptInputEvent::Ctrl('u')),
        (PromptInputEvent::Char('p'), PromptInputEvent::Ctrl('y')),
        (PromptInputEvent::Char('q'), PromptInputEvent::Ctrl('c')),
        (PromptInputEvent::Char('x'), PromptInputEvent::Delete),
        (PromptInputEvent::Delete, PromptInputEvent::Delete),
        (PromptInputEvent::Char('j'), PromptInputEvent::Down),
        (PromptInputEvent::Down, PromptInputEvent::Down),
        (PromptInputEvent::Char('h'), PromptInputEvent::Left),
        (PromptInputEvent::Left, PromptInputEvent::Left),
        (PromptInputEvent::Char('l'), PromptInputEvent::Right),
        (PromptInputEvent::Right, PromptInputEvent::Right),
        (PromptInputEvent::Char('k'), PromptInputEvent::Up),
        (PromptInputEvent::Up, PromptInputEvent::Up),
        (PromptInputEvent::Ctrl('h'), PromptInputEvent::Ctrl('h')),
        (PromptInputEvent::Ctrl('c'), PromptInputEvent::Ctrl('c')),
        (PromptInputEvent::Enter, PromptInputEvent::Enter),
    ];
    for (key, expected) in table {
        let mut mode = PromptEditMode::Command;
        assert_eq!(
            translate(&mut mode, key.clone()),
            ViTranslation::Event(expected.clone()),
            "{key:?}"
        );
        assert_eq!(mode, PromptEditMode::Command, "{key:?}");
    }
}

#[test]
fn command_mode_insert_keys_return_to_entry() {
    let table = [
        (
            PromptInputEvent::Char('A'),
            ViTranslation::Event(PromptInputEvent::End),
        ),
        (
            PromptInputEvent::Char('I'),
            ViTranslation::Event(PromptInputEvent::Home),
        ),
        (
            PromptInputEvent::Char('C'),
            ViTranslation::Event(PromptInputEvent::Ctrl('k')),
        ),
        (
            PromptInputEvent::Char('s'),
            ViTranslation::Event(PromptInputEvent::Delete),
        ),
        (
            PromptInputEvent::Char('a'),
            ViTranslation::Event(PromptInputEvent::Right),
        ),
        (
            PromptInputEvent::Char('S'),
            ViTranslation::Event(PromptInputEvent::Ctrl('u')),
        ),
        (PromptInputEvent::Char('i'), ViTranslation::Consumed),
    ];
    for (key, expected) in table {
        let mut mode = PromptEditMode::Command;
        assert_eq!(translate(&mut mode, key.clone()), expected, "{key:?}");
        assert_eq!(mode, PromptEditMode::Entry, "{key:?}");
    }
}

#[test]
fn command_mode_maps_the_six_word_motions() {
    // A lowercase key honours word-separators and an uppercase one runs on an
    // empty list (status.c:1597-1634).
    let table = [
        (
            'b',
            ViWordMotion::PreviousWordStart {
                honour_separators: true,
            },
        ),
        (
            'B',
            ViWordMotion::PreviousWordStart {
                honour_separators: false,
            },
        ),
        (
            'e',
            ViWordMotion::WordEnd {
                honour_separators: true,
            },
        ),
        (
            'E',
            ViWordMotion::WordEnd {
                honour_separators: false,
            },
        ),
        (
            'w',
            ViWordMotion::NextWordStart {
                honour_separators: true,
            },
        ),
        (
            'W',
            ViWordMotion::NextWordStart {
                honour_separators: false,
            },
        ),
    ];
    for (key, motion) in table {
        let mut mode = PromptEditMode::Command;
        assert_eq!(
            translate(&mut mode, PromptInputEvent::Char(key)),
            ViTranslation::Motion(motion),
            "{key}"
        );
        assert_eq!(mode, PromptEditMode::Command, "{key}");
    }
}

const ORACLE_CELLS: &[ViCell] = &[
    // b from the last character: a lowercase motion has to honour word-separators
    ViCell {
        name: "b-default-from-end",
        payload: "foo bar-baz qux@quux",
        keys: "b",
        separators: DEFAULT_WORD_SEPARATORS,
        measured: "foo bar-baz qux@Zquux",
    },
    // B from the same place: an uppercase motion ignores word-separators
    ViCell {
        name: "B-default-from-end",
        payload: "foo bar-baz qux@quux",
        keys: "B",
        separators: DEFAULT_WORD_SEPARATORS,
        measured: "foo bar-baz Zqux@quux",
    },
    // the effective option really drives b: @ is no longer a separator
    ViCell {
        name: "b-custom-separators-dash-only",
        payload: "foo bar-baz qux@quux",
        keys: "b",
        separators: "-",
        measured: "foo bar-baz Zqux@quux",
    },
    // an empty word-separators collapses b onto the big-word behaviour
    ViCell {
        name: "b-empty-separators",
        payload: "foo bar-baz qux@quux",
        keys: "b",
        separators: "",
        measured: "foo bar-baz Zqux@quux",
    },
    // B is unmoved by the option
    ViCell {
        name: "B-custom-separators-dash-only",
        payload: "foo bar-baz qux@quux",
        keys: "B",
        separators: "-",
        measured: "foo bar-baz Zqux@quux",
    },
    // e stops at the end of a separated word
    ViCell {
        name: "e-twice-from-home",
        payload: "foo bar-baz qux@quux",
        keys: "0ee",
        separators: DEFAULT_WORD_SEPARATORS,
        measured: "foo baZr-baz qux@quux",
    },
    // E crosses the separator inside bar-baz
    ViCell {
        name: "E-twice-from-home",
        payload: "foo bar-baz qux@quux",
        keys: "0EE",
        separators: DEFAULT_WORD_SEPARATORS,
        measured: "foo bar-baZz qux@quux",
    },
    // w stops on the separator itself
    ViCell {
        name: "w-twice-from-home",
        payload: "foo bar-baz qux@quux",
        keys: "0ww",
        separators: DEFAULT_WORD_SEPARATORS,
        measured: "foo barZ-baz qux@quux",
    },
    // W crosses it and lands on the next space-separated word
    ViCell {
        name: "W-twice-from-home",
        payload: "foo bar-baz qux@quux",
        keys: "0WW",
        separators: DEFAULT_WORD_SEPARATORS,
        measured: "foo bar-baz Zqux@quux",
    },
    // boundary: a forward motion with no next word
    ViCell {
        name: "w-at-the-last-character",
        payload: "foo bar-baz qux@quux",
        keys: "w",
        separators: DEFAULT_WORD_SEPARATORS,
        measured: "foo bar-baz qux@quuxZ",
    },
    // boundary: a backward motion already at column zero
    ViCell {
        name: "b-at-home",
        payload: "foo bar-baz qux@quux",
        keys: "0b",
        separators: DEFAULT_WORD_SEPARATORS,
        measured: "Zfoo bar-baz qux@quux",
    },
    // boundary: an end-of-word motion with no next word
    ViCell {
        name: "e-at-the-last-character",
        payload: "foo bar-baz qux@quux",
        keys: "e",
        separators: DEFAULT_WORD_SEPARATORS,
        measured: "foo bar-baz qux@quuxZ",
    },
    // character indices, not bytes, around éclair
    ViCell {
        name: "bb-across-a-two-byte-character",
        payload: "foo bar-baz  qux@quux éclair end",
        keys: "bb",
        separators: DEFAULT_WORD_SEPARATORS,
        measured: "foo bar-baz  qux@quux Zéclair end",
    },
    // the double space and the separators together
    ViCell {
        name: "e-four-times-across-the-double-space",
        payload: "foo bar-baz  qux@quux éclair end",
        keys: "0eeee",
        separators: DEFAULT_WORD_SEPARATORS,
        measured: "foo bar-baZz  qux@quux éclair end",
    },
    // w skips a run of spaces to the next word start
    ViCell {
        name: "w-four-times-across-the-double-space",
        payload: "foo bar-baz  qux@quux éclair end",
        keys: "0wwww",
        separators: DEFAULT_WORD_SEPARATORS,
        measured: "foo bar-baz  Zqux@quux éclair end",
    },
];

#[test]
fn the_measured_word_motions_replay_key_by_key() {
    for cell in ORACLE_CELLS {
        let mut prompt = command_prompt(cell.payload, 0);
        let mut history = PromptHistoryStore::default();
        let mut keys = vec![PromptInputEvent::Escape];
        keys.extend(chars(cell.keys));
        keys.extend(chars("iZ"));
        keys.push(PromptInputEvent::Enter);
        let action = feed_with_separators(
            &mut prompt,
            &mut history,
            &keys,
            StatusKeys::Vi,
            cell.separators,
        );
        assert_eq!(
            responses(action),
            vec![cell.measured.to_owned()],
            "{}",
            cell.name
        );
    }
}

#[test]
fn the_measured_word_motions_replay_as_a_grouped_run() {
    // The same stimulus as the attached client sends it: one write for the
    // motion keys, one for the insert. Both have to refuse the fast path and
    // land exactly where the individual keys did.
    for cell in ORACLE_CELLS {
        let mut prompt = command_prompt(cell.payload, 0);
        let mut history = PromptHistoryStore::default();
        feed_with_separators(
            &mut prompt,
            &mut history,
            &[PromptInputEvent::Escape],
            StatusKeys::Vi,
            cell.separators,
        );
        let motion_batched = feed_text_like_attached_input_with_separators(
            &mut prompt,
            &mut history,
            cell.keys,
            StatusKeys::Vi,
            cell.separators,
        );
        assert!(
            !motion_batched,
            "{}: the motion run must not batch",
            cell.name
        );
        let insert_batched = feed_text_like_attached_input_with_separators(
            &mut prompt,
            &mut history,
            "iZ",
            StatusKeys::Vi,
            cell.separators,
        );
        assert!(
            !insert_batched,
            "{}: the insert run must not batch",
            cell.name
        );
        let action = feed_with_separators(
            &mut prompt,
            &mut history,
            &[PromptInputEvent::Enter],
            StatusKeys::Vi,
            cell.separators,
        );
        assert_eq!(
            responses(action),
            vec![cell.measured.to_owned()],
            "{}",
            cell.name
        );
    }
}

#[test]
fn the_word_motions_are_command_mode_keys_only() {
    // In entry mode the same letters are literal text, as the source appends
    // every key it does not list.
    let mut prompt = command_prompt("", 0);
    let mut history = PromptHistoryStore::default();
    let action = feed(
        &mut prompt,
        &mut history,
        &[
            PromptInputEvent::Char('w'),
            PromptInputEvent::Char('B'),
            PromptInputEvent::Char('e'),
            PromptInputEvent::Enter,
        ],
        StatusKeys::Vi,
    );
    assert_eq!(responses(action), vec!["wBe".to_owned()]);
}

#[test]
fn a_word_motion_repaints_and_re_runs_an_incremental_prompt() {
    // tmux reaches `changed:` for every word motion, which repaints and, on an
    // incremental prompt, dispatches the search again.
    let mut prompt = command_prompt("", PROMPT_FLAG_INCREMENTAL);
    let mut history = PromptHistoryStore::default();
    feed(&mut prompt, &mut history, &chars("foo bar"), StatusKeys::Vi);
    feed(
        &mut prompt,
        &mut history,
        &[PromptInputEvent::Escape],
        StatusKeys::Vi,
    );
    let action = feed(
        &mut prompt,
        &mut history,
        &[PromptInputEvent::Char('b')],
        StatusKeys::Vi,
    );
    assert!(action.refresh);
    assert!(action.finalize.is_none());
    let dispatch = action
        .dispatch
        .expect("an incremental prompt re-dispatches");
    assert_eq!(dispatch.responses, vec!["=foo bar".to_owned()]);
    assert_eq!(prompt.rendered_prompt().cursor, 4);
}

#[test]
fn the_word_motions_are_safe_on_an_empty_buffer_and_at_the_edges() {
    for motion in [
        ViWordMotion::NextWordStart {
            honour_separators: true,
        },
        ViWordMotion::WordEnd {
            honour_separators: true,
        },
        ViWordMotion::PreviousWordStart {
            honour_separators: true,
        },
    ] {
        assert_eq!(
            motion.apply("", 0, DEFAULT_WORD_SEPARATORS),
            0,
            "{motion:?}"
        );
    }
    // A cursor left one past the last character by a previous motion is a real
    // state here, and the source reads the buffer terminator there: the two
    // forward motions stay put and the backward one walks back into the word.
    for motion in [
        ViWordMotion::NextWordStart {
            honour_separators: true,
        },
        ViWordMotion::WordEnd {
            honour_separators: true,
        },
    ] {
        assert_eq!(
            motion.apply("ab", 2, DEFAULT_WORD_SEPARATORS),
            2,
            "{motion:?}"
        );
    }
    assert_eq!(
        ViWordMotion::PreviousWordStart {
            honour_separators: true,
        }
        .apply("ab", 2, DEFAULT_WORD_SEPARATORS),
        0
    );
    let backward = ViWordMotion::PreviousWordStart {
        honour_separators: true,
    };
    assert_eq!(backward.apply("   ", 3, DEFAULT_WORD_SEPARATORS), 0);
    let forward = ViWordMotion::NextWordStart {
        honour_separators: true,
    };
    assert_eq!(forward.apply("a   ", 0, DEFAULT_WORD_SEPARATORS), 4);
}

#[test]
fn vi_escape_enters_command_mode_and_steps_the_cursor_back() {
    let mut prompt = command_prompt("", 0);
    let mut history = PromptHistoryStore::default();

    feed(&mut prompt, &mut history, &chars("abc"), StatusKeys::Vi);
    assert!(!prompt.rendered_prompt().command_prompt);

    let action = feed(
        &mut prompt,
        &mut history,
        &[PromptInputEvent::Escape],
        StatusKeys::Vi,
    );
    assert!(action.finalize.is_none(), "escape must not cancel under vi");
    assert!(action.refresh, "the mode change repaints the prompt");
    let rendered = prompt.rendered_prompt();
    assert!(rendered.command_prompt);
    assert_eq!(rendered.input, "abc");
    assert_eq!(rendered.cursor, 2);
}

#[test]
fn vi_escape_on_an_empty_buffer_keeps_column_zero() {
    let mut prompt = command_prompt("", 0);
    let mut history = PromptHistoryStore::default();

    feed(
        &mut prompt,
        &mut history,
        &[PromptInputEvent::Escape],
        StatusKeys::Vi,
    );
    let rendered = prompt.rendered_prompt();
    assert!(rendered.command_prompt);
    assert_eq!(rendered.cursor, 0);
}

#[test]
fn emacs_escape_still_cancels_the_prompt() {
    let mut prompt = command_prompt("", 0);
    let mut history = PromptHistoryStore::default();

    feed(&mut prompt, &mut history, &chars("abc"), StatusKeys::Emacs);
    let action = feed(
        &mut prompt,
        &mut history,
        &[PromptInputEvent::Escape],
        StatusKeys::Emacs,
    );
    assert!(matches!(action.finalize, Some(PromptFinalizeKind::Cancel)));
    assert!(!prompt.rendered_prompt().command_prompt);
}

#[test]
fn vi_delete_then_insert_replaces_the_character_under_the_cursor() {
    // tmux 3.7b measured: `set -g @sk1 SK1-é-X`, Escape, `xiY`, Enter leaves
    // `SK1-é-Y`, cursor arithmetic included.
    let mut prompt = command_prompt("SK1-é-X", 0);
    let mut history = PromptHistoryStore::default();

    let action = feed(
        &mut prompt,
        &mut history,
        &[
            PromptInputEvent::Escape,
            PromptInputEvent::Char('x'),
            PromptInputEvent::Char('i'),
            PromptInputEvent::Char('Y'),
            PromptInputEvent::Enter,
        ],
        StatusKeys::Vi,
    );
    assert_eq!(responses(action), vec!["SK1-é-Y".to_owned()]);
}

#[test]
fn vi_q_cancels_the_prompt() {
    let mut prompt = command_prompt("SK1-é-X", 0);
    let mut history = PromptHistoryStore::default();

    let action = feed(
        &mut prompt,
        &mut history,
        &[PromptInputEvent::Escape, PromptInputEvent::Char('q')],
        StatusKeys::Vi,
    );
    assert!(matches!(action.finalize, Some(PromptFinalizeKind::Cancel)));
}

#[test]
fn vi_home_then_insert_completes_the_command() {
    // tmux 3.7b measured: typing `et -g @sk1 HOME`, Escape, `0`, `is` submits
    // the completed `set` command.
    let mut prompt = command_prompt("et -g @sk1 HOME", 0);
    let mut history = PromptHistoryStore::default();

    let action = feed(
        &mut prompt,
        &mut history,
        &[
            PromptInputEvent::Escape,
            PromptInputEvent::Char('0'),
            PromptInputEvent::Char('i'),
            PromptInputEvent::Char('s'),
            PromptInputEvent::Enter,
        ],
        StatusKeys::Vi,
    );
    assert_eq!(responses(action), vec!["set -g @sk1 HOME".to_owned()]);
}

#[test]
fn vi_dollar_then_append_adds_after_the_last_character() {
    // tmux 3.7b measured: `AB`, Escape, `$`, `aZ` submits `ABZ`.
    let mut prompt = command_prompt("AB", 0);
    let mut history = PromptHistoryStore::default();

    let action = feed(
        &mut prompt,
        &mut history,
        &[
            PromptInputEvent::Escape,
            PromptInputEvent::Char('$'),
            PromptInputEvent::Char('a'),
            PromptInputEvent::Char('Z'),
            PromptInputEvent::Enter,
        ],
        StatusKeys::Vi,
    );
    assert_eq!(responses(action), vec!["ABZ".to_owned()]);
}

#[test]
fn a_hot_status_keys_change_applies_to_the_very_next_key() {
    // tmux re-reads status-keys for every prompt key (status.c:1472).
    let mut prompt = command_prompt("SK1-é-X", 0);
    let mut history = PromptHistoryStore::default();

    // Still emacs: an ordinary character is inserted, nothing switches mode.
    feed(
        &mut prompt,
        &mut history,
        &[PromptInputEvent::Char('!')],
        StatusKeys::Emacs,
    );
    assert!(!prompt.rendered_prompt().command_prompt);

    let action = feed(
        &mut prompt,
        &mut history,
        &[
            PromptInputEvent::Backspace,
            PromptInputEvent::Escape,
            PromptInputEvent::Char('x'),
            PromptInputEvent::Char('i'),
            PromptInputEvent::Char('Y'),
            PromptInputEvent::Enter,
        ],
        StatusKeys::Vi,
    );
    assert_eq!(responses(action), vec!["SK1-é-Y".to_owned()]);
}

#[test]
fn a_hot_switch_back_to_emacs_stops_translating_keys() {
    let mut prompt = command_prompt("AB", 0);
    let mut history = PromptHistoryStore::default();

    feed(
        &mut prompt,
        &mut history,
        &[PromptInputEvent::Escape],
        StatusKeys::Vi,
    );
    assert!(prompt.rendered_prompt().command_prompt);

    // Back on emacs the same keys are literal text again, while the mode the
    // prompt already carries keeps driving the command prompt rendering.
    let action = feed(
        &mut prompt,
        &mut history,
        &[
            PromptInputEvent::Char('x'),
            PromptInputEvent::Char('i'),
            PromptInputEvent::Enter,
        ],
        StatusKeys::Emacs,
    );
    assert_eq!(responses(action), vec!["AxiB".to_owned()]);
}

#[test]
fn the_edit_mode_resets_when_the_prompt_advances_to_the_next_field() {
    let plan = CommandPromptPlan {
        origin: RequesterOrigin::new(1, DetachedRequesterAuthority::Denied),
        target_client: None,
        context: QueueExecutionContext::without_caller_cwd(),
        fields: vec![
            PromptField {
                prompt: "one: ".to_owned(),
                input: "first".to_owned(),
            },
            PromptField {
                prompt: "two: ".to_owned(),
                input: "second".to_owned(),
            },
        ],
        template: "%% %%".to_owned(),
        flags: 0,
        prompt_type: PromptType::Command,
        background: false,
        format_values: Vec::new(),
    };
    let mut prompt = ClientPromptState::new_command(plan, PromptCompletion::Background);
    let mut history = PromptHistoryStore::default();

    feed(
        &mut prompt,
        &mut history,
        &[PromptInputEvent::Escape],
        StatusKeys::Vi,
    );
    assert!(prompt.rendered_prompt().command_prompt);

    let action = feed(
        &mut prompt,
        &mut history,
        &[PromptInputEvent::Enter],
        StatusKeys::Vi,
    );
    assert!(action.finalize.is_none(), "the second field is still open");
    let rendered = prompt.rendered_prompt();
    assert!(!rendered.command_prompt, "each field opens in entry mode");
    assert_eq!(rendered.input, "second");
}

#[test]
fn vi_does_not_reach_the_key_numeric_and_single_prompts() {
    // tmux returns from those prompts before it reads status-keys
    // (status.c:1449-1472), so Escape keeps cancelling them.
    for flags in [PROMPT_FLAG_KEY, PROMPT_FLAG_NUMERIC, PROMPT_FLAG_SINGLE] {
        let mut prompt = command_prompt("", flags);
        let mut history = PromptHistoryStore::default();
        let action = feed(
            &mut prompt,
            &mut history,
            &[PromptInputEvent::Escape],
            StatusKeys::Vi,
        );
        assert!(action.finalize.is_some(), "flags={flags:#x}");
        assert!(!prompt.rendered_prompt().command_prompt, "flags={flags:#x}");
    }
}

#[test]
fn vi_enters_command_mode_on_an_incremental_prompt_without_dispatching() {
    let mut prompt = command_prompt("", PROMPT_FLAG_INCREMENTAL);
    let mut history = PromptHistoryStore::default();

    let typed = feed(&mut prompt, &mut history, &chars("ab"), StatusKeys::Vi);
    assert!(typed.dispatch.is_some(), "typing still searches");

    let action = feed(
        &mut prompt,
        &mut history,
        &[PromptInputEvent::Escape],
        StatusKeys::Vi,
    );
    assert!(action.finalize.is_none());
    assert!(action.dispatch.is_none(), "a mode change is not a search");
    assert!(prompt.rendered_prompt().command_prompt);
}

#[test]
fn vi_command_mode_refuses_the_batched_fast_path() {
    let mut prompt = command_prompt("SK1-é-X", 0);
    let mut history = PromptHistoryStore::default();

    assert!(
        prompt.insert_batched_text("!", StatusKeys::Vi),
        "entry mode still batches text"
    );
    feed(
        &mut prompt,
        &mut history,
        &[PromptInputEvent::Backspace, PromptInputEvent::Escape],
        StatusKeys::Vi,
    );

    assert!(
        !prompt.insert_batched_text("xiY", StatusKeys::Vi),
        "command mode must hand the run back to the event path"
    );
    assert_eq!(prompt.rendered_prompt().input, "SK1-é-X");
}

#[test]
fn a_batched_run_in_command_mode_edits_like_the_individual_keys() {
    let mut batched = command_prompt("SK1-é-X", 0);
    let mut batched_history = PromptHistoryStore::default();
    feed(
        &mut batched,
        &mut batched_history,
        &[PromptInputEvent::Escape],
        StatusKeys::Vi,
    );
    let took_fast_path =
        feed_text_like_attached_input(&mut batched, &mut batched_history, "xiY", StatusKeys::Vi);
    assert!(!took_fast_path, "the run is replayed as prompt events");

    let mut typed = command_prompt("SK1-é-X", 0);
    let mut typed_history = PromptHistoryStore::default();
    feed(
        &mut typed,
        &mut typed_history,
        &[PromptInputEvent::Escape],
        StatusKeys::Vi,
    );
    feed(
        &mut typed,
        &mut typed_history,
        &chars("xiY"),
        StatusKeys::Vi,
    );

    assert_eq!(batched.rendered_prompt(), typed.rendered_prompt());
    assert_eq!(batched.rendered_prompt().input, "SK1-é-Y");
}

#[test]
fn an_emacs_batched_run_matches_its_individual_keys_after_a_hot_switch() {
    // The mode the prompt already carries only changes the fast path under vi:
    // on emacs both routes insert the same literal text.
    let mut batched = command_prompt("AB", 0);
    let mut batched_history = PromptHistoryStore::default();
    feed(
        &mut batched,
        &mut batched_history,
        &[PromptInputEvent::Escape],
        StatusKeys::Vi,
    );
    let took_fast_path =
        feed_text_like_attached_input(&mut batched, &mut batched_history, "xi", StatusKeys::Emacs);
    assert!(took_fast_path);

    let mut typed = command_prompt("AB", 0);
    let mut typed_history = PromptHistoryStore::default();
    feed(
        &mut typed,
        &mut typed_history,
        &[PromptInputEvent::Escape],
        StatusKeys::Vi,
    );
    feed(
        &mut typed,
        &mut typed_history,
        &chars("xi"),
        StatusKeys::Emacs,
    );

    assert_eq!(batched.rendered_prompt(), typed.rendered_prompt());
    assert_eq!(batched.rendered_prompt().input, "AxiB");
}
