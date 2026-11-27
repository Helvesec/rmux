mod family_tests;

use super::{CursorScope, OuterTerminal, OuterTerminalContext};
use rmux_core::{alternate_screen_exit_sequence, input::mode, OptionStore};
use rmux_proto::{
    ClientTerminalContext, OptionName, PaneTarget, ScopeSelector, SessionName, SetOptionMode,
    TerminalSize, WindowTarget,
};

use crate::pane_screen_state::PaneScreenState;

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn make_session() -> crate::session_fixture::SessionFixture {
    crate::session_fixture::SessionFixture::new(
        session_name("alpha"),
        TerminalSize { cols: 80, rows: 24 },
    )
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
    assert!(stop.ends_with("\u{1b}[?1049l\u{1b}[23;0;0t\u{1b}[?25h"));
}

#[test]
fn attach_stop_restores_outer_cursor_after_alternate_screen_exit() {
    const TERM: &str = "xterm-256color";

    let terminal = OuterTerminal::resolve(
        &OptionStore::new(),
        OuterTerminalContext::from_pairs(&[("TERM", TERM)]),
    );
    let stop = terminal.attach_stop_sequence();
    let mut expected_suffix = alternate_screen_exit_sequence(TERM).to_vec();
    expected_suffix.extend_from_slice(b"\x1b[?25h");

    assert!(
        stop.ends_with(&expected_suffix),
        "attach stop must exit the alternate screen before its final outer-terminal cursor show: {stop:?}"
    );
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
fn active_pane_mouse_tracking_enables_outer_mouse_with_mouse_option_off() {
    // Issue #93: tmux enables outer mouse reporting when the `mouse` option
    // is on OR the active pane's application requested a tracking mode, so
    // vim/htop over SSH must get mouse events with `mouse off`.
    let terminal = OuterTerminal::resolve_for_session(
        &OptionStore::new(),
        Some(&session_name("alpha")),
        OuterTerminalContext::default().with_client_terminal(&ClientTerminalContext {
            terminal_features: vec!["mouse".to_owned()],
            utf8: true,
        }),
    )
    .with_active_pane_mouse_mode(rmux_core::input::mode::MODE_MOUSE_BUTTON);

    let start = String::from_utf8(terminal.attach_start_sequence()).expect("utf8");
    assert!(
        start.contains(MOUSE_ENABLE_SEQUENCE),
        "pane-driven tracking must enable outer mouse despite mouse=off"
    );
}

#[test]
fn without_option_or_pane_tracking_outer_mouse_stays_disabled() {
    let terminal = OuterTerminal::resolve_for_session(
        &OptionStore::new(),
        Some(&session_name("alpha")),
        OuterTerminalContext::default().with_client_terminal(&ClientTerminalContext {
            terminal_features: vec!["mouse".to_owned()],
            utf8: true,
        }),
    )
    .with_active_pane_mouse_mode(0);

    let start = String::from_utf8(terminal.attach_start_sequence()).expect("utf8");
    assert!(
        !start.contains("\u{1b}[?1000h"),
        "no option and no pane tracking must not enable outer mouse"
    );
}

#[test]
fn transition_disables_outer_mouse_when_the_pane_stops_tracking() {
    let context = || {
        OuterTerminalContext::default().with_client_terminal(&ClientTerminalContext {
            terminal_features: vec!["mouse".to_owned()],
            utf8: true,
        })
    };
    let tracking = OuterTerminal::resolve_for_session(
        &OptionStore::new(),
        Some(&session_name("alpha")),
        context(),
    )
    .with_active_pane_mouse_mode(rmux_core::input::mode::MODE_MOUSE_BUTTON);
    let idle = OuterTerminal::resolve_for_session(
        &OptionStore::new(),
        Some(&session_name("alpha")),
        context(),
    )
    .with_active_pane_mouse_mode(0);

    let enable = String::from_utf8(tracking.transition_sequence_from(&idle)).expect("utf8");
    assert!(
        enable.contains(MOUSE_ENABLE_SEQUENCE),
        "pane starting to track must enable outer mouse on refresh"
    );
    let disable = String::from_utf8(idle.transition_sequence_from(&tracking)).expect("utf8");
    assert!(
        disable.contains(MOUSE_DISABLE_SEQUENCE),
        "pane resetting its tracking mode must disable outer mouse on refresh"
    );
}

#[test]
fn active_pane_all_motion_tracking_preserves_decset_1003() {
    let terminal = OuterTerminal::resolve_for_session(
        &OptionStore::new(),
        Some(&session_name("alpha")),
        OuterTerminalContext::default().with_client_terminal(&ClientTerminalContext {
            terminal_features: vec!["mouse".to_owned()],
            utf8: true,
        }),
    )
    .with_active_pane_mouse_mode(rmux_core::input::mode::MODE_MOUSE_ALL);

    let start = String::from_utf8(terminal.attach_start_sequence()).expect("utf8");
    assert!(start.contains("\u{1b}[?1003h"));
    assert!(!start.contains("\u{1b}[?1002h"));

    let stop = String::from_utf8(terminal.attach_stop_sequence()).expect("utf8");
    assert!(stop.contains("\u{1b}[?1003l"));
}

#[test]
fn focus_follows_mouse_option_upgrades_mouse_tracking_to_all_motion() {
    let context = || {
        OuterTerminalContext::default().with_client_terminal(&ClientTerminalContext {
            terminal_features: vec!["mouse".to_owned()],
            utf8: true,
        })
    };
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::FocusFollowsMouse,
            "on".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("focus-follows-mouse set succeeds");

    let mouse_off =
        OuterTerminal::resolve_for_session(&options, Some(&session_name("alpha")), context());
    let off_start = String::from_utf8(mouse_off.attach_start_sequence()).expect("utf8");
    assert!(!off_start.contains("\u{1b}[?1003h"));

    options
        .set(
            ScopeSelector::Global,
            OptionName::Mouse,
            "on".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("mouse set succeeds");
    let mouse_on =
        OuterTerminal::resolve_for_session(&options, Some(&session_name("alpha")), context());
    let on_start = String::from_utf8(mouse_on.attach_start_sequence()).expect("utf8");
    assert!(on_start.contains("\u{1b}[?1003h"));
    assert!(!on_start.contains("\u{1b}[?1002h"));

    options
        .set(
            ScopeSelector::Global,
            OptionName::FocusFollowsMouse,
            "off".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("focus-follows-mouse unset succeeds");
    let button_tracking =
        OuterTerminal::resolve_for_session(&options, Some(&session_name("alpha")), context());
    let button_start = String::from_utf8(button_tracking.attach_start_sequence()).expect("utf8");
    assert!(button_start.contains("\u{1b}[?1002h"));
    assert!(!button_start.contains("\u{1b}[?1003h"));
}

/// The prelude's title/path gate, exercised straight against the capability
/// layer. `set-titles off` resolves no title, which on tmux 3.7b means the
/// attached client writes neither OSC 0 nor OSC 7 even when the terminal
/// advertises `title` and `osc7`.
#[test]
fn render_prelude_without_resolved_title_writes_neither_title_nor_path() {
    let mut options = title_capable_options();
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

    let terminal = title_capable_terminal(&options);
    let prelude = render_title_prelude(
        &terminal,
        &options,
        super::ClientTitleUpdate {
            resolved: None,
            path: super::ClientPathUpdate::Reported(TITLE_PANE_PATH),
            previous: None,
        },
    );

    assert!(
        !prelude.contains("\u{1b}]0;"),
        "set-titles off must not drive the outer title, got {prelude:?}"
    );
    assert!(
        !prelude.contains("\u{1b}]7;"),
        "set-titles off must not drive OSC 7 either, got {prelude:?}"
    );
    // The rest of the prelude is untouched by the title gate.
    assert!(prelude.contains("\u{1b}]12;rgb:80/00/00\u{7}"));
}

/// A resolved title is the expanded `set-titles-string`, never the bare pane
/// title (issue #182), and it unlocks the neighbouring OSC 7 path.
#[test]
fn render_prelude_writes_resolved_title_and_path() {
    let options = title_capable_options();
    let terminal = title_capable_terminal(&options);

    let prelude = render_title_prelude(
        &terminal,
        &options,
        super::ClientTitleUpdate {
            resolved: Some("RMUXTEST alpha:0"),
            path: super::ClientPathUpdate::Reported(TITLE_PANE_PATH),
            previous: None,
        },
    );

    assert!(prelude.contains("\u{1b}]0;RMUXTEST alpha:0\u{7}"));
    assert!(prelude.contains("\u{1b}]7;file:///tmp/project\u{7}"));
}

/// tmux compares each expansion against `c->title` / `c->path` and writes only
/// what differs, so a refresh resolving the same values is silent. Measured on
/// tmux 3.7b: a static title and path survive four forced `refresh-client`
/// redraws with exactly one OSC 0 and one OSC 7 on the wire.
#[test]
fn render_prelude_skips_an_unchanged_title_and_path() {
    let options = title_capable_options();
    let terminal = title_capable_terminal(&options);
    let shown = super::ClientTitleState {
        title: Some("RMUXTEST alpha:0".to_owned()),
        path: Some(TITLE_PANE_PATH.to_owned()),
    };

    let prelude = render_title_prelude(
        &terminal,
        &options,
        super::ClientTitleUpdate {
            resolved: Some("RMUXTEST alpha:0"),
            path: super::ClientPathUpdate::Reported(TITLE_PANE_PATH),
            previous: Some(&shown),
        },
    );
    assert!(
        !prelude.contains("\u{1b}]0;") && !prelude.contains("\u{1b}]7;"),
        "unchanged title and path must not be re-emitted, got {prelude:?}"
    );

    let changed = render_title_prelude(
        &terminal,
        &options,
        super::ClientTitleUpdate {
            resolved: Some("RMUXTEST alpha:1"),
            path: super::ClientPathUpdate::Reported("file:///tmp/other"),
            previous: Some(&shown),
        },
    );
    assert!(changed.contains("\u{1b}]0;RMUXTEST alpha:1\u{7}"));
    assert!(changed.contains("\u{1b}]7;file:///tmp/other\u{7}"));
}

/// tmux 3.7b writes the expanded title verbatim, so a title carrying `ESC ] 0 ;`
/// closes the sequence early and injects into the outer terminal. RMUX keeps
/// its deliberate divergence: control characters are neutralised before the
/// payload reaches the terminal.
#[test]
fn render_prelude_neutralises_control_characters_in_the_title() {
    let options = title_capable_options();
    let terminal = title_capable_terminal(&options);

    let prelude = render_title_prelude(
        &terminal,
        &options,
        super::ClientTitleUpdate {
            resolved: Some("A\u{1b}]0;INJECT\u{7}B\tC"),
            path: super::ClientPathUpdate::Reported(TITLE_PANE_PATH),
            previous: None,
        },
    );

    let title = prelude
        .split_once("\u{1b}]0;")
        .expect("a title is written")
        .1
        .split_once('\u{7}')
        .expect("the title terminates")
        .0;
    // ESC, BEL and TAB each become a space; the surviving "]0;" is inert text.
    assert_eq!(title, "A ]0;INJECT B C");
    assert!(
        !title.contains('\u{1b}') && !title.contains('\u{7}'),
        "no control character may survive into the title, got {title:?}"
    );
    // Exactly one OSC 0 introducer: the payload cannot open a second one.
    assert_eq!(prelude.matches("\u{1b}]0;").count(), 1);
}

/// A terminal that never advertised TSL/FSL keeps its title untouched, exactly
/// as tmux's `tty_set_title()` returns early without both capabilities. The
/// value is still remembered, matching tmux assigning `c->title` regardless.
#[test]
fn render_prelude_leaves_a_title_incapable_terminal_alone() {
    let options = OptionStore::new();
    let terminal = OuterTerminal::resolve(
        &options,
        // No TERM at all: the Windows Terminal case from issue #182, where no
        // terminal family and no XT flag supply a title capability.
        OuterTerminalContext::default(),
    );
    assert!(
        !terminal.features_string().contains("title"),
        "fixture must not advertise the title capability"
    );

    let update = super::ClientTitleUpdate {
        resolved: Some("RMUXTEST alpha:0"),
        path: super::ClientPathUpdate::Reported(TITLE_PANE_PATH),
        previous: None,
    };
    let prelude = render_title_prelude(&terminal, &options, update);

    assert!(
        !prelude.contains("\u{1b}]0;") && !prelude.contains("RMUXTEST"),
        "a title-incapable terminal must receive no title, got {prelude:?}"
    );
    let rendered = terminal
        .rendered_client_title(update)
        .expect("set-titles on commits");
    assert_eq!(
        rendered.state().title(),
        Some("RMUXTEST alpha:0"),
        "tmux remembers c->title even when the tty cannot write it"
    );
    // Nothing reached the wire, so the frame stays replaceable in the control
    // queue: a client with no title template must not lose render coalescing
    // just because `set-titles` is on.
    assert!(
        !rendered.wrote(),
        "a terminal with no title template emits no OSC bytes"
    );
}

/// While `set-titles` is off nothing is committed, so the value the terminal
/// still shows survives the option going off and back on (oracle: tmux 3.7b
/// emits exactly one title across an on -> off -> on toggle).
#[test]
fn a_suppressed_title_commits_nothing_and_keeps_the_previous_path() {
    let shown = super::ClientTitleState {
        title: Some("STABLE".to_owned()),
        path: Some(TITLE_PANE_PATH.to_owned()),
    };
    assert!(super::ClientTitleUpdate {
        resolved: None,
        path: super::ClientPathUpdate::Reported("file:///tmp/other"),
        previous: Some(&shown),
    }
    .rendered_by(&title_capable_outer_terminal())
    .is_none());

    // A render that resolves a title but reads no pane path keeps the path the
    // client was already given rather than forgetting it, and writes nothing.
    let rendered = super::ClientTitleUpdate {
        resolved: Some("STABLE"),
        path: super::ClientPathUpdate::Unread,
        previous: Some(&shown),
    }
    .rendered_by(&title_capable_outer_terminal())
    .expect("set-titles on commits");
    assert_eq!(rendered.state(), &shown);
    assert!(
        !rendered.wrote(),
        "a fully deduplicated render puts no OSC bytes in the frame"
    );
}

/// A frame that actually carries OSC 0 / OSC 7 must stay in the control queue:
/// its successor deduplicates against it, so replacing it would strand the
/// outer terminal on the previous title with nothing left to correct it.
#[test]
fn a_title_carrying_render_is_not_a_replaceable_refresh() {
    let shown = super::ClientTitleState {
        title: Some("STABLE".to_owned()),
        path: Some(TITLE_PANE_PATH.to_owned()),
    };
    let wrote = super::ClientTitleUpdate {
        resolved: Some("CHANGED"),
        path: super::ClientPathUpdate::Reported(TITLE_PANE_PATH),
        previous: Some(&shown),
    }
    .rendered_by(&title_capable_outer_terminal())
    .expect("set-titles on commits");
    assert!(wrote.wrote(), "a changed title puts OSC 0 in the frame");

    let path_only = super::ClientTitleUpdate {
        resolved: Some("STABLE"),
        path: super::ClientPathUpdate::Reported("file:///tmp/other"),
        previous: Some(&shown),
    }
    .rendered_by(&title_capable_outer_terminal())
    .expect("set-titles on commits");
    assert!(path_only.wrote(), "a changed path alone still writes OSC 7");
}

/// A render only commits to the client's remembered identity what it actually
/// put on the wire. The status tick expands under the state lock and commits
/// under the attach lock, so a concurrent refresh can deliver a different title
/// in between; adopting a value this render did not write would then claim the
/// outer terminal shows something it does not, and the next expansion back to
/// that value would be skipped — the stale title of issue #182.
#[test]
fn a_render_that_wrote_nothing_commits_nothing() {
    let shown = super::ClientTitleState {
        title: Some("STABLE".to_owned()),
        path: Some(TITLE_PANE_PATH.to_owned()),
    };

    let deduplicated = super::ClientTitleUpdate {
        resolved: Some("STABLE"),
        path: super::ClientPathUpdate::Reported(TITLE_PANE_PATH),
        previous: Some(&shown),
    }
    .rendered_by(&title_capable_outer_terminal())
    .expect("set-titles on commits");
    assert!(!deduplicated.wrote());
    assert_eq!(
        deduplicated.committed(),
        None,
        "a fully deduplicated render must not overwrite the remembered title"
    );

    let wrote = super::ClientTitleUpdate {
        resolved: Some("CHANGED"),
        path: super::ClientPathUpdate::Reported(TITLE_PANE_PATH),
        previous: Some(&shown),
    }
    .rendered_by(&title_capable_outer_terminal())
    .expect("set-titles on commits");
    assert_eq!(
        wrote.committed().and_then(super::ClientTitleState::title),
        Some("CHANGED"),
        "a render that wrote OSC 0 commits what the terminal now shows"
    );

    // A terminal that cannot write the sequence commits nothing either: it was
    // never told, so the next render must still consider the title pending.
    let incapable = super::ClientTitleUpdate {
        resolved: Some("CHANGED"),
        path: super::ClientPathUpdate::Reported(TITLE_PANE_PATH),
        previous: Some(&shown),
    }
    .rendered_by(&title_incapable_outer_terminal())
    .expect("set-titles on commits");
    assert_eq!(incapable.committed(), None);
}

const TITLE_PANE_PATH: &str = "file:///tmp/project";

fn title_capable_options() -> OptionStore {
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
}

fn title_capable_terminal(options: &OptionStore) -> OuterTerminal {
    OuterTerminal::resolve(
        options,
        OuterTerminalContext::from_pairs(&[("TERM", "tmux-256color")]),
    )
}

/// A terminal advertising both the title and OSC 7 templates, so what a render
/// records is what it would really put on the wire.
fn title_capable_outer_terminal() -> OuterTerminal {
    title_capable_terminal(&title_capable_options())
}

/// No TERM at all: the Windows Terminal case from issue #182, where no terminal
/// family and no XT flag supply a title capability.
fn title_incapable_outer_terminal() -> OuterTerminal {
    OuterTerminal::resolve(&OptionStore::new(), OuterTerminalContext::default())
}

fn render_title_prelude(
    terminal: &OuterTerminal,
    options: &OptionStore,
    update: super::ClientTitleUpdate<'_>,
) -> String {
    String::from_utf8(terminal.render_prelude(
        make_session().view(),
        options,
        CursorScope::Pane,
        update,
    ))
    .expect("utf8")
}

#[test]
fn attach_start_queries_client_theme_reports() {
    let terminal = OuterTerminal::resolve(
        &OptionStore::new(),
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
    );

    let start = String::from_utf8(terminal.attach_start_sequence()).expect("utf8");
    let stop = String::from_utf8(terminal.attach_stop_sequence()).expect("utf8");

    assert!(start.contains("\u{1b}[?2031h"));
    assert!(start.contains("\u{1b}[?996n"));
    assert!(stop.contains("\u{1b}[?2031l"));
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
fn cursor_state_transition_replays_explicit_blink_for_terminal_default_shape() {
    let terminal = OuterTerminal::resolve(
        &OptionStore::new(),
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
    );
    let inherited = rmux_core::resolve_cursor_style_state(0, 0, 0);
    let blinking = rmux_core::resolve_cursor_style_state(
        0,
        mode::MODE_CURSOR_BLINKING | mode::MODE_CURSOR_BLINKING_SET,
        0,
    );
    let steady = rmux_core::resolve_cursor_style_state(0, mode::MODE_CURSOR_BLINKING_SET, 0);

    assert_eq!(
        terminal.render_cursor_state_transition(None, inherited),
        None
    );
    assert_eq!(
        terminal.render_cursor_state_transition(None, blinking),
        Some("\x1b[?12h".to_owned())
    );
    assert_eq!(
        terminal.render_cursor_state_transition(Some(blinking), steady),
        Some("\x1b[?12l".to_owned())
    );
    assert_eq!(
        terminal.render_cursor_state_transition(Some(steady), inherited),
        Some("\x1b[2 q".to_owned())
    );
}

#[test]
fn cursor_state_transition_clears_default_blink_without_style_capabilities() {
    let options = terminal_options(&[(OptionName::TerminalOverrides, "xterm*:Ss@:Se@")]);
    let terminal = OuterTerminal::resolve(
        &options,
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
    );
    let inherited = rmux_core::resolve_cursor_style_state(0, 0, 0);
    let blinking = rmux_core::resolve_cursor_style_state(
        0,
        mode::MODE_CURSOR_BLINKING | mode::MODE_CURSOR_BLINKING_SET,
        0,
    );
    let unsupported_bar = rmux_core::resolve_cursor_style_state(6, 0, 0);

    assert_eq!(
        terminal.render_cursor_state_transition(None, blinking),
        Some("\x1b[?12h".to_owned())
    );
    assert_eq!(
        terminal.render_cursor_state_transition(Some(blinking), inherited),
        Some("\x1b[0 q".to_owned())
    );
    assert_eq!(
        terminal.render_cursor_state_transition(Some(blinking), unsupported_bar),
        Some("\x1b[0 q".to_owned())
    );
    assert_eq!(
        terminal.render_cursor_state_transition(Some(unsupported_bar), inherited),
        Some("\x1b[0 q".to_owned())
    );
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
fn cursor_style_transition_replays_pane_a_to_b_to_a() {
    let terminal = OuterTerminal::resolve(
        &OptionStore::new(),
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
    );

    assert_eq!(
        terminal.render_cursor_style_transition(Some(6), 3),
        Some("\x1b[3 q".to_owned())
    );
    assert_eq!(
        terminal.render_cursor_style_transition(Some(3), 6),
        Some("\x1b[6 q".to_owned())
    );
}

#[test]
fn pane_cursor_resolution_combines_shape_blink_and_scoped_default() {
    let session = make_session();
    let terminal = OuterTerminal::resolve(
        &OptionStore::new(),
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
    );
    let blinking = mode::MODE_CURSOR_BLINKING | mode::MODE_CURSOR_BLINKING_SET;
    let mut pane_state = PaneScreenState {
        mode: blinking,
        alternate_on: false,
        title: String::new(),
        path: String::new(),
        cursor_position: (0, 0),
        cursor_style: 6,
    };
    assert_eq!(
        terminal.resolve_cursor_style(
            session.view(),
            &OptionStore::new(),
            Some(&pane_state),
            CursorScope::Pane,
        ),
        5
    );

    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Window(WindowTarget::with_window(session.name().clone(), 0)),
            OptionName::CursorStyle,
            "bar".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("window cursor style");
    pane_state.cursor_style = 0;
    assert_eq!(
        terminal.resolve_cursor_style(
            session.view(),
            &options,
            Some(&pane_state),
            CursorScope::Pane,
        ),
        5
    );

    options
        .set(
            ScopeSelector::Pane(PaneTarget::with_window(session.name().clone(), 0, 0)),
            OptionName::CursorStyle,
            "underline".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("pane cursor style override");
    assert_eq!(
        terminal.resolve_cursor_style(
            session.view(),
            &options,
            Some(&pane_state),
            CursorScope::Pane,
        ),
        3
    );
}

#[test]
fn prompt_cursor_scopes_do_not_mutate_or_inherit_the_pane_cursor() {
    let session = make_session();
    let terminal = OuterTerminal::resolve(
        &OptionStore::new(),
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
    );
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::PromptCursorStyle,
            "underline".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("prompt cursor style");
    options
        .set(
            ScopeSelector::Global,
            OptionName::PromptCommandCursorStyle,
            "blinking-bar".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("command prompt cursor style");
    let pane_state = PaneScreenState {
        mode: 0,
        alternate_on: false,
        title: String::new(),
        path: String::new(),
        cursor_position: (0, 0),
        cursor_style: 2,
    };

    assert_eq!(
        terminal.resolve_cursor_style(
            session.view(),
            &options,
            Some(&pane_state),
            CursorScope::Prompt,
        ),
        4
    );
    assert_eq!(
        terminal.resolve_cursor_style(
            session.view(),
            &options,
            Some(&pane_state),
            CursorScope::CommandPrompt,
        ),
        5
    );
    assert_eq!(pane_state.cursor_style, 2);
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
            .encode_forced_clipboard_set(b"hi")
            .expect("clipboard write is available"),
    )
    .expect("utf8");
    assert_eq!(encoded, "\u{1b}]52;;aGk=\u{7}");
    // Under `external` an application's inbound OSC 52 is NOT relayed to the
    // outer terminal: tmux gates that path on set-clipboard == on only
    // (input.c input_osc_52 returns early unless set-clipboard == 2), so an
    // untrusted pane cannot drive the system clipboard under the default.
    assert!(!enabled.clipboard_passthrough_enabled());
    // tmux's own selections (copy-mode yank / `set-buffer -w`) still forward
    // under `external` (window-copy.c gates them on set-clipboard != 0).
    assert!(enabled.encode_clipboard_set(b"hi").is_some());

    let mut on_options = OptionStore::new();
    on_options
        .set(
            ScopeSelector::Global,
            OptionName::SetClipboard,
            "on".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("set-clipboard set succeeds");
    let on = OuterTerminal::resolve(
        &on_options,
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
    );
    // `on` is the opt-in that relays inbound application OSC 52 to the outer.
    assert!(on.clipboard_passthrough_enabled());

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
    assert!(!disabled.clipboard_passthrough_enabled());
    assert!(disabled.encode_forced_clipboard_set(b"hi").is_some());
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
fn terminal_override_can_disable_sync_wrapper_after_feature_match() {
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::TerminalFeatures,
            "xterm*:sync".to_owned(),
            SetOptionMode::Append,
        )
        .expect("terminal-features append succeeds");
    options
        .set(
            ScopeSelector::Global,
            OptionName::TerminalOverrides,
            "xterm*:Sync@".to_owned(),
            SetOptionMode::Append,
        )
        .expect("terminal-overrides append succeeds");
    let terminal = OuterTerminal::resolve(
        &options,
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
    );

    assert_eq!(terminal.wrap_render_frame(b"frame"), b"frame");
    assert!(!terminal
        .features_string()
        .split(',')
        .any(|feature| feature == "sync"));
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

/// Appends global terminal-option entries the way `set -as` does, on top of the
/// shipped defaults.
fn terminal_options(entries: &[(OptionName, &str)]) -> OptionStore {
    let mut options = OptionStore::new();
    for (option, value) in entries {
        options
            .set(
                ScopeSelector::Global,
                *option,
                (*value).to_owned(),
                SetOptionMode::Append,
            )
            .expect("terminal option append succeeds");
    }
    options
}

/// A native Windows attach: the client reports no `TERM` at all.
fn empty_term_context() -> OuterTerminalContext {
    let context = OuterTerminalContext::default();
    assert_eq!(context.term_name(), "");
    context
}

/// Both an absent `TERM` and an explicitly empty one resolve to the empty name.
fn empty_term_contexts() -> [OuterTerminalContext; 2] {
    let contexts = [
        empty_term_context(),
        OuterTerminalContext::from_pairs(&[("TERM", "")]),
    ];
    assert!(contexts
        .iter()
        .all(|context| context.term_name().is_empty()));
    contexts
}

#[test]
fn default_terminal_selectors_do_not_match_empty_term() {
    // The shipped defaults are all named selectors, so a client with no TERM
    // still resolves to nothing. Both arrays are pinned: now that an empty name
    // is matched as it stands, a default that ever gained an all-wildcard
    // selector would silently switch itself on for every such client.
    let options = OptionStore::new();
    assert_eq!(
        options.resolve_array_values(None, OptionName::TerminalFeatures),
        vec![
            "xterm*:clipboard:ccolour:cstyle:focus:title".to_owned(),
            "screen*:title".to_owned(),
            "rxvt*:ignorefkeys".to_owned(),
        ]
    );
    assert_eq!(
        options.resolve_array_values(None, OptionName::TerminalOverrides),
        vec!["linux*:AX@".to_owned()]
    );

    for context in empty_term_contexts() {
        let terminal = OuterTerminal::resolve(&options, context);
        assert_eq!(terminal.features_string(), "");
        assert!(!terminal.supports_sixel());
    }
}

#[test]
fn wildcard_terminal_feature_enables_sixel_for_empty_term() {
    // Issue #211: native Windows sets no TERM. Every all-wildcard selector must
    // be matched rather than skipped merely because the terminal name is empty.
    for selector in ["*:sixel", "**:sixel"] {
        let options = terminal_options(&[(OptionName::TerminalFeatures, selector)]);
        for context in empty_term_contexts() {
            let terminal = OuterTerminal::resolve(&options, context);
            assert!(terminal.supports_sixel(), "selector {selector:?}");
            assert!(terminal.features_string().contains("sixel"));
        }
    }
}

#[test]
fn wildcard_terminal_override_enables_sixel_for_empty_term() {
    // The override route is the capability-side twin and must stay symmetric.
    for selector in ["*:Sxl", "**:Sxl"] {
        let options = terminal_options(&[(OptionName::TerminalOverrides, selector)]);
        for context in empty_term_contexts() {
            let terminal = OuterTerminal::resolve(&options, context);
            assert!(terminal.supports_sixel(), "selector {selector:?}");
            assert!(terminal.features_string().contains("sixel"));
        }
    }
}

#[test]
fn named_terminal_selectors_still_do_not_match_empty_term() {
    // The documented `xterm*:sixel` recipe genuinely does not apply to a client
    // that reports no terminal name; only an explicit wildcard does. An absent
    // TERM must not be treated as matching every named pattern.
    for entries in [
        vec![(OptionName::TerminalFeatures, "xterm:sixel")],
        vec![(OptionName::TerminalOverrides, "xterm:Sxl")],
        vec![(OptionName::TerminalFeatures, "xterm*:sixel")],
        vec![(OptionName::TerminalOverrides, "xterm*:Sxl")],
        vec![(OptionName::TerminalFeatures, "?:sixel")],
        vec![(OptionName::TerminalOverrides, "?:Sxl")],
        vec![(OptionName::TerminalFeatures, "[x]:sixel")],
        vec![(OptionName::TerminalOverrides, "[x]:Sxl")],
        vec![(OptionName::TerminalFeatures, "\\*:sixel")],
        vec![(OptionName::TerminalOverrides, "\\*:Sxl")],
    ] {
        let options = terminal_options(&entries);
        for context in empty_term_contexts() {
            let terminal = OuterTerminal::resolve(&options, context);
            assert!(
                !terminal.supports_sixel(),
                "{entries:?} must not match an empty terminal name"
            );
            assert_eq!(terminal.features_string(), "");
        }
    }
}

#[test]
fn malformed_terminal_selectors_stay_rejected_for_empty_term() {
    // An empty selector matches the empty string under fnmatch, so the explicit
    // malformed-entry rejection is what keeps `:sixel` and `:Sxl` inert. Both
    // routes must keep rejecting them rather than letting them through on a
    // client with no TERM.
    for entries in [
        vec![(OptionName::TerminalFeatures, ":sixel")],
        vec![(OptionName::TerminalFeatures, "   :sixel")],
        vec![(OptionName::TerminalOverrides, ":Sxl")],
    ] {
        let options = terminal_options(&entries);
        let terminal = OuterTerminal::resolve(&options, empty_term_context());
        assert!(
            !terminal.supports_sixel(),
            "{entries:?} is malformed and must not apply"
        );
        assert_eq!(terminal.features_string(), "");
    }
}

#[test]
fn wildcard_terminal_selectors_still_match_named_terms() {
    // The empty-name route must not be bought by weakening the ordinary one.
    for entries in [
        vec![(OptionName::TerminalFeatures, "*:sixel")],
        vec![(OptionName::TerminalOverrides, "*:Sxl")],
    ] {
        let options = terminal_options(&entries);
        let terminal = OuterTerminal::resolve(
            &options,
            OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
        );
        assert!(
            terminal.supports_sixel(),
            "{entries:?} must match xterm too"
        );
        // The named defaults keep applying alongside the wildcard entry.
        assert!(terminal.features_string().contains("clipboard"));
    }
}

#[test]
fn terminal_override_removal_wins_over_wildcard_feature_for_empty_term() {
    // Overrides are applied after features, so `Sxl@` still removes what
    // `*:sixel` enabled — precedence is unchanged by the empty terminal name.
    let options = terminal_options(&[
        (OptionName::TerminalFeatures, "*:sixel"),
        (OptionName::TerminalOverrides, "*:Sxl@"),
    ]);

    let terminal = OuterTerminal::resolve(&options, empty_term_context());

    assert!(!terminal.supports_sixel());
}

#[test]
fn xt_reintroduction_and_its_removal_both_reach_an_empty_term() {
    // A wildcard `XT` now reaches a client with no TERM, which makes the second
    // override pass reachable there too: XT re-introduces bpaste/focus/title,
    // and the explicit `Enbp@`/`Dsbp@` removals in the same entry still win.
    let options = terminal_options(&[(OptionName::TerminalOverrides, "*:XT:Enbp@:Dsbp@")]);

    let terminal = OuterTerminal::resolve(&options, empty_term_context());

    let features = terminal.features_string();
    assert!(features.contains("focus"), "XT should enable focus");
    assert!(features.contains("title"), "XT should enable title");
    assert!(
        !features.contains("bpaste"),
        "the second override pass must still honour the explicit removal"
    );
}

#[test]
fn context_features_still_enable_sixel_for_empty_term() {
    // `rmux -T sixel` reaches the terminal through the context features, which
    // are applied last and stay authoritative even against an explicit `Sxl@`
    // removal.
    let options = terminal_options(&[(OptionName::TerminalOverrides, "*:Sxl@")]);
    let context = empty_term_context().with_client_terminal(&ClientTerminalContext {
        terminal_features: vec!["sixel".to_owned()],
        utf8: false,
    });

    let terminal = OuterTerminal::resolve(&options, context);

    assert!(terminal.supports_sixel());
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
    assert!(terminal.encode_forced_clipboard_set(b"").is_none());
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
    assert_eq!(super::colour_to_rgb(90), Some((128, 128, 128)));
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
fn override_removal_wins_over_vt100_family_reintroduction() {
    let options = terminal_options(&[(OptionName::TerminalOverrides, "xterm*:Enbp@")]);
    let terminal = OuterTerminal::resolve(
        &options,
        OuterTerminalContext::from_pairs(&[("TERM", "xterm-256color")]),
    );
    assert!(!terminal.has_bracketed_paste());
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

#[test]
fn cursor_colour_uses_the_same_tmux_palette_as_colour_formats() {
    let palette = [
        (0, 0, 0),
        (128, 0, 0),
        (0, 128, 0),
        (128, 128, 0),
        (0, 0, 128),
        (128, 0, 128),
        (0, 128, 128),
        (192, 192, 192),
        (128, 128, 128),
        (255, 0, 0),
        (0, 255, 0),
        (255, 255, 0),
        (0, 0, 255),
        (255, 0, 255),
        (0, 255, 255),
        (255, 255, 255),
    ];
    for (index, rgb) in palette.into_iter().enumerate() {
        assert_eq!(
            super::colour_to_rgb(super::COLOUR_FLAG_256 | index as i32),
            Some(rgb)
        );
        let ansi = if index < 8 {
            index as i32
        } else {
            index as i32 + 82
        };
        assert_eq!(super::colour_to_rgb(ansi), Some(rgb));
    }
}

#[test]
fn terminal_features_reports_declarations_without_inventing_xterm_fallback_features() {
    for (value, colorterm, expected) in [
        ("xterm*:title", "", "bpaste,focus,title"),
        ("xterm*:title:RGB", "", "bpaste,focus,RGB,title"),
        ("xterm*:title", "truecolor", "bpaste,focus,RGB,title"),
        (
            "xterm*:clipboard:ccolour:cstyle:focus:title",
            "",
            "bpaste,ccolour,clipboard,cstyle,focus,title",
        ),
    ] {
        let mut options = OptionStore::new();
        options
            .set(
                ScopeSelector::Global,
                OptionName::TerminalFeatures,
                value.into(),
                SetOptionMode::Replace,
            )
            .unwrap();
        let terminal = OuterTerminal::resolve(
            &options,
            OuterTerminalContext::from_pairs(&[("TERM", "xterm"), ("COLORTERM", colorterm)]),
        );
        assert_eq!(terminal.features_string(), expected);
        assert!(terminal.supports_rgb);
        assert!(terminal.supports_mouse);
    }
}
