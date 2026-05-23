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
fn set_environment_requires_scope_group() {
    let error = parse_args(&["set-environment", "TERM", "screen"]).unwrap_err();
    assert_eq!(
        error.kind(),
        clap::error::ErrorKind::MissingRequiredArgument
    );
}

#[test]
fn set_hook_requires_scope_group() {
    let error = parse_args(&["set-hook", "client-attached", "true"]).unwrap_err();
    assert_eq!(
        error.kind(),
        clap::error::ErrorKind::MissingRequiredArgument
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
