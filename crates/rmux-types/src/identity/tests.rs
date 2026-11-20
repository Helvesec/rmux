use super::{is_display_unsafe_format_char, PaneId, SessionId, SessionName, WindowId};
use crate::SessionNameError;

#[test]
fn canonical_inventory_ingestion_preserves_escaped_identity_without_serde() {
    let mut raw_names = vec![
        "ordinary".to_owned(),
        "a\\b".into(),
        "a\\\\b".into(),
        "literal\\n".into(),
        "line\nbreak".into(),
        "café界".into(),
        "line\u{2028}bidi\u{202e}zero\u{200b}".into(),
    ];
    raw_names.extend(
        (0_u8..=31)
            .chain([127])
            .map(|byte| format!("a{}b", char::from(byte))),
    );
    for raw in raw_names {
        let original = SessionName::new(raw).unwrap();
        assert_eq!(
            SessionName::from_canonical(original.as_str()).unwrap(),
            original
        );
    }
    assert_ne!(
        SessionName::new("literal\\n").unwrap(),
        SessionName::new("literal\n").unwrap()
    );
}

#[test]
fn canonical_inventory_ingestion_validates_and_normalizes_unsafe_text() {
    assert_eq!(SessionName::from_canonical(""), Err(SessionNameError));
    assert_eq!(
        SessionName::from_canonical("a:b.c\n\u{202e}\\q")
            .unwrap()
            .as_str(),
        "a_b_c\\n\\342\\200\\256\\q"
    );
}

#[test]
#[cfg(feature = "serde")]
fn canonical_inventory_ingestion_matches_existing_serde_policy() {
    for value in ["ordinary", "a\\\\b", "a:b.c\n\u{202e}\\q", "café界"] {
        let payload = bincode::serialize(value).unwrap();
        let from_wire: SessionName = bincode::deserialize(&payload).unwrap();
        assert_eq!(SessionName::from_canonical(value).unwrap(), from_wire);
    }
}

#[test]
fn session_name_rejects_empty_values() {
    assert_eq!(SessionName::new(""), Err(SessionNameError));
}

#[test]
fn session_name_rewrites_colon_and_dot() {
    assert_eq!(
        SessionName::new("alpha:beta.gamma")
            .expect("rewritten")
            .as_str(),
        "alpha_beta_gamma"
    );
}

#[test]
#[cfg(feature = "serde")]
fn session_name_round_trips_through_serde() {
    let payload = bincode::serialize("alpha.beta").expect("string encodes");
    assert_eq!(
        bincode::deserialize::<SessionName>(&payload).expect("rewritten on the wire"),
        SessionName::new("alpha_beta").expect("valid")
    );
}

#[test]
#[cfg(feature = "serde")]
fn session_name_serde_rejects_empty_payloads_truthfully() {
    let payload = bincode::serialize("").expect("empty string encodes");
    assert!(
        bincode::deserialize::<SessionName>(&payload).is_err(),
        "empty session names must fail deserialization rather than silently \
         producing an empty inner value"
    );
}

#[test]
#[cfg(feature = "serde")]
fn session_name_serialize_round_trips_after_rewriting() {
    let original = SessionName::new("alpha.beta").expect("rewrites dots");
    let bytes = bincode::serialize(&original).expect("session name encodes");
    let restored: SessionName =
        bincode::deserialize(&bytes).expect("session name decodes idempotently");
    assert_eq!(restored, original);
    assert_eq!(restored.as_str(), "alpha_beta");
}

#[test]
fn session_name_from_str_and_try_from_match_constructor() {
    let from_str: SessionName = "alpha:beta".parse().expect("FromStr rewrites");
    let try_from_ref: SessionName =
        SessionName::try_from("alpha:beta").expect("TryFrom<&str> rewrites");
    let try_from_owned: SessionName =
        SessionName::try_from(String::from("alpha:beta")).expect("TryFrom<String> rewrites");
    assert_eq!(from_str, try_from_ref);
    assert_eq!(from_str, try_from_owned);
    assert_eq!(from_str.as_str(), "alpha_beta");
}

#[test]
fn session_name_into_inner_returns_sanitized_string() {
    let owned = SessionName::new("alpha:beta")
        .expect("rewrites colons")
        .into_inner();
    assert_eq!(owned, "alpha_beta");
}

#[test]
fn session_name_escapes_line_and_paragraph_separators() {
    // U+2028/U+2029 are not Cc controls but break the single-line invariant.
    assert_eq!(
        SessionName::new("a\u{2028}b\u{2029}c")
            .expect("escaped")
            .as_str(),
        "a\\342\\200\\250b\\342\\200\\251c"
    );
}

#[test]
fn session_name_escapes_bidi_overrides_and_zero_width_marks() {
    // A right-to-left override could otherwise spoof the rendered name; a
    // zero-width space could hide content. Both must be escaped.
    let rendered = SessionName::new("a\u{202e}b\u{200b}c").expect("escaped");
    assert_eq!(rendered.as_str(), "a\\342\\200\\256b\\342\\200\\213c");
    assert!(!rendered.as_str().chars().any(is_display_unsafe_format_char));
}

#[test]
#[cfg(feature = "serde")]
fn session_name_sanitization_is_idempotent_through_serde() {
    // Deserialize still normalizes raw wire strings, but already-escaped
    // backslashes must survive unchanged.
    let original = SessionName::new("tab\there\u{2028}line\u{202e}rtl\x01ctl").expect("escaped");
    let bytes = bincode::serialize(&original).expect("encodes");
    let restored: SessionName = bincode::deserialize(&bytes).expect("decodes");
    assert_eq!(restored, original, "serde must preserve canonical escapes");
}

#[test]
fn session_name_escapes_backslashes_to_avoid_control_escape_collisions() {
    let literal_escape = SessionName::new("test\\nsession").expect("valid");
    let newline = SessionName::new("test\nsession").expect("valid");

    assert_eq!(literal_escape.as_str(), "test\\\\nsession");
    assert_eq!(newline.as_str(), "test\\nsession");
    assert_ne!(literal_escape, newline);
}

#[test]
fn session_id_displays_with_dollar_prefix() {
    assert_eq!(SessionId::new(7).to_string(), "$7");
    assert_eq!(SessionId::new(7).as_u32(), 7);
}

#[test]
fn window_id_displays_with_at_prefix() {
    assert_eq!(WindowId::new(9).to_string(), "@9");
    assert_eq!(WindowId::new(9).as_u32(), 9);
}

#[test]
fn window_id_zero_and_max_render_as_at_prefixed_decimal() {
    assert_eq!(WindowId::new(0).to_string(), "@0");
    assert_eq!(
        WindowId::new(u32::MAX).to_string(),
        format!("@{}", u32::MAX)
    );
}

#[test]
fn pane_id_displays_with_percent_prefix() {
    assert_eq!(PaneId::new(3).to_string(), "%3");
    assert_eq!(PaneId::new(3).as_u32(), 3);
}

#[test]
fn pane_id_zero_and_max_render_as_percent_prefixed_decimal() {
    assert_eq!(PaneId::new(0).to_string(), "%0");
    assert_eq!(PaneId::new(u32::MAX).to_string(), format!("%{}", u32::MAX));
}

#[test]
fn session_id_zero_and_max_render_as_dollar_prefixed_decimal() {
    assert_eq!(SessionId::new(0).to_string(), "$0");
    assert_eq!(
        SessionId::new(u32::MAX).to_string(),
        format!("${}", u32::MAX)
    );
}

#[test]
fn identity_newtypes_round_trip_through_u32_conversions() {
    for value in [0_u32, 1, 17, u32::MAX] {
        assert_eq!(u32::from(SessionId::from(value)), value);
        assert_eq!(u32::from(WindowId::from(value)), value);
        assert_eq!(u32::from(PaneId::from(value)), value);
        assert_eq!(SessionId::from(value).as_u32(), value);
        assert_eq!(WindowId::from(value).as_u32(), value);
        assert_eq!(PaneId::from(value).as_u32(), value);
    }
}

#[test]
#[cfg(feature = "serde")]
fn identity_newtypes_are_serde_transparent() {
    assert_eq!(
        bincode::serialize(&PaneId::new(11)).expect("encodes"),
        bincode::serialize(&11_u32).expect("encodes")
    );
    assert_eq!(
        bincode::serialize(&WindowId::new(11)).expect("encodes"),
        bincode::serialize(&11_u32).expect("encodes")
    );
    assert_eq!(
        bincode::serialize(&SessionId::new(11)).expect("encodes"),
        bincode::serialize(&11_u32).expect("encodes")
    );
}

#[test]
#[cfg(feature = "serde")]
fn identity_id_newtypes_decode_back_through_serde() {
    for value in [0_u32, 7, 257, u32::MAX] {
        let session_bytes = bincode::serialize(&SessionId::new(value)).expect("session id encodes");
        let window_bytes = bincode::serialize(&WindowId::new(value)).expect("window id encodes");
        let pane_bytes = bincode::serialize(&PaneId::new(value)).expect("pane id encodes");

        assert_eq!(
            bincode::deserialize::<SessionId>(&session_bytes).expect("session id decodes"),
            SessionId::new(value),
        );
        assert_eq!(
            bincode::deserialize::<WindowId>(&window_bytes).expect("window id decodes"),
            WindowId::new(value),
        );
        assert_eq!(
            bincode::deserialize::<PaneId>(&pane_bytes).expect("pane id decodes"),
            PaneId::new(value),
        );
    }
}

#[test]
fn identity_id_newtypes_total_order_matches_inner_u32() {
    let mut ids = [PaneId::new(3), PaneId::new(0), PaneId::new(1)];
    ids.sort();
    assert_eq!(ids, [PaneId::new(0), PaneId::new(1), PaneId::new(3)]);
}

#[test]
#[cfg(feature = "serde")]
fn session_name_already_sanitized_round_trips_through_serde() {
    let original = SessionName::new("alpha-beta_gamma").expect("printable name");
    let bytes = bincode::serialize(&original).expect("session name encodes");
    let restored: SessionName =
        bincode::deserialize(&bytes).expect("session name decodes idempotently");
    assert_eq!(restored, original);
}
