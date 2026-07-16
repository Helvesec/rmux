use super::*;

#[test]
fn pipe_pane_accepts_bidirectional_and_once_flags() {
    let cli = parse_args(&[
        "pipe-pane",
        "-I",
        "-O",
        "-o",
        "-t",
        "alpha:0.1",
        "cat >/tmp/pipe-pane.out",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::PipePane(args) => {
            assert!(args.stdin);
            assert!(args.stdout);
            assert!(args.once);
            assert_eq!(target_text(&args.target), "alpha:0.1");
            assert_eq!(args.command, ["cat >/tmp/pipe-pane.out"]);
        }
        _ => panic!("expected PipePane command"),
    }
}

#[test]
fn respawn_pane_accepts_kill_directory_environment_and_command() {
    let cli = parse_args(&[
        "respawn-pane",
        "-k",
        "-c",
        "/tmp/work",
        "-e",
        "FOO=bar",
        "-e",
        "BAR=baz",
        "-t",
        "alpha:0.1",
        "printf",
        "done",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::RespawnPane(args) => {
            assert!(args.kill);
            assert_eq!(args.start_directory, Some(PathBuf::from("/tmp/work")));
            assert_eq!(args.environment, ["FOO=bar", "BAR=baz"]);
            assert_eq!(target_text(&args.target), "alpha:0.1");
            assert_eq!(args.command, ["printf", "done"]);
        }
        _ => panic!("expected RespawnPane command"),
    }
}

#[test]
fn display_panes_accepts_duration_no_command_and_template_flags() {
    let cli = parse_args(&[
        "display-panes",
        "-b",
        "-d",
        "250",
        "-N",
        "-t",
        "alpha",
        "select-pane",
        "-t",
        "%%",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::DisplayPanes(args) => {
            assert!(args.non_blocking);
            assert_eq!(args.duration_ms, Some(250));
            assert!(args.no_command);
            assert_eq!(args.target_client.as_deref(), Some("alpha"));
            assert_eq!(
                args.template_command().as_deref(),
                Some("select-pane -t %%")
            );
        }
        _ => panic!("expected DisplayPanes command"),
    }
}

#[test]
fn last_pane_preserves_tmux_style_raw_targets() {
    let cli = parse_args(&["last-pane", "-d", "-Z", "-t", "alpha:0.1"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::LastPane(args) => {
            assert!(args.disable_input);
            assert!(!args.enable_input);
            assert!(args.keep_zoom);
            assert_eq!(
                args.target.as_ref().expect("target").to_string(),
                "alpha:0.1"
            )
        }
        _ => panic!("expected LastPane command"),
    }
}

#[test]
fn last_pane_accepts_combined_input_flags_like_tmux() {
    for flags in [&["-de"][..], &["-d", "-e"][..], &["-e", "-d"][..]] {
        let mut argv = vec!["last-pane"];
        argv.extend_from_slice(flags);
        let cli = parse_args(&argv).expect("tmux accepts both last-pane input flags");
        match cli.command.expect("parsed command") {
            super::super::Command::LastPane(args) => {
                assert!(args.disable_input);
                assert!(args.enable_input);
            }
            _ => panic!("expected LastPane command"),
        }
    }
}

#[test]
fn kill_pane_accepts_session_targets_like_tmux() {
    let cli = parse_args(&["kill-pane", "-t", "alpha"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::KillPane(args) => {
            assert!(!args.kill_all_except);
            assert_eq!(args.target.expect("target exists").to_string(), "alpha")
        }
        _ => panic!("expected KillPane command"),
    }
}

#[test]
fn kill_pane_accepts_all_except_flag_like_tmux() {
    let cli = parse_args(&["kill-pane", "-a", "-t", "alpha:0.0"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::KillPane(args) => {
            assert!(args.kill_all_except);
            assert_eq!(args.target.expect("target exists").to_string(), "alpha:0.0");
        }
        _ => panic!("expected KillPane command"),
    }
}

#[test]
fn kill_pane_allows_implicit_current_pane_target() {
    let cli = parse_args(&["kill-pane"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::KillPane(args) => {
            assert!(!args.kill_all_except);
            assert!(args.target.is_none());
        }
        _ => panic!("expected KillPane command"),
    }
}

#[test]
fn select_pane_accepts_session_targets_like_tmux() {
    let cli = parse_args(&["select-pane", "-t", "alpha"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SelectPane(args) => {
            assert_eq!(args.target.expect("target exists").to_string(), "alpha")
        }
        _ => panic!("expected SelectPane command"),
    }
}

#[test]
fn select_pane_accepts_pane_style_flag() {
    let cli = parse_args(&["select-pane", "-t", "%0", "-P", "bg=blue,fg=white"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SelectPane(args) => {
            assert_eq!(args.target.expect("target exists").to_string(), "%0");
            assert_eq!(args.style.as_deref(), Some("bg=blue,fg=white"));
        }
        _ => panic!("expected SelectPane command"),
    }
}

#[test]
fn select_pane_accepts_bare_current_target_like_tmux() {
    let cli = parse_args(&["select-pane"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SelectPane(args) => {
            assert!(args.target.is_none());
            assert!(args.direction().is_none());
            assert!(!args.mark);
            assert!(!args.clear_marked);
            assert!(args.title.is_none());
        }
        _ => panic!("expected SelectPane command"),
    }
}

#[test]
fn select_pane_accepts_non_zero_window_targets() {
    let cli = parse_args(&["select-pane", "-t", "alpha:5.2"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SelectPane(args) => {
            assert_eq!(args.target.expect("target exists").to_string(), "alpha:5.2")
        }
        _ => panic!("expected SelectPane command"),
    }
}

#[test]
fn select_pane_accepts_title_with_and_without_explicit_target() {
    let cli = parse_args(&["select-pane", "-t", "alpha:5.2", "-T", "build"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SelectPane(args) => {
            assert_eq!(args.target.expect("target exists").to_string(), "alpha:5.2");
            assert_eq!(args.title.as_deref(), Some("build"));
        }
        _ => panic!("expected SelectPane command"),
    }

    let cli = parse_args(&["select-pane", "-T", "current-title"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SelectPane(args) => {
            assert!(args.target.is_none());
            assert_eq!(args.title.as_deref(), Some("current-title"));
        }
        _ => panic!("expected SelectPane command"),
    }
}

#[test]
fn select_pane_accepts_directional_flags_with_optional_target() {
    let cli = parse_args(&["select-pane", "-R", "-t", "alpha:5.2"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SelectPane(args) => {
            assert_eq!(
                args.target.as_ref().expect("target exists").to_string(),
                "alpha:5.2"
            );
            assert_eq!(
                args.direction(),
                Some(rmux_proto::SelectPaneDirection::Right)
            );
        }
        _ => panic!("expected SelectPane command"),
    }

    let cli = parse_args(&["select-pane", "-D"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SelectPane(args) => {
            assert!(args.target.is_none());
            assert_eq!(
                args.direction(),
                Some(rmux_proto::SelectPaneDirection::Down)
            );
        }
        _ => panic!("expected SelectPane command"),
    }
}

#[test]
fn select_pane_directional_flags_follow_tmux_priority() {
    for argv in [
        ["select-pane", "-L", "-R", "-t", "alpha:5.2"],
        ["select-pane", "-R", "-L", "-t", "alpha:5.2"],
    ] {
        let cli = parse_args(&argv).unwrap();
        match cli.command.expect("parsed command") {
            super::super::Command::SelectPane(args) => {
                assert_eq!(
                    args.direction(),
                    Some(rmux_proto::SelectPaneDirection::Left)
                );
            }
            _ => panic!("expected SelectPane command"),
        }
    }

    for argv in [
        ["select-pane", "-U", "-D", "-t", "alpha:5.2"],
        ["select-pane", "-D", "-U", "-t", "alpha:5.2"],
    ] {
        let cli = parse_args(&argv).unwrap();
        match cli.command.expect("parsed command") {
            super::super::Command::SelectPane(args) => {
                assert_eq!(args.direction(), Some(rmux_proto::SelectPaneDirection::Up));
            }
            _ => panic!("expected SelectPane command"),
        }
    }
}

#[test]
fn select_pane_accepts_last_keep_zoom_and_input_flags() {
    let cli = parse_args(&["select-pane", "-l", "-Z", "-t", "%1"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SelectPane(args) => {
            assert!(args.last);
            assert!(args.keep_zoom);
            assert_eq!(args.target.as_ref().expect("target").to_string(), "%1");
        }
        _ => panic!("expected SelectPane command"),
    }

    let cli = parse_args(&["select-pane", "-d", "-t", "alpha:0.1"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SelectPane(args) => {
            assert!(args.disable_input);
            assert!(!args.enable_input);
            assert_eq!(
                args.target.as_ref().expect("target").to_string(),
                "alpha:0.1"
            );
        }
        _ => panic!("expected SelectPane command"),
    }

    let cli = parse_args(&["select-pane", "-e", "-t", "alpha:0.1"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SelectPane(args) => {
            assert!(!args.disable_input);
            assert!(args.enable_input);
            assert_eq!(
                args.target.as_ref().expect("target").to_string(),
                "alpha:0.1"
            );
        }
        _ => panic!("expected SelectPane command"),
    }
}

#[test]
fn select_pane_preserves_tmux_runtime_resolved_raw_targets() {
    for target in [
        "%0", "@0", "alpha:.", "alpha:.+", "alpha:.-", ".", ":", ":.+",
    ] {
        let cli = parse_args(&["select-pane", "-t", target]).unwrap();
        match cli.command.clone().expect("parsed command") {
            super::super::Command::SelectPane(args) => {
                assert_eq!(args.target.expect("target exists").to_string(), target);
            }
            _ => panic!("expected SelectPane command"),
        }
    }
}

#[test]
fn select_pane_preserves_runtime_resolved_tmux_targets() {
    for target in ["alpha:", "alpha:x.0", "alpha:0.", "alpha:0.-1", ":0"] {
        let cli = parse_args(&["select-pane", "-t", target]).unwrap();
        match cli.command.clone().expect("parsed command") {
            super::super::Command::SelectPane(args) => {
                assert_eq!(args.target.expect("target exists").to_string(), target);
            }
            _ => panic!("expected SelectPane command"),
        }
    }
}

#[test]
fn select_pane_accepts_mark_without_explicit_target() {
    let cli = parse_args(&["select-pane", "-m"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SelectPane(args) => {
            assert!(args.mark);
            assert!(!args.clear_marked);
            assert!(args.target.is_none());
        }
        _ => panic!("expected SelectPane command"),
    }
}

#[test]
fn clock_mode_accepts_optional_pane_target() {
    let cli = parse_args(&["clock-mode", "-t", "alpha:5.2"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::ClockMode(args) => {
            assert_eq!(args.target.expect("target exists").to_string(), "alpha:5.2");
        }
        _ => panic!("expected ClockMode command"),
    }
}

#[test]
fn send_keys_accepts_zero_keys() {
    let cli = parse_args(&["send-keys", "-t", "alpha:0.0"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SendKeys(args) => assert!(args.keys.is_empty()),
        _ => panic!("expected SendKeys command"),
    }
}

#[test]
fn send_keys_accepts_hyphen_prefixed_values() {
    let cli = parse_args(&["send-keys", "-t", "alpha:0.0", "-l", "test"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SendKeys(args) => {
            assert!(args.literal);
            assert_eq!(args.keys, vec!["test"]);
        }
        _ => panic!("expected SendKeys command"),
    }
}

#[test]
fn send_keys_parses_target_client_without_treating_it_as_input() {
    let cli = parse_args(&["send-keys", "-c", "123", "-t", "alpha:0.0", "Enter"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SendKeys(args) => {
            assert_eq!(args.client_target.as_deref(), Some("123"));
            assert_eq!(args.keys, vec!["Enter"]);
        }
        _ => panic!("expected SendKeys command"),
    }
}

#[test]
fn send_keys_marks_unsupported_prefix_flag_before_input_values() {
    let cli = parse_args(&["send-keys", "-p", "-t", "alpha:0.0", "abc"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SendKeys(args) => {
            assert!(args.unsupported_prefix);
            assert_eq!(
                args.target.as_ref().expect("target").to_string(),
                "alpha:0.0"
            );
            assert_eq!(args.keys, vec!["abc"]);
        }
        _ => panic!("expected SendKeys command"),
    }
}

#[test]
fn capture_pane_accepts_public_command_name_and_flags() {
    let cli = parse_args(&[
        "capture-pane",
        "-t",
        "alpha:0.0",
        "-S",
        "-3",
        "-E",
        "-1",
        "-p",
        "-b",
        "cap",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::CapturePane(args) => {
            assert_eq!(target_text(&args.target), "alpha:0.0");
            assert_eq!(args.start.as_deref(), Some("-3"));
            assert_eq!(args.end.as_deref(), Some("-1"));
            assert!(args.print);
            assert_eq!(args.buffer_name.as_deref(), Some("cap"));
        }
        _ => panic!("expected CapturePane command"),
    }
}

#[test]
fn capture_pane_repeated_flags_follow_tmux_last_wins() {
    let cli = parse_args(&[
        "capture-pane",
        "-p",
        "-p",
        "-t",
        "alpha:0.0",
        "-S",
        "0",
        "-S",
        "1",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::CapturePane(args) => {
            assert!(args.print);
            assert_eq!(args.start.as_deref(), Some("1"));
        }
        _ => panic!("expected CapturePane command"),
    }
}

#[test]
fn send_keys_repeated_target_follows_tmux_last_wins() {
    let cli = parse_args(&["send-keys", "-t", "alpha:0.0", "-t", "alpha:0.1", "Enter"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SendKeys(args) => {
            assert_eq!(args.target.expect("target").to_string(), "alpha:0.1");
            assert_eq!(args.keys, ["Enter"]);
        }
        _ => panic!("expected SendKeys command"),
    }
}

#[test]
fn capture_pane_alias_accepts_print_mode() {
    let cli = parse_args(&["capturep", "-p", "-t", "alpha:0.0"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::CapturePane(args) => {
            assert!(args.print);
            assert_eq!(target_text(&args.target), "alpha:0.0");
        }
        _ => panic!("expected CapturePane command"),
    }
}

#[test]
fn capture_pane_accepts_tmux_mode_screen_flag() {
    let cli = parse_args(&["capture-pane", "-M", "-p", "-t", "alpha:0.0"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::CapturePane(args) => {
            assert!(args.use_mode_screen);
            assert!(args.print);
            assert_eq!(target_text(&args.target), "alpha:0.0");
        }
        _ => panic!("expected CapturePane command"),
    }
}

#[test]
fn copy_mode_accepts_tmux_page_down_and_scrollbar_flags() {
    let cli = parse_args(&["copy-mode", "-dS", "-t", "alpha:0.0"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::CopyMode(args) => {
            assert!(args.page_down);
            assert!(args.scrollbar_scroll);
            assert_eq!(target_text(&args.target), "alpha:0.0");
        }
        _ => panic!("expected CopyMode command"),
    }
}

#[test]
fn pane_commands_accept_session_or_window_targets_like_tmux() {
    let capture = parse_args(&["capture-pane", "-p", "-t", "alpha"]).unwrap();
    match capture.command.expect("parsed command") {
        super::super::Command::CapturePane(args) => assert_eq!(target_text(&args.target), "alpha"),
        _ => panic!("expected CapturePane command"),
    }

    let send_prefix = parse_args(&["send-prefix", "-t", "alpha:2"]).unwrap();
    match send_prefix.command.expect("parsed command") {
        super::super::Command::SendPrefix(args) => {
            assert_eq!(
                args.target.expect("send-prefix target").to_string(),
                "alpha:2"
            )
        }
        _ => panic!("expected SendPrefix command"),
    }

    let copy_mode = parse_args(&["copy-mode", "-t", "alpha"]).unwrap();
    match copy_mode.command.expect("parsed command") {
        super::super::Command::CopyMode(args) => {
            assert_eq!(args.target.expect("copy-mode target").to_string(), "alpha")
        }
        _ => panic!("expected CopyMode command"),
    }
}
