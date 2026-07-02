use super::*;

#[test]
fn set_option_accepts_default_scope_like_tmux() {
    let cli = parse_args(&["set-option", "status", "off"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SetOption(args) => {
            assert!(!args.global);
            assert!(!args.server);
            assert!(!args.window);
            assert!(!args.pane);
            assert_eq!(args.target, None);
            assert_eq!(args.option, "status");
            assert_eq!(args.value.as_deref(), Some("off"));
        }
        _ => panic!("expected SetOption command"),
    }
}

#[test]
fn set_option_accepts_global_and_target_for_server_scope_compatibility() {
    let cli = parse_args(&["set-option", "-gs", "-t", "alpha", "buffer-limit", "10"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SetOption(args) => {
            assert!(args.global);
            assert!(args.server);
            assert_eq!(
                args.target
                    .as_ref()
                    .and_then(|target| target.exact().cloned()),
                Some(rmux_proto::Target::Session(
                    rmux_proto::SessionName::new("alpha").unwrap()
                ))
            );
        }
        _ => panic!("expected SetOption command"),
    }
}

#[test]
fn set_option_accepts_trailing_colon_session_targets_like_tmux() {
    let cli = parse_args(&["set-option", "-t", "alpha:", "status-left", "LEFT"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SetOption(args) => {
            assert_eq!(target_text(&args.target), "alpha:");
        }
        _ => panic!("expected SetOption command"),
    }
}

#[test]
fn set_option_accepts_combined_append_and_server_flags() {
    let cli = parse_args(&[
        "set-option",
        "-as",
        "terminal-features",
        "xterm-256color:RGB",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SetOption(args) => {
            assert!(args.append);
            assert!(args.server);
            assert_eq!(args.target, None);
        }
        _ => panic!("expected SetOption command"),
    }
}

#[test]
fn set_option_accepts_separate_append_and_server_flags() {
    let cli = parse_args(&[
        "set-option",
        "-a",
        "-s",
        "terminal-features",
        "xterm-256color:RGB",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SetOption(args) => {
            assert!(args.append);
            assert!(args.server);
            assert_eq!(args.target, None);
        }
        _ => panic!("expected SetOption command"),
    }
}

#[test]
fn set_option_accepts_window_scope_and_optional_value() {
    let cli = parse_args(&["set-option", "-w", "-t", "alpha:2.3", "synchronize-panes"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SetOption(args) => {
            assert!(args.window);
            assert_eq!(
                args.target
                    .as_ref()
                    .and_then(|target| target.exact().cloned()),
                Some(rmux_proto::Target::Pane(
                    rmux_proto::PaneTarget::with_window(
                        rmux_proto::SessionName::new("alpha").unwrap(),
                        2,
                        3,
                    )
                ))
            );
            assert_eq!(args.value, None);
        }
        _ => panic!("expected SetOption command"),
    }
}

#[test]
fn set_option_rejects_dash_dash_before_value() {
    let error = parse_args(&["set-option", "-g", "status-left", "--", "-abc"]).unwrap_err();
    assert!(
        error
            .to_string()
            .contains("command set-option: too many arguments (need at most 2)"),
        "{error}"
    );
}

#[test]
fn set_option_accepts_dash_dash_before_option_name() {
    let cli = parse_args(&["set-option", "-g", "--", "status-left", "plain"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SetOption(args) => {
            assert!(args.global);
            assert_eq!(args.option, "status-left");
            assert_eq!(args.value.as_deref(), Some("plain"));
        }
        _ => panic!("expected SetOption command"),
    }
}

#[test]
fn set_option_accepts_trailing_dash_dash_as_value() {
    let cli = parse_args(&["set-option", "-g", "status-left", "--"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SetOption(args) => {
            assert!(args.global);
            assert_eq!(args.option, "status-left");
            assert_eq!(args.value.as_deref(), Some("--"));
        }
        _ => panic!("expected SetOption command"),
    }
}

#[test]
fn set_window_option_parses_as_a_distinct_public_command() {
    let cli = parse_args(&["set-window-option", "-g", "pane-border-style", "fg=colour1"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SetWindowOption(args) => {
            assert!(args.global);
            assert_eq!(args.option, "pane-border-style");
            assert_eq!(args.value.as_deref(), Some("fg=colour1"));
        }
        _ => panic!("expected SetWindowOption command"),
    }
}

#[test]
fn show_window_options_parses_as_a_distinct_public_command() {
    let cli = parse_args(&[
        "show-window-options",
        "-v",
        "-t",
        "alpha:2.3",
        "pane-border-style",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::ShowWindowOptions(args) => {
            assert!(args.value_only);
            assert_eq!(args.name.as_deref(), Some("pane-border-style"));
            assert_eq!(
                args.target
                    .as_ref()
                    .and_then(|target| target.exact().cloned()),
                Some(rmux_proto::Target::Pane(
                    rmux_proto::PaneTarget::with_window(
                        rmux_proto::SessionName::new("alpha").unwrap(),
                        2,
                        3,
                    )
                ))
            );
        }
        _ => panic!("expected ShowWindowOptions command"),
    }
}

#[test]
fn show_window_options_rejects_server_only_include_inherited_flag() {
    let error = parse_args(&["show-window-options", "-A"]).unwrap_err();

    assert!(
        error
            .to_string()
            .contains("command show-window-options: unknown flag -A"),
        "{error}"
    );
}

#[test]
fn set_environment_accepts_default_global_and_session_scope() {
    parse_args(&["set-environment", "TERM", "screen"]).expect("default set-environment parses");
    parse_args(&["set-environment", "-g", "TERM", "screen"])
        .expect("global set-environment parses");
    parse_args(&["set-environment", "-t", "alpha", "TERM", "screen"])
        .expect("session set-environment parses");
}

#[test]
fn set_hook_accepts_current_pane_and_window_scopes_without_target() {
    for flag in ["-p", "-w"] {
        let cli = parse_args(&["set-hook", flag, "pane-died", "display-message hi"]).unwrap();

        match cli.command.expect("parsed command") {
            super::super::Command::SetHook(args) => {
                assert_eq!(args.pane, flag == "-p");
                assert_eq!(args.window, flag == "-w");
                assert!(args.target.is_none());
            }
            _ => panic!("expected SetHook command"),
        }
    }
}

#[test]
fn set_hook_unknown_hook_uses_tmux_error_text() {
    let error = parse_args(&["set-hook", "-g", "no-such-hook", "display hi"]).unwrap_err();

    assert!(
        error.to_string().contains("invalid option: no-such-hook"),
        "{error}"
    );
}

#[test]
fn detach_client_rejects_trailing_arguments() {
    let error = parse_args(&["detach-client", "unexpected"]).unwrap_err();
    assert!(matches!(
        error.kind(),
        clap::error::ErrorKind::UnknownArgument
    ));
}

#[test]
fn detach_client_accepts_session_id_target() {
    let cli = parse_args(&["detach-client", "-s", "$1"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::DetachClient(args) => {
            assert_eq!(args.target_session.expect("target").to_string(), "$1");
            assert!(args.target_client.is_none());
        }
        _ => panic!("expected DetachClient command"),
    }
}

#[test]
fn unrecognized_subcommand_fails() {
    let error = parse_args(&["bogus-command"]).unwrap_err();
    assert_eq!(error.kind(), clap::error::ErrorKind::InvalidSubcommand);
}

#[test]
fn help_produces_display_help_kind() {
    let error = parse_args(&["--help"]).unwrap_err();
    assert_eq!(error.kind(), clap::error::ErrorKind::DisplayHelp);
}

#[test]
fn resize_pane_accepts_target_only_noop_like_tmux() {
    let cli = parse_args(&["resize-pane", "-t", "alpha:0.0"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::ResizePane(args) => {
            assert_eq!(args.target.expect("target").to_string(), "alpha:0.0");
            assert!(args.down.is_none());
            assert!(args.up.is_none());
            assert!(args.left.is_none());
            assert!(args.right.is_none());
            assert!(args.columns.is_none());
            assert!(args.rows.is_none());
            assert!(!args.zoom);
        }
        other => panic!("expected ResizePane command, got {other:?}"),
    }
}

#[test]
fn resize_pane_valueless_adjustment_groups_follow_tmux_priority_and_composition() {
    let relative = parse_args(&["resize-pane", "-R", "-L", "-t", "alpha:0.0"]).unwrap();
    match relative.command.expect("parsed command") {
        super::super::Command::ResizePane(args) => {
            assert_eq!(args.left, Some(1));
            assert_eq!(args.right, None);
        }
        _ => panic!("expected ResizePane command"),
    }

    let absolute = parse_args(&["resize-pane", "-R", "-x", "80", "-t", "alpha:0.0"]).unwrap();
    match absolute.command.expect("parsed command") {
        super::super::Command::ResizePane(args) => {
            assert_eq!(args.right, Some(1));
            assert!(args.columns.is_some());
        }
        _ => panic!("expected ResizePane command"),
    }

    let zoom = parse_args(&["resize-pane", "-Z", "-R", "-t", "alpha:0.0"]).unwrap();
    match zoom.command.expect("parsed command") {
        super::super::Command::ResizePane(args) => {
            assert!(args.zoom);
            assert_eq!(args.right, Some(1));
        }
        _ => panic!("expected ResizePane command"),
    }
}

#[test]
fn resize_pane_explicit_adjustment_groups_follow_tmux_priority_and_composition() {
    let relative = parse_args(&["resize-pane", "-t", "alpha:0.0", "-R", "-L", "5"]).unwrap();
    match relative.command.expect("parsed command") {
        super::super::Command::ResizePane(args) => {
            assert_eq!(args.left, Some(5));
            assert_eq!(args.right, None);
        }
        _ => panic!("expected ResizePane command"),
    }

    let absolute = parse_args(&["resize-pane", "-t", "alpha:0.0", "-x", "80", "-R", "5"]).unwrap();
    match absolute.command.expect("parsed command") {
        super::super::Command::ResizePane(args) => {
            assert_eq!(args.right, Some(5));
            assert!(args.columns.is_some());
        }
        _ => panic!("expected ResizePane command"),
    }

    let zoom = parse_args(&["resize-pane", "-t", "alpha:0.0", "-Z", "-R", "5"]).unwrap();
    match zoom.command.expect("parsed command") {
        super::super::Command::ResizePane(args) => {
            assert!(args.zoom);
            assert_eq!(args.right, Some(5));
        }
        _ => panic!("expected ResizePane command"),
    }
}

#[test]
fn resize_pane_explicit_relative_deltas_still_reject_too_many_arguments_like_tmux() {
    for args in [
        ["resize-pane", "-R", "5", "-L", "3", "-t", "alpha:0.0"].as_slice(),
        ["resize-pane", "-R", "5", "-x", "80", "-t", "alpha:0.0"].as_slice(),
    ] {
        let error = parse_args(args).unwrap_err();
        assert!(
            error.to_string().contains("too many arguments")
                || error.to_string().contains("unexpected argument")
                || error
                    .to_string()
                    .contains("accepts only one relative adjustment"),
            "{error}"
        );
    }
}

#[test]
fn resize_pane_rejects_attached_relative_delta_like_tmux() {
    let error = parse_args(&["resize-pane", "-t", "alpha:0.0", "-R5"]).unwrap_err();

    assert_eq!(error.kind(), clap::error::ErrorKind::UnknownArgument);
    assert!(
        error.to_string().contains("unknown flag -5"),
        "resize-pane -R5 should reject the attached delta like tmux, got {error}"
    );
}

#[test]
fn select_layout_accepts_target_only_noop_like_tmux() {
    let cli = parse_args(&["select-layout", "-t", "alpha:0"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SelectLayout(args) => {
            assert_eq!(args.target.expect("target").to_string(), "alpha:0");
            assert!(args.layout.is_none());
        }
        other => panic!("expected SelectLayout command, got {other:?}"),
    }
}

#[test]
fn select_layout_preserves_layout_argument_for_runtime_validation() {
    let cli = parse_args(&["select-layout", "-t", "alpha:0", "invalid-layout"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SelectLayout(args) => {
            assert_eq!(args.layout.as_deref(), Some("invalid-layout"));
        }
        _ => panic!("expected SelectLayout command"),
    }
}

#[test]
fn select_layout_accepts_tmux_layout_names() {
    for layout_name in [
        "even-horizontal",
        "even-vertical",
        "main-horizontal",
        "main-vertical",
        "tiled",
    ] {
        let cli = parse_args(&["select-layout", "-t", "alpha:0", layout_name]).unwrap();

        match cli.command.expect("parsed command") {
            super::super::Command::SelectLayout(args) => {
                assert_eq!(args.layout.as_deref(), Some(layout_name));
            }
            _ => panic!("expected SelectLayout command"),
        }
    }
}
