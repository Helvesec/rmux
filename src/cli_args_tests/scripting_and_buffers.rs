use super::*;

#[test]
fn display_message_accepts_print_target_and_hyphen_prefixed_format_text() {
    let cli = parse_args(&[
        "display-message",
        "-p",
        "-t",
        "alpha:0.1",
        "-#{session_name}",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::DisplayMessage(args) => {
            assert!(args.print);
            assert_eq!(args.target.expect("target").to_string(), "alpha:0.1");
            assert_eq!(args.message, vec!["-#{session_name}"]);
        }
        _ => panic!("expected DisplayMessage command"),
    }
}

#[test]
fn run_shell_accepts_background_and_hyphen_prefixed_shell_text() {
    let cli = parse_args(&["run-shell", "-b", "-printf", "ok"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::RunShell(args) => {
            assert!(args.background);
            assert_eq!(args.command, vec!["-printf", "ok"]);
        }
        _ => panic!("expected RunShell command"),
    }
}

#[test]
fn run_shell_preserves_hyphenated_subcommand_arguments_after_first_token() {
    let cli = parse_args(&[
        "run-shell",
        "env",
        "-C",
        "/tmp/example path",
        "touch",
        "name with spaces",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::RunShell(args) => {
            assert!(!args.as_commands);
            assert_eq!(
                args.command,
                vec![
                    "env",
                    "-C",
                    "/tmp/example path",
                    "touch",
                    "name with spaces",
                ]
            );
        }
        _ => panic!("expected RunShell command"),
    }
}

#[test]
fn source_file_accepts_flags_target_and_hyphen_path() {
    let cli = parse_args(&["source", "-F", "-n", "-q", "-v", "-t", "alpha:0.1", "-"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SourceFile(args) => {
            assert!(args.expand_paths);
            assert!(args.parse_only);
            assert!(args.quiet);
            assert!(args.verbose);
            assert_eq!(args.target.expect("target").to_string(), "alpha:0.1");
            assert_eq!(args.paths, vec!["-"]);
        }
        _ => panic!("expected SourceFile command"),
    }
}

#[test]
fn set_buffer_and_show_aliases_accept_tmux_short_forms() {
    let cli = parse_args(&["setb", "-b", "named", "payload"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SetBuffer(args) => {
            assert_eq!(args.name.as_deref(), Some("named"));
            assert_eq!(args.content.as_deref(), Some("payload"));
        }
        _ => panic!("expected SetBuffer command"),
    }

    let cli = parse_args(&["showb", "-b", "named"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::ShowBuffer(args) => assert_eq!(args.name.as_deref(), Some("named")),
        _ => panic!("expected ShowBuffer command"),
    }

    let cli = parse_args(&["showenv", "-t", "alpha"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::ShowEnvironment(args) => {
            assert_eq!(
                args.target,
                Some(rmux_proto::SessionName::new("alpha").unwrap())
            );
            assert!(!args.global);
        }
        _ => panic!("expected ShowEnvironment command"),
    }

    let cli = parse_args(&["show", "-gqv"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::ShowOptions(args) => {
            assert!(args.global);
            assert!(args.quiet);
            assert!(args.value_only);
            assert_eq!(args.target, None);
        }
        _ => panic!("expected ShowOptions command"),
    }

    assert!(parse_args(&["show-window-options", "-gqv", "pane-border-style"]).is_err());
}

#[test]
fn set_buffer_requires_double_dash_for_hyphen_prefixed_content() {
    assert!(parse_args(&["set-buffer", "-b", "named", "-world"]).is_err());

    let cli = parse_args(&["set-buffer", "-b", "named", "--", "-world"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::SetBuffer(args) => {
            assert_eq!(args.name.as_deref(), Some("named"));
            assert_eq!(args.content.as_deref(), Some("-world"));
        }
        _ => panic!("expected SetBuffer command"),
    }
}

#[test]
fn if_shell_accepts_format_mode_target_and_optional_else_command() {
    let cli = parse_args(&[
        "if-shell",
        "-F",
        "-t",
        "alpha:0.1",
        "#{pane_active}",
        "set-buffer yes",
        "set-buffer no",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::IfShell(args) => {
            assert!(args.format_mode);
            assert_eq!(args.target.expect("target").to_string(), "alpha:0.1");
            assert_eq!(args.condition, "#{pane_active}");
            assert_eq!(args.then_command, "set-buffer yes");
            assert_eq!(args.else_command.as_deref(), Some("set-buffer no"));
        }
        _ => panic!("expected IfShell command"),
    }
}

#[test]
fn set_hook_accepts_target_scope_and_indexed_hook() {
    let cli = parse_args(&["set-hook", "-t", "alpha", "client-attached[2]", "true"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::SetHook(args) => {
            assert_eq!(args.target.expect("target").to_string(), "alpha");
            assert_eq!(args.hook.hook, rmux_proto::HookName::ClientAttached);
            assert_eq!(args.hook.index, Some(2));
            assert_eq!(args.command.as_deref(), Some("true"));
        }
        _ => panic!("expected SetHook command"),
    }
}

#[test]
fn show_hooks_accepts_global_and_target_scope_flags() {
    let cli = parse_args(&["show-hooks", "-g", "client-attached"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::ShowHooks(args) => {
            assert!(args.global);
            assert_eq!(args.hook, Some(rmux_proto::HookName::ClientAttached));
            assert_eq!(args.target, None);
        }
        _ => panic!("expected ShowHooks command"),
    }

    let cli = parse_args(&["show-hooks", "-p", "-t", "alpha:0.1", "client-attached"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::ShowHooks(args) => {
            assert!(args.pane);
            assert_eq!(args.target.expect("target").to_string(), "alpha:0.1");
            assert_eq!(args.hook, Some(rmux_proto::HookName::ClientAttached));
        }
        _ => panic!("expected ShowHooks command"),
    }
}

#[test]
fn wait_for_accepts_all_modes() {
    for (flag, expected) in [
        ("-S", rmux_proto::WaitForMode::Signal),
        ("-L", rmux_proto::WaitForMode::Lock),
        ("-U", rmux_proto::WaitForMode::Unlock),
    ] {
        let cli = parse_args(&["wait-for", flag, "channel"]).unwrap();
        match cli.command.expect("parsed command") {
            super::super::Command::WaitFor(args) => assert_eq!(args.mode(), expected),
            _ => panic!("expected WaitFor command"),
        }
    }

    let cli = parse_args(&["wait-for", "channel"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::WaitFor(args) => {
            assert_eq!(args.mode(), rmux_proto::WaitForMode::Wait)
        }
        _ => panic!("expected WaitFor command"),
    }
}

#[test]
fn link_window_accepts_tmux_position_and_target_flags() {
    let cli = parse_args(&[
        "link-window",
        "-a",
        "-d",
        "-k",
        "-s",
        "alpha:0",
        "-t",
        "beta:1",
    ])
    .unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::LinkWindow(args) => {
            assert!(args.after);
            assert!(!args.before);
            assert!(args.detached);
            assert!(args.kill_target);
            assert_eq!(args.source.to_string(), "alpha:0");
            assert_eq!(target_text(&args.target), "beta:1");
        }
        _ => panic!("expected LinkWindow command"),
    }

    let cli = parse_args(&["link-window", "-b", "-s", "alpha:0", "-t", "beta:1"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::LinkWindow(args) => {
            assert!(!args.after);
            assert!(args.before);
            assert!(!args.detached);
            assert!(!args.kill_target);
            assert_eq!(args.source.to_string(), "alpha:0");
            assert_eq!(target_text(&args.target), "beta:1");
        }
        _ => panic!("expected LinkWindow command"),
    }
}

#[test]
fn unlink_window_accepts_target_and_kill_if_last_flag() {
    let cli = parse_args(&["unlink-window", "-k", "-t", "alpha:0"]).unwrap();

    match cli.command.expect("parsed command") {
        super::super::Command::UnlinkWindow(args) => {
            assert!(args.kill_if_last);
            assert_eq!(target_text(&args.target), "alpha:0");
        }
        _ => panic!("expected UnlinkWindow command"),
    }
}

#[test]
fn link_and_unlink_window_aliases_dispatch_to_the_window_commands() {
    let cli = parse_args(&["link", "-s", "alpha:0", "-t", "beta:1"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::LinkWindow(args) => {
            assert_eq!(args.source.to_string(), "alpha:0");
            assert_eq!(target_text(&args.target), "beta:1");
        }
        _ => panic!("expected LinkWindow command"),
    }

    let cli = parse_args(&["linkw", "-s", "alpha:0", "-t", "beta:1"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::LinkWindow(args) => {
            assert_eq!(args.source.to_string(), "alpha:0");
            assert_eq!(target_text(&args.target), "beta:1");
        }
        _ => panic!("expected LinkWindow command"),
    }

    let cli = parse_args(&["unlinkw", "-t", "alpha:0"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::UnlinkWindow(args) => {
            assert_eq!(target_text(&args.target), "alpha:0");
            assert!(!args.kill_if_last);
        }
        _ => panic!("expected UnlinkWindow command"),
    }
}

#[test]
fn wait_alias_accepts_lock_mode() {
    let cli = parse_args(&["wait", "-L", "channel"]).unwrap();
    match cli.command.expect("parsed command") {
        super::super::Command::WaitFor(args) => {
            assert_eq!(args.channel, "channel");
            assert_eq!(args.mode(), rmux_proto::WaitForMode::Lock);
        }
        _ => panic!("expected WaitFor command"),
    }
}
