use super::*;

#[test]
fn new_session_accepts_omitted_session_name() {
    let cli = parse_args(&["new-session"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::NewSession(args) => {
            assert_eq!(args.session_name, None);
            assert!(!args.detached);
        }
        _ => panic!("expected NewSession command"),
    }
}

#[test]
fn new_session_accepts_start_directory_flag() {
    let cli = parse_args(&["new-session", "-d", "-s", "alpha", "-c", "/tmp"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::NewSession(args) => {
            assert_eq!(
                args.session_name.as_ref().map(ToString::to_string),
                Some("alpha".to_owned())
            );
            assert!(args.detached);
            assert_eq!(args.working_directory.as_deref(), Some("/tmp"));
        }
        _ => panic!("expected NewSession command"),
    }
}

#[test]
fn new_session_accepts_skip_environment_update_flag() {
    let cli = parse_args(&["new-session", "-E", "-d", "-s", "alpha"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::NewSession(args) => {
            assert!(args.skip_environment_update);
            assert!(args.detached);
            assert_eq!(
                args.session_name.as_ref().map(ToString::to_string),
                Some("alpha".to_owned())
            );
        }
        _ => panic!("expected NewSession command"),
    }
}

#[test]
fn command_free_invocation_leaves_command_for_default_client_command() {
    let cli = parse_args(&[]).unwrap();

    assert!(cli.command.is_none());
}

#[test]
fn top_level_flags_parse_before_the_command() {
    let cli = parse_args(&[
        "-2",
        "-CC",
        "-f",
        "first.conf",
        "-f",
        "second.conf",
        "-l",
        "-L",
        "named",
        "-N",
        "-S",
        "/tmp/rmux.sock",
        "-T",
        "RGB",
        "-u",
        "-vv",
        "list-sessions",
    ])
    .unwrap();

    assert!(cli.assume_256_colors);
    assert_eq!(cli.control_mode, 2);
    assert!(cli.login_shell);
    assert_eq!(cli.socket_name(), Some(std::ffi::OsStr::new("named")));
    assert!(cli.no_start_server);
    assert_eq!(
        cli.socket_path(),
        Some(std::path::Path::new("/tmp/rmux.sock"))
    );
    assert_eq!(cli.terminal_features(), &["RGB".to_owned()]);
    assert!(cli.utf8);
    assert_eq!(cli.verbose, 2);
    match cli.config_file_selection() {
        super::super::ConfigFileSelection::Custom(files) => {
            assert_eq!(
                files,
                [PathBuf::from("first.conf"), PathBuf::from("second.conf")]
            );
        }
        super::super::ConfigFileSelection::Default => panic!("expected custom config files"),
    }
    assert!(matches!(
        cli.command.expect("parsed command"),
        super::super::Command::ListSessions(_)
    ));
}

#[test]
fn top_level_accepts_attached_socket_name_before_the_command() {
    let cli = parse_args(&["-Lnamed", "list-sessions"]).unwrap();

    assert_eq!(cli.socket_name(), Some(std::ffi::OsStr::new("named")));
    assert!(matches!(
        cli.command.expect("parsed command"),
        super::super::Command::ListSessions(_)
    ));
}

#[test]
fn top_level_empty_socket_path_is_preserved_as_explicit_selection() {
    let cli = parse_args(&["-S", "", "list-sessions"]).unwrap();

    assert_eq!(cli.socket_path(), Some(std::path::Path::new("")));
    assert!(matches!(
        cli.command.expect("parsed command"),
        super::super::Command::ListSessions(_)
    ));
}

#[test]
fn single_dash_help_and_version_use_display_exits() {
    assert_eq!(
        parse_args(&["-h"]).unwrap_err().kind(),
        clap::error::ErrorKind::DisplayHelp
    );
    assert_eq!(
        parse_args(&["-V"]).unwrap_err().kind(),
        clap::error::ErrorKind::DisplayVersion
    );
}

#[test]
fn new_session_accepts_attached_short_value_flags() {
    let cli = parse_args(&["new-session", "-P", "-F#{pane_id}", "-sfoo", "-d"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::NewSession(args) => {
            assert!(args.detached);
            assert!(args.print_session_info);
            assert_eq!(args.print_format.as_deref(), Some("#{pane_id}"));
            assert_eq!(args.session_name.expect("session name").to_string(), "foo");
            assert!(args.command.is_empty());
        }
        _ => panic!("expected NewSession command"),
    }
}

#[test]
fn new_session_rejects_unknown_flags_before_shell_command() {
    let error = parse_args(&["new-session", "-d", "-Z", "-s", "alpha"]).unwrap_err();

    assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    assert!(error
        .to_string()
        .contains("command new-session: unknown flag -Z"));
}

#[test]
fn command_targets_accept_tmux_last_wins_repetition() {
    let cli = parse_args(&[
        "new-window",
        "-d",
        "-t$1:",
        "-P",
        "-F#{window_id}",
        "-t",
        "$1:",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::NewWindow(args) => {
            assert!(args.detached);
            assert!(args.print_target);
            assert_eq!(args.format.as_deref(), Some("#{window_id}"));
            assert_eq!(args.target.expect("target").to_string(), "$1:");
        }
        _ => panic!("expected NewWindow command"),
    }
}

#[test]
fn new_session_sanitizes_colon_in_session_name() {
    let cli = parse_args(&["new-session", "-s", "bad:name"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::NewSession(args) => {
            assert_eq!(
                args.session_name.expect("session name").to_string(),
                "bad_name"
            );
        }
        _ => panic!("expected NewSession command"),
    }
}

#[test]
fn new_session_sanitizes_dot_in_session_name() {
    let cli = parse_args(&["new-session", "-s", "bad.name"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::NewSession(args) => {
            assert_eq!(
                args.session_name.expect("session name").to_string(),
                "bad_name"
            );
        }
        _ => panic!("expected NewSession command"),
    }
}

#[test]
fn new_session_rejects_empty_session_name() {
    let error = parse_args(&["new-session", "-s", ""]).unwrap_err();
    assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
}

#[test]
fn has_session_allows_implicit_current_session_target() {
    let cli = parse_args(&["has-session"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::HasSession(args) => assert!(args.target.is_none()),
        _ => panic!("expected HasSession command"),
    }
}

#[test]
fn has_session_preserves_exact_target_marker_in_attached_short_value() {
    let cli = parse_args(&["has-session", "-t=foo"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::HasSession(args) => {
            assert_eq!(target_text(&args.target), "=foo");
        }
        _ => panic!("expected HasSession command"),
    }
}

#[test]
fn kill_session_requires_target() {
    let cli = parse_args(&["kill-session"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::KillSession(args) => assert!(args.target.is_none()),
        _ => panic!("expected KillSession command"),
    }
}

#[test]
fn switch_client_rejects_non_tmux_f_flag() {
    let error = parse_args(&["switch-client", "-f", "read-only"]).unwrap_err();
    assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
}

#[test]
fn rename_session_accepts_a_positional_new_name() {
    let cli = parse_args(&["rename-session", "-t", "alpha", "beta"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::RenameSession(args) => {
            assert_eq!(target_text(&args.target), "alpha");
            assert_eq!(args.new_name.to_string(), "beta");
        }
        _ => panic!("expected RenameSession command"),
    }
}

#[test]
fn rename_session_rejects_named_new_name_flag() {
    let error = parse_args(&["rename-session", "-t", "alpha", "-n", "beta"]).unwrap_err();
    assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
}

#[test]
fn rename_alias_parses_like_rename_session() {
    let cli = parse_args(&["rename", "-t", "alpha", "beta"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::RenameSession(args) => {
            assert_eq!(target_text(&args.target), "alpha");
            assert_eq!(args.new_name.to_string(), "beta");
        }
        _ => panic!("expected RenameSession command"),
    }
}

#[test]
fn kill_session_accepts_all_except_and_clear_alerts_flags() {
    let cli = parse_args(&["kill-session", "-a", "-C", "-t", "alpha"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::KillSession(args) => {
            assert!(args.kill_all_except_target);
            assert!(args.clear_alerts);
            assert_eq!(target_text(&args.target), "alpha");
        }
        _ => panic!("expected KillSession command"),
    }
}

#[test]
fn list_sessions_preserves_equals_in_attached_format_value() {
    let cli = parse_args(&["list-sessions", "-F=#{session_name}"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ListSessions(args) => {
            assert_eq!(args.format.as_deref(), Some("=#{session_name}"));
        }
        _ => panic!("expected ListSessions command"),
    }
}
