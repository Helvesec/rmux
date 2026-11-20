use serde_json::Value;

use super::*;

mod acquired;

const VECTORS: &str = include_str!("../../../../tests/conformance/kitty_keyboard/vectors.jsonl");

const EVENT_IDS: &[&str] = &[
    "K-FUNC-ENTER-1",
    "K-FUNC-TAB-1",
    "K-FUNC-BACKSPACE-1",
    "K-FUNC-UP-1",
    "K-FUNC-HOME-1",
    "K-FUNC-HOME-2",
    "K-FUNC-END-1",
    "K-FUNC-END-2",
    "K-FUNC-F1-1",
    "K-FUNC-F1-2",
    "K-FUNC-F2-1",
    "K-FUNC-F2-2",
    "K-FUNC-F3-1",
    "K-FUNC-F4-1",
    "K-FUNC-F4-2",
    "K-FUNC-F35-1",
    "K-FUNC-KP_BEGIN-1",
    "K-FUNC-KP_BEGIN-2",
    "K-FUNC-MEDIA_PLAY-1",
    "K-FUNC-LEFT_CONTROL-1",
    "K-MOD-000",
    "K-MOD-001",
    "K-MOD-002",
    "K-MOD-004",
    "K-MOD-008",
    "K-MOD-016",
    "K-MOD-032",
    "K-MOD-064",
    "K-MOD-128",
    "K-MOD-003",
    "K-MOD-005",
    "K-MOD-007",
    "K-MOD-063",
    "K-MOD-192",
    "K-MOD-255",
    "K-MODIFIER-LEFT-DOWN",
    "K-MODIFIER-LEFT-UP",
    "K-MODIFIER-LEFT-UP-RIGHT-HELD",
    "K-MODIFIER-CAPS-ON",
    "K-SYNTAX-MINIMAL",
    "K-SYNTAX-MOD-OMITTED",
    "K-SYNTAX-PRESS-EXPLICIT",
    "K-SYNTAX-EVENT-WITHOUT-MOD",
    "K-SYNTAX-ALT-SHIFT",
    "K-SYNTAX-ALT-BASE",
    "K-SYNTAX-ALT-BOTH",
    "K-SYNTAX-TEXT-EMPTY-MOD",
    "K-SYNTAX-TEXT-NONBMP",
    "K-SYNTAX-TEXT-COMBINING",
    "K-SYNTAX-TEXT-MULTI",
    "K-SYNTAX-KEY-NONBMP",
    "K-SYNTAX-KEY-SCALAR-MAX",
    "K-SYNTAX-CTRL-SHIFT-A",
    "K-SYNTAX-CTRL-SHIFT-TAB",
    "R-UNKNOWN-PUA",
    "R-LEADING-ZEROS",
    "R-DEFAULT-MOD-EVENT-EMPTY",
    "R-DEFAULT-EVENT-EMPTY",
];

const INVALID_EVENT_ROWS: &[(&str, Error)] = &[
    ("R-INVALID-NO-KEY", Error::MissingKey),
    ("R-INVALID-NEGATIVE", Error::Decimal),
    ("R-INVALID-SURROGATE", Error::Scalar),
    ("R-INVALID-TOO-HIGH", Error::Scalar),
    ("R-INVALID-MOD-ZERO", Error::ModifierRange),
    ("R-INVALID-MOD-UNKNOWN", Error::ModifierRange),
    ("R-INVALID-EVENT-0", Error::EventKind),
    ("R-INVALID-EVENT-4", Error::EventKind),
    ("R-INVALID-EXTRA-FIELD", Error::Fields),
    ("R-INVALID-EXTRA-ALT", Error::Fields),
    ("R-INVALID-SHIFTED-WITHOUT-SHIFT", Error::ShiftRequired),
    ("R-INVALID-TEXT-C0", Error::TextControl),
    ("R-INVALID-TEXT-DEL", Error::TextControl),
    ("R-INVALID-TEXT-C1", Error::TextControl),
    ("R-INVALID-TEXT-SURROGATE", Error::Scalar),
    ("R-INVALID-DECIMAL-OVERFLOW", Error::DecimalOverflow),
    ("R-INVALID-FRAME-OVERSIZE", Error::FrameLimit),
    ("R-INVALID-TEXT-LIMIT", Error::TextLimit),
    ("R-INVALID-EMPTY-TEXT-ELEMENT", Error::MissingKey),
    ("R-INVALID-BAD-ASCII", Error::Decimal),
    ("R-DEFAULT-TEXT-ONLY-MISSING-KEY", Error::MissingKey),
];

const CONTROL_IDS: &[&str] = &[
    "K-FLAG-00-QUERY",
    "K-FLAG-16-QUERY",
    "K-FLAG-23-QUERY",
    "K-FLAG-31-QUERY",
    "K-STATE-SET-DEFAULT",
    "K-STATE-SET-REPLACE",
    "K-STATE-SET-ADD",
    "K-STATE-SET-CLEAR",
    "K-STATE-PUSH-DEFAULT",
    "K-STATE-PUSH",
    "K-STATE-POP-DEFAULT",
    "K-STATE-POP-MANY",
    "K-STATE-POP-EMPTY",
    "K-STATE-POP-ALREADY-EMPTY",
    "K-STATE-STACK-EVICT",
    "K-STATE-ALT-SET",
    "R-NEG-UNKNOWN-FLAG",
    "R-NEG-POP-ZERO",
    "R-NEG-EMPTY-SET",
    "K-QUICK-MAIN",
    "K-STATE-ALT-POP",
];

#[test]
fn kitty_complete_event_frames_match_selected_corpus_semantics() {
    assert_eq!(EVENT_IDS.len(), 58);
    for id in EVENT_IDS {
        let row = corpus_row(id);
        let bytes = decode_hex(string_field(&row, "input_hex"));
        let event = parse_event_frame(&bytes).unwrap_or_else(|error| panic!("{id}: {error}"));
        assert_event(&event, &row["event"], &bytes, id);
    }
}

#[test]
fn kitty_invalid_complete_event_frames_match_selected_errors() {
    assert_eq!(INVALID_EVENT_ROWS.len(), 21);
    for (id, expected) in INVALID_EVENT_ROWS {
        let row = corpus_row(id);
        let bytes = decode_hex(string_field(&row, "input_hex"));
        assert_eq!(parse_event_frame(&bytes), Err(*expected), "{id}");
    }

    assert_eq!(parse_event_frame(b"\x1b[R"), Err(Error::InvalidFrame));
    assert_eq!(
        parse_event_frame(b"\x1b[57344u").unwrap().key,
        Key::UnassignedPua('\u{e000}')
    );
}

#[test]
fn kitty_canonical_encoder_matches_selected_corpus_outputs() {
    let mut exercised = 0;
    let mut decoded_semantics = 0;
    for row in corpus_rows() {
        if !selected_encoder_row(&row) {
            continue;
        }
        let id = string_field(&row, "id");
        let event = if string_field(&row, "direction") == "event-reencode" {
            let frame = decode_hex(string_field(&row, "input_hex"));
            let event = parse_event_frame(&frame).unwrap();
            assert_event(&event, &row["event"], &frame, id);
            decoded_semantics += 1;
            event
        } else {
            event_from_expectation(&row["event"])
        };
        let flags = row["initial"]["main"]
            .as_array()
            .and_then(|stack| stack.last())
            .and_then(Value::as_u64)
            .unwrap_or(0) as u8;
        let expected_hex = row["output_hex"]
            .as_str()
            .or_else(|| row["policy_output_hex"].as_str())
            .unwrap_or_else(|| panic!("{id}: no selected output"));
        assert_eq!(
            encode_event(&event, flags).unwrap(),
            decode_hex(expected_hex),
            "{id}"
        );
        exercised += 1;
    }
    assert_eq!(exercised, 89);
    assert_eq!(decoded_semantics, 81);
}

#[test]
fn kitty_review_r1_all_reencode_vectors_keep_the_pinned_profile() {
    let mut count = 0;
    let mut failures = Vec::new();
    for row in corpus_rows().filter(|row| row["direction"] == "event-reencode") {
        let id = string_field(&row, "id");
        let event = parse_event_frame(&decode_hex(string_field(&row, "input_hex"))).unwrap();
        let flags = row["initial"][string_field(&row["initial"], "screen")]
            .as_array()
            .unwrap()
            .last()
            .unwrap()
            .as_u64()
            .unwrap() as u8;
        let expected = row["output_hex"]
            .as_str()
            .or_else(|| row["policy_output_hex"].as_str())
            .unwrap();
        let actual = encode_event(&event, flags);
        if actual.as_ref().ok() != Some(&decode_hex(expected)) {
            failures.push(format!("{id}: {actual:?}, expected {expected}"));
        }
        count += 1;
    }
    assert_eq!(count, 358);
    assert!(failures.is_empty(), "{}", failures.join("\n"));
}

#[test]
fn kitty_review_r1_alternates_do_not_enable_disambiguation() {
    for flags in [0, 2, 4, 6, 16, 18, 20, 22] {
        for (frame, legacy) in [
            (b"\x1b[91;3u".as_slice(), b"\x1b[".as_slice()),
            (b"\x1b[97;4u", b"\x1bA"),
            (b"\x1b[97:65;4u", b"\x1bA"),
            (b"\x1b[97;7u", b"\x1b\x01"),
        ] {
            let mut event = parse_event_frame(frame).unwrap();
            for kind in [Kind::Press, Kind::Repeat, Kind::Release] {
                event.kind = kind;
                let expected = if kind == Kind::Release && flags & 2 == 0 {
                    Vec::new()
                } else if kind == Kind::Press || flags & 2 == 0 {
                    legacy.to_vec()
                } else {
                    let mut expected = frame[..frame.len() - 1].to_vec();
                    expected.extend_from_slice(format!(":{}u", kind as u8).as_bytes());
                    if flags & 4 == 0 {
                        expected = String::from_utf8(expected)
                            .unwrap()
                            .replace(":65", "")
                            .into_bytes();
                    }
                    expected
                };
                assert_eq!(
                    encode_event(&event, flags).unwrap(),
                    expected,
                    "flags={flags}, {event:?}"
                );
            }
        }
    }
}

#[test]
fn kitty_review_r1_alt_unicode_uses_supplied_shift_without_guessing_a_layout() {
    for flags in [0, 4, 16, 20] {
        let plain = parse_event_frame(b"\x1b[233;3u").unwrap();
        let shifted = parse_event_frame(b"\x1b[233:201;4u").unwrap();
        assert_eq!(encode_event(&plain, flags).unwrap(), "\x1bé".as_bytes());
        assert_eq!(encode_event(&shifted, flags).unwrap(), "\x1bÉ".as_bytes());
        let unavailable = parse_event_frame(b"\x1b[233;4u").unwrap();
        assert_eq!(
            encode_event(&unavailable, flags),
            Err(Error::UnavailableText)
        );
    }
    for flags in [1, 5, 8, 9] {
        let event = parse_event_frame(b"\x1b[91;3u").unwrap();
        assert_eq!(encode_event(&event, flags).unwrap(), b"\x1b[91;3u");
    }
}

#[test]
fn kitty_complete_controls_match_selected_stack_and_query_rows() {
    assert_eq!(CONTROL_IDS.len(), 21);
    for id in CONTROL_IDS {
        let row = corpus_row(id);
        let screen = screen_field(&row["initial"]);
        let mut state = state_field(&row["initial"]);
        let mut replies = Vec::new();
        let input = decode_hex(string_field(&row, "input_hex"));
        for frame in complete_control_frames(&input) {
            let control =
                parse_control_frame(frame).unwrap_or_else(|error| panic!("{id}: {error}"));
            replies.extend(state.apply(screen, control).bytes());
        }
        assert_eq!(state, state_field(&row["next"]), "{id}");
        assert_eq!(
            replies,
            decode_hex(string_field(&row, "output_hex")),
            "{id}"
        );
    }

    let row = corpus_row("R-NEG-UNKNOWN-MODE");
    let input = decode_hex(string_field(&row, "input_hex"));
    assert_eq!(parse_control_frame(&input), Err(Error::InvalidFrame));
}

#[test]
fn kitty_text_availability_identity_and_direct_validation_are_preserved() {
    let unavailable = parse_event_frame(b"\x1b[97u").unwrap();
    let known_empty = parse_event_frame(b"\x1b[97;1;u").unwrap();
    assert_eq!(unavailable.text, Text::Unavailable);
    assert_eq!(known_empty.text, Text::Known(Vec::new()));
    assert_ne!(unavailable, known_empty);

    let invalid_scalar_pua = Event {
        key: Key::Scalar('\u{e000}'),
        modifiers: 0,
        kind: Kind::Press,
        shifted: None,
        base: None,
        text: Text::Unavailable,
    };
    assert_eq!(invalid_scalar_pua.validate(), Err(Error::UnknownIdentity));

    let invalid_named_pua = Event {
        key: Key::UnassignedPua('a'),
        ..invalid_scalar_pua
    };
    assert_eq!(invalid_named_pua.validate(), Err(Error::UnknownIdentity));
}

#[test]
fn kitty_modifier_arithmetic_and_policy_bounds_are_exact() {
    for modifiers in u8::MIN..=u8::MAX {
        let frame = format!("\x1b[97;{}u", u16::from(modifiers) + 1);
        assert_eq!(
            parse_event_frame(frame.as_bytes()).unwrap().modifiers,
            modifiers
        );
    }

    for length in [FRAME_LIMIT - 1, FRAME_LIMIT] {
        let zeros = "0".repeat(length - 5);
        let frame = format!("\x1b[{zeros}97u");
        assert_eq!(frame.len(), length);
        assert_eq!(
            parse_event_frame(frame.as_bytes()).unwrap().key,
            Key::Scalar('a')
        );
    }
    let oversized = vec![b'0'; FRAME_LIMIT + 1];
    assert_eq!(parse_event_frame(&oversized), Err(Error::FrameLimit));

    let text_256 = repeated_text_frame(TEXT_LIMIT);
    assert_eq!(
        parse_event_frame(text_256.as_bytes())
            .unwrap()
            .text
            .values()
            .len(),
        TEXT_LIMIT
    );
    let text_257 = repeated_text_frame(TEXT_LIMIT + 1);
    assert_eq!(
        parse_event_frame(text_257.as_bytes()),
        Err(Error::TextLimit)
    );

    assert!(PaneKeyboard::from_stacks(vec![0; STACK_LIMIT], vec![]).is_ok());
    assert_eq!(
        PaneKeyboard::from_stacks(vec![0; STACK_LIMIT + 1], vec![]),
        Err(Error::StackLimit)
    );
}

#[test]
fn kitty_requested_effective_reset_and_unsupported_controls_are_explicit() {
    let mut state = PaneKeyboard::from_stacks(vec![16], vec![31]).unwrap();
    assert_eq!(state.requested(Screen::Main), 16);
    assert_eq!(state.effective(Screen::Main), 0);
    assert_eq!(state.requested(Screen::Alternate), 31);
    assert_eq!(state.effective(Screen::Alternate), 31);
    state.decstr();
    assert_eq!(state.stack(Screen::Main), &[16]);
    state.ris();
    assert_eq!(state, PaneKeyboard::default());

    assert_eq!(parse_control_frame(b"\x1b[?1u"), Err(Error::Fields));
    assert_eq!(parse_control_frame(b"\x1b[>1;2u"), Err(Error::Fields));
    assert_eq!(parse_control_frame(b"\x1b[=1;4u"), Err(Error::InvalidFrame));
    assert_eq!(
        parse_control_frame(b"\x1b[=4294967296u"),
        Err(Error::DecimalOverflow)
    );
}

fn corpus_rows() -> impl Iterator<Item = Value> {
    VECTORS
        .lines()
        .map(|line| serde_json::from_str(line).unwrap())
}

fn corpus_row(id: &str) -> Value {
    corpus_rows()
        .find(|row| row["id"].as_str() == Some(id))
        .unwrap_or_else(|| panic!("missing corpus row {id}"))
}

fn selected_encoder_row(row: &Value) -> bool {
    let id = row["id"].as_str().unwrap_or("");
    let selected_flag = ["00", "04", "08", "16", "23"]
        .iter()
        .any(|flags| id.starts_with(&format!("K-FLAG-{flags}-TEXT-")))
        || ["02", "31"]
            .iter()
            .any(|flags| id.starts_with(&format!("K-FLAG-{flags}-")) && !id.ends_with("-QUERY"));
    let selected_c0 = ["F02", "F03", "F08", "F10"]
        .iter()
        .any(|flags| id.starts_with(&format!("K-C0-{flags}-")));
    let selected_tab = [
        "K-C0-TAB-F02-M5-",
        "K-C0-TAB-F03-M5-",
        "K-C0-TAB-F08-M5-",
        "K-C0-TAB-F10-M5-",
        "K-C0-TAB-F01-M1-",
        "K-C0-TAB-F03-M1-",
    ]
    .iter()
    .any(|prefix| id.starts_with(prefix));
    let selected_legacy = ["F1", "F2", "F3", "F4"]
        .iter()
        .any(|function| id.starts_with(&format!("K-LEGACY-FUNC-{function}-")));
    selected_flag || selected_c0 || selected_tab || selected_legacy
}

fn assert_event(actual: &Event, expected: &Value, frame: &[u8], id: &str) {
    match &expected["key"] {
        Value::String(name) => assert_eq!(
            actual.key,
            Key::Function(Function::named(name).unwrap()),
            "{id}"
        ),
        Value::Number(number) => assert_eq!(
            actual.key,
            expected_numeric_key(number.as_u64().unwrap() as u32),
            "{id}"
        ),
        other => panic!("{id}: unsupported expected key {other}"),
    }
    assert_eq!(
        actual.modifiers,
        expected["modifiers"].as_u64().unwrap() as u8,
        "{id}"
    );
    assert_eq!(
        actual.kind as u8,
        expected["event_type"].as_u64().unwrap() as u8,
        "{id}"
    );
    assert_eq!(
        actual.shifted,
        optional_scalar(&expected["shifted"]),
        "{id}"
    );
    assert_eq!(actual.base, optional_scalar(&expected["base"]), "{id}");
    let text: Vec<_> = expected["text"]
        .as_array()
        .unwrap()
        .iter()
        .map(|value| char::from_u32(value.as_u64().unwrap() as u32).unwrap())
        .collect();
    assert_eq!(actual.text.values(), text, "{id}");
    let text_field_present = frame.last() == Some(&b'u')
        && frame[2..frame.len() - 1]
            .iter()
            .filter(|byte| **byte == b';')
            .count()
            >= 2;
    assert_eq!(actual.text.is_available(), text_field_present, "{id}");
}

fn event_from_expectation(expected: &Value) -> Event {
    let key = match &expected["key"] {
        Value::String(name) => Key::Function(Function::named(name).unwrap()),
        Value::Number(number) => expected_numeric_key(number.as_u64().unwrap() as u32),
        other => panic!("unsupported event key {other}"),
    };
    Event {
        key,
        modifiers: expected["modifiers"].as_u64().unwrap() as u8,
        kind: Kind::try_from(expected["event_type"].as_u64().unwrap() as u32).unwrap(),
        shifted: optional_scalar(&expected["shifted"]),
        base: optional_scalar(&expected["base"]),
        text: Text::Unavailable,
    }
}

fn expected_numeric_key(number: u32) -> Key {
    let scalar = char::from_u32(number).unwrap();
    match number {
        0 => Key::TextOnly,
        9 => Key::Function(Function::named("TAB").unwrap()),
        13 => Key::Function(Function::named("ENTER").unwrap()),
        27 => Key::Function(Function::named("ESCAPE").unwrap()),
        127 => Key::Function(Function::named("BACKSPACE").unwrap()),
        57358 => Key::Function(Function::named("CAPS_LOCK").unwrap()),
        57442 => Key::Function(Function::named("LEFT_CONTROL").unwrap()),
        0xe000..=0xf8ff => Key::UnassignedPua(scalar),
        _ => Key::Scalar(scalar),
    }
}

fn state_field(value: &Value) -> PaneKeyboard {
    PaneKeyboard::from_stacks(
        stack_field(&value["main"]),
        stack_field(&value["alternate"]),
    )
    .unwrap()
}

fn stack_field(value: &Value) -> Vec<u8> {
    value
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry.as_u64().unwrap() as u8)
        .collect()
}

fn screen_field(value: &Value) -> Screen {
    match string_field(value, "screen") {
        "main" => Screen::Main,
        "alternate" => Screen::Alternate,
        screen => panic!("unsupported screen {screen}"),
    }
}

fn complete_control_frames(bytes: &[u8]) -> impl Iterator<Item = &[u8]> {
    bytes.split_inclusive(|byte| *byte == b'u')
}

fn repeated_text_frame(count: usize) -> String {
    format!(
        "\x1b[0;1;{}u",
        std::iter::repeat_n("97", count)
            .collect::<Vec<_>>()
            .join(":")
    )
}

fn optional_scalar(value: &Value) -> Option<char> {
    value
        .as_u64()
        .map(|number| char::from_u32(number as u32).unwrap())
}

fn string_field<'a>(value: &'a Value, name: &str) -> &'a str {
    value[name].as_str().unwrap_or("")
}

fn decode_hex(hex: &str) -> Vec<u8> {
    assert_eq!(hex.len() % 2, 0);
    hex.as_bytes()
        .chunks_exact(2)
        .map(|pair| {
            let pair = std::str::from_utf8(pair).unwrap();
            u8::from_str_radix(pair, 16).unwrap()
        })
        .collect()
}
