use std::collections::hash_map::DefaultHasher;
use std::fmt::Debug;
use std::hash::{Hash, Hasher};
use std::time::{Duration, Instant};

use serde::de::DeserializeOwned;
use serde::Serialize;

use rmux_sdk::{
    DetachChord, DetachDetector, DetachOutcome, KeyCode, KeyConversionError, KeyEvent, KeyModifiers,
};

fn assert_dto_bounds<T>()
where
    T: Send + Sync + 'static + Clone + Eq + Hash + Debug,
{
}

fn assert_send_sync_static<T: Send + Sync + 'static>() {}

fn round_trip<T>(value: T) -> T
where
    T: Serialize + DeserializeOwned + PartialEq + Debug,
{
    let bytes = bincode::serialize(&value).expect("bincode serializes");
    let decoded = bincode::deserialize::<T>(&bytes).expect("bincode deserializes");
    assert_eq!(decoded, value);

    let json = serde_json::to_string(&value).expect("json serializes");
    let decoded_json = serde_json::from_str::<T>(&json).expect("json deserializes");
    assert_eq!(decoded_json, value);

    decoded
}

#[test]
fn dto_value_objects_pin_thread_safe_value_bounds() {
    assert_dto_bounds::<KeyEvent>();
    assert_dto_bounds::<KeyCode>();
    assert_dto_bounds::<KeyModifiers>();
    assert_dto_bounds::<DetachChord>();
    assert_send_sync_static::<DetachDetector>();
}

#[test]
fn modifier_bitfield_rejects_reserved_bits() {
    assert!(KeyModifiers::from_bits(0b0011_1111).is_some());
    assert!(KeyModifiers::from_bits(0b0100_0000).is_none());
    assert!(KeyModifiers::from_bits(0b1000_0000).is_none());

    let truncated = KeyModifiers::from_bits_truncate(0b1100_0001);
    assert_eq!(truncated, KeyModifiers::SHIFT);
    assert_eq!(truncated.bits(), 0b0000_0001);
}

#[test]
fn modifier_bit_ops_match_set_algebra() {
    let combined = KeyModifiers::CONTROL | KeyModifiers::ALT;
    assert!(combined.contains(KeyModifiers::CONTROL));
    assert!(combined.contains(KeyModifiers::ALT));
    assert!(!combined.contains(KeyModifiers::SHIFT));
    assert_eq!(combined & KeyModifiers::CONTROL, KeyModifiers::CONTROL);
    assert_eq!(
        combined ^ KeyModifiers::CONTROL,
        KeyModifiers::ALT,
        "xor with a held bit clears it",
    );
    assert!(KeyModifiers::empty().is_empty());
}

#[test]
fn key_event_serde_round_trip_preserves_code_and_modifiers() {
    round_trip(KeyEvent::bare(KeyCode::Char('a')));
    round_trip(KeyEvent::ctrl('b'));
    round_trip(KeyEvent::new(
        KeyCode::F(12),
        KeyModifiers::SHIFT | KeyModifiers::ALT,
    ));
    round_trip(KeyEvent::bare(KeyCode::Esc));
    round_trip(KeyEvent::bare(KeyCode::PageDown));
    round_trip(KeyEvent::bare(KeyCode::BackTab));
    round_trip(DetachChord::tmux_default());
}

#[test]
fn modifiers_serialize_as_transparent_bits() {
    let json = serde_json::to_string(&(KeyModifiers::CONTROL | KeyModifiers::SHIFT))
        .expect("modifiers serialize");
    assert_eq!(json, "3");
    let decoded: KeyModifiers = serde_json::from_str("3").expect("modifiers deserialize");
    assert_eq!(decoded, KeyModifiers::CONTROL | KeyModifiers::SHIFT);
}

#[test]
fn modifiers_deserialize_rejects_reserved_bits_via_json() {
    let err = serde_json::from_str::<KeyModifiers>("128")
        .expect_err("reserved bit 0b1000_0000 must be rejected");
    let msg = err.to_string();
    assert!(
        msg.contains("KeyModifiers"),
        "deserializer message should reference the type, got: {msg}",
    );

    assert!(
        serde_json::from_str::<KeyModifiers>("64").is_err(),
        "bit 0b0100_0000 is reserved and must be rejected",
    );

    assert!(
        serde_json::from_str::<KeyModifiers>("63").is_ok(),
        "bits 0b0011_1111 must round-trip in full",
    );
}

#[test]
fn modifiers_deserialize_rejects_reserved_bits_via_bincode() {
    let raw: u8 = 0b1100_0000;
    let bytes = bincode::serialize(&raw).expect("u8 bincode serializes");
    let result: bincode::Result<KeyModifiers> = bincode::deserialize(&bytes);
    assert!(
        result.is_err(),
        "bincode must propagate the from_bits validation",
    );
}

#[test]
fn modifiers_deserialize_zero_bits_yields_empty() {
    let decoded: KeyModifiers = serde_json::from_str("0").expect("zero must deserialize as empty");
    assert!(decoded.is_empty());
}

#[test]
fn detach_detector_idle_passes_through_non_prefix_keys() {
    let mut det = DetachDetector::new(DetachChord::tmux_default());
    let now = Instant::now();
    let event = KeyEvent::bare(KeyCode::Char('a'));
    assert_eq!(det.feed(event, now), DetachOutcome::Forward(vec![event]));
    assert!(!det.is_prefix_armed());
}

#[test]
fn detach_detector_arms_on_prefix_only() {
    let mut det = DetachDetector::new(DetachChord::tmux_default());
    let now = Instant::now();
    assert_eq!(
        det.feed(KeyEvent::ctrl('b'), now),
        DetachOutcome::Armed,
        "prefix-only state should be reported as Armed",
    );
    assert!(det.is_prefix_armed());

    assert_eq!(
        det.tick(now + Duration::from_millis(10)),
        DetachOutcome::Armed,
        "tick before timeout keeps the detector armed",
    );
    assert!(det.is_prefix_armed());
}

#[test]
fn detach_detector_completes_chord_with_detach_key() {
    let mut det = DetachDetector::new(DetachChord::tmux_default());
    let t0 = Instant::now();
    assert_eq!(det.feed(KeyEvent::ctrl('b'), t0), DetachOutcome::Armed);

    let t1 = t0 + Duration::from_millis(50);
    assert_eq!(
        det.feed(KeyEvent::bare(KeyCode::Char('d')), t1),
        DetachOutcome::DetachRequested,
    );
    assert!(!det.is_prefix_armed());
}

#[test]
fn detach_detector_mismatch_forwards_prefix_then_event_and_resets() {
    let mut det = DetachDetector::new(DetachChord::tmux_default());
    let t0 = Instant::now();
    assert_eq!(det.feed(KeyEvent::ctrl('b'), t0), DetachOutcome::Armed);

    let t1 = t0 + Duration::from_millis(40);
    let stray = KeyEvent::bare(KeyCode::Char('x'));
    assert_eq!(
        det.feed(stray, t1),
        DetachOutcome::Forward(vec![KeyEvent::ctrl('b'), stray]),
    );
    assert!(
        !det.is_prefix_armed(),
        "mismatch must drop the held prefix to idle",
    );
}

#[test]
fn detach_detector_double_prefix_emits_one_prefix_then_re_arms() {
    let mut det = DetachDetector::new(DetachChord::tmux_default());
    let t0 = Instant::now();
    assert_eq!(det.feed(KeyEvent::ctrl('b'), t0), DetachOutcome::Armed);

    let t1 = t0 + Duration::from_millis(20);
    assert_eq!(
        det.feed(KeyEvent::ctrl('b'), t1),
        DetachOutcome::Forward(vec![KeyEvent::ctrl('b')]),
        "double-prefix forwards the first prefix and re-arms",
    );
    assert!(det.is_prefix_armed());

    let t2 = t1 + Duration::from_millis(10);
    assert_eq!(
        det.feed(KeyEvent::bare(KeyCode::Char('d')), t2),
        DetachOutcome::DetachRequested,
    );
}

#[test]
fn detach_detector_timeout_releases_prefix_via_tick() {
    let timeout = Duration::from_millis(500);
    let mut det = DetachDetector::with_timeout(DetachChord::tmux_default(), timeout);
    let t0 = Instant::now();
    assert_eq!(det.feed(KeyEvent::ctrl('b'), t0), DetachOutcome::Armed);

    assert_eq!(
        det.tick(t0 + timeout - Duration::from_millis(1)),
        DetachOutcome::Armed,
    );
    assert!(det.is_prefix_armed());

    assert_eq!(
        det.tick(t0 + timeout),
        DetachOutcome::Forward(vec![KeyEvent::ctrl('b')]),
        "tick at the timeout boundary releases the prefix",
    );
    assert!(!det.is_prefix_armed());

    assert_eq!(
        det.tick(t0 + timeout + Duration::from_millis(10)),
        DetachOutcome::Forward(Vec::new()),
        "post-release tick is a no-op forward",
    );
}

#[test]
fn detach_detector_feed_after_timeout_releases_prefix_then_processes_event() {
    let timeout = Duration::from_millis(250);
    let mut det = DetachDetector::with_timeout(DetachChord::tmux_default(), timeout);
    let t0 = Instant::now();
    assert_eq!(det.feed(KeyEvent::ctrl('b'), t0), DetachOutcome::Armed);

    let t1 = t0 + timeout + Duration::from_millis(5);
    let printable = KeyEvent::bare(KeyCode::Char('z'));
    assert_eq!(
        det.feed(printable, t1),
        DetachOutcome::Forward(vec![KeyEvent::ctrl('b'), printable]),
        "expired prefix must be flushed before the new event is forwarded",
    );
    assert!(!det.is_prefix_armed());
}

#[test]
fn detach_detector_feed_after_timeout_with_detach_key_does_not_detach() {
    let timeout = Duration::from_millis(100);
    let mut det = DetachDetector::with_timeout(DetachChord::tmux_default(), timeout);
    let t0 = Instant::now();
    assert_eq!(det.feed(KeyEvent::ctrl('b'), t0), DetachOutcome::Armed);

    let t1 = t0 + timeout + Duration::from_millis(50);
    let detach_key = KeyEvent::bare(KeyCode::Char('d'));
    assert_eq!(
        det.feed(detach_key, t1),
        DetachOutcome::Forward(vec![KeyEvent::ctrl('b'), detach_key]),
        "after timeout the detach key is forwarded, not treated as a chord completion",
    );
    assert!(!det.is_prefix_armed());
}

#[test]
fn detach_detector_reset_clears_armed_state_without_emitting() {
    let mut det = DetachDetector::new(DetachChord::tmux_default());
    let now = Instant::now();
    assert_eq!(det.feed(KeyEvent::ctrl('b'), now), DetachOutcome::Armed);
    det.reset();
    assert!(!det.is_prefix_armed());
    assert_eq!(det.tick(now), DetachOutcome::Forward(Vec::new()));
}

#[test]
fn detach_detector_supports_custom_chord_with_distinct_modifiers() {
    let chord = DetachChord::new(
        KeyEvent::ctrl('a'),
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::SHIFT),
    );
    let mut det = DetachDetector::new(chord);
    let t0 = Instant::now();
    assert_eq!(
        det.feed(KeyEvent::bare(KeyCode::Char('q')), t0),
        DetachOutcome::Forward(vec![KeyEvent::bare(KeyCode::Char('q'))]),
        "unmodified detach key without prefix is forwarded as a normal key",
    );

    assert_eq!(det.feed(KeyEvent::ctrl('a'), t0), DetachOutcome::Armed);
    assert_eq!(
        det.feed(KeyEvent::bare(KeyCode::Char('q')), t0),
        DetachOutcome::Forward(vec![
            KeyEvent::ctrl('a'),
            KeyEvent::bare(KeyCode::Char('q')),
        ]),
        "follow-up that lacks the configured modifier must not detach",
    );

    assert_eq!(det.feed(KeyEvent::ctrl('a'), t0), DetachOutcome::Armed);
    assert_eq!(
        det.feed(KeyEvent::new(KeyCode::Char('q'), KeyModifiers::SHIFT), t0,),
        DetachOutcome::DetachRequested,
    );
}

#[test]
fn detach_detector_clock_uses_saturating_durations() {
    let mut det = DetachDetector::new(DetachChord::tmux_default());
    let t0 = Instant::now();
    assert_eq!(det.feed(KeyEvent::ctrl('b'), t0), DetachOutcome::Armed);
    assert_eq!(
        det.tick(t0),
        DetachOutcome::Armed,
        "tick at the same instant the prefix arrived must not expire it",
    );
    assert!(det.is_prefix_armed());
}

#[test]
fn detach_detector_clock_skew_does_not_expire_prefix() {
    let mut det = DetachDetector::new(DetachChord::tmux_default());
    let t0 = Instant::now();
    assert_eq!(det.feed(KeyEvent::ctrl('b'), t0), DetachOutcome::Armed);

    // A `now` that's "earlier" than `since` saturates to zero, so the
    // detector must stay armed instead of treating clock skew as a timeout.
    let earlier = t0;
    assert_eq!(det.tick(earlier), DetachOutcome::Armed);
    assert!(det.is_prefix_armed());
}

#[test]
fn detach_detector_feed_after_timeout_with_prefix_flushes_old_and_rearms() {
    let timeout = Duration::from_millis(150);
    let mut det = DetachDetector::with_timeout(DetachChord::tmux_default(), timeout);
    let t0 = Instant::now();
    assert_eq!(det.feed(KeyEvent::ctrl('b'), t0), DetachOutcome::Armed);

    // After the prefix expires, feeding the prefix again flushes the old
    // one and silently re-arms with the new timestamp.
    let t1 = t0 + timeout + Duration::from_millis(5);
    assert_eq!(
        det.feed(KeyEvent::ctrl('b'), t1),
        DetachOutcome::Forward(vec![KeyEvent::ctrl('b')]),
        "expired prefix must flush before the new prefix re-arms the detector",
    );
    assert!(
        det.is_prefix_armed(),
        "second prefix must leave the detector in PrefixHeld state",
    );

    // Confirm the new arming uses the new timestamp by completing the
    // chord well within the timeout measured from `t1`.
    let t2 = t1 + Duration::from_millis(10);
    assert_eq!(
        det.feed(KeyEvent::bare(KeyCode::Char('d')), t2),
        DetachOutcome::DetachRequested,
    );
}

#[test]
fn detach_detector_zero_duration_timeout_expires_immediately() {
    let mut det = DetachDetector::with_timeout(DetachChord::tmux_default(), Duration::ZERO);
    assert_eq!(det.timeout(), Duration::ZERO);

    let t0 = Instant::now();
    assert_eq!(det.feed(KeyEvent::ctrl('b'), t0), DetachOutcome::Armed);

    // 0 >= 0 so the very next tick at the same instant releases the prefix.
    assert_eq!(
        det.tick(t0),
        DetachOutcome::Forward(vec![KeyEvent::ctrl('b')]),
        "zero timeout must release the prefix on the very next observation",
    );
    assert!(!det.is_prefix_armed());

    // Even feeding the detach key directly after another armed prefix is
    // forwarded as two events because the prefix expired before the
    // follow-up was observed.
    let t1 = t0 + Duration::from_nanos(1);
    assert_eq!(det.feed(KeyEvent::ctrl('b'), t1), DetachOutcome::Armed);
    let t2 = t1 + Duration::from_nanos(1);
    let detach_key = KeyEvent::bare(KeyCode::Char('d'));
    assert_eq!(
        det.feed(detach_key, t2),
        DetachOutcome::Forward(vec![KeyEvent::ctrl('b'), detach_key]),
        "zero timeout makes any non-coincident follow-up a flush, never a chord",
    );
}

#[test]
fn detach_detector_degenerate_chord_with_equal_keys_detaches_on_repeat() {
    // When prefix and detach are the same event, the contract is that the
    // detach branch wins on the second press because `process_prefix_held`
    // checks the detach key first.
    let key = KeyEvent::ctrl('z');
    let chord = DetachChord::new(key, key);
    let mut det = DetachDetector::new(chord);
    let t0 = Instant::now();
    assert_eq!(det.feed(key, t0), DetachOutcome::Armed);
    assert_eq!(
        det.feed(key, t0 + Duration::from_millis(10)),
        DetachOutcome::DetachRequested,
        "for a degenerate chord, the detach branch must take precedence",
    );
    assert!(!det.is_prefix_armed());
}

#[test]
fn detach_detector_reusable_after_successful_detach() {
    let mut det = DetachDetector::new(DetachChord::tmux_default());
    let t0 = Instant::now();
    assert_eq!(det.feed(KeyEvent::ctrl('b'), t0), DetachOutcome::Armed);
    assert_eq!(
        det.feed(KeyEvent::bare(KeyCode::Char('d')), t0),
        DetachOutcome::DetachRequested,
    );
    assert!(!det.is_prefix_armed());

    // The detector must accept a brand-new chord cycle after a successful
    // detach. (Hosts may keep the detector across sessions.)
    let later = t0 + Duration::from_secs(60);
    assert_eq!(det.feed(KeyEvent::ctrl('b'), later), DetachOutcome::Armed);
    assert_eq!(
        det.feed(
            KeyEvent::bare(KeyCode::Char('d')),
            later + Duration::from_millis(1),
        ),
        DetachOutcome::DetachRequested,
    );
}

#[test]
fn detach_detector_chord_modifier_strict_match_does_not_arm_on_extra_modifiers() {
    // The default chord prefix is `Ctrl+B`; pressing `Ctrl+Shift+B` is a
    // distinct event and must NOT arm the detector.
    let mut det = DetachDetector::new(DetachChord::tmux_default());
    let t0 = Instant::now();
    let ctrl_shift_b = KeyEvent::new(
        KeyCode::Char('b'),
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    assert_eq!(
        det.feed(ctrl_shift_b, t0),
        DetachOutcome::Forward(vec![ctrl_shift_b]),
        "extra modifiers on the prefix key must not arm the detector",
    );
    assert!(!det.is_prefix_armed());
}

#[test]
fn detach_detector_idle_tick_yields_empty_forward() {
    // Ticking an idle detector is a no-op forward, regardless of timestamp.
    let mut det = DetachDetector::new(DetachChord::tmux_default());
    let t0 = Instant::now();
    assert_eq!(det.tick(t0), DetachOutcome::Forward(Vec::new()));
    assert_eq!(
        det.tick(t0 + Duration::from_secs(3600)),
        DetachOutcome::Forward(Vec::new()),
    );
    assert!(!det.is_prefix_armed());
}

#[test]
fn detach_detector_chord_accessor_returns_configured_chord() {
    let chord = DetachChord::new(
        KeyEvent::ctrl('a'),
        KeyEvent::new(KeyCode::Char('q'), KeyModifiers::SHIFT),
    );
    let det = DetachDetector::new(chord);
    assert_eq!(det.chord(), &chord);
    assert_eq!(det.timeout(), DetachDetector::DEFAULT_TIMEOUT);
}

#[test]
fn key_conversion_error_pins_value_object_bounds_and_display() {
    fn assert_error<E>()
    where
        E: Send + Sync + 'static + Clone + Eq + Hash + Debug + std::error::Error,
    {
    }
    assert_error::<KeyConversionError>();

    assert!(KeyConversionError::UnsupportedKeyCode("Foo")
        .to_string()
        .contains("Foo"));
    assert!(KeyConversionError::UnsupportedModifier("Bar")
        .to_string()
        .contains("Bar"));
    assert!(!KeyConversionError::NonPressEvent.to_string().is_empty());
}

#[test]
fn key_event_round_trip_covers_high_function_keys() {
    // The `KeyCode::F(u8)` carrier admits F1..=F35 by contract; round-trip
    // the upper end so encoders aren't accidentally truncating to a smaller
    // numeric type.
    let event = KeyEvent::new(KeyCode::F(35), KeyModifiers::CONTROL | KeyModifiers::ALT);
    round_trip(event);
}

fn hash_of<T: Hash>(value: &T) -> u64 {
    let mut hasher = DefaultHasher::new();
    value.hash(&mut hasher);
    hasher.finish()
}

#[test]
fn key_modifiers_hash_matches_eq_contract() {
    let a = KeyModifiers::CONTROL | KeyModifiers::SHIFT;
    let b = KeyModifiers::SHIFT | KeyModifiers::CONTROL;
    assert_eq!(a, b, "bitwise OR is commutative on modifier sets");
    assert_eq!(
        hash_of(&a),
        hash_of(&b),
        "Eq-equal KeyModifiers must hash equal so HashMap keys are stable",
    );
}

#[test]
fn key_event_hash_matches_eq_contract() {
    let a = KeyEvent::ctrl('b');
    let b = KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL);
    assert_eq!(a, b);
    assert_eq!(
        hash_of(&a),
        hash_of(&b),
        "Eq-equal KeyEvents must hash equal",
    );
}

#[test]
fn detach_outcome_variants_are_distinct() {
    // Forward(empty) and Armed must compare unequal so callers can branch
    // on the outcome without collapsing the "no events to forward" and
    // "swallowed prefix" cases.
    assert_ne!(DetachOutcome::Forward(Vec::new()), DetachOutcome::Armed);
    assert_ne!(DetachOutcome::Armed, DetachOutcome::DetachRequested);
    assert_ne!(
        DetachOutcome::Forward(Vec::new()),
        DetachOutcome::DetachRequested,
    );
}

#[test]
fn detach_detector_idle_after_successful_detach_emits_no_tick_output() {
    // After DetachRequested the detector must be idle so callers that
    // continue to poll the detector (e.g. before tearing down the attach
    // loop) do not see a stray prefix flush on the next tick.
    let mut det = DetachDetector::new(DetachChord::tmux_default());
    let t0 = Instant::now();
    assert_eq!(det.feed(KeyEvent::ctrl('b'), t0), DetachOutcome::Armed);
    assert_eq!(
        det.feed(KeyEvent::bare(KeyCode::Char('d')), t0),
        DetachOutcome::DetachRequested,
    );
    assert!(!det.is_prefix_armed());

    assert_eq!(
        det.tick(t0 + Duration::from_secs(60)),
        DetachOutcome::Forward(Vec::new()),
        "tick on an idle detector after detach must not flush a prefix",
    );
    assert_eq!(
        det.tick(t0),
        DetachOutcome::Forward(Vec::new()),
        "tick at any timestamp on an idle detector remains a no-op",
    );
}

#[test]
fn detach_detector_can_be_rearmed_after_tick_releases_prefix() {
    // tick() must not only release a held prefix but also leave the
    // detector in a state that accepts a brand-new chord cycle.
    let timeout = Duration::from_millis(100);
    let mut det = DetachDetector::with_timeout(DetachChord::tmux_default(), timeout);
    let t0 = Instant::now();
    assert_eq!(det.feed(KeyEvent::ctrl('b'), t0), DetachOutcome::Armed);
    assert_eq!(
        det.tick(t0 + timeout),
        DetachOutcome::Forward(vec![KeyEvent::ctrl('b')]),
    );
    assert!(!det.is_prefix_armed());

    let t1 = t0 + timeout + Duration::from_millis(5);
    assert_eq!(det.feed(KeyEvent::ctrl('b'), t1), DetachOutcome::Armed);
    assert!(det.is_prefix_armed());
    assert_eq!(
        det.feed(KeyEvent::bare(KeyCode::Char('d')), t1),
        DetachOutcome::DetachRequested,
    );
}

#[test]
fn detach_detector_clone_snapshots_armed_state_independently() {
    // Hosts may clone a detector to fork session state; the clone must
    // share the chord/timeout but evolve its state independently.
    let mut det = DetachDetector::new(DetachChord::tmux_default());
    let t0 = Instant::now();
    assert_eq!(det.feed(KeyEvent::ctrl('b'), t0), DetachOutcome::Armed);

    let mut twin = det.clone();
    assert!(twin.is_prefix_armed());
    assert_eq!(twin.chord(), det.chord());
    assert_eq!(twin.timeout(), det.timeout());

    twin.reset();
    assert!(!twin.is_prefix_armed());
    assert!(
        det.is_prefix_armed(),
        "resetting the clone must not mutate the source detector",
    );
}

#[test]
fn detach_chord_serde_round_trip_preserves_strict_modifier_set() {
    // Chord (de)serialization must keep modifier strictness so a
    // round-tripped chord doesn't accidentally start matching a relaxed
    // key combination.
    let chord = DetachChord::new(
        KeyEvent::new(
            KeyCode::Char('b'),
            KeyModifiers::CONTROL | KeyModifiers::SHIFT,
        ),
        KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT),
    );
    let decoded = round_trip(chord);
    assert_eq!(decoded, chord);
    assert_eq!(
        decoded.prefix.modifiers,
        KeyModifiers::CONTROL | KeyModifiers::SHIFT,
    );
    assert_eq!(decoded.detach.modifiers, KeyModifiers::ALT);
}

#[test]
fn key_event_constructors_agree() {
    // The ergonomic constructors must produce identical values to the
    // canonical `new` form so refactors that swap them stay safe.
    assert_eq!(
        KeyEvent::ctrl('b'),
        KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL),
    );
    assert_eq!(
        KeyEvent::bare(KeyCode::Esc),
        KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE),
    );
}

#[test]
fn detach_detector_idle_does_not_consume_non_prefix_events_state() {
    // Non-prefix events fed in idle must leave the detector idle even if
    // they happen to share a key code with the configured detach key.
    let mut det = DetachDetector::new(DetachChord::tmux_default());
    let t0 = Instant::now();
    let stray_d = KeyEvent::bare(KeyCode::Char('d'));
    assert_eq!(det.feed(stray_d, t0), DetachOutcome::Forward(vec![stray_d]),);
    assert!(!det.is_prefix_armed());
    let stray_d_with_alt = KeyEvent::new(KeyCode::Char('d'), KeyModifiers::ALT);
    assert_eq!(
        det.feed(stray_d_with_alt, t0),
        DetachOutcome::Forward(vec![stray_d_with_alt]),
    );
    assert!(!det.is_prefix_armed());
}

#[cfg(feature = "crossterm")]
mod crossterm_compat {
    use super::*;
    use crossterm::event::{
        KeyCode as CtKeyCode, KeyEvent as CtKeyEvent, KeyEventKind as CtKeyEventKind,
        KeyEventState as CtKeyEventState, KeyModifiers as CtKeyModifiers, MediaKeyCode,
    };

    #[test]
    fn crossterm_modifiers_round_trip_through_sdk() {
        let modifiers = CtKeyModifiers::CONTROL | CtKeyModifiers::ALT | CtKeyModifiers::SUPER;
        let sdk = KeyModifiers::from(modifiers);
        assert!(sdk.contains(KeyModifiers::CONTROL));
        assert!(sdk.contains(KeyModifiers::ALT));
        assert!(sdk.contains(KeyModifiers::SUPER));
        let back = CtKeyModifiers::from(sdk);
        assert_eq!(back, modifiers);
    }

    #[test]
    fn crossterm_supported_key_codes_round_trip() {
        let cases = [
            CtKeyCode::Char('a'),
            CtKeyCode::Enter,
            CtKeyCode::Esc,
            CtKeyCode::Backspace,
            CtKeyCode::Tab,
            CtKeyCode::BackTab,
            CtKeyCode::Up,
            CtKeyCode::Down,
            CtKeyCode::Left,
            CtKeyCode::Right,
            CtKeyCode::Home,
            CtKeyCode::End,
            CtKeyCode::PageUp,
            CtKeyCode::PageDown,
            CtKeyCode::Insert,
            CtKeyCode::Delete,
            CtKeyCode::F(5),
        ];
        for ct in cases {
            let sdk = KeyCode::try_from(ct).expect("supported key");
            assert_eq!(CtKeyCode::from(sdk), ct);
        }
    }

    #[test]
    fn crossterm_unsupported_key_codes_surface_typed_errors() {
        for ct in [
            CtKeyCode::Null,
            CtKeyCode::CapsLock,
            CtKeyCode::ScrollLock,
            CtKeyCode::NumLock,
            CtKeyCode::PrintScreen,
            CtKeyCode::Pause,
            CtKeyCode::Menu,
            CtKeyCode::KeypadBegin,
            CtKeyCode::Media(MediaKeyCode::Play),
        ] {
            let err = KeyCode::try_from(ct).expect_err("must reject unsupported variant");
            assert!(matches!(err, KeyConversionError::UnsupportedKeyCode(_)));
        }
    }

    #[test]
    fn crossterm_non_press_kinds_are_rejected() {
        let release = CtKeyEvent {
            code: CtKeyCode::Char('a'),
            modifiers: CtKeyModifiers::NONE,
            kind: CtKeyEventKind::Release,
            state: CtKeyEventState::empty(),
        };
        assert_eq!(
            KeyEvent::try_from(release),
            Err(KeyConversionError::NonPressEvent),
        );

        let repeat = CtKeyEvent {
            code: CtKeyCode::Char('a'),
            modifiers: CtKeyModifiers::NONE,
            kind: CtKeyEventKind::Repeat,
            state: CtKeyEventState::empty(),
        };
        assert_eq!(
            KeyEvent::try_from(repeat),
            Err(KeyConversionError::NonPressEvent),
            "repeat events must be rejected so callers don't double-count holds",
        );
    }

    #[test]
    fn crossterm_modifier_round_trip_drops_unknown_state_bits() {
        // KeyEventState bits (CapsLock, NumLock, KEYPAD) are intentionally
        // not modeled by the SDK; converting a press that carries them
        // must succeed without smuggling them into the SDK modifiers.
        let press = CtKeyEvent {
            code: CtKeyCode::Char('a'),
            modifiers: CtKeyModifiers::SHIFT,
            kind: CtKeyEventKind::Press,
            state: CtKeyEventState::CAPS_LOCK | CtKeyEventState::NUM_LOCK,
        };
        let sdk = KeyEvent::try_from(press).expect("press converts");
        assert_eq!(sdk.modifiers, KeyModifiers::SHIFT);
    }

    #[test]
    fn crossterm_press_round_trip_preserves_code_and_modifiers() {
        let press = CtKeyEvent::new(
            CtKeyCode::Char('q'),
            CtKeyModifiers::CONTROL | CtKeyModifiers::SHIFT,
        );
        let sdk = KeyEvent::try_from(press).expect("press converts");
        assert_eq!(sdk.code, KeyCode::Char('q'));
        assert!(sdk.modifiers.contains(KeyModifiers::CONTROL));
        assert!(sdk.modifiers.contains(KeyModifiers::SHIFT));

        let back = CtKeyEvent::from(sdk);
        assert_eq!(back.code, press.code);
        assert_eq!(back.modifiers, press.modifiers);
    }
}
