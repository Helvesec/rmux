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
            assert!(!args.all_sessions);
            assert!(!args.short_format);
        }
        _ => panic!("expected ListPanes command"),
    }
}

#[test]
fn list_panes_accepts_all_sessions_and_short_output_without_a_target() {
    let cli = parse_args(&["list-panes", "-a", "-s"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ListPanes(args) => {
            assert!(args.all_sessions);
            assert!(args.short_format);
            assert!(args.target.is_none());
            assert!(args.format.is_none());
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
            assert_eq!(args.source.to_string(), "alpha:0.1");
            assert_eq!(target_text(&args.target), "alpha:1.0");
            assert_eq!(args.direction(), rmux_proto::SplitDirection::Vertical);
        }
        _ => panic!("expected JoinPane command"),
    }
}

#[test]
fn join_pane_accepts_before_full_size_and_percentage_size_flags() {
    let cli = parse_args(&[
        "join-pane",
        "-b",
        "-f",
        "-p",
        "30",
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
            assert_eq!(args.source.to_string(), "alpha:0.1");
            assert_eq!(target_text(&args.target), "beta:1.2");
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
            assert_eq!(args.source.to_string(), "alpha:1.2");
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
