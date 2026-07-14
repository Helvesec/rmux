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
fn runtime_canonical_queue_keeps_every_group_on_the_typed_path() {
    let cli = super::super::parse_with_runtime_command_groups(
        [
            "rmux",
            "list-sessions",
            ";",
            "display-message",
            "-p",
            "unalias",
            ";",
            "ls",
        ],
        &[
            super::super::RuntimeCommandGroup::Canonical("display-message -p canonical".to_owned()),
            super::super::RuntimeCommandGroup::Canonical(
                "display-message -p unalias ; display-message -p short".to_owned(),
            ),
        ],
    )
    .unwrap();
    let commands = cli.into_command_queue();

    assert!(matches!(
        &commands[0],
        super::super::Command::DisplayMessage(args)
            if args.message == ["canonical".to_owned()]
    ));
    assert!(matches!(
        &commands[1],
        super::super::Command::DisplayMessage(_)
    ));
    assert!(matches!(
        &commands[2],
        super::super::Command::DisplayMessage(args)
            if args.message == ["short".to_owned()]
    ));
}

#[test]
fn runtime_canonical_queue_preserves_reparse_escaped_literals() {
    let literal = "space ; dollar $HOME slash\\ quote' double\"";
    let rendered = rmux_core::command_parser::CommandArgument::String(literal.to_owned())
        .to_tmux_reparse_string();
    let cli = super::super::parse_with_runtime_command_groups(
        ["rmux", "list-sessions", literal],
        &[super::super::RuntimeCommandGroup::Canonical(format!(
            "display-message -p -- {rendered}"
        ))],
    )
    .unwrap();
    let commands = cli.into_command_queue();

    assert!(matches!(
        &commands[0],
        super::super::Command::DisplayMessage(args)
            if args.message == [literal.to_owned()]
    ));
}

#[test]
fn runtime_canonical_queue_preserves_server_expanded_control_characters() {
    let expected = "line1\nline2\rline3\t$slash\\quote\"`";
    let rendered = rmux_core::command_parser::CommandArgument::String(expected.to_owned())
        .to_tmux_reparse_string();
    let cli = super::super::parse_with_runtime_command_groups(
        ["rmux", "list-sessions"],
        &[super::super::RuntimeCommandGroup::Canonical(format!(
            "display-message -p {rendered}"
        ))],
    )
    .unwrap();
    let commands = cli.into_command_queue();

    assert!(matches!(
        &commands[0],
        super::super::Command::DisplayMessage(args)
            if args.message == [expected.to_owned()]
    ));
}

#[test]
fn runtime_canonical_extension_preserves_trailing_semicolon_literals() {
    let cli = super::super::parse_with_runtime_command_groups(
        [
            "rmux",
            "list-sessions",
            ";",
            "display-message",
            "-p",
            "semi\\;",
        ],
        &[
            super::super::RuntimeCommandGroup::Canonical("display-message -p alias".to_owned()),
            super::super::RuntimeCommandGroup::Canonical(
                "find-sessions --name \"semi\\;\"".to_owned(),
            ),
        ],
    )
    .unwrap();
    let commands = cli.into_command_queue();

    assert!(matches!(
        &commands[1],
        super::super::Command::FindSessions(args)
            if args.name.as_deref() == Some("semi;")
    ));
}

#[test]
fn runtime_alias_assignments_are_applied_before_the_validated_command() {
    let cli = super::super::parse_with_runtime_command_groups(
        ["rmux", "zz"],
        &[super::super::RuntimeCommandGroup::Canonical(
            "FOO=bar ; display-message -p bar".to_owned(),
        )],
    )
    .expect("runtime alias assignment parses");
    assert!(matches!(
        cli.command,
        Some(super::super::Command::DisplayMessage(_))
    ));

    let commands = cli.into_command_queue();
    assert!(matches!(
        &commands[0],
        super::super::Command::ApplyParseTimeAssignments(assignments)
            if assignments == "FOO=bar"
    ));
    assert!(matches!(
        &commands[1],
        super::super::Command::DisplayMessage(_)
    ));
}

#[test]
fn invalid_runtime_alias_tail_is_rejected_before_assignments_can_dispatch() {
    let error = super::super::parse_with_runtime_command_groups(
        ["rmux", "zz"],
        &[super::super::RuntimeCommandGroup::Canonical(
            "FOO=bar ; new-window -Q".to_owned(),
        )],
    )
    .expect_err("invalid canonical tail must fail typed validation");

    let message = error.to_string();
    assert!(message.contains("new-window"), "{message}");
    assert!(message.contains("-Q"), "{message}");
}

#[test]
fn runtime_command_groups_use_the_preparsed_snapshot() {
    let cli = super::super::parse_with_runtime_command_groups(
        [
            "rmux",
            "set-option",
            "-s",
            "command-alias[0]",
            "list-sessions=display-message -p new",
            ";",
            "list-sessions",
        ],
        &[super::super::RuntimeCommandGroup::Canonical(
            "set-option -s command-alias[0] 'list-sessions=display-message -p new' ; display-message -p old"
                .to_owned(),
        )],
    )
    .unwrap();
    let commands = cli.into_command_queue();

    assert!(matches!(&commands[0], super::super::Command::SetOption(_)));
    assert!(matches!(
        &commands[1],
        super::super::Command::DisplayMessage(args)
            if args.message == ["old".to_owned()]
    ));
}

#[test]
fn runtime_canonical_reparse_does_not_apply_builtin_aliases_twice() {
    let error = super::super::parse_with_runtime_command_groups(
        ["rmux", "split-pane", "-d", "-t", "alpha:0.0"],
        &[super::super::RuntimeCommandGroup::Canonical(
            "split-pane -d -t alpha:0.0".to_owned(),
        )],
    )
    .unwrap_err();
    assert!(error.to_string().contains("unknown command: split-pane"));

    let cli = super::super::parse(["rmux", "split-pane", "-d", "-t", "alpha:0.0"])
        .expect("an absent server keeps built-in defaults");
    assert!(matches!(
        cli.command,
        Some(super::super::Command::SplitWindow(_))
    ));
}

#[test]
fn runtime_canonical_terminal_commands_stay_on_typed_dispatch_paths() {
    let attach = super::super::parse_with_runtime_command_groups(
        ["rmux", "list-sessions"],
        &[super::super::RuntimeCommandGroup::Canonical(
            "attach-session -t alpha".to_owned(),
        )],
    )
    .unwrap();
    assert!(matches!(
        attach.command,
        Some(super::super::Command::AttachSession(_))
    ));

    let kill = super::super::parse_with_runtime_command_groups(
        ["rmux", "list-sessions"],
        &[super::super::RuntimeCommandGroup::Canonical(
            "kill-server".to_owned(),
        )],
    )
    .unwrap();
    assert!(matches!(
        kill.command,
        Some(super::super::Command::KillServer)
    ));
}

#[test]
fn runtime_canonical_reparse_rejects_an_unexpanded_alias_name() {
    let error = super::super::parse_with_runtime_command_groups(
        ["rmux", "foo"],
        &[super::super::RuntimeCommandGroup::Canonical(
            "bar".to_owned(),
        )],
    )
    .unwrap_err();
    assert!(error.to_string().contains("unknown command: bar"));
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
fn swap_window_compact_flags_stop_at_the_first_value_flag_like_tmux() {
    let detached = parse_args(&["swap-window", "-ds", "alpha:0", "-tbeta:1"]).unwrap();
    match detached.command.expect("parsed command") {
        super::super::Command::SwapWindow(args) => {
            assert!(args.detached);
            assert_eq!(args.source.as_ref().expect("source").to_string(), "alpha:0");
            assert_eq!(args.target.as_ref().expect("target").to_string(), "beta:1");
        }
        _ => panic!("expected SwapWindow command"),
    }

    let source_suffix = parse_args(&["swap-window", "-sd", "-t", "alpha:1"]).unwrap();
    match source_suffix.command.expect("parsed command") {
        super::super::Command::SwapWindow(args) => {
            assert!(!args.detached, "-sd is -s d, not -s plus -d");
            assert_eq!(args.source.as_ref().expect("source").to_string(), "d");
            assert_eq!(args.target.as_ref().expect("target").to_string(), "alpha:1");
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
