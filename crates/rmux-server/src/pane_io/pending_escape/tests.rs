use std::{thread, time::Duration};

use tokio::time::Instant;

use super::{
    is_pending_escape, keyboard_escape_time, PendingEscapeFlush, PendingInputDispatch,
    RETAINED_CONTROL_IDLE_TIMEOUT,
};

#[path = "adapter_clock_tests.rs"]
mod adapter_clock_tests;

fn observe(
    flush: &mut PendingEscapeFlush,
    retained_before: usize,
    appended: usize,
    pending: &[u8],
) {
    flush.observe_input_dispatch(
        PendingInputDispatch {
            retained_before,
            appended,
            continued_bounded_keyboard: false,
        },
        pending,
    );
}

#[test]
fn kitty_review_r3_quarantine_keeps_its_original_escape_deadline() {
    for prefix in [
        b"\x1b[97;x".to_vec(),
        format!("\x1b[97;x{}", "0".repeat(4090)).into_bytes(),
    ] {
        let mut flush = PendingEscapeFlush::default();
        flush.sync(b"\x1b[97;", Duration::from_millis(50));
        let first = flush.deadline().unwrap();
        observe(&mut flush, 5, prefix.len() - 5, &prefix);
        flush.sync(&prefix, Duration::from_secs(1));
        assert_eq!(flush.deadline(), Some(first));
        flush.sync(b"\x1b[0n", Duration::from_secs(1));
        assert!(
            flush.deadline().is_none(),
            "completed foreign CSI is not quarantined"
        );
    }
}

#[test]
fn every_retained_short_escape_family_arms_without_moving_its_deadline() {
    for input in [
        b"\x1b".as_slice(),
        b"\x1b[".as_slice(),
        b"\x1bO".as_slice(),
        b"\x1bP".as_slice(),
        b"\x1bX".as_slice(),
        b"\x1b]".as_slice(),
        b"\x1b^".as_slice(),
        b"\x1b_".as_slice(),
        b"\x1b\x1b".as_slice(),
        b"\x1b\x1b[".as_slice(),
        b"\x1b\x1b[15".as_slice(),
        b"\x1b\x1b[1;".as_slice(),
        b"\x1b\x1bO".as_slice(),
        b"\x90".as_slice(),
        b"\x98".as_slice(),
        b"\x9d".as_slice(),
        b"\x9e".as_slice(),
        b"\x9f".as_slice(),
        b"\x1b[20".as_slice(),
        b"\x1b[12;".as_slice(),
        b"\x1b[<".as_slice(),
        b"\x1b[M!".as_slice(),
        b"\x1b\xe6".as_slice(),
        b"\x1b\xe6\x97".as_slice(),
        b"\x1b\xf0\x9f".as_slice(),
        b"\x1b\xf0\x9f\x92".as_slice(),
    ] {
        assert!(is_pending_escape(input), "{input:?} must be classified");
        let mut flush = PendingEscapeFlush::default();
        flush.sync(input, Duration::from_millis(500));
        assert!(flush.deadline().is_some(), "{input:?} must arm");
    }

    let mut flush = PendingEscapeFlush::default();
    flush.sync(b"\x1b[", Duration::from_millis(500));
    let first = flush.deadline().expect("CSI opener arms");
    flush.sync(b"\x1b[12", Duration::from_millis(1));
    assert_eq!(
        flush.deadline(),
        Some(first),
        "numeric CSI growth must keep the original ambiguity deadline"
    );

    let mut meta_utf8 = PendingEscapeFlush::default();
    meta_utf8.sync(b"\x1b\xf0", Duration::from_millis(500));
    let first = meta_utf8.deadline().expect("Meta-UTF-8 lead arms");
    meta_utf8.sync(b"\x1b\xf0\x9f\x92", Duration::from_millis(1));
    assert_eq!(
        meta_utf8.deadline(),
        Some(first),
        "Meta-UTF-8 growth must stay classified until its expected length"
    );
}

#[test]
fn unterminated_palette_response_keeps_the_keyboard_deadline() {
    // tmux 3.7b: `tty_keys_palette` returns partial, so `ESC ] 4 ;` waits for
    // escape-time and is then read as M-] and typed text (`4;abc` in a prompt).
    for input in [
        b"\x1b]4".as_slice(),
        b"\x1b]4;".as_slice(),
        b"\x1b]4;abc".as_slice(),
        b"\x1b]4;1;rgb:1/2".as_slice(),
    ] {
        let mut flush = PendingEscapeFlush::default();
        observe(&mut flush, 0, input.len(), input);
        let before = Instant::now();
        flush.sync(input, Duration::from_millis(500));
        let deadline = flush.deadline().expect("unterminated OSC 4 must arm");
        assert!(
            deadline >= before + Duration::from_millis(500)
                && deadline < before + Duration::from_secs(1),
            "{input:?} must wait for escape-time, not the stream idle budget"
        );
    }

    let mut split = PendingEscapeFlush::default();
    observe(&mut split, 0, 2, b"\x1b]");
    split.sync(b"\x1b]", Duration::from_millis(500));
    let first = split.deadline().expect("ESC ] arms");
    for (before, grown) in [(2, b"\x1b]4;".as_slice()), (4, b"\x1b]4;abc".as_slice())] {
        observe(&mut split, before, grown.len() - before, grown);
        split.sync(grown, Duration::from_millis(1));
        assert_eq!(
            split.deadline(),
            Some(first),
            "{grown:?} must keep the deadline its first byte armed"
        );
    }
}

#[test]
fn active_palette_query_raises_the_keyboard_deadline_to_the_tmux_floor() {
    // tmux 3.7b tty-keys.c:979-985: `if (delay < 500) delay = 500;` while a
    // query is active, and never lowers a larger escape-time.
    let short = Duration::from_millis(10);
    let long = Duration::from_millis(3_000);
    assert_eq!(keyboard_escape_time(short, false), short);
    assert_eq!(
        keyboard_escape_time(short, true),
        Duration::from_millis(500)
    );
    assert_eq!(keyboard_escape_time(long, true), long);
}

#[test]
fn invalid_meta_bytes_use_the_keyboard_deadline() {
    for invalid in (0x80..=0xbf).chain(0xf8..=0xff) {
        let input = [b'\x1b', invalid];
        assert!(
            is_pending_escape(&input),
            "ESC + invalid UTF-8 byte {invalid:#04x} is retained by the key decoder"
        );

        let mut flush = PendingEscapeFlush::default();
        let before = Instant::now();
        flush.sync(&input, Duration::from_millis(500));
        let deadline = flush.deadline().expect("retained input must time out");
        assert!(deadline >= before + Duration::from_millis(500));
        assert!(
            deadline < before + Duration::from_secs(1),
            "invalid Meta input must not receive the 8-second stream budget"
        );
    }
}

#[test]
fn streaming_escape_bodies_use_a_long_idle_and_rearm_only_on_growth() {
    for (opener, grown) in [
        (b"\x1b]52;".as_slice(), b"\x1b]52;c;AAAA".as_slice()),
        (b"\x1b_G".as_slice(), b"\x1b_Gi=7;AAAA".as_slice()),
        (b"\x1b[200~".as_slice(), b"\x1b[200~slow body".as_slice()),
    ] {
        let mut flush = PendingEscapeFlush::default();
        let before = Instant::now();
        flush.sync(opener, Duration::from_millis(10));
        let armed = flush.deadline().expect("stream opener must arm");
        assert!(
            armed >= before + RETAINED_CONTROL_IDLE_TIMEOUT,
            "{opener:?} must not inherit the 10 ms keyboard escape-time"
        );
        assert!(
            armed <= Instant::now() + RETAINED_CONTROL_IDLE_TIMEOUT,
            "{opener:?} must still expire after the dedicated idle budget"
        );

        flush.sync(grown, Duration::from_millis(10));
        let rearmed = flush.deadline().expect("stream growth must rearm");
        assert!(rearmed >= armed, "{grown:?} must move the idle deadline");

        flush.sync(grown, Duration::from_millis(1));
        assert_eq!(
            flush.deadline(),
            Some(rearmed),
            "unchanged stream must not move its deadline"
        );
    }
}

#[test]
fn every_modal_terminal_string_body_gets_a_bounded_streaming_deadline() {
    for input in [
        b"\x1bPbody".as_slice(),
        b"\x1bXbody".as_slice(),
        b"\x1b]777;body".as_slice(),
        b"\x1b^body".as_slice(),
        b"\x1b_Qbody".as_slice(),
        b"\x90body".as_slice(),
        b"\x98body".as_slice(),
        b"\x9dbody".as_slice(),
        b"\x9ebody".as_slice(),
        b"\x9fbody".as_slice(),
    ] {
        let mut flush = PendingEscapeFlush::default();
        let before = Instant::now();
        flush.sync(input, Duration::from_millis(10));
        let deadline = flush.deadline().expect("terminal string body must arm");
        assert!(
            deadline >= before + RETAINED_CONTROL_IDLE_TIMEOUT,
            "{input:?} must use the bounded streaming idle budget"
        );
        assert!(
            deadline <= Instant::now() + RETAINED_CONTROL_IDLE_TIMEOUT,
            "{input:?} must not be retained indefinitely"
        );
    }
}

#[test]
fn recognized_terminal_control_openers_promote_to_streaming_deadline() {
    for (prefix, control_like, grown) in [
        (
            b"\x1b]".as_slice(),
            b"\x1b]52;".as_slice(),
            b"\x1b]52;c;AAAA".as_slice(),
        ),
        (
            b"\x1b_".as_slice(),
            b"\x1b_G".as_slice(),
            b"\x1b_Gi=7;AAAA".as_slice(),
        ),
        (
            b"\x1bP".as_slice(),
            b"\x1bPq".as_slice(),
            b"\x1bPquery".as_slice(),
        ),
        (
            b"\x90".as_slice(),
            b"\x90q".as_slice(),
            b"\x90query".as_slice(),
        ),
    ] {
        let mut flush = PendingEscapeFlush::default();
        flush.sync(prefix, Duration::from_millis(500));
        let keyboard_deadline = flush.deadline().expect("Meta prefix must arm");

        let before_stream = Instant::now();
        flush.sync(control_like, Duration::from_millis(500));
        let streaming_deadline = flush.deadline().expect("stream opener must arm");
        assert!(
            streaming_deadline >= before_stream + RETAINED_CONTROL_IDLE_TIMEOUT,
            "{control_like:?} must outlive the Meta-key deadline"
        );
        assert!(streaming_deadline > keyboard_deadline);

        flush.sync(grown, Duration::from_millis(500));
        assert!(
            flush.deadline().expect("grown stream remains armed") >= streaming_deadline,
            "{grown:?} growth must refresh the streaming idle deadline"
        );
    }

    let mut split_prefix = PendingEscapeFlush::default();
    split_prefix.sync(b"\x1b", Duration::from_millis(500));
    let keyboard_deadline = split_prefix.deadline().expect("Escape must arm");
    let before_stream = Instant::now();
    split_prefix.sync(b"\x1b]52;c;AAAA", Duration::from_millis(500));
    assert!(
        split_prefix
            .deadline()
            .is_some_and(|deadline| deadline >= before_stream + RETAINED_CONTROL_IDLE_TIMEOUT),
        "an ESC / ] transport split must promote once the OSC opener is recognized"
    );
    assert!(split_prefix.deadline() > Some(keyboard_deadline));
}

#[test]
fn coalesced_true_osc_and_apc_growth_keep_streaming_provenance() {
    for (opener, grown) in [
        (b"\x1b]52;c;AA".as_slice(), b"\x1b]52;c;AAAA".as_slice()),
        (b"\x1b_Gi=7;PAY".as_slice(), b"\x1b_Gi=7;PAYLOAD".as_slice()),
    ] {
        let mut flush = PendingEscapeFlush::default();
        observe(&mut flush, 0, opener.len(), opener);
        observe(&mut flush, opener.len(), grown.len() - opener.len(), grown);

        let before = Instant::now();
        flush.sync(grown, Duration::from_millis(500));
        assert!(
            flush
                .deadline()
                .is_some_and(|deadline| deadline >= before + RETAINED_CONTROL_IDLE_TIMEOUT),
            "{grown:?} must not acquire Meta provenance while frames coalesce"
        );
    }
}

#[test]
fn validated_paste_and_apc_survive_a_pause_longer_than_escape_time() {
    let escape_time = Duration::from_millis(5);
    let mut paste = PendingEscapeFlush::default();
    let mut apc = PendingEscapeFlush::default();
    paste.sync(b"\x1b[200~slow body", escape_time);
    apc.sync(b"\x1b_Gi=7;slow-body", escape_time);
    let paste_deadline = paste.deadline().expect("paste idle deadline");
    let apc_deadline = apc.deadline().expect("APC idle deadline");

    thread::sleep(Duration::from_millis(15));

    assert!(Instant::now() < paste_deadline);
    assert!(Instant::now() < apc_deadline);
    paste.sync(b"\x1b[200~slow body", escape_time);
    apc.sync(b"\x1b_Gi=7;slow-body", escape_time);
    assert_eq!(paste.deadline(), Some(paste_deadline));
    assert_eq!(apc.deadline(), Some(apc_deadline));
}

#[test]
fn stream_completion_rearms_a_new_ambiguous_suffix_with_escape_time() {
    let mut flush = PendingEscapeFlush::default();
    let paste = b"\x1b[200~body";
    flush.sync(paste, Duration::from_millis(10));
    let streaming_deadline = flush.deadline().expect("paste stream arms");

    observe(&mut flush, paste.len(), 1, b"\x1b");
    let before_escape = Instant::now();
    flush.sync(b"\x1b", Duration::from_millis(10));
    let escape_deadline = flush.deadline().expect("new Escape suffix arms");
    assert!(escape_deadline >= before_escape + Duration::from_millis(10));
    assert!(escape_deadline < streaming_deadline);

    observe(&mut flush, 1, 3, b"\x1b[12");
    flush.sync(b"\x1b[12", Duration::from_secs(1));
    assert_eq!(
        flush.deadline(),
        Some(escape_deadline),
        "growth of the same keyboard ambiguity keeps its original deadline"
    );

    observe(&mut flush, 4, 3, b"\x1b_G");
    let before_stream = Instant::now();
    flush.sync(b"\x1b_G", Duration::from_millis(10));
    assert!(
        flush.deadline().expect("APC stream arms") >= before_stream + RETAINED_CONTROL_IDLE_TIMEOUT,
        "an ambiguous-to-streaming transition restores the long idle budget"
    );
}

#[test]
fn dispatch_observation_distinguishes_continuation_from_replacement() {
    let mut flush = PendingEscapeFlush::default();
    flush.sync(b"\x1b", Duration::from_secs(1));
    let original_deadline = flush.deadline().expect("Escape arms");

    observe(&mut flush, 1, 2, b"\x1b[1");
    flush.sync(b"\x1b[1", Duration::from_secs(3));
    assert_eq!(
        flush.deadline(),
        Some(original_deadline),
        "retaining the entire old prefix plus every appended byte is a continuation"
    );

    observe(&mut flush, 3, 2, b"\x1b");
    assert!(
        flush.deadline().is_none(),
        "consuming input before retaining a suffix starts a new lifetime"
    );
    flush.sync(b"\x1b", Duration::from_secs(3));
    assert!(
        flush.deadline().expect("replacement Escape arms")
            > original_deadline + Duration::from_secs(1),
        "the replacement receives its own deadline"
    );
}

#[test]
fn non_escape_retention_and_complete_sequences_do_not_arm() {
    for input in [
        b"".as_slice(),
        b"plain".as_slice(),
        b"\xe6\x97".as_slice(),
        b"\xf0\x9f\x92".as_slice(),
        b"\x1b\xe6\x97\xa5".as_slice(),
        b"\x1b\xf0\x9f\x92\xa1".as_slice(),
        b"\x1b[A".as_slice(),
        b"\x1b[<a".as_slice(),
        b"\x1b\x1b[15~".as_slice(),
        b"\x1b\x1bOP".as_slice(),
    ] {
        let mut flush = PendingEscapeFlush::default();
        flush.sync(input, Duration::from_millis(500));
        assert!(
            flush.deadline().is_none(),
            "{input:?} is not a timed retained escape state"
        );
    }
}
fn retained_keyboard(bytes: &[u8]) -> Vec<u8> {
    let mut decoder = super::AcquiredDecoder::new();
    decoder.feed(bytes, |_| {});
    assert!(decoder.has_pending_keyboard_frame());
    let mut retained = decoder.retained().to_vec();
    if decoder.pending_len() > retained.len() {
        retained.push(b'0');
    }
    retained
}

#[test]
fn kitty_rereview_r3_storage_cap_does_not_restart_the_deadline() {
    for count in [4080, 4090, 4100] {
        let before = retained_keyboard(format!("\x1b[97;x{}", "0".repeat(count)).as_bytes());
        let appended = vec![b'0'; 40];
        let after = retained_keyboard(&[before.as_slice(), &appended].concat());
        let mut flush = PendingEscapeFlush::default();
        flush.sync(&before, Duration::from_millis(50));
        let first = flush.deadline();
        flush.observe_input_dispatch(PendingInputDispatch::capture(&before, &appended), &after);
        flush.sync(&after, Duration::from_secs(1));
        assert_eq!(flush.deadline(), first, "storage cap at {count}");
    }
}

#[test]
fn kitty_rereview_r3_short_opener_keeps_deadline_when_one_dispatch_crosses_the_cap() {
    for (before, appended) in [
        (b"\x1b".as_slice(), format!("[97;x{}", "0".repeat(4100))),
        (b"\x1b[".as_slice(), format!("97;x{}", "0".repeat(4100))),
    ] {
        let after = retained_keyboard(&[before, appended.as_bytes()].concat());
        let mut flush = PendingEscapeFlush::default();
        flush.sync(before, Duration::from_millis(50));
        let first = flush.deadline();
        flush.observe_input_dispatch(
            PendingInputDispatch::capture(before, appended.as_bytes()),
            &after,
        );
        flush.sync(&after, Duration::from_secs(1));
        assert_eq!(flush.deadline(), first);
    }
}

#[test]
fn kitty_rereview_r3_identical_bounded_suffix_has_a_new_lifetime_after_boundary() {
    let prefix = format!("\x1b[97;x{}", "0".repeat(4100)).into_bytes();
    let before = retained_keyboard(&prefix);
    for boundary in [b"u".as_slice(), b"\r", b""] {
        // The replacement opener itself is an ESC interruption in the last case.
        let appended = [boundary, &prefix].concat();
        let after = retained_keyboard(&[before.as_slice(), &appended].concat());
        assert_eq!(after, before);
        let mut flush = PendingEscapeFlush::default();
        flush.sync(&before, Duration::from_millis(50));
        let first = flush.deadline().unwrap();
        flush.observe_input_dispatch(PendingInputDispatch::capture(&before, &appended), &after);
        assert_eq!(flush.deadline(), None);
        flush.sync(&after, Duration::from_secs(1));
        assert!(flush.deadline().unwrap() > first);
    }
}
