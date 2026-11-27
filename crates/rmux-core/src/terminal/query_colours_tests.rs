use super::*;

#[test]
fn control_query_colours_override_only_fg_bg_preserving_application_state_and_recovery() {
    let mut terminal = TerminalParser::new(TerminalSize { cols: 80, rows: 24 }, 100);
    let app = b"\x1b]10;red\x07\x1b]11;blue\x07\x1b]12;green\x07";
    terminal.feed(app);
    let recovery = terminal.recovery_parser_state_ansi();
    let query = b"\x1b]10;?\x07\x1b]11;?\x1b\\\x1b]12;?\x07";
    terminal.feed_with_query_colours(query, [Some([0x12, 0x34, 0xab]), Some([0xfe, 0xdc, 0xba])]);
    assert_eq!(
        terminal.take_replies(),
        b"\x1b]10;rgb:1212/3434/abab\x07\x1b]11;rgb:fefe/dcdc/baba\x1b\\\x1b]12;green\x07"
    );
    assert_eq!(terminal.recovery_parser_state_ansi(), recovery);
    terminal.feed(query);
    assert_eq!(
        terminal.take_replies(),
        b"\x1b]10;red\x07\x1b]11;blue\x1b\\\x1b]12;green\x07"
    );
    terminal.feed_with_query_colours(
        b"\x1b]110;\x07\x1b]111;\x07\x1b]112;\x07",
        [Some([1, 2, 3]); 2],
    );
    terminal.feed(query);
    assert!(terminal.take_replies().is_empty());
    terminal.feed_with_query_colours(query, [Some([1, 2, 3]); 2]);
    assert_eq!(
        terminal.take_replies(),
        b"\x1b]10;rgb:0101/0202/0303\x07\x1b]11;rgb:0101/0202/0303\x1b\\"
    );
}

#[test]
fn control_query_colours_use_completion_context_at_every_fragment_boundary() {
    let query = b"\x1b]11;?\x1b\\";
    // OSC dispatch occurs when ESC leaves the OSC state, before the final slash.
    for split in 0..query.len() - 1 {
        let mut terminal = TerminalParser::new(TerminalSize { cols: 80, rows: 24 }, 100);
        terminal.feed_with_query_colours(&query[..split], [Some([1, 2, 3]); 2]);
        assert!(terminal.take_replies().is_empty());
        terminal.feed_with_query_colours(&query[split..], [None, Some([0x45, 0x67, 0x89])]);
        assert_eq!(terminal.take_replies(), b"\x1b]11;rgb:4545/6767/8989\x1b\\");
        terminal.feed(b"\x1b]11;?\x07");
        assert!(
            terminal.take_replies().is_empty(),
            "the override cannot escape its feed"
        );
    }
}

#[test]
fn application_extended_key_requests_respect_off_on_and_always_policy() {
    use crate::input::{mode, ExtendedKeyPolicy};
    let mut terminal = TerminalParser::new(TerminalSize { cols: 80, rows: 24 }, 100);
    for policy in [
        ExtendedKeyPolicy::Off,
        ExtendedKeyPolicy::On,
        ExtendedKeyPolicy::Always,
    ] {
        terminal.screen_mut().set_extended_key_policy(policy);
        terminal.feed(b"\x1b[>4n\x1b[>4;2m");
        assert_eq!(
            terminal.screen().mode() & mode::EXTENDED_KEY_MODES,
            if policy == ExtendedKeyPolicy::Off {
                0
            } else {
                mode::MODE_KEYS_EXTENDED_2
            }
        );
        terminal.feed(b"\x1b[>4n");
        assert_eq!(
            terminal.screen().mode() & mode::EXTENDED_KEY_MODES,
            if policy == ExtendedKeyPolicy::Always {
                mode::MODE_KEYS_EXTENDED
            } else {
                0
            }
        );
        terminal.reset_parser();
        terminal.feed(b"\x1b[>4;0m");
        assert_eq!(
            terminal.screen().mode() & mode::EXTENDED_KEY_MODES,
            if policy == ExtendedKeyPolicy::Always {
                mode::MODE_KEYS_EXTENDED
            } else {
                0
            }
        );
    }
}
