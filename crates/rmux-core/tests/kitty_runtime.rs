use rmux_core::TerminalScreen;
use rmux_proto::TerminalSize;

#[test]
fn kitty_runtime_negotiates_fragmented_application_controls() {
    let mut terminal = TerminalScreen::new(TerminalSize { cols: 20, rows: 4 }, 10);
    for byte in b"\x1b[?u\x1b[=8u\x1b[>3u\x1b[?u\x1b[<u\x1b[?u\x1b[<99u\x1b[?u" {
        terminal.feed(&[*byte]);
    }
    assert_eq!(terminal.take_replies(), b"\x1b[?0u\x1b[?3u\x1b[?8u\x1b[?0u");
}

#[test]
fn kitty_runtime_screen_stacks_reset_and_xterm_are_independent() {
    let mut terminal = TerminalScreen::new(TerminalSize { cols: 20, rows: 4 }, 10);
    terminal.feed(b"\x1b[>4;2m\x1b[=8u\x1b[?1049h\x1b[?u\x1b[=31u\x1b[?1049l\x1b[?u\x1b[!p\x1b[?u\x1b[?1049h\x1b[?u\x1bc\x1b[?u\x1b[?1049h\x1b[?u");
    assert_eq!(
        terminal.take_replies(),
        b"\x1b[?0u\x1b[?8u\x1b[?8u\x1b[?31u\x1b[?0u\x1b[?0u"
    );
}

#[test]
fn kitty_runtime_replays_all_pinned_application_negotiation_rows() {
    use serde_json::Value;
    let mut count = 0;
    for line in include_str!("../../../tests/conformance/kitty_keyboard/vectors.jsonl").lines() {
        let row: Value = serde_json::from_str(line).unwrap();
        if row["direction"] != "application-negotiation" {
            continue;
        }
        count += 1;
        let mut terminal = TerminalScreen::new(TerminalSize { cols: 20, rows: 4 }, 10);
        for (screen, sequence) in [
            ("main", b"\x1b[?1049l".as_slice()),
            ("alternate", b"\x1b[?1049h".as_slice()),
        ] {
            terminal.feed(sequence);
            terminal.feed(b"\x1b[<99u");
            for flag in row["initial"][screen].as_array().unwrap() {
                terminal.feed(format!("\x1b[>{flag}u").as_bytes());
            }
        }
        if row["initial"]["screen"] == "main" {
            terminal.feed(b"\x1b[?1049l");
        }
        for byte in hex(row["input_hex"].as_str().unwrap()) {
            terminal.feed(&[byte]);
        }
        assert_eq!(
            terminal.take_replies(),
            hex(row["output_hex"].as_str().unwrap()),
            "{}",
            row["id"]
        );
        assert_eq!(
            terminal.screen().is_alternate(),
            row["next"]["screen"] == "alternate",
            "{}",
            row["id"]
        );
        for (screen, sequence) in [
            ("main", b"\x1b[?1049l".as_slice()),
            ("alternate", b"\x1b[?1049h".as_slice()),
        ] {
            terminal.feed(sequence);
            for flag in row["next"][screen].as_array().unwrap().iter().rev() {
                terminal.feed(b"\x1b[?u\x1b[<u");
                assert_eq!(
                    terminal.take_replies(),
                    format!("\x1b[?{flag}u").into_bytes(),
                    "{} {screen}",
                    row["id"]
                );
            }
            terminal.feed(b"\x1b[?u");
            assert_eq!(
                terminal.take_replies(),
                b"\x1b[?0u",
                "{} {screen}",
                row["id"]
            );
        }
    }
    assert_eq!(count, 148);
}

fn hex(text: &str) -> Vec<u8> {
    text.as_bytes()
        .chunks_exact(2)
        .map(|pair| u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap())
        .collect()
}

#[test]
fn kitty_runtime_rejects_invalid_controls_without_losing_neighbor_output() {
    let mut terminal = TerminalScreen::new(TerminalSize { cols: 20, rows: 4 }, 10);
    terminal.feed(b"\x1b[=8u\x1b[=4294967295u\x1b[?u\x1b[=4294967296u\x1b[=3;4u\x1b[>2;3u\x1b[=1:2u\x1b[?99u\x1b[?u");
    assert_eq!(terminal.take_replies(), b"\x1b[?31u\x1b[?31u");
    terminal.feed(b"\x1b[>4;2m\x1b[<99u\x1b[?u\x1b[c");
    assert_eq!(terminal.take_replies(), b"\x1b[?0u\x1b[?1;2c");
    assert_ne!(
        terminal.screen().mode() & rmux_core::input::mode::MODE_KEYS_EXTENDED_2,
        0
    );
}

#[test]
fn kitty_runtime_control_frame_limit_is_atomic_and_separate_from_legacy_csi() {
    let mut terminal = TerminalScreen::new(TerminalSize { cols: 20, rows: 4 }, 10);
    let valid = format!("\x1b[={}8u", "0".repeat(4091));
    assert_eq!(valid.len(), rmux_core::kitty_keyboard::FRAME_LIMIT);
    for byte in valid.bytes() {
        terminal.feed(&[byte]);
    }
    terminal.feed(b"\x1b[?u");
    assert_eq!(terminal.take_replies(), b"\x1b[?8u");
    let invalid = format!("\x1b[={}3u", "0".repeat(4092));
    for byte in invalid.bytes() {
        terminal.feed(&[byte]);
    }
    terminal.feed(b"\x1b[?u");
    assert_eq!(terminal.take_replies(), b"\x1b[?8u");
    terminal.feed(format!("\x1b[>{}4;2m", "0".repeat(70)).as_bytes());
    assert_eq!(
        terminal.screen().mode() & rmux_core::input::mode::MODE_KEYS_EXTENDED_2,
        0
    );
}
