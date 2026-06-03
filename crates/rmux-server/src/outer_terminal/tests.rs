use super::{CursorScope, OuterTerminal, OuterTerminalContext};
use crate::pane_screen_state::PaneScreenState;
use rmux_core::{OptionStore, Session};
use rmux_proto::{
    ClientTerminalContext, OptionName, ScopeSelector, SessionName, SetOptionMode, TerminalSize,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn make_session() -> Session {
    Session::new(session_name("alpha"), TerminalSize { cols: 80, rows: 24 })
}

const MOUSE_ENABLE_SEQUENCE: &str = "\u{1b}[?1006h\u{1b}[?1000h\u{1b}[?1002h";
const MOUSE_DISABLE_SEQUENCE: &str = "\u{1b}[?1002l\u{1b}[?1000l\u{1b}[?1006l";

#[test]
fn terminal_features_match_globs_and_case_insensitive_feature_names() {
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::TerminalFeatures,
            "xterm-kitty*:ClIpBoArD:EXTKEYS".to_owned(),
            SetOptionMode::Append,
        )
        .expect("terminal-features append succeeds");

    let terminal = OuterTerminal::resolve(
        &options,
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-kitty")]),
    );

    assert!(terminal.features_string().contains("clipboard"));
    assert!(terminal.features_string().contains("extkeys"));
}

#[test]
fn xterm_kitty_enables_kitty_graphics_feature() {
    let terminal = OuterTerminal::resolve(
        &OptionStore::default(),
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-kitty")]),
    );

    assert!(terminal.supports_kitty_graphics());
    assert!(terminal.features_string().contains("kitty-graphics"));
}

#[test]
fn modern_kitty_graphics_terminals_enable_kitty_graphics_feature() {
    for context in [
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-ghostty")]),
        OuterTerminalContext::from_pairs(&[("TERM", "wezterm")]),
        OuterTerminalContext::from_pairs(&[
            ("TERM", "xterm-256color"),
            ("TERM_PROGRAM", "ghostty"),
        ]),
        OuterTerminalContext::from_pairs(&[
            ("TERM", "xterm-256color"),
            ("TERM_PROGRAM", "WezTerm"),
        ]),
    ] {
        let terminal = OuterTerminal::resolve(&OptionStore::default(), context);
        assert!(terminal.supports_kitty_graphics());
        assert!(terminal.features_string().contains("kitty-graphics"));
    }
}

#[test]
fn known_sixel_terminals_enable_sixel_feature() {
    for context in [
        OuterTerminalContext::from_pairs(&[("TERM", "mintty")]),
        OuterTerminalContext::from_pairs(&[("TERM", "foot")]),
        OuterTerminalContext::from_pairs(&[("TERM", "mlterm")]),
        OuterTerminalContext::from_pairs(&[
            ("TERM", "xterm-256color"),
            ("TERM_PROGRAM", "WezTerm"),
        ]),
    ] {
        let terminal = OuterTerminal::resolve(&OptionStore::default(), context);
        assert!(terminal.supports_sixel());
        assert!(terminal.features_string().contains("sixel"));
    }
}

#[test]
fn terminal_features_can_enable_sixel_for_other_terms() {
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::TerminalFeatures,
            "xterm*:sixel".to_owned(),
            SetOptionMode::Append,
        )
        .expect("terminal-features append succeeds");

    let terminal = OuterTerminal::resolve(
        &options,
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
    );

    assert!(terminal.supports_sixel());
}

#[test]
fn terminal_overrides_apply_legacy_tc_xt_and_ax_flags() {
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::TerminalOverrides,
            "linux*:Tc:XT:AX@".to_owned(),
            SetOptionMode::Append,
        )
        .expect("terminal-overrides append succeeds");

    let terminal = OuterTerminal::resolve(
        &options,
        OuterTerminalContext::from_pairs(&[("TERM", "linux")]),
    );

    let features = terminal.features_string();
    assert!(features.contains("RGB"));
    assert!(features.contains("bpaste"));
    assert!(features.contains("focus"));
    assert!(features.contains("title"));
}

#[test]
fn attach_sequences_follow_focus_and_extended_key_options() {
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::FocusEvents,
            "on".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("focus-events set succeeds");
    options
        .set(
            ScopeSelector::Global,
            OptionName::ExtendedKeys,
            "always".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("extended-keys set succeeds");
    options
        .set(
            ScopeSelector::Global,
            OptionName::Mouse,
            "on".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("mouse set succeeds");

    let terminal = OuterTerminal::resolve_for_session(
        &options,
        Some(&session_name("alpha")),
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color"), ("COLORTERM", "truecolor")]),
    );

    let start = String::from_utf8(terminal.attach_start_sequence()).expect("utf8");
    let stop = String::from_utf8(terminal.attach_stop_sequence()).expect("utf8");

    assert!(start.starts_with("\u{1b}[?1049h\u{1b}[22;0;0t\u{1b}[0m\u{1b}[?25l\u{1b}[H\u{1b}[2J"));
    assert!(start.contains("\u{1b}[22;0;0t"));
    assert!(start.contains("\u{1b}[?2004h"));
    assert!(start.contains("\u{1b}[?1006h"));
    assert!(start.contains("\u{1b}[?1002h"));
    assert!(start.contains("\u{1b}[?1000h"));
    assert!(start.contains(MOUSE_ENABLE_SEQUENCE));
    assert!(start.contains("\u{1b}[?1004h"));
    assert!(start.contains("\u{1b}[>4;2m"));
    assert!(stop.contains("\u{1b}[?2004l"));
    assert!(stop.contains("\u{1b}[?1000l"));
    assert!(stop.contains("\u{1b}[?1002l"));
    assert!(stop.contains("\u{1b}[?1006l"));
    assert!(stop.contains(MOUSE_DISABLE_SEQUENCE));
    assert!(stop.contains("\u{1b}[?1004l"));
    assert!(stop.contains("\u{1b}[>4m"));
    assert!(stop.ends_with("\u{1b}[?1049l\u{1b}[23;0;0t"));
}

#[test]
fn client_mouse_feature_enables_mouse_attach_sequences_when_mouse_option_is_on() {
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::Mouse,
            "on".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("mouse set succeeds");

    let terminal = OuterTerminal::resolve_for_session(
        &options,
        Some(&session_name("alpha")),
        OuterTerminalContext::default().with_client_terminal(&ClientTerminalContext {
            terminal_features: vec!["mouse".to_owned()],
            utf8: true,
        }),
    );

    let start = String::from_utf8(terminal.attach_start_sequence()).expect("utf8");
    let stop = String::from_utf8(terminal.attach_stop_sequence()).expect("utf8");

    assert!(start.contains("\u{1b}[?1006h"));
    assert!(start.contains("\u{1b}[?1002h"));
    assert!(start.contains("\u{1b}[?1000h"));
    assert!(start.contains(MOUSE_ENABLE_SEQUENCE));
    assert!(stop.contains("\u{1b}[?1000l"));
    assert!(stop.contains("\u{1b}[?1002l"));
    assert!(stop.contains("\u{1b}[?1006l"));
    assert!(stop.contains(MOUSE_DISABLE_SEQUENCE));
}

#[test]
fn render_prelude_emits_title_path_and_cursor_colour() {
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::TerminalFeatures,
            "tmux*:osc7".to_owned(),
            SetOptionMode::Append,
        )
        .expect("terminal-features append succeeds");
    options
        .set(
            ScopeSelector::Window(rmux_proto::WindowTarget::with_window(
                session_name("alpha"),
                0,
            )),
            OptionName::CursorColour,
            "red".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("cursor colour set succeeds");

    let terminal = OuterTerminal::resolve(
        &options,
        OuterTerminalContext::from_pairs(&[("TERM", "tmux-256color")]),
    );
    let pane_state = PaneScreenState {
        mode: 0,
        alternate_on: false,
        title: "build logs".to_owned(),
        path: "file:///tmp/project".to_owned(),
        cursor_style: 6,
    };
    let prelude = String::from_utf8(terminal.render_prelude(
        &make_session(),
        &options,
        Some(&pane_state),
        CursorScope::Pane,
    ))
    .expect("utf8");

    assert!(prelude.contains("\u{1b}]0;build logs\u{7}"));
    assert!(prelude.contains("\u{1b}]7;file:///tmp/project\u{7}"));
    assert!(prelude.contains("\u{1b}]12;rgb:cd/00/00\u{7}"));
}

#[test]
fn cursor_style_transition_preserves_terminal_default_on_initial_default_attach() {
    let terminal = OuterTerminal::resolve(
        &OptionStore::new(),
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
    );

    assert_eq!(terminal.render_cursor_style_transition(None, 0), None);
}

#[test]
fn cursor_style_transition_resets_only_when_leaving_an_explicit_style() {
    let terminal = OuterTerminal::resolve(
        &OptionStore::new(),
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
    );

    assert_eq!(
        terminal.render_cursor_style_transition(Some(6), 0),
        Some("\u{1b}[2 q".to_owned())
    );
    assert_eq!(
        terminal.render_cursor_style_transition(Some(0), 6),
        Some("\u{1b}[6 q".to_owned())
    );
    assert_eq!(terminal.render_cursor_style_transition(Some(6), 6), None);
}

#[test]
fn clipboard_encoding_honours_feature_and_set_clipboard_option() {
    let mut enabled_options = OptionStore::new();
    enabled_options
        .set(
            ScopeSelector::Global,
            OptionName::SetClipboard,
            "external".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("set-clipboard set succeeds");
    let enabled = OuterTerminal::resolve(
        &enabled_options,
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
    );
    let encoded = String::from_utf8(
        enabled
            .encode_clipboard_set(b"hi")
            .expect("clipboard write is available"),
    )
    .expect("utf8");
    assert_eq!(encoded, "\u{1b}]52;;aGk=\u{7}");

    let mut disabled_options = OptionStore::new();
    disabled_options
        .set(
            ScopeSelector::Global,
            OptionName::SetClipboard,
            "off".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("set-clipboard set succeeds");
    let disabled = OuterTerminal::resolve(
        &disabled_options,
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
    );
    assert!(disabled.encode_clipboard_set(b"hi").is_none());
}

#[test]
fn sync_wrapper_brackets_render_frames_when_supported() {
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::TerminalFeatures,
            "xterm*:sync".to_owned(),
            SetOptionMode::Append,
        )
        .expect("terminal-features append succeeds");
    let terminal = OuterTerminal::resolve(
        &options,
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
    );

    let wrapped = String::from_utf8(terminal.wrap_render_frame(b"frame")).expect("utf8");
    assert_eq!(wrapped, "\u{1b}[?2026hframe\u{1b}[?2026l");
}

#[test]
fn decode_capability_string_handles_octal_escapes() {
    assert_eq!(
        super::decode_capability_string("\\033[H"),
        "\x1b[H",
        "\\033 should decode to ESC"
    );
    assert_eq!(
        super::decode_capability_string("\\007"),
        "\x07",
        "\\007 should decode to BEL"
    );
    assert_eq!(
        super::decode_capability_string("\\0"),
        "\x00",
        "\\0 alone should decode to NUL"
    );
}

#[test]
fn decode_capability_string_handles_vis_escapes() {
    assert_eq!(
        super::decode_capability_string("\\s"),
        " ",
        "\\s should decode to space"
    );
    assert_eq!(
        super::decode_capability_string("\\v"),
        "\x0b",
        "\\v should decode to vertical tab"
    );
    assert_eq!(
        super::decode_capability_string("\\^C"),
        "\x03",
        "\\^C should decode to ctrl-C"
    );
    assert_eq!(
        super::decode_capability_string("\\^?"),
        "\x7f",
        "\\^? should decode to DEL"
    );
}

#[test]
fn decode_capability_string_preserves_existing_escapes() {
    assert_eq!(super::decode_capability_string("\\E[H"), "\x1b[H");
    assert_eq!(super::decode_capability_string("\\e[H"), "\x1b[H");
    assert_eq!(super::decode_capability_string("\\n"), "\n");
    assert_eq!(super::decode_capability_string("\\\\"), "\\");
    assert_eq!(super::decode_capability_string("\\:"), ":");
    assert_eq!(super::decode_capability_string("\\"), "\\");
}

#[test]
fn decode_capability_string_with_mixed_octal_and_text() {
    assert_eq!(
        super::decode_capability_string("\\033[?2026%p1%dq"),
        "\x1b[?2026%p1%dq"
    );
}

#[test]
fn override_with_octal_encoded_value_resolves_correctly() {
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::TerminalOverrides,
            "dumb*:Ss=\\033[%p1%d q".to_owned(),
            SetOptionMode::Append,
        )
        .expect("terminal-overrides append succeeds");

    let terminal = OuterTerminal::resolve(
        &options,
        OuterTerminalContext::from_pairs(&[("TERM", "dumb")]),
    );

    let style = terminal
        .render_cursor_style(2)
        .expect("cursor style should be available");
    assert_eq!(style, "\x1b[2 q");
}

#[test]
fn split_override_segments_handles_escaped_colons_and_empty_segments() {
    let segments = super::split_override_segments("a::b:c");
    assert_eq!(segments, vec!["a:b", "c"]);

    let segments = super::split_override_segments("pattern:");
    assert_eq!(segments, vec!["pattern", ""]);

    let segments = super::split_override_segments("");
    assert_eq!(segments, vec![""]);
}

#[test]
fn empty_term_skips_feature_and_override_matching() {
    let options = OptionStore::new();
    let terminal = OuterTerminal::resolve(&options, OuterTerminalContext::default());
    assert_eq!(terminal.features_string(), "");
}

#[test]
fn sync_wrapper_passes_through_empty_frames() {
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::TerminalFeatures,
            "xterm*:sync".to_owned(),
            SetOptionMode::Append,
        )
        .expect("terminal-features append succeeds");
    let terminal = OuterTerminal::resolve(
        &options,
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
    );
    let wrapped = terminal.wrap_render_frame(b"");
    assert!(wrapped.is_empty());
}

#[test]
fn sanitize_osc_payload_strips_bel_and_esc() {
    let sanitized = super::sanitize_osc_payload("hello\x07world\x1b[0m");
    assert!(!sanitized.contains('\x07'));
    assert!(!sanitized.contains('\x1b'));
    assert_eq!(sanitized, "hello world [0m");
}

#[test]
fn base64_encoding_edge_cases() {
    assert_eq!(super::encode_base64(b""), "");
    assert_eq!(super::encode_base64(b"f"), "Zg==");
    assert_eq!(super::encode_base64(b"fo"), "Zm8=");
    assert_eq!(super::encode_base64(b"foo"), "Zm9v");
    assert_eq!(super::encode_base64(b"foob"), "Zm9vYg==");
    assert_eq!(super::encode_base64(b"fooba"), "Zm9vYmE=");
    assert_eq!(super::encode_base64(b"foobar"), "Zm9vYmFy");
}

#[test]
fn clipboard_encoding_rejects_empty_bytes() {
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::SetClipboard,
            "on".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("set-clipboard set succeeds");
    let terminal = OuterTerminal::resolve(
        &options,
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
    );
    assert!(terminal.encode_clipboard_set(b"").is_none());
}

#[test]
fn colour_to_rgb_none_default_terminal_return_none() {
    assert!(super::colour_to_rgb(super::COLOUR_NONE).is_none());
    assert!(super::colour_to_rgb(super::COLOUR_DEFAULT).is_none());
    assert!(super::colour_to_rgb(super::COLOUR_TERMINAL).is_none());
}

#[test]
fn colour_to_rgb_256_palette_boundaries() {
    // Index 0 = basic black
    assert_eq!(
        super::colour_to_rgb(super::COLOUR_FLAG_256),
        Some((0, 0, 0))
    );
    // Index 15 = basic bright white
    assert_eq!(
        super::colour_to_rgb(super::COLOUR_FLAG_256 | 15),
        Some((255, 255, 255))
    );
    // Index 16 = first cube colour (black)
    assert_eq!(
        super::colour_to_rgb(super::COLOUR_FLAG_256 | 16),
        Some((0, 0, 0))
    );
    // Index 231 = last cube colour (white)
    assert_eq!(
        super::colour_to_rgb(super::COLOUR_FLAG_256 | 231),
        Some((255, 255, 255))
    );
    // Index 232 = first greyscale
    assert_eq!(
        super::colour_to_rgb(super::COLOUR_FLAG_256 | 232),
        Some((8, 8, 8))
    );
    // Index 255 = last greyscale
    assert_eq!(
        super::colour_to_rgb(super::COLOUR_FLAG_256 | 255),
        Some((238, 238, 238))
    );
}

#[test]
fn colour_to_rgb_bright_ansi_colours() {
    // SGR 90 = bright black
    assert_eq!(super::colour_to_rgb(90), Some((127, 127, 127)));
    // SGR 97 = bright white
    assert_eq!(super::colour_to_rgb(97), Some((255, 255, 255)));
}

#[test]
fn transition_sequence_emits_disable_then_enable_on_change() {
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::FocusEvents,
            "on".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("focus-events set succeeds");

    let with_focus = OuterTerminal::resolve_for_session(
        &options,
        Some(&session_name("alpha")),
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
    );
    let without_focus = OuterTerminal::resolve_for_session(
        &OptionStore::new(),
        Some(&session_name("alpha")),
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
    );

    // Transition from focus-enabled to focus-disabled should emit disable.
    let seq = String::from_utf8(without_focus.transition_sequence_from(&with_focus)).expect("utf8");
    assert!(seq.contains("\u{1b}[?1004l"));

    // Transition from focus-disabled to focus-enabled should emit enable.
    let seq = String::from_utf8(with_focus.transition_sequence_from(&without_focus)).expect("utf8");
    assert!(seq.contains("\u{1b}[?1004h"));
}

#[test]
fn transition_sequence_toggles_mouse_reporting_with_session_scope() {
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::Mouse,
            "on".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("mouse set succeeds");

    let enabled = OuterTerminal::resolve_for_session(
        &options,
        Some(&session_name("alpha")),
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
    );
    let disabled = OuterTerminal::resolve_for_session(
        &OptionStore::new(),
        Some(&session_name("alpha")),
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
    );

    let seq = String::from_utf8(disabled.transition_sequence_from(&enabled)).expect("utf8");
    assert!(seq.contains("\u{1b}[?1000l"));
    assert!(seq.contains("\u{1b}[?1002l"));
    assert!(seq.contains("\u{1b}[?1006l"));
    assert!(seq.contains(MOUSE_DISABLE_SEQUENCE));

    let seq = String::from_utf8(enabled.transition_sequence_from(&disabled)).expect("utf8");
    assert!(seq.contains("\u{1b}[?1006h"));
    assert!(seq.contains("\u{1b}[?1002h"));
    assert!(seq.contains("\u{1b}[?1000h"));
    assert!(seq.contains(MOUSE_ENABLE_SEQUENCE));
}

#[test]
fn parse_capability_override_edge_cases() {
    // Bare name (no = or @)
    let (name, value, remove) = super::parse_capability_override("Tc").unwrap();
    assert_eq!(name, "Tc");
    assert!(value.is_none());
    assert!(!remove);

    // Remove with @
    let (name, value, remove) = super::parse_capability_override("AX@").unwrap();
    assert_eq!(name, "AX");
    assert!(value.is_none());
    assert!(remove);

    // Value with =
    let (name, value, remove) = super::parse_capability_override("Ss=\\E[q").unwrap();
    assert_eq!(name, "Ss");
    assert_eq!(value, Some("\\E[q"));
    assert!(!remove);

    // Empty string
    assert!(super::parse_capability_override("").is_none());

    // Whitespace trimmed
    let (name, value, remove) = super::parse_capability_override("  Tc  ").unwrap();
    assert_eq!(name, "Tc");
    assert!(value.is_none());
    assert!(!remove);
}

#[test]
fn override_removal_wins_over_xt_reintroduction() {
    // XT triggers bpaste/focus/title, but if an explicit Enbp@ override
    // removes bpaste, the second override pass must honour the removal.
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::TerminalOverrides,
            "custom*:XT:Enbp@:Dsbp@".to_owned(),
            SetOptionMode::Append,
        )
        .expect("terminal-overrides append succeeds");

    let terminal = OuterTerminal::resolve(
        &options,
        OuterTerminalContext::from_pairs(&[("TERM", "custom-term")]),
    );

    let features = terminal.features_string();
    // XT should enable focus and title.
    assert!(features.contains("focus"), "focus should be active");
    assert!(features.contains("title"), "title should be active");
    // But bpaste was explicitly removed.
    assert!(
        !features.contains("bpaste"),
        "bpaste should be removed by override"
    );
}

#[test]
fn override_removal_wins_over_tc_rgb() {
    // Tc triggers RGB, but if AX@ removes default_colours, RGB should
    // still be set (Tc only controls RGB, not AX). Verify Tc works and
    // AX@ is independent.
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::TerminalOverrides,
            "plain*:Tc:AX@".to_owned(),
            SetOptionMode::Append,
        )
        .expect("terminal-overrides append succeeds");

    let terminal = OuterTerminal::resolve(
        &options,
        OuterTerminalContext::from_pairs(&[("TERM", "plain-term")]),
    );

    let features = terminal.features_string();
    assert!(features.contains("RGB"), "Tc should enable RGB");
}
