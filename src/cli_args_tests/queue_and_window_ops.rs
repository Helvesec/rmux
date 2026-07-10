use super::*;

#[test]
fn argv_semicolons_build_an_ordered_command_queue() {
    let cli = parse_args(&["list-sessions;", "display-message", "-p", "ok"]).unwrap();
    let commands = cli.into_command_queue();

    assert_eq!(commands.len(), 2);
    assert!(matches!(
        &commands[0],
        super::super::Command::ListSessions(_)
    ));
    assert!(matches!(
        &commands[1],
        super::super::Command::DisplayMessage(_)
    ));
}

#[test]
fn standalone_argv_semicolon_builds_an_ordered_command_queue() {
    let cli = parse_args(&["attach-session", "-t", "alpha", ";", "detach-client"]).unwrap();
    let commands = cli.into_command_queue();

    assert_eq!(commands.len(), 2);
    assert!(matches!(
        &commands[0],
        super::super::Command::AttachSession(_)
    ));
    assert!(matches!(
        &commands[1],
        super::super::Command::DetachClient(_)
    ));
}

#[test]
fn target_client_and_hook_flags_survive_argv_queue_parsing() {
    let cli = parse_args(&[
        "load-buffer",
        "-wt",
        "/dev/pts/10",
        "/tmp/input",
        ";",
        "show-options",
        "-gH",
    ])
    .unwrap();
    let commands = cli.into_command_queue();

    match &commands[0] {
        super::super::Command::LoadBuffer(args) => {
            assert!(args.set_clipboard);
            assert_eq!(args.target_client.as_deref(), Some("/dev/pts/10"));
        }
        other => panic!("expected load-buffer, got {other:?}"),
    }
    match &commands[1] {
        super::super::Command::ShowOptions(args) => assert!(args.include_hooks),
        other => panic!("expected show-options, got {other:?}"),
    }
}

#[test]
fn trailing_semicolon_after_send_keys_payload_is_a_queue_separator() {
    let error = parse_args(&["send-keys", "-t", "alpha:0.0", "xyz;", "final"]).unwrap_err();
    assert!(
        error.to_string().contains("unknown command: final"),
        "{error}"
    );
}

#[test]
fn bare_semicolon_builds_a_noop_command_queue() {
    let cli = parse_args(&[";"]).unwrap();
    let commands = cli.into_command_queue();

    assert_eq!(commands.len(), 1);
    assert!(matches!(&commands[0], super::super::Command::Noop));
}

#[test]
fn list_keys_accepts_tmux_sort_format_and_reverse_flags() {
    let cli = parse_args(&["list-keys", "-r", "-F", "#{key_table}", "-Okey"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ListKeys(args) => {
            assert!(args.reversed);
            assert_eq!(args.format.as_deref(), Some("#{key_table}"));
            assert_eq!(args.sort_order.as_deref(), Some("key"));
        }
        _ => panic!("expected ListKeys command"),
    }
}

#[cfg(unix)]
#[test]
fn command_arguments_reject_invalid_utf8_without_lossy_replacement() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;

    let error = parse(vec![
        OsString::from("rmux"),
        OsString::from("display-message"),
        OsString::from_vec(vec![0xff]),
    ])
    .unwrap_err();

    assert_eq!(error.kind(), clap::error::ErrorKind::InvalidUtf8);
    assert!(error.to_string().contains("invalid UTF-8"));
}

#[test]
fn move_window_accepts_reindex_with_a_session_target() {
    let cli = parse_args(&["move-window", "-r", "-t", "alpha"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::MoveWindow(args) => {
            assert!(args.reindex);
            assert_eq!(args.source, None);
            assert_eq!(
                args.target.as_ref().expect("target exists").to_string(),
                "alpha"
            );
        }
        _ => panic!("expected MoveWindow command"),
    }
}

#[test]
fn move_window_accepts_reindex_without_a_target() {
    let cli = parse_args(&["move-window", "-r"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::MoveWindow(args) => {
            assert!(args.reindex);
            assert_eq!(args.source, None);
            assert_eq!(args.target, None);
        }
        _ => panic!("expected MoveWindow command"),
    }
}

#[test]
fn move_window_accepts_reindex_with_source_for_tmux_compatibility() {
    let cli = parse_args(&["move-window", "-r", "-s", "$1:2", "-t", "$1"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::MoveWindow(args) => {
            assert!(args.reindex);
            assert_eq!(args.source.as_ref().expect("source").to_string(), "$1:2");
            assert_eq!(args.target.as_ref().expect("target").to_string(), "$1");
        }
        _ => panic!("expected MoveWindow command"),
    }
}

#[test]
fn move_window_accepts_source_destination_and_kill_flags() {
    let cli = parse_args(&["move-window", "-k", "-d", "-s", "alpha:2", "-t", "beta:5"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::MoveWindow(args) => {
            assert!(!args.reindex);
            assert!(args.kill_target);
            assert!(args.detached);
            assert_eq!(args.source.expect("source exists").to_string(), "alpha:2");
            assert_eq!(
                args.target.as_ref().expect("target exists").to_string(),
                "beta:5"
            );
        }
        _ => panic!("expected MoveWindow command"),
    }
}

#[test]
fn move_window_placement_flags_follow_tmux_priority() {
    for argv in [
        ["move-window", "-a", "-b", "-s", "alpha:2", "-t", "beta:5"],
        ["move-window", "-b", "-a", "-s", "alpha:2", "-t", "beta:5"],
    ] {
        let cli = parse_args(&argv).unwrap();
        match cli.command.expect("parsed command") {
            super::super::Command::MoveWindow(args) => {
                assert!(!args.after);
                assert!(args.before);
            }
            _ => panic!("expected MoveWindow command"),
        }
    }
}

#[test]
fn move_window_accepts_implicit_source_and_relative_destination() {
    let cli = parse_args(&["move-window", "-t", "-1"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::MoveWindow(args) => {
            assert!(!args.reindex);
            assert!(args.source.is_none());
            assert_eq!(args.target.as_ref().expect("target").to_string(), "-1");
        }
        _ => panic!("expected MoveWindow command"),
    }
}

#[test]
fn move_window_accepts_position_flags_and_id_targets() {
    let cli = parse_args(&["move-window", "-a", "-s", "@1", "-t", "$2:0"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::MoveWindow(args) => {
            assert!(args.after);
            assert!(!args.before);
            assert_eq!(args.source.as_ref().expect("source").to_string(), "@1");
            assert_eq!(args.target.as_ref().expect("target").to_string(), "$2:0");
        }
        _ => panic!("expected MoveWindow command"),
    }

    let cli = parse_args(&["move-window", "-b", "-s", "alpha:1", "-t", "$2:0"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::MoveWindow(args) => {
            assert!(!args.after);
            assert!(args.before);
            assert_eq!(args.source.as_ref().expect("source").to_string(), "alpha:1");
            assert_eq!(args.target.as_ref().expect("target").to_string(), "$2:0");
        }
        _ => panic!("expected MoveWindow command"),
    }
}

#[test]
fn swap_window_preserves_session_targets_for_runtime_resolution() {
    let cli = parse_args(&["swap-window", "-s", "alpha", "-t", "beta:1"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SwapWindow(args) => {
            assert_eq!(
                args.source.as_ref().expect("source exists").to_string(),
                "alpha"
            );
            assert_eq!(args.target.as_ref().expect("target").to_string(), "beta:1");
        }
        _ => panic!("expected SwapWindow command"),
    }
}

#[test]
fn swap_window_accepts_implicit_source() {
    let cli = parse_args(&["swap-window", "-t", "beta:1"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SwapWindow(args) => {
            assert!(args.source.is_none());
            assert_eq!(args.target.as_ref().expect("target").to_string(), "beta:1");
        }
        _ => panic!("expected SwapWindow command"),
    }
}

#[test]
fn rotate_window_defaults_to_up_direction() {
    let cli = parse_args(&["rotate-window", "-t", "alpha:2"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::RotateWindow(args) => {
            assert_eq!(args.target.as_ref().expect("target").to_string(), "alpha:2");
            assert_eq!(args.direction(), rmux_proto::RotateWindowDirection::Up);
        }
        _ => panic!("expected RotateWindow command"),
    }
}

#[test]
fn rotate_window_accepts_zoom_restore_flag() {
    let cli = parse_args(&["rotate-window", "-Z", "-t", "alpha:2"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::RotateWindow(args) => {
            assert_eq!(args.target.as_ref().expect("target").to_string(), "alpha:2");
            assert!(args.restore_zoom);
        }
        _ => panic!("expected RotateWindow command"),
    }
}

#[test]
fn rotate_window_rejects_both_directions() {
    let error = parse_args(&["rotate-window", "-D", "-U", "-t", "alpha:2"]).unwrap_err();
    assert_eq!(error.kind(), clap::error::ErrorKind::ArgumentConflict);
}

#[test]
fn next_window_accepts_alert_navigation_flag() {
    let cli = parse_args(&["next-window", "-a", "-t", "alpha"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::NextWindow(args) => {
            assert!(args.alerts_only);
            assert_eq!(args.target.expect("target exists").to_string(), "alpha");
        }
        _ => panic!("expected NextWindow command"),
    }
}

#[test]
fn show_messages_accepts_tmux_flags() {
    let cli = parse_args(&["show-messages", "-J", "-T", "-t", "="]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ShowMessages(args) => {
            assert!(args.jobs);
            assert!(args.terminals);
            assert_eq!(args.target_client.as_deref(), Some("="));
        }
        _ => panic!("expected ShowMessages command"),
    }
}
