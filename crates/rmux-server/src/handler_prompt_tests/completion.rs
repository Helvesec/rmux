use super::*;

async fn input(handler: &RequestHandler, bytes: &[u8]) {
    handler
        .handle_attached_live_input_for_test(std::process::id(), bytes)
        .await
        .expect("prompt input succeeds");
}

async fn assert_input(handler: &RequestHandler, expected: &str, cursor: usize) {
    let prompt = handler
        .attached_prompt_render(std::process::id())
        .await
        .expect("completion keeps the prompt open");
    assert_eq!(prompt.input, expected);
    assert_eq!(prompt.cursor, cursor);
}

// Measured with tmux 3.7b: unique names and aliases retain their spelling
// and gain a trailing space. Exercise raw and extended terminal input.
#[tokio::test]
async fn prompt_completion_unique_name_and_alias_through_both_key_paths() {
    let handler = RequestHandler::new();
    let pid = std::process::id();
    let _control_rx = create_attached_session(&handler, pid, "complete-unique").await;
    for mode in ["emacs", "vi"] {
        handler
            .execute_parsed_commands_for_test(
                pid,
                parse_command(&format!("set-option -g status-keys {mode}")),
            )
            .await
            .unwrap();
        for (prefix, expected) in [("list-comm", "list-commands "), ("lscm", "lscm ")] {
            for terminal_key in [&b"\t"[..], &b"\x1b[27;1;9~"[..]] {
                input(&handler, b"\x02:").await;
                input(&handler, prefix.as_bytes()).await;
                input(&handler, terminal_key).await;
                assert_input(&handler, expected, expected.len()).await;
                input(&handler, b"\x03").await;
            }
        }
    }
}

#[tokio::test]
async fn prompt_completion_menu_keyboard_navigation() {
    let handler = RequestHandler::new();
    let pid = std::process::id();
    let _controls = create_attached_session(&handler, pid, "complete-navigation").await;
    for mode in ["emacs", "vi"] {
        handler
            .execute_parsed_commands_for_test(
                pid,
                parse_command(&format!("set-option -g status-keys {mode}")),
            )
            .await
            .unwrap();
        for (keys, expected) in [
            (&b"\r"[..], "lsc"),
            (&b"\x1b[B\r"[..], "lscm"),
            (&b"\t\r"[..], "lscm"),
        ] {
            input(&handler, b"\x02:lsc\t").await;
            input(&handler, keys).await;
            assert_input(&handler, expected, expected.len()).await;
            input(&handler, b"\x03").await;
        }
    }
}

#[tokio::test]
async fn prompt_completion_multiple_matches_choose_or_cancel_without_submitting() {
    let handler = RequestHandler::new();
    let pid = std::process::id();
    let mut controls = create_attached_session(&handler, pid, "complete-menu").await;
    input(&handler, b"\x02:lsc\t").await;
    assert_input(&handler, "lsc", 3).await;
    let frame = recv_switch_frame_containing(&mut controls, "lscm").await;
    assert!(frame.contains("lsc"));
    input(&handler, b"1").await;
    assert_input(&handler, "lscm", 4).await;
    input(&handler, b"\x15lsc\t\x03").await;
    assert_input(&handler, "lsc", 3).await;
    input(&handler, b"x").await;
    assert_input(&handler, "lscx", 4).await;
    input(&handler, b"\x03").await;
}

#[tokio::test]
async fn prompt_completion_no_match_and_argument_leave_text_unchanged() {
    let handler = RequestHandler::new();
    let pid = std::process::id();
    let _controls = create_attached_session(&handler, pid, "complete-none").await;
    for text in ["zz-no-command", "display-message -p café"] {
        input(&handler, b"\x02:").await;
        input(&handler, text.as_bytes()).await;
        input(&handler, b"\t").await;
        assert_input(&handler, text, text.chars().count()).await;
        input(&handler, b"\x03").await;
    }
}

#[tokio::test]
async fn prompt_completion_common_prefix_custom_alias_and_middle_cursor() {
    let handler = RequestHandler::new();
    let pid = std::process::id();
    let _controls = create_attached_session(&handler, pid, "complete-edit").await;
    handler
        .execute_parsed_commands_for_test(
            pid,
            parse_command("set-option -s command-alias[99] issue261=display-message"),
        )
        .await
        .unwrap();
    for (text, expected) in [("lis", "list-"), ("inf", "info "), ("issue26", "issue261 ")] {
        input(&handler, b"\x02:").await;
        input(&handler, text.as_bytes()).await;
        input(&handler, b"\t").await;
        assert_input(&handler, expected, expected.len()).await;
        input(&handler, b"\x03").await;
    }
    input(&handler, b"\x02:list-comm tail").await;
    input(&handler, b"\x1b[D\x1b[D\x1b[D\x1b[D\x1b[D\t").await;
    assert_input(&handler, "list-commands  tail", 14).await;
    input(&handler, b"\x03").await;
}

async fn assert_menu(handler: &RequestHandler, expected: &[(&str, &str, bool)]) {
    let prompt = handler
        .attached_prompt_render(std::process::id())
        .await
        .expect("resize keeps the prompt open");
    let actual: Vec<_> = prompt
        .completion_menu
        .iter()
        .map(|item| {
            (
                item.label.as_str(),
                item.shortcut.as_deref().unwrap(),
                item.selected,
            )
        })
        .collect();
    assert_eq!(actual, expected);
}

#[tokio::test]
async fn prompt_completion_resize_reconciles_choices_shortcuts_and_selection() {
    let handler = RequestHandler::new();
    let pid = std::process::id();
    let mut controls = create_attached_session(&handler, pid, "complete-resize").await;
    for (status, position, rows) in [
        ("on", "bottom", 4),
        ("on", "top", 4),
        ("off", "bottom", 4),
        ("2", "bottom", 5),
    ] {
        for mode in ["emacs", "vi"] {
            handler
                .execute_parsed_commands_for_test(
                    pid,
                    parse_command(&format!(
                        "set -g status {status}; set -g status-position {position}; set -g status-keys {mode}"
                    )),
                )
                .await
                .unwrap();
            for choose in [&b"\x1b[B\r"[..], &b"1\x1b[A0"[..]] {
                handler
                    .handle_attached_resize(
                        pid,
                        TerminalSize {
                            cols: 100,
                            rows: 24,
                        },
                    )
                    .await
                    .unwrap();
                input(&handler, b"\x02:lsc\t\x1b[B").await;
                assert_menu(&handler, &[("lsc", "(0)", false), ("lscm", "(1)", true)]).await;
                while controls.try_recv().is_ok() {}
                handler
                    .handle_attached_resize(pid, TerminalSize { cols: 100, rows })
                    .await
                    .unwrap();
                assert_menu(&handler, &[("lscm", "(0)", true)]).await;
                let frame = recv_switch_frame_containing(&mut controls, "lscm").await;
                assert!(frame.contains("(0)"));
                assert!(!frame.contains("(1)"));
                // The old shortcut must not choose an invisible/stale entry.
                input(&handler, b"1").await;
                assert_input(&handler, "lsc", 3).await;
                assert_menu(&handler, &[("lscm", "(0)", true)]).await;
                input(&handler, choose).await;
                assert_input(&handler, "lscm", 4).await;
                assert_menu(&handler, &[]).await;
                input(&handler, b"\x03").await;
                while controls.try_recv().is_ok() {}
            }
        }
    }
}

#[tokio::test]
async fn prompt_completion_resize_grows_and_closes_when_no_choice_fits() {
    let handler = RequestHandler::new();
    let pid = std::process::id();
    let _controls = create_attached_session(&handler, pid, "complete-grow").await;
    input(&handler, b"\x02:lsc\t").await;
    for rows in [4, 24] {
        handler
            .handle_attached_resize(pid, TerminalSize { cols: 100, rows })
            .await
            .unwrap();
    }
    assert_menu(&handler, &[("lsc", "(0)", false), ("lscm", "(1)", true)]).await;
    input(&handler, b"\r").await;
    assert_input(&handler, "lscm", 4).await;
    input(&handler, b"\x15lsc\t").await;
    handler
        .handle_attached_resize(pid, TerminalSize { cols: 100, rows: 3 })
        .await
        .unwrap();
    assert_menu(&handler, &[]).await;
    input(&handler, b"\t").await;
    assert_input(&handler, "lsc", 3).await;
    assert_menu(&handler, &[]).await;
    input(&handler, b"\x03").await;
}

#[tokio::test]
async fn prompt_completion_at_first_word_start_keeps_suffix() {
    let handler = RequestHandler::new();
    let pid = std::process::id();
    let _controls = create_attached_session(&handler, pid, "complete-start").await;
    for mode in ["emacs", "vi"] {
        handler
            .execute_parsed_commands_for_test(
                pid,
                parse_command(&format!("set -g status-keys {mode}")),
            )
            .await
            .unwrap();
        for tab in [&b"\t"[..], &b"\x1b[27;1;9~"[..]] {
            for (text, expected, cursor) in [
                ("list-comm", "list-commands ", 14),
                ("list-comm tail", "list-commands  tail", 14),
                ("  list-comm tail", "  list-comm tail", 2),
            ] {
                input(&handler, b"\x02:").await;
                input(&handler, text.as_bytes()).await;
                let lefts = text.trim_start_matches(' ').len();
                input(&handler, &b"\x1b[D".repeat(lefts)).await;
                input(&handler, tab).await;
                assert_input(&handler, expected, cursor).await;
                input(&handler, b"\x03").await;
            }
        }
    }
    handler
        .execute_parsed_commands_for_test(pid, parse_command("set -g status-keys emacs"))
        .await
        .unwrap();
    input(&handler, b"\x02:list-comm\x01\t").await;
    assert_input(&handler, "list-commands ", 14).await;
    input(&handler, b"\x03").await;
}
