//! The Kitty keyboard protocol's negotiation, against the specification at
//! <https://sw.kovidgoyal.net/kitty/keyboard-protocol/>.
//!
//! rmux implements one progressive enhancement — disambiguate escape codes
//! (`0b1`) — so the flags an application asks for are masked to that, and the
//! query reply is how it learns what it got. Everything else here is
//! negotiation machinery the specification requires whatever is implemented:
//! the set modes, the stack, its eviction, and one keyboard mode per screen.

use super::*;

/// One parser and one screen across several writes, the way a pane keeps them.
fn session() -> (InputParser, RecordingWriter) {
    (InputParser::new(), RecordingWriter::new(80, 24))
}

/// Whether disambiguation is in force.
fn on(w: &RecordingWriter) -> bool {
    (w.mode & MODE_KEYS_KITTY) != 0
}

#[test]
fn kitty_requests_for_unimplemented_flags_change_nothing() {
    // event types, alternate keys, associated text: asked for, not honoured
    for request in [
        b"\x1b[=8u".as_slice(),
        b"\x1b[=0u".as_slice(),
        b"\x1b[=1;3u".as_slice(),
        b"\x1b[>0u".as_slice(),
        b"\x1b[<u".as_slice(),
        b"\x1b[<2u".as_slice(),
    ] {
        let (_parser, writer) = parse(request);
        assert_eq!(writer.mode, MODE_CURSOR | MODE_WRAP, "request {request:?}");
    }
}

#[test]
fn a_kitty_query_always_answers() {
    // silence is what leaves an application guessing; zero is an answer
    let (mut parser, _writer) = parse(b"\x1b[?u");
    assert_eq!(parser.take_replies(), b"\x1b[?0u".to_vec());
}

#[test]
fn kitty_keyboard_requests_preserve_xterm_extended_key_mode() {
    let (_parser, writer) = parse(b"\x1b[>4;2m\x1b[=8u\x1b[>1u\x1b[<u\x1b[?u");

    assert_eq!(
        writer.mode,
        MODE_CURSOR | MODE_WRAP | MODE_KEYS_EXTENDED_2,
        "a pane that enabled modifyOtherKeys keeps it across a kitty exchange"
    );
}

// ─── CSI = flags ; mode u — set ────────────────────────────────────

#[test]
fn mode_one_replaces_every_flag() {
    // "The value 1 means all set bits are set and all unset bits are reset."
    let (mut p, mut w) = session();
    p.parse(b"\x1b[=1;1u", &mut w);
    assert!(on(&w));
    p.parse(b"\x1b[=0;1u", &mut w);
    assert!(!on(&w), "a flag left out of a replace is reset");
}

#[test]
fn the_mode_parameter_defaults_to_replace() {
    // "The second, mode parameter is optional (defaulting to 1)"
    let (mut p, mut w) = session();
    p.parse(b"\x1b[=1u", &mut w);
    assert!(on(&w));
    p.parse(b"\x1b[=0u", &mut w);
    assert!(!on(&w));
}

#[test]
fn mode_two_sets_and_mode_three_resets_without_touching_the_rest() {
    // "The value 2 means all set bits are set, unset bits are left unchanged.
    //  The value 3 means all set bits are reset, unset bits are left unchanged."
    let (mut p, mut w) = session();
    p.parse(b"\x1b[=1;2u", &mut w);
    assert!(on(&w));
    p.parse(b"\x1b[=0;2u", &mut w);
    assert!(on(&w), "naming no bits changes nothing");
    p.parse(b"\x1b[=1;3u", &mut w);
    assert!(!on(&w));
    p.parse(b"\x1b[=0;3u", &mut w);
    assert!(!on(&w));
}

// ─── CSI ? u — query ───────────────────────────────────────────────

#[test]
fn the_query_always_answers() {
    // "The terminal will reply with: CSI ? flags u" — including with nothing on
    let (mut p, mut w) = session();
    p.parse(b"\x1b[?u", &mut w);
    assert_eq!(p.take_replies(), b"\x1b[?0u".to_vec());
    p.parse(b"\x1b[>1u\x1b[?u", &mut w);
    assert_eq!(p.take_replies(), b"\x1b[?1u".to_vec());
}

#[test]
fn the_query_answers_with_what_was_honoured_not_what_was_asked() {
    // event types, alternate keys, all-keys and associated text are not
    // implemented; the reply is where an application finds that out
    let (mut p, mut w) = session();
    p.parse(b"\x1b[=31;1u\x1b[?u", &mut w);
    assert_eq!(p.take_replies(), b"\x1b[?1u".to_vec());
    assert!(on(&w), "the one flag rmux does implement still applies");
}

#[test]
fn asking_only_for_flags_rmux_cannot_honour_turns_nothing_on() {
    let (mut p, mut w) = session();
    for request in [
        b"\x1b[=2;1u".as_slice(),  // report event types
        b"\x1b[=4;1u".as_slice(),  // report alternate keys
        b"\x1b[=8;1u".as_slice(),  // report all keys as escape codes
        b"\x1b[=16;1u".as_slice(), // report associated text
    ] {
        let (mut p, mut w) = session();
        p.parse(request, &mut w);
        assert!(!on(&w), "request {request:?}");
    }
    p.parse(b"\x1b[=30;1u\x1b[?u", &mut w);
    assert_eq!(p.take_replies(), b"\x1b[?0u".to_vec());
}

// ─── CSI > flags u / CSI < number u — the stack ────────────────────

#[test]
fn a_push_turns_disambiguation_on_and_a_pop_takes_it_away() {
    let (mut p, mut w) = session();
    p.parse(b"\x1b[>1u", &mut w);
    assert!(on(&w));
    p.parse(b"\x1b[<u", &mut w);
    assert!(!on(&w));
}

#[test]
fn push_defaults_to_no_flags_and_pop_to_one_entry() {
    // "CSI > flags u  # for push, if flags omitted default to zero
    //  CSI < number u # to pop number entries, defaulting to 1 if unspecified"
    let (mut p, mut w) = session();
    p.parse(b"\x1b[>1u", &mut w);
    p.parse(b"\x1b[>u", &mut w);
    assert!(!on(&w), "a push with no flags asks for none");
    p.parse(b"\x1b[<u", &mut w);
    assert!(on(&w), "one entry came back");
}

#[test]
fn a_pop_restores_what_the_matching_push_saved() {
    let (mut p, mut w) = session();
    p.parse(b"\x1b[>1u", &mut w);
    p.parse(b"\x1b[>0u", &mut w);
    assert!(!on(&w));
    p.parse(b"\x1b[<1u", &mut w);
    assert!(on(&w), "the outer push is still in force");
}

#[test]
fn one_pop_can_take_several_entries() {
    let (mut p, mut w) = session();
    p.parse(b"\x1b[>1u\x1b[>0u\x1b[>1u", &mut w);
    p.parse(b"\x1b[<2u", &mut w);
    assert!(on(&w), "two entries back is the first push's flags");
}

#[test]
fn a_pop_that_empties_the_stack_resets_every_flag() {
    // "If a pop request is received that empties the stack, all flags are reset."
    let (mut p, mut w) = session();
    p.parse(b"\x1b[>1u", &mut w);
    p.parse(b"\x1b[<9u", &mut w);
    assert!(!on(&w));
    p.parse(b"\x1b[=1;1u\x1b[<1u", &mut w);
    assert!(!on(&w), "a pop with nothing saved still resets");
}

#[test]
fn a_full_stack_evicts_its_oldest_entry() {
    // "If a push request is received and the stack is full, the oldest entry
    //  from the stack must be evicted."
    let (mut p, mut w) = session();
    // the first push saves off; every later one saves on
    for _ in 0..17 {
        p.parse(b"\x1b[>1u", &mut w);
    }
    for _ in 0..16 {
        p.parse(b"\x1b[<1u", &mut w);
    }
    assert!(
        on(&w),
        "the oldest save went, so sixteen pops land on a push that saved on"
    );
}

// ─── One keyboard mode per screen ──────────────────────────────────

#[test]
fn the_alternate_screen_keeps_its_own_keyboard_mode() {
    // "a program that uses the alternate screen such as an editor can change
    //  the keyboard mode in the alternate screen only, without affecting the
    //  mode in the main screen or even knowing what that mode is"
    let (mut p, mut w) = session();
    p.parse(b"\x1b[>1u", &mut w);
    p.parse(b"\x1b[?1049h", &mut w);
    assert!(!on(&w), "the alternate screen starts from nothing");
    p.parse(b"\x1b[>0u\x1b[?1049l", &mut w);
    assert!(
        on(&w),
        "what the editor did in there does not follow it out"
    );
}

#[test]
fn the_alternate_screen_keeps_its_own_stack() {
    // "The main and alternate screens in the terminal emulator must maintain
    //  their own, independent, keyboard mode stacks."
    let (mut p, mut w) = session();
    p.parse(b"\x1b[>1u", &mut w);
    p.parse(b"\x1b[?1049h", &mut w);
    p.parse(b"\x1b[<9u", &mut w);
    p.parse(b"\x1b[?1049l", &mut w);
    assert!(on(&w), "an editor cannot pop the main screen's stack away");
}

// ─── Resets ────────────────────────────────────────────────────────

#[test]
fn a_full_reset_empties_the_stack_with_the_mode() {
    let (mut p, mut w) = session();
    p.parse(b"\x1b[>1u", &mut w);
    p.parse(b"\x1bc\x1b[?u", &mut w);
    assert_eq!(
        p.take_replies(),
        b"\x1b[?0u".to_vec(),
        "RIS cleared the flags"
    );
    p.parse(b"\x1b[<u\x1b[?u", &mut w);
    assert_eq!(
        p.take_replies(),
        b"\x1b[?0u".to_vec(),
        "a pop cannot restore flags the reset took away"
    );
    assert!(!on(&w));
}

// ─── Living beside the xterm extended-key modes ────────────────────

#[test]
fn an_xterm_extended_key_mode_survives_a_kitty_pop() {
    let (mut p, mut w) = session();
    p.parse(b"\x1b[>4;2m", &mut w);
    p.parse(b"\x1b[>1u", &mut w);
    p.parse(b"\x1b[<u", &mut w);
    assert_ne!(
        w.mode & MODE_KEYS_EXTENDED_2,
        0,
        "an application that never spoke kitty keeps its mode"
    );
}

// ─── Living beside the xterm extended-key modes, the other way round ───

#[test]
fn turning_on_modify_other_keys_takes_disambiguation_with_it() {
    // CSI > 4 ; 2 m clears every extended-key mode, the kitty bit among them,
    // so the negotiation has to end too
    let (mut p, mut w) = session();
    p.parse(b"\x1b[>1u", &mut w);
    p.parse(b"\x1b[>4;2m", &mut w);
    assert!(!on(&w), "the mode the pane is in");
    p.parse(b"\x1b[?u", &mut w);
    assert_eq!(
        p.take_replies(),
        b"\x1b[?0u".to_vec(),
        "and the mode the application is told about"
    );
}

#[test]
fn a_later_kitty_request_does_not_resurrect_a_cleared_mode() {
    // a request that names no bits changed nothing, so it must turn nothing on
    let (mut p, mut w) = session();
    p.parse(b"\x1b[>1u\x1b[>4;2m", &mut w);
    p.parse(b"\x1b[=0;2u", &mut w);
    assert!(!on(&w));
    assert_ne!(
        w.mode & MODE_KEYS_EXTENDED_2,
        0,
        "the xterm mode the application asked for is still its own"
    );
}

#[test]
fn modify_other_keys_off_ends_the_negotiation_too() {
    let (mut p, mut w) = session();
    p.parse(b"\x1b[>1u\x1b[>4m\x1b[?u", &mut w);
    assert!(!on(&w));
    assert_eq!(p.take_replies(), b"\x1b[?0u".to_vec());
}

#[test]
fn a_pop_can_still_bring_disambiguation_back() {
    // clearing the flags does not empty the stack: a pop is a kitty request,
    // and those own this bit
    let (mut p, mut w) = session();
    p.parse(b"\x1b[>1u\x1b[>1u\x1b[>4;2m", &mut w);
    assert!(!on(&w));
    p.parse(b"\x1b[<1u", &mut w);
    assert!(on(&w), "the save is untouched");
}

#[test]
fn a_second_request_for_the_alternate_screen_keeps_its_negotiation() {
    // the editor is already in there; ?1049h again is not a fresh start
    let (mut p, mut w) = session();
    p.parse(b"\x1b[?1049h", &mut w);
    p.parse(b"\x1b[>1u", &mut w);
    assert!(on(&w));
    p.parse(b"\x1b[?1049h", &mut w);
    assert!(on(&w), "nothing switched, so nothing was renegotiated");
}
