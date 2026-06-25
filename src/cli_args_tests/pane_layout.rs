use super::*;

#[test]
fn split_window_defaults_to_vertical_direction_when_unspecified() {
    let cli = parse_args(&["split-window", "-t", "alpha"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SplitWindow(args) => {
            assert!(!args.horizontal);
            assert!(!args.vertical);
            assert_eq!(args.direction(), rmux_proto::SplitDirection::Vertical);
        }
        _ => panic!("expected SplitWindow command"),
    }
}

#[test]
fn list_panes_accepts_session_target_and_optional_format() {
    let cli = parse_args(&["list-panes", "-t", "alpha", "-F", "#{pane_id}"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ListPanes(args) => {
            assert_eq!(args.target.expect("session target").to_string(), "alpha");
            assert_eq!(args.format.as_deref(), Some("#{pane_id}"));
            assert!(args.filter.is_none());
            assert!(!args.all_sessions);
            assert!(!args.session_scope);
        }
        _ => panic!("expected ListPanes command"),
    }
}

#[test]
fn list_panes_accepts_tmux_filter_flag() {
    let cli = parse_args(&[
        "list-panes",
        "-t",
        "$1",
        "-f",
        "#{m:%0,#{pane_id}}",
        "-F",
        "#{pane_id}",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ListPanes(args) => {
            assert_eq!(args.target.expect("session target").to_string(), "$1");
            assert_eq!(args.filter.as_deref(), Some("#{m:%0,#{pane_id}}"));
            assert_eq!(args.format.as_deref(), Some("#{pane_id}"));
        }
        _ => panic!("expected ListPanes command"),
    }
}

#[test]
fn list_panes_accepts_all_sessions_and_session_scope_without_a_target() {
    let cli = parse_args(&["list-panes", "-a", "-s"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ListPanes(args) => {
            assert!(args.all_sessions);
            assert!(args.session_scope);
            assert!(args.target.is_none());
            assert!(args.format.is_none());
            assert!(args.filter.is_none());
        }
        _ => panic!("expected ListPanes command"),
    }
}

#[test]
fn split_window_accepts_horizontal_direction() {
    let cli = parse_args(&["split-window", "-h", "-t", "alpha"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SplitWindow(args) => {
            assert!(args.horizontal);
            assert!(!args.vertical);
            assert_eq!(args.direction(), rmux_proto::SplitDirection::Horizontal);
        }
        _ => panic!("expected SplitWindow command"),
    }
}

#[test]
fn split_window_direction_flags_follow_tmux_last_wins() {
    let vertical = parse_args(&["split-window", "-h", "-v", "-t", "alpha"]).unwrap();
    match vertical.command.expect("parsed command") {
        super::super::Command::SplitWindow(args) => {
            assert!(matches!(
                args.direction(),
                rmux_proto::SplitDirection::Vertical
            ));
        }
        _ => panic!("expected SplitWindow command"),
    }

    let horizontal = parse_args(&["split-window", "-v", "-h", "-t", "alpha"]).unwrap();
    match horizontal.command.expect("parsed command") {
        super::super::Command::SplitWindow(args) => {
            assert!(matches!(
                args.direction(),
                rmux_proto::SplitDirection::Horizontal
            ));
        }
        _ => panic!("expected SplitWindow command"),
    }
}

#[test]
fn split_window_attached_cluster_values_preserve_target_and_size() {
    let cli = parse_args(&["split-window", "-htalpha:0", "sleep", "1"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SplitWindow(args) => {
            assert_eq!(args.direction(), rmux_proto::SplitDirection::Horizontal);
            assert_eq!(args.target.as_ref().expect("target").to_string(), "alpha:0");
            assert_eq!(args.command, ["sleep".to_owned(), "1".to_owned()]);
        }
        _ => panic!("expected SplitWindow command"),
    }

    let cells = parse_args(&["split-window", "-hl10", "-t", "alpha"]).unwrap();
    match cells.command.expect("parsed command") {
        super::super::Command::SplitWindow(args) => {
            assert_eq!(args.direction(), rmux_proto::SplitDirection::Horizontal);
            assert_eq!(args.size_spec().as_deref(), Some("10"));
        }
        _ => panic!("expected SplitWindow command"),
    }

    let percentage = parse_args(&["split-window", "-hp50", "-t", "alpha"]).unwrap();
    match percentage.command.expect("parsed command") {
        super::super::Command::SplitWindow(args) => {
            assert_eq!(args.direction(), rmux_proto::SplitDirection::Horizontal);
            assert_eq!(args.size_spec().as_deref(), Some("50%"));
        }
        _ => panic!("expected SplitWindow command"),
    }
}

#[test]
fn split_window_size_flags_follow_tmux_last_wins() {
    let percentage = parse_args(&["split-window", "-l", "5", "-p", "50", "-t", "alpha"]).unwrap();
    match percentage.command.expect("parsed command") {
        super::super::Command::SplitWindow(args) => {
            assert_eq!(args.size_spec().as_deref(), Some("50%"));
        }
        _ => panic!("expected SplitWindow command"),
    }

    let cells = parse_args(&["split-window", "-p", "50", "-l", "5", "-t", "alpha"]).unwrap();
    match cells.command.expect("parsed command") {
        super::super::Command::SplitWindow(args) => {
            assert_eq!(args.size_spec().as_deref(), Some("5"));
        }
        _ => panic!("expected SplitWindow command"),
    }
}

#[test]
fn split_window_accepts_trailing_command_argv() {
    let cli = parse_args(&[
        "split-window",
        "-h",
        "-t",
        "alpha",
        "sh",
        "-c",
        "printf split-command",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SplitWindow(args) => {
            assert!(args.horizontal);
            assert_eq!(
                args.command,
                ["sh", "-c", "printf split-command"].map(str::to_owned)
            );
        }
        _ => panic!("expected SplitWindow command"),
    }
}

#[test]
fn split_window_preserves_glued_flags_after_trailing_command_starts() {
    let cli = parse_args(&[
        "split-window",
        "-t",
        "alpha",
        "bash",
        "-lc",
        "printf split-command",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SplitWindow(args) => {
            assert_eq!(
                args.command,
                ["bash", "-lc", "printf split-command"].map(str::to_owned)
            );
        }
        _ => panic!("expected SplitWindow command"),
    }
}

#[test]
fn split_window_allows_trailing_command_l_flag() {
    for argv in [
        &["split-window", "-d", "ls", "-l"][..],
        &["split-window", "-d", "--", "ls", "-l"][..],
    ] {
        let cli = parse_args(argv).unwrap();

        match cli.command.expect("parsed command") {
            super::super::Command::SplitWindow(args) => {
                assert!(args.detached);
                assert_eq!(args.command, ["ls", "-l"].map(str::to_owned));
            }
            _ => panic!("expected SplitWindow command"),
        }
    }
}

#[test]
fn split_window_accepts_start_directory_before_trailing_command() {
    let cli = parse_args(&[
        "split-window",
        "-h",
        "-c",
        "/tmp/work",
        "-t",
        "alpha",
        "sh",
        "-c",
        "pwd",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SplitWindow(args) => {
            assert!(args.horizontal);
            assert_eq!(
                args.start_directory.as_deref(),
                Some(std::path::Path::new("/tmp/work"))
            );
            assert_eq!(args.command, ["sh", "-c", "pwd"].map(str::to_owned));
        }
        _ => panic!("expected SplitWindow command"),
    }
}

#[test]
fn split_window_accepts_tmux_compat_flags_before_command() {
    let cli = parse_args(&[
        "split-window",
        "-b",
        "-d",
        "-Z",
        "-l",
        "12",
        "-t",
        "alpha:0.0",
        "sh",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SplitWindow(args) => {
            assert!(args.before);
            assert!(args.detached);
            assert!(args.preserve_zoom);
            assert_eq!(args.size.as_deref(), Some("12"));
            assert_eq!(args.command, vec!["sh".to_owned()]);
        }
        _ => panic!("expected SplitWindow command"),
    }
}

#[test]
fn split_window_accepts_percentage_size() {
    let cli = parse_args(&["split-window", "-p25", "-f", "-t", "alpha:0.0"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SplitWindow(args) => {
            assert!(args.full_size);
            assert_eq!(args.percentage, Some(25));
            assert_eq!(args.size_spec().as_deref(), Some("25%"));
        }
        _ => panic!("expected SplitWindow command"),
    }
}

#[test]
fn join_pane_accepts_percentage_size() {
    let cli = parse_args(&[
        "join-pane",
        "-p",
        "50",
        "-s",
        "alpha:0.1",
        "-t",
        "alpha:0.0",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::JoinPane(args) => {
            assert_eq!(args.percentage, Some(50));
            assert_eq!(args.size_spec().as_deref(), Some("50%"));
        }
        _ => panic!("expected JoinPane command"),
    }
}

#[test]
fn split_window_accepts_full_size_flag() {
    let cli = parse_args(&["split-window", "-f", "-t", "alpha:0.0"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SplitWindow(args) => {
            assert_eq!(args.percentage, None);
            assert_eq!(args.size_spec(), None);
            assert!(args.full_size);
            assert_eq!(
                args.target.as_ref().expect("target").to_string(),
                "alpha:0.0"
            );
        }
        _ => panic!("expected SplitWindow command"),
    }
}

#[test]
fn split_window_rejects_unknown_flag_before_trailing_command() {
    let error = parse_args(&["split-window", "-Q", "printf ok"]).unwrap_err();

    assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    assert!(error
        .to_string()
        .contains("command split-window: unknown flag -Q"));
}

#[test]
fn split_window_preserves_hyphenated_values_after_trailing_command_starts() {
    let cli = parse_args(&["split-window", "env", "-Q", "value"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SplitWindow(args) => {
            assert_eq!(args.command, ["env", "-Q", "value"].map(str::to_owned));
        }
        _ => panic!("expected SplitWindow command"),
    }
}

#[test]
fn split_window_parses_stdin_flag_without_treating_it_as_command() {
    let cli = parse_args(&["split-window", "-I", "-t", "alpha:0.0"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SplitWindow(args) => {
            assert!(args.stdin);
            assert!(args.command.is_empty());
        }
        _ => panic!("expected SplitWindow command"),
    }
}

#[test]
fn swap_pane_accepts_relative_direction_without_a_source() {
    let cli = parse_args(&["swap-pane", "-D", "-t", "alpha:2.3"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SwapPane(args) => {
            assert!(args.down);
            assert!(!args.up);
            assert!(args.source.is_none());
            assert!(args.uses_relative_target());
            assert_eq!(
                args.target.as_ref().expect("target").to_string(),
                "alpha:2.3"
            );
        }
        _ => panic!("expected SwapPane command"),
    }
}

#[test]
fn swap_pane_accepts_explicit_source_and_target_panes() {
    let cli = parse_args(&["swap-pane", "-s", "alpha:0.1", "-t", "beta:3.2", "-d"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SwapPane(args) => {
            assert!(args.detached);
            assert_eq!(
                args.source.as_ref().expect("source exists").to_string(),
                "alpha:0.1"
            );
            assert_eq!(
                args.target.as_ref().expect("target").to_string(),
                "beta:3.2"
            );
            assert!(!args.uses_relative_target());
        }
        _ => panic!("expected SwapPane command"),
    }
}

#[test]
fn swap_pane_accepts_zoom_preservation_flag() {
    let cli = parse_args(&["swap-pane", "-Z", "-s", "alpha:0.1", "-t", "beta:3.2"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SwapPane(args) => {
            assert!(args.preserve_zoom);
            assert_eq!(
                args.source.as_ref().expect("source exists").to_string(),
                "alpha:0.1"
            );
            assert_eq!(
                args.target.as_ref().expect("target").to_string(),
                "beta:3.2"
            );
        }
        _ => panic!("expected SwapPane command"),
    }
}

#[test]
fn join_pane_defaults_to_vertical_direction() {
    let cli = parse_args(&["join-pane", "-s", "alpha:0.1", "-t", "alpha:1.0"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::JoinPane(args) => {
            assert_eq!(
                args.source.as_ref().expect("source exists").to_string(),
                "alpha:0.1"
            );
            assert_eq!(target_text(&args.target), "alpha:1.0");
            assert_eq!(args.direction(), rmux_proto::SplitDirection::Vertical);
        }
        _ => panic!("expected JoinPane command"),
    }
}

#[test]
fn join_pane_accepts_implicit_marked_source() {
    let cli = parse_args(&["join-pane", "-t", "alpha:1.0"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::JoinPane(args) => {
            assert!(args.source.is_none());
            assert_eq!(target_text(&args.target), "alpha:1.0");
        }
        _ => panic!("expected JoinPane command"),
    }
}

#[test]
fn join_pane_accepts_before_full_size_and_percentage_length_flags() {
    let cli = parse_args(&[
        "join-pane",
        "-b",
        "-f",
        "-l",
        "30%",
        "-s",
        "alpha:0.1",
        "-t",
        "alpha:1.0",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::JoinPane(args) => {
            assert!(args.before);
            assert!(args.full_size);
            assert_eq!(args.size_spec().as_deref(), Some("30%"));
        }
        _ => panic!("expected JoinPane command"),
    }
}

#[test]
fn move_pane_parses_the_full_join_pane_flag_surface() {
    let cli = parse_args(&[
        "move-pane",
        "-b",
        "-d",
        "-f",
        "-h",
        "-l",
        "12",
        "-s",
        "alpha:0.1",
        "-t",
        "beta:1.2",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::MovePane(args) => {
            assert!(args.before);
            assert!(args.detached);
            assert!(args.full_size);
            assert_eq!(args.direction(), rmux_proto::SplitDirection::Horizontal);
            assert_eq!(args.size_spec().as_deref(), Some("12"));
            assert_eq!(
                args.source.as_ref().expect("source exists").to_string(),
                "alpha:0.1"
            );
            assert_eq!(target_text(&args.target), "beta:1.2");
        }
        _ => panic!("expected MovePane command"),
    }
}

#[test]
fn move_pane_accepts_percentage_size() {
    let cli = parse_args(&["move-pane", "-p", "35", "-s", "alpha:0.1", "-t", "beta:1.2"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::MovePane(args) => {
            assert_eq!(args.percentage, Some(35));
            assert_eq!(args.size_spec().as_deref(), Some("35%"));
        }
        _ => panic!("expected MovePane command"),
    }
}

#[test]
fn break_pane_accepts_optional_target_and_name() {
    let cli = parse_args(&[
        "break-pane",
        "-s",
        "alpha:1.2",
        "-t",
        "beta:4",
        "-n",
        "logs",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::BreakPane(args) => {
            assert_eq!(
                args.source.as_ref().expect("source exists").to_string(),
                "alpha:1.2"
            );
            assert_eq!(args.target.expect("target exists").to_string(), "beta:4");
            assert_eq!(args.name.as_deref(), Some("logs"));
        }
        _ => panic!("expected BreakPane command"),
    }
}

#[test]
fn break_pane_accepts_placement_and_print_flags() {
    let cli = parse_args(&[
        "break-pane",
        "-a",
        "-P",
        "-F",
        "#{window_index}.#{pane_index}",
        "-s",
        "alpha:1.2",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::BreakPane(args) => {
            assert!(args.after);
            assert!(!args.before);
            assert!(args.print_target);
            assert_eq!(
                args.format.as_deref(),
                Some("#{window_index}.#{pane_index}")
            );
        }
        _ => panic!("expected BreakPane command"),
    }
}
