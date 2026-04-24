use super::*;

#[test]
fn new_window_accepts_implicit_target() {
    let cli = parse_args(&["new-window"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::NewWindow(args) => {
            assert!(args.target.is_none());
            assert_eq!(args.name, None);
            assert!(!args.detached);
        }
        _ => panic!("expected NewWindow command"),
    }
}

#[test]
fn new_window_accepts_name_and_detached_flags() {
    let cli = parse_args(&[
        "new-window",
        "-t",
        "alpha",
        "-n",
        "logs",
        "-d",
        "-c",
        "/tmp/work",
        "--",
        "printf hi",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::NewWindow(args) => {
            assert_eq!(
                args.target.expect("target should parse").to_string(),
                "alpha"
            );
            assert_eq!(args.name.as_deref(), Some("logs"));
            assert!(args.detached);
            assert_eq!(args.start_directory, Some(PathBuf::from("/tmp/work")));
            assert_eq!(args.command, vec!["printf hi".to_owned()]);
        }
        _ => panic!("expected NewWindow command"),
    }
}

#[test]
fn respawn_window_accepts_directory_environment_and_command() {
    let cli = parse_args(&[
        "respawn-window",
        "-k",
        "-e",
        "FOO=1",
        "-t",
        "alpha:1",
        "-c",
        "/tmp/work",
        "--",
        "sleep",
        "30",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::RespawnWindow(args) => {
            assert!(args.kill);
            assert_eq!(args.environment, vec!["FOO=1".to_owned()]);
            assert_eq!(target_text(&args.target), "alpha:1");
            assert_eq!(args.start_directory, Some(PathBuf::from("/tmp/work")));
            assert_eq!(args.command, vec!["sleep".to_owned(), "30".to_owned()]);
        }
        _ => panic!("expected RespawnWindow command"),
    }
}

#[test]
fn kill_window_accepts_window_targets_and_kill_others() {
    let cli = parse_args(&["kill-window", "-a", "-t", "alpha:5"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::KillWindow(args) => {
            assert_eq!(args.target.as_ref().expect("target").to_string(), "alpha:5");
            assert!(args.kill_others);
        }
        _ => panic!("expected KillWindow command"),
    }
}

#[test]
fn select_window_preserves_session_only_targets_for_runtime_resolution() {
    let cli = parse_args(&["select-window", "-t", "alpha"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SelectWindow(args) => {
            assert_eq!(args.target.as_ref().expect("target").to_string(), "alpha")
        }
        _ => panic!("expected SelectWindow command"),
    }
}

#[test]
fn rename_window_accepts_hyphen_prefixed_names() {
    let cli = parse_args(&["rename-window", "-t", "alpha:2", "-scratch"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::RenameWindow(args) => {
            assert_eq!(args.target.as_ref().expect("target").to_string(), "alpha:2");
            assert_eq!(args.new_name, "-scratch");
        }
        _ => panic!("expected RenameWindow command"),
    }
}

#[test]
fn next_window_accepts_session_targets() {
    let cli = parse_args(&["next-window", "-t", "alpha"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::NextWindow(args) => {
            assert_eq!(args.target.expect("target exists").to_string(), "alpha")
        }
        _ => panic!("expected NextWindow command"),
    }
}

#[test]
fn next_window_allows_implicit_current_session_target() {
    let cli = parse_args(&["next-window"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::NextWindow(args) => assert!(args.target.is_none()),
        _ => panic!("expected NextWindow command"),
    }
}

#[test]
fn choose_window_alias_routes_through_mode_tree_queue_command() {
    let cli = parse_args(&["choose-window"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ChooseTree(args) => {
            assert!(args.sessions_collapsed || args.windows_collapsed);
            assert_eq!(args.queue_command, "choose-tree -w");
        }
        _ => panic!("expected ChooseTree command"),
    }
}

#[test]
fn choose_buffer_parses_as_queued_mode_tree_command() {
    let cli = parse_args(&["choose-buffer", "-NN", "-O", "size"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ChooseBuffer(args) => {
            assert_eq!(args.preview, 2);
            assert_eq!(args.sort_order.as_deref(), Some("size"));
            assert_eq!(args.queue_command, "choose-buffer -NN -O size");
        }
        _ => panic!("expected ChooseBuffer command"),
    }
}

#[test]
fn previous_window_preserves_tmux_style_raw_targets() {
    let cli = parse_args(&["previous-window", "-t", "alpha:1"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::PreviousWindow(args) => {
            assert_eq!(args.target.expect("target exists").to_string(), "alpha:1");
        }
        _ => panic!("expected PreviousWindow command"),
    }
}

#[test]
fn list_windows_accepts_optional_compatibility_format() {
    let cli = parse_args(&["list-windows", "-t", "alpha", "-F", "#{window_index}"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ListWindows(args) => {
            assert_eq!(args.target.expect("session target").to_string(), "alpha");
            assert_eq!(args.format.as_deref(), Some("#{window_index}"));
            assert!(!args.all_sessions);
        }
        _ => panic!("expected ListWindows command"),
    }
}

#[test]
fn list_windows_accepts_all_sessions_without_an_explicit_target() {
    let cli = parse_args(&["list-windows", "-a"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ListWindows(args) => {
            assert!(args.all_sessions);
            assert!(args.target.is_none());
            assert!(args.format.is_none());
        }
        _ => panic!("expected ListWindows command"),
    }
}

#[test]
fn list_sessions_accepts_optional_compatibility_format() {
    let cli = parse_args(&["list-sessions", "-F", "#{session_name}"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ListSessions(args) => {
            assert_eq!(args.format.as_deref(), Some("#{session_name}"));
            assert_eq!(args.filter, None);
            assert_eq!(args.sort_order, None);
            assert!(!args.reversed);
        }
        _ => panic!("expected ListSessions command"),
    }
}

#[test]
fn list_sessions_accepts_filter_sort_order_and_reverse() {
    let cli = parse_args(&[
        "list-sessions",
        "-f",
        "#{==:#{session_name},alpha}",
        "-O",
        "index",
        "-r",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ListSessions(args) => {
            assert_eq!(args.filter.as_deref(), Some("#{==:#{session_name},alpha}"));
            assert_eq!(args.sort_order.as_deref(), Some("index"));
            assert!(args.reversed);
        }
        _ => panic!("expected ListSessions command"),
    }
}

#[test]
fn new_session_accepts_print_and_window_name_flags() {
    let cli = parse_args(&[
        "new-session",
        "-d",
        "-P",
        "-F",
        "#{session_name}",
        "-n",
        "logs",
        "-s",
        "alpha",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::NewSession(args) => {
            assert!(args.detached);
            assert!(args.print_session_info);
            assert_eq!(args.print_format.as_deref(), Some("#{session_name}"));
            assert_eq!(args.window_name.as_deref(), Some("logs"));
            assert_eq!(
                args.session_name.expect("session name").to_string(),
                "alpha"
            );
        }
        _ => panic!("expected NewSession command"),
    }
}

#[test]
fn new_session_accepts_trailing_shell_command() {
    let cli = parse_args(&["new-session", "-d", "-s", "alpha", "sleep 30"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::NewSession(args) => {
            assert!(args.detached);
            assert_eq!(
                args.session_name.expect("session name").to_string(),
                "alpha"
            );
            assert_eq!(args.command, vec!["sleep 30"]);
        }
        _ => panic!("expected NewSession command"),
    }
}

#[test]
fn command_aliases_and_unique_prefixes_resolve_before_flag_parsing() {
    for command in ["ls", "list-s"] {
        let cli = parse_args(&[command, "-F", "#{session_name}"]).unwrap();

        match cli.command.expect("parsed command") {
            super::super::Command::ListSessions(args) => {
                assert_eq!(args.format.as_deref(), Some("#{session_name}"));
            }
            _ => panic!("expected ListSessions command"),
        }
    }
}

#[test]
fn ambiguous_command_prefix_fails_before_flag_parsing() {
    let error = parse_args(&["list"]).unwrap_err();

    assert_eq!(error.kind(), clap::error::ErrorKind::InvalidSubcommand);
    assert!(error
        .to_string()
        .contains("ambiguous command: list, could be:"));
}

#[test]
fn list_commands_parse_with_format_and_optional_target() {
    let cli = parse_args(&["list-commands", "-F", "#{command_name}"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ListCommands(args) => {
            assert_eq!(args.format.as_deref(), Some("#{command_name}"));
            assert_eq!(args.command, None);
        }
        _ => panic!("expected ListCommands command"),
    }
}

#[test]
fn top_level_parse_preserves_hyphenated_list_windows_flags() {
    let cli = parse_args(&["list-windows", "-a"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ListWindows(args) => {
            assert!(args.all_sessions);
            assert!(args.target.is_none());
        }
        other => panic!("expected ListWindows command, got {other:?}"),
    }
}

#[test]
fn top_level_parse_preserves_hyphenated_split_window_flags() {
    let cli = parse_args(&["split-window", "-l", "10", "-t", "alpha:0.0"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SplitWindow(args) => {
            assert_eq!(args.size.as_deref(), Some("10"));
            let target = args.target.as_ref().expect("target");
            assert_eq!(target.raw(), "alpha:0.0");
            assert_eq!(
                target.exact(),
                Some(&rmux_proto::Target::Pane(
                    rmux_proto::PaneTarget::with_window(
                        rmux_proto::SessionName::new("alpha").expect("valid session"),
                        0,
                        0,
                    )
                ))
            );
        }
        other => panic!("expected SplitWindow command, got {other:?}"),
    }
}

#[test]
fn top_level_parse_preserves_hyphenated_resize_pane_flags() {
    let cli = parse_args(&["resize-pane", "-D", "-t", "alpha:0.0"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ResizePane(args) => {
            assert_eq!(args.down, Some(1));
            assert_eq!(
                args.target.as_ref().expect("target").to_string(),
                "alpha:0.0"
            );
        }
        other => panic!("expected ResizePane command, got {other:?}"),
    }
}

#[test]
fn resize_pane_accepts_tmux_style_space_separated_direction_delta() {
    let cli = parse_args(&["resize-pane", "-t", "alpha:0.1", "-R", "5"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ResizePane(args) => {
            assert_eq!(args.right, Some(5));
            assert_eq!(
                args.target.as_ref().expect("target").to_string(),
                "alpha:0.1"
            );
        }
        other => panic!("expected ResizePane command, got {other:?}"),
    }

    let cli = parse_args(&["resize-pane", "-R", "-t", "alpha:0.1", "5"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::ResizePane(args) => {
            assert_eq!(args.right, Some(5));
            assert_eq!(
                args.target.as_ref().expect("target").to_string(),
                "alpha:0.1"
            );
        }
        other => panic!("expected ResizePane command, got {other:?}"),
    }
}

#[test]
fn resize_pane_rejects_direction_delta_before_later_flags_like_tmux() {
    let error = parse_args(&["resize-pane", "-R", "5", "-t", "alpha:0.1"]).unwrap_err();
    assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);

    let error = parse_args(&["resize-pane", "-R=5", "-t", "alpha:0.1"]).unwrap_err();
    assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
}

#[test]
fn queued_and_gated_commands_use_clap_help() {
    for command in [
        "command-prompt",
        "choose-tree",
        "clear-prompt-history",
        "display-menu",
        "display-popup",
        "link-window",
        "show-prompt-history",
        "unlink-window",
        "set-window-option",
        "show-window-options",
    ] {
        let error = parse_args(&[command, "--help"]).unwrap_err();
        assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
    }
}
