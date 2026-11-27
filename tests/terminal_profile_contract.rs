#[path = "support/terminal_profiles.rs"]
mod terminal_profiles;

use terminal_profiles::{
    assert_profile_line, attach_terminal_profiles, control_terminal_profiles, TerminalProfile,
};

// Fixed payloads exercise the same checker and profiles as both native Windows
// matrices. These are contract fixtures, not evidence of a native Windows run.
const ATTACHED_LINES: &[&str] = &[
    "vt100|256,RGB|1|attached,focused,UTF-8",
    "xterm-256color|RGB,bpaste,mouse,sync|1|attached,focused,UTF-8",
    "xterm-kitty|kitty-graphics,hyperlinks,margins,osc7,sync|0|attached,focused",
    "wezterm|kitty-graphics,sixel,sync,RGB|0|attached,focused",
    "mintty|clipboard,mouse,sixel|0|attached,focused",
];
const CONTROL_LINES: &[&str] = &[
    "vt100|256,RGB|1|attached,focused,UTF-8,control-mode",
    "xterm-256color|bpaste,mouse,sync|1|attached,focused,UTF-8,control-mode",
    "xterm-kitty||0|attached,focused,control-mode",
    "wezterm||0|attached,focused,control-mode",
    "mintty||0|attached,focused,control-mode",
];

fn check_matrix(profiles: &[TerminalProfile], lines: &[&str], control: bool) {
    assert_eq!(profiles.len(), lines.len());
    for (profile, line) in profiles.iter().zip(lines) {
        // Every environment value supplied by the native harness must be a
        // valid command environment value, including empty optional hints.
        for value in [profile.term, profile.term_program, profile.colorterm] {
            assert!(!value.contains('\0'), "invalid profile {}", profile.name);
        }
        assert_profile_line(profile, line, control).unwrap();
    }
}

#[test]
fn attached_profiles_accept_contract_payloads() {
    check_matrix(attach_terminal_profiles(), ATTACHED_LINES, false);
}

#[test]
fn control_profiles_accept_contract_payloads() {
    check_matrix(control_terminal_profiles(), CONTROL_LINES, true);
}

#[test]
#[should_panic(expected = "must not infer 256 without -2")]
fn attached_windows_terminal_rejects_implicit_256() {
    let profile = attach_terminal_profiles()
        .iter()
        .find(|p| p.wt_session)
        .unwrap();
    assert_profile_line(
        profile,
        "xterm-256color|256,RGB,bpaste,mouse,sync|1|attached,focused,UTF-8",
        false,
    )
    .unwrap();
}

#[test]
#[should_panic(expected = "must not infer 256 without -2")]
fn control_windows_terminal_rejects_implicit_256() {
    let profile = control_terminal_profiles()
        .iter()
        .find(|p| p.wt_session)
        .unwrap();
    assert_profile_line(
        profile,
        "xterm-256color|256,bpaste,mouse,sync|1|attached,focused,UTF-8,control-mode",
        true,
    )
    .unwrap();
}

#[test]
fn windows_terminal_accepts_explicit_256_in_both_modes() {
    for (profiles, line, control) in [
        (
            attach_terminal_profiles(),
            "xterm-256color|256,RGB,bpaste,mouse,sync|1|attached,focused,UTF-8",
            false,
        ),
        (
            control_terminal_profiles(),
            "xterm-256color|256,bpaste,mouse,sync|1|attached,focused,UTF-8,control-mode",
            true,
        ),
    ] {
        let mut profile = *profiles.iter().find(|p| p.wt_session).unwrap();
        profile.top_level_args = &["-2"];
        assert_profile_line(&profile, line, control).unwrap();
    }
}
