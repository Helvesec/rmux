use super::parse;

fn parse_args(args: &[&str]) -> Result<super::Cli, clap::Error> {
    let mut full_args = vec!["rmux"];
    full_args.extend_from_slice(args);
    parse(full_args)
}

#[test]
fn show_options_accepts_global_server_window_pane_and_value_only_scopes() {
    for args in [
        &["show-options"][..],
        &["show-options", "-A", "-t", "alpha", "status"][..],
        &["show-options", "-g"][..],
        &["show-options", "-g", "-t", "alpha", "status"][..],
        &["show-options", "-g", "@status-line"][..],
        &["show-options", "-s"][..],
        &["show-options", "-gs", "-t", "alpha", "message-limit"][..],
        &["show-options", "-gA", "status"][..],
        &["show-options", "-s", "-v", "terminal-features"][..],
        &["show-options", "-gw", "-t", "alpha:2", "pane-border-style"][..],
        &["show-options", "-w", "-t", "alpha:2"][..],
        &["show-options", "-w", "-t", "alpha:2.3"][..],
        &["show-options", "-p", "-t", "alpha:2.3", "-v"][..],
        &["show-options", "-t", "alpha"][..],
        &["show-window-options", "-t", "alpha:2"][..],
        &["show-window-options"][..],
        &[
            "show-window-options",
            "-g",
            "-t",
            "alpha",
            "pane-border-style",
        ][..],
        &[
            "show-window-options",
            "-v",
            "-t",
            "alpha:2.3",
            "pane-border-style",
        ][..],
    ] {
        parse_args(args).expect("show-options scope parses");
    }
}

#[test]
fn show_options_rejects_conflicting_scope_flags() {
    let error = parse_args(&["show-options", "-s", "-w"]).unwrap_err();
    assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn show_environment_accepts_global_and_session_scope() {
    parse_args(&["show-environment"]).expect("default show-environment parses");
    parse_args(&["show-environment", "-g"]).expect("global show-environment parses");
    parse_args(&["show-environment", "-t", "alpha"]).expect("session show-environment parses");
}

#[test]
fn show_hooks_accepts_default_current_session_scope() {
    parse_args(&["show-hooks"]).expect("default show-hooks parses");
}

#[test]
fn expanded_option_surface_accepts_representative_options() {
    for option in [
        "base-index",
        "buffer-limit",
        "main-pane-width",
        "status-left",
        "window-status-current-format",
        "window-style",
    ] {
        parse_args(&["set-option", "-g", option, "1"]).expect("expanded option parses");
    }

    parse_args(&[
        "set-option",
        "-w",
        "-t",
        "alpha:2",
        "synchronize-panes",
        "on",
    ])
    .expect("window-scoped set-option parses");
    parse_args(&["set-option", "-w", "-t", "alpha:2.3", "synchronize-panes"])
        .expect("window-scoped toggle parses");
    parse_args(&[
        "set-window-option",
        "-t",
        "alpha:2",
        "synchronize-panes",
        "on",
    ])
    .expect("set-window-option parses");
    parse_args(&[
        "set-window-option",
        "-t",
        "alpha",
        "synchronize-panes",
        "on",
    ])
    .expect("set-window-option accepts session targets");
}

#[test]
fn set_option_accepts_hyphen_prefixed_values() {
    let cli = parse_args(&["set-option", "-g", "base-index", "-1"])
        .expect("set-option accepts hyphen-prefixed numeric values");
    match cli.command.expect("parsed command") {
        super::Command::SetOption(args) => {
            assert!(args.global);
            assert_eq!(args.option, "base-index");
            assert_eq!(args.value.as_deref(), Some("-1"));
        }
        _ => panic!("expected SetOption command"),
    }
}

#[test]
fn set_window_option_accepts_hyphen_prefixed_values() {
    let cli = parse_args(&["set-window-option", "-g", "main-pane-width", "-5"])
        .expect("set-window-option accepts hyphen-prefixed numeric values");
    match cli.command.expect("parsed command") {
        super::Command::SetWindowOption(args) => {
            assert!(args.global);
            assert_eq!(args.option, "main-pane-width");
            assert_eq!(args.value.as_deref(), Some("-5"));
        }
        _ => panic!("expected SetWindowOption command"),
    }
}

#[test]
fn set_window_option_rejects_server_window_pane_and_unset_overrides_flags() {
    for flag in ["-s", "-w", "-p", "-U"] {
        let error = parse_args(&[
            "set-window-option",
            flag,
            "-t",
            "alpha",
            "synchronize-panes",
            "on",
        ])
        .expect_err(&format!("set-window-option must reject {flag}"));
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::UnknownArgument,
            "set-window-option {flag} should surface UnknownArgument"
        );
    }
}

#[test]
fn show_window_options_rejects_server_window_and_pane_flags() {
    for flag in ["-s", "-w", "-p"] {
        let error = parse_args(&["show-window-options", flag, "-t", "alpha"])
            .expect_err(&format!("show-window-options must reject {flag}"));
        assert_eq!(
            error.kind(),
            clap::error::ErrorKind::UnknownArgument,
            "show-window-options {flag} should surface UnknownArgument"
        );
    }
}

#[test]
fn show_window_options_accepts_global_and_value_only_combinations() {
    parse_args(&["show-window-options", "-g"]).expect("global show-window-options parses");
    parse_args(&["show-window-options", "-gv", "synchronize-panes"])
        .expect("global value-only show-window-options parses");
    parse_args(&[
        "show-window-options",
        "-v",
        "-t",
        "alpha:2",
        "synchronize-panes",
    ])
    .expect("value-only target show-window-options parses");
}
