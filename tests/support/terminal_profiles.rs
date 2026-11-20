use std::error::Error;

const EXPLICIT_TOP_LEVEL_TERMINAL_ARGS: &[&str] = &["-u", "-2", "-T", "RGB"];
const NO_TOP_LEVEL_ARGS: &[&str] = &[];

#[derive(Debug, Clone, Copy)]
pub struct TerminalProfile {
    pub name: &'static str,
    pub term: &'static str,
    pub term_program: &'static str,
    pub colorterm: &'static str,
    pub wt_session: bool,
    pub top_level_args: &'static [&'static str],
    pub expected_utf8: &'static str,
    pub expected_features: &'static [&'static str],
}

pub fn attach_terminal_profiles() -> &'static [TerminalProfile] {
    &[
        TerminalProfile {
            name: "explicit-vt100-top-level",
            term: "vt100",
            term_program: "",
            colorterm: "",
            wt_session: false,
            top_level_args: EXPLICIT_TOP_LEVEL_TERMINAL_ARGS,
            expected_utf8: "1",
            expected_features: &["256", "RGB"],
        },
        TerminalProfile {
            name: "windows-terminal",
            term: "xterm-256color",
            term_program: "Windows Terminal",
            colorterm: "truecolor",
            wt_session: true,
            top_level_args: NO_TOP_LEVEL_ARGS,
            expected_utf8: "1",
            // tmux 3.7b reports RGB without 256 for TERM=xterm-256color plus
            // COLORTERM=truecolor; only -2 or a declared feature adds 256.
            expected_features: &["RGB", "bpaste", "mouse", "sync"],
        },
        TerminalProfile {
            name: "kitty-term-profile",
            term: "xterm-kitty",
            term_program: "",
            colorterm: "",
            wt_session: false,
            top_level_args: NO_TOP_LEVEL_ARGS,
            expected_utf8: "0",
            expected_features: &["kitty-graphics", "hyperlinks", "margins", "osc7", "sync"],
        },
        TerminalProfile {
            name: "wezterm-profile",
            term: "wezterm",
            term_program: "WezTerm",
            colorterm: "truecolor",
            wt_session: false,
            top_level_args: NO_TOP_LEVEL_ARGS,
            expected_utf8: "0",
            expected_features: &["kitty-graphics", "sixel", "sync", "RGB"],
        },
        TerminalProfile {
            name: "mintty-profile",
            term: "mintty",
            term_program: "mintty",
            colorterm: "",
            wt_session: false,
            top_level_args: NO_TOP_LEVEL_ARGS,
            expected_utf8: "0",
            expected_features: &["clipboard", "mouse", "sixel"],
        },
    ]
}

pub fn control_terminal_profiles() -> &'static [TerminalProfile] {
    &[
        TerminalProfile {
            name: "control-explicit-vt100-top-level",
            term: "vt100",
            term_program: "",
            colorterm: "",
            wt_session: false,
            top_level_args: EXPLICIT_TOP_LEVEL_TERMINAL_ARGS,
            expected_utf8: "1",
            expected_features: &["256", "RGB"],
        },
        TerminalProfile {
            name: "control-windows-terminal",
            term: "xterm-256color",
            term_program: "Windows Terminal",
            colorterm: "truecolor",
            wt_session: true,
            top_level_args: NO_TOP_LEVEL_ARGS,
            expected_utf8: "1",
            expected_features: &["bpaste", "mouse", "sync"],
        },
        TerminalProfile {
            name: "control-kitty-term-profile",
            term: "xterm-kitty",
            term_program: "",
            colorterm: "",
            wt_session: false,
            top_level_args: NO_TOP_LEVEL_ARGS,
            expected_utf8: "0",
            expected_features: &[],
        },
        TerminalProfile {
            name: "control-wezterm-profile",
            term: "wezterm",
            term_program: "WezTerm",
            colorterm: "truecolor",
            wt_session: false,
            top_level_args: NO_TOP_LEVEL_ARGS,
            expected_utf8: "0",
            expected_features: &[],
        },
        TerminalProfile {
            name: "control-mintty-profile",
            term: "mintty",
            term_program: "mintty",
            colorterm: "",
            wt_session: false,
            top_level_args: NO_TOP_LEVEL_ARGS,
            expected_utf8: "0",
            expected_features: &[],
        },
    ]
}

pub fn assert_profile_line(
    profile: &TerminalProfile,
    line: &str,
    control_mode: bool,
) -> Result<(), Box<dyn Error>> {
    let parts = line.trim_end().split('|').collect::<Vec<_>>();
    if parts.len() != 4 {
        return Err(format!(
            "profile {} produced malformed list-clients line {line:?}",
            profile.name
        )
        .into());
    }

    assert_eq!(
        parts[0], profile.term,
        "profile {} should preserve client_termname",
        profile.name
    );
    assert_eq!(
        parts[2], profile.expected_utf8,
        "profile {} should preserve client_utf8",
        profile.name
    );

    let features = parts[1].split(',').collect::<Vec<_>>();
    for expected in profile.expected_features {
        assert!(
            features.iter().any(|feature| feature == expected),
            "profile {} should include feature {expected:?}; got {features:?} from {line:?}",
            profile.name
        );
    }

    if profile.wt_session && !profile.top_level_args.contains(&"-2") {
        assert!(
            !features.contains(&"256"),
            "profile {} must not infer 256 without -2; got {features:?}",
            profile.name
        );
    }

    let flags = parts[3].split(',').collect::<Vec<_>>();
    assert!(
        flags.contains(&"attached") && flags.contains(&"focused"),
        "profile {} should report attached/focused client flags; got {flags:?}",
        profile.name
    );
    if profile.expected_utf8 == "1" {
        assert!(
            flags.contains(&"UTF-8"),
            "profile {} should expose UTF-8 client flag; got {flags:?}",
            profile.name
        );
    } else {
        assert!(
            !flags.contains(&"UTF-8"),
            "profile {} should not expose UTF-8 client flag; got {flags:?}",
            profile.name
        );
    }
    assert_eq!(
        flags.contains(&"control-mode"),
        control_mode,
        "profile {} control-mode flag mismatch in {flags:?}",
        profile.name
    );

    Ok(())
}
