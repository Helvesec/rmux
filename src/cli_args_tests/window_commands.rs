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
fn new_window_accepts_kill_and_select_existing_flags() {
    let cli = parse_args(&["new-window", "-t$1:5", "-k", "-S", "-n", "logs"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::NewWindow(args) => {
            assert_eq!(
                args.target.expect("target should parse").to_string(),
                "$1:5"
            );
            assert!(args.kill_existing);
            assert!(args.select_existing);
            assert_eq!(args.name.as_deref(), Some("logs"));
            assert!(args.command.is_empty());
        }
        _ => panic!("expected NewWindow command"),
    }
}

#[test]
fn new_window_placement_flags_follow_tmux_priority() {
    for argv in [
        ["new-window", "-a", "-b", "-t", "alpha"],
        ["new-window", "-b", "-a", "-t", "alpha"],
    ] {
        let cli = parse_args(&argv).unwrap();
        match cli.command.expect("parsed command") {
            super::super::Command::NewWindow(args) => {
                assert!(!args.after);
                assert!(args.before);
            }
            _ => panic!("expected NewWindow command"),
        }
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
            assert_eq!(args.target.as_ref().expect("target").to_string(), "alpha");
            assert!(!args.last);
            assert!(!args.next);
            assert!(!args.previous);
            assert!(!args.toggle_last);
        }
        _ => panic!("expected SelectWindow command"),
    }
}

#[test]
fn select_window_accepts_negative_relative_targets() {
    let cli = parse_args(&["select-window", "-t", "-1"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SelectWindow(args) => {
            assert_eq!(args.target.as_ref().expect("target").to_string(), "-1");
            assert!(!args.last);
            assert!(!args.next);
            assert!(!args.previous);
            assert!(!args.toggle_last);
        }
        _ => panic!("expected SelectWindow command"),
    }
}

#[test]
fn select_window_accepts_exact_match_targets() {
    let cli = parse_args(&["select-window", "-t", "=alpha:1"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SelectWindow(args) => {
            let target = args.target.as_ref().expect("target");
            assert_eq!(target.to_string(), "=alpha:1");
            assert!(matches!(
                target.exact(),
                Some(rmux_proto::Target::Window(window))
                    if window.session_name().as_str() == "alpha" && window.window_index() == 1
            ));
        }
        _ => panic!("expected SelectWindow command"),
    }
}

#[test]
fn select_window_accepts_navigation_and_toggle_flags() {
    for (flag, expect_last, expect_next, expect_previous, expect_toggle) in [
        ("-l", true, false, false, false),
        ("-n", false, true, false, false),
        ("-p", false, false, true, false),
        ("-T", false, false, false, true),
    ] {
        let cli = parse_args(&["select-window", flag, "-t", "alpha:1"]).unwrap();
        match cli.command.expect("parsed command") {
            super::super::Command::SelectWindow(args) => {
                assert_eq!(args.target.as_ref().expect("target").to_string(), "alpha:1");
                assert_eq!(args.last, expect_last);
                assert_eq!(args.next, expect_next);
                assert_eq!(args.previous, expect_previous);
                assert_eq!(args.toggle_last, expect_toggle);
            }
            _ => panic!("expected SelectWindow command"),
        }
    }
}

#[test]
fn select_window_navigation_flags_follow_tmux_priority() {
    for argv in [
        ["select-window", "-n", "-p", "-t", "alpha:1"],
        ["select-window", "-p", "-n", "-t", "alpha:1"],
    ] {
        let cli = parse_args(&argv).unwrap();
        match cli.command.expect("parsed command") {
            super::super::Command::SelectWindow(args) => {
                assert!(args.next);
                assert!(!args.previous);
                assert!(!args.last);
            }
            _ => panic!("expected SelectWindow command"),
        }
    }
}

#[test]
fn select_window_rejects_zoom_flag_like_tmux() {
    let error = parse_args(&["select-window", "-Z"]).unwrap_err();

    assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    assert!(error
        .to_string()
        .contains("command select-window: unknown flag -Z"));
}

#[test]
fn rename_window_accepts_hyphen_prefixed_names() {
    let cli = parse_args(&["rename-window", "-t", "alpha:2", "--", "-scratch"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::RenameWindow(args) => {
            assert_eq!(args.target.as_ref().expect("target").to_string(), "alpha:2");
            assert_eq!(args.new_name, "-scratch");
        }
        _ => panic!("expected RenameWindow command"),
    }
}

#[test]
fn rename_window_rejects_extra_names_like_tmux() {
    let error = parse_args(&["rename-window", "-t", "alpha:0", "logs", "extra"]).unwrap_err();

    assert_eq!(error.kind(), clap::error::ErrorKind::TooManyValues);
    assert!(error
        .to_string()
        .contains("command rename-window: too many arguments (need at most 1)"));
}

#[test]
fn rename_window_rejects_missing_name_like_tmux() {
    let error = parse_args(&["rename-window"]).unwrap_err();

    assert_eq!(error.kind(), clap::error::ErrorKind::TooFewValues);
    assert!(error
        .to_string()
        .contains("command rename-window: too few arguments (need at least 1)"));
}

#[test]
fn swap_window_rejects_after_flag_like_tmux() {
    let error = parse_args(&["swap-window", "-a", "-s", "alpha:0", "-t", "alpha:1"]).unwrap_err();

    assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    assert!(error
        .to_string()
        .contains("command swap-window: unknown flag -a"));
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
fn mode_tree_commands_reject_tmux_invalid_auto_accept_flag() {
    for command in ["choose-tree", "choose-buffer", "choose-client"] {
        let error = parse_args(&[command, "-y", "1"]).unwrap_err();

        assert!(
            error
                .to_string()
                .contains(&format!("command {command}: unknown flag -y")),
            "{error}"
        );
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
            assert!(args.filter.is_none());
            assert!(!args.all_sessions);
        }
        _ => panic!("expected ListWindows command"),
    }
}

#[test]
fn list_windows_accepts_tmux_filter_flag() {
    let cli = parse_args(&[
        "list-windows",
        "-t",
        "$1",
        "-f",
        "#{m:logs*,#{window_name}}",
        "-F",
        "#{window_id}",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ListWindows(args) => {
            assert_eq!(args.target.expect("session target").to_string(), "$1");
            assert_eq!(args.filter.as_deref(), Some("#{m:logs*,#{window_name}}"));
            assert_eq!(args.format.as_deref(), Some("#{window_id}"));
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
            assert!(args.filter.is_none());
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
        }
        _ => panic!("expected ListSessions command"),
    }
}

#[test]
fn list_sessions_rejects_sort_order_and_reverse() {
    let error = parse_args(&[
        "list-sessions",
        "-f",
        "#{==:#{session_name},alpha}",
        "-O",
        "index",
        "-r",
    ])
    .unwrap_err();

    assert!(error.to_string().contains("unknown flag -O"), "{error}");

    let error = parse_args(&["list-sessions", "-O"]).unwrap_err();
    assert!(error.to_string().contains("unknown flag -O"), "{error}");

    let error = parse_args(&["list-sessions", "-r"]).unwrap_err();
    assert!(error.to_string().contains("unknown flag -r"), "{error}");
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
fn new_session_dimensions_report_short_errors() {
    for (flag, value, expected) in [
        ("-x", "70000", "width too large"),
        ("-x", "abc", "width invalid"),
        ("-y", "70000", "height too large"),
        ("-y", "abc", "height invalid"),
    ] {
        let error = parse_args(&["new-session", flag, value, "-d", "-s", "alpha"]).unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "expected {expected:?} in {error}"
        );
    }
}

#[test]
fn pane_size_flags_report_short_errors() {
    for (args, expected) in [
        (
            &["resize-pane", "-x"][..],
            "command resize-pane: -x expects an argument",
        ),
        (
            &["split-window", "-l"][..],
            "command split-window: -l expects an argument",
        ),
        (&["resize-pane", "-D", "abc"][..], "adjustment invalid"),
    ] {
        let error = parse_args(args).unwrap_err();
        assert!(
            error.to_string().contains(expected),
            "expected {expected:?} in {error}"
        );
    }
}

#[test]
fn new_window_preserves_target_flags_after_trailing_command_starts() {
    let cli = parse_args(&[
        "new-window",
        "-t",
        "alpha:1",
        "bash",
        "-tc",
        "printf new-window-command",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::NewWindow(args) => {
            assert_eq!(args.target.as_ref().expect("target").to_string(), "alpha:1");
            assert_eq!(
                args.command,
                ["bash", "-tc", "printf new-window-command"].map(str::to_owned)
            );
        }
        _ => panic!("expected NewWindow command"),
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
fn resize_window_accepts_expand_and_shrink_flags() {
    let cli = parse_args(&["resize-window", "-A", "-t", "$1:0"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ResizeWindow(args) => {
            assert!(args.expand);
            assert!(!args.shrink);
            assert_eq!(args.target.as_ref().expect("target").to_string(), "$1:0");
        }
        _ => panic!("expected ResizeWindow command"),
    }

    let cli = parse_args(&["resize-window", "-a", "-t", "$1:0"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::ResizeWindow(args) => {
            assert!(!args.expand);
            assert!(args.shrink);
            assert_eq!(args.target.as_ref().expect("target").to_string(), "$1:0");
        }
        _ => panic!("expected ResizeWindow command"),
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
fn resize_pane_accepts_mouse_and_trim_flags() {
    let cli = parse_args(&["resize-pane", "-M", "-T", "-t", "alpha:0.0"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ResizePane(args) => {
            assert!(args.mouse);
            assert!(args.trim_below);
            assert_eq!(
                args.target.as_ref().expect("target").to_string(),
                "alpha:0.0"
            );
        }
        other => panic!("expected ResizePane command, got {other:?}"),
    }
}

#[test]
fn resize_pane_trim_flag_accepts_size_flags_like_tmux() {
    let cli = parse_args(&["resize-pane", "-T", "-y", "5", "-t", "alpha:0.0"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ResizePane(args) => {
            assert!(args.trim_below);
            assert!(args.rows.is_some());
        }
        other => panic!("expected ResizePane command, got {other:?}"),
    }
}

#[test]
fn resize_pane_rejects_invalid_absolute_size_edges() {
    for (axis, invalid, message) in [
        ("-x", "-5", "width too small"),
        ("-x", "abc", "width invalid"),
        ("-x", "2147483648", "width too large"),
        ("-y", "-5", "height too small"),
        ("-y", "abc", "height invalid"),
        ("-y", "2147483648", "height too large"),
    ] {
        let error = parse_args(&["resize-pane", axis, invalid]).unwrap_err();

        assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
        assert!(
            error.to_string().contains(message),
            "{axis} {invalid} should report {message}, got {error}"
        );
    }
}

#[test]
fn resize_pane_clamps_large_absolute_and_relative_sizes() {
    let cli = parse_args(&["resize-pane", "-x", "0", "-t", "alpha:0.0"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::ResizePane(args) => {
            assert_eq!(args.columns, Some(super::super::ResizePaneSize::Cells(0)));
        }
        other => panic!("expected ResizePane command, got {other:?}"),
    }

    let cli = parse_args(&["resize-pane", "-x", "70000", "-t", "alpha:0.0"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::ResizePane(args) => {
            assert_eq!(
                args.columns,
                Some(super::super::ResizePaneSize::Cells(u16::MAX))
            );
        }
        other => panic!("expected ResizePane command, got {other:?}"),
    }

    let cli = parse_args(&["resize-pane", "-t", "alpha:0.0", "-D", "70000"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::ResizePane(args) => {
            assert_eq!(args.down, Some(u16::MAX));
        }
        other => panic!("expected ResizePane command, got {other:?}"),
    }
}

#[test]
fn resize_pane_rejects_zero_and_overflow_adjustments_with_short_messages() {
    for (value, message) in [
        ("0", "adjustment too small"),
        ("2147483648", "adjustment too large"),
    ] {
        let error = parse_args(&["resize-pane", "-D", value]).unwrap_err();

        assert_eq!(error.kind(), clap::error::ErrorKind::ValueValidation);
        assert!(
            error.to_string().contains(message),
            "-D {value} should report {message}, got {error}"
        );
    }
}

#[test]
fn resize_pane_accepts_percentage_dimensions() {
    let cli = parse_args(&["resize-pane", "-x15%", "-y", "20%", "-t", "alpha:0.0"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ResizePane(args) => {
            assert_eq!(
                args.columns,
                Some(super::super::ResizePaneSize::Percent(15))
            );
            assert_eq!(args.rows, Some(super::super::ResizePaneSize::Percent(20)));
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
fn resize_pane_valueless_relative_before_absolute_composes_like_tmux() {
    let cli = parse_args(&["resize-pane", "-U", "-x", "10"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ResizePane(args) => {
            assert_eq!(args.up, Some(1));
            assert_eq!(args.columns, Some(super::super::ResizePaneSize::Cells(10)));
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
