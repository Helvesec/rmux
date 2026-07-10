#![cfg(unix)]

use std::collections::BTreeSet;
use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::time::Duration;

mod common;

use common::{send_request, session_name, start_server, wait_for_socket_removal, TestHarness};
use rmux_proto::{
    CapturePaneRequest, DeleteBufferRequest, DisplayMessageRequest, HasSessionRequest,
    IfShellRequest, KillServerRequest, ListBuffersRequest, ListPanesRequest, ListSessionsRequest,
    LoadBufferRequest, NewSessionRequest, NewWindowRequest, PaneTarget, PasteBufferRequest,
    RenameSessionRequest, Request, Response, RunShellRequest, SaveBufferRequest, SendKeysRequest,
    SetBufferRequest, ShowBufferRequest, SplitDirection, SplitWindowRequest, SplitWindowTarget,
    Target, TerminalSize, WaitForMode, WaitForRequest,
};
use tokio::time::sleep;

const COMMAND_SURFACE_COUNT: usize = 79;
const INTERNAL_REQUEST_COMMANDS: [&str; 49] = [
    "attach-session-ext",
    "attach-session-ext2",
    "attach-session-ext3",
    "cancel-sdk-wait",
    "capture-pane-target-action",
    "control-mode",
    "daemon-status",
    "detach-client-ext",
    "display-message-ext",
    "handshake",
    "new-session-ext",
    "create-session-lease",
    "pane-broadcast-input",
    "pane-input",
    "pane-kill",
    "pane-option-get",
    "pane-option-set",
    "pane-foreground-state",
    "pane-resize",
    "pane-respawn",
    "pane-state-cursor",
    "pane-select",
    "pane-snapshot",
    "pane-snapshot-ref",
    "resolve-target",
    "release-session-lease",
    "renew-session-lease",
    "resize-pane-target-action",
    "select-custom-layout",
    "select-pane-mark",
    "select-pane-adjacent",
    "select-old-layout",
    "send-keys-ext",
    "send-keys-ext2",
    "spread-layout",
    "shutdown-if-idle",
    "sdk-wait-output",
    "sdk-wait-for-output-ref",
    "sdk-wait-for-output",
    "subscribe-pane-state",
    "subscribe-pane-output-ref",
    "set-hook-mutation",
    "set-option-by-name",
    "split-window-ext",
    "split-window-target-action",
    "switch-client-ext",
    "switch-client-ext2",
    "switch-client-ext3",
    "unsubscribe-pane-state",
];
const COMMAND_SURFACE: [&str; COMMAND_SURFACE_COUNT] = [
    "new-session",
    "kill-server",
    "has-session",
    "kill-session",
    "server-access",
    "lock-server",
    "lock-session",
    "lock-client",
    "new-window",
    "kill-window",
    "select-window",
    "rename-window",
    "next-window",
    "previous-window",
    "last-window",
    "list-windows",
    "link-window",
    "move-window",
    "swap-window",
    "rotate-window",
    "resize-window",
    "respawn-window",
    "split-window",
    "swap-pane",
    "last-pane",
    "join-pane",
    "move-pane",
    "break-pane",
    "pipe-pane",
    "respawn-pane",
    "kill-pane",
    "select-layout",
    "next-layout",
    "previous-layout",
    "resize-pane",
    "display-panes",
    "select-pane",
    "send-keys",
    "bind-key",
    "unbind-key",
    "list-keys",
    "send-prefix",
    "attach-session",
    "refresh-client",
    "list-clients",
    "switch-client",
    "detach-client",
    "suspend-client",
    "set-option",
    "set-environment",
    "show-options",
    "show-environment",
    "show-hooks",
    "show-messages",
    "source-file",
    "unlink-window",
    "set-hook",
    "set-buffer",
    "show-buffer",
    "paste-buffer",
    "list-buffers",
    "delete-buffer",
    "load-buffer",
    "save-buffer",
    "capture-pane",
    "clear-history",
    "copy-mode",
    "clock-mode",
    "display-message",
    "run-shell",
    "if-shell",
    "wait-for",
    "subscribe-pane-output",
    "unsubscribe-pane-output",
    "pane-output-cursor",
    "rename-session",
    "list-sessions",
    "list-panes",
    "web-share",
];

#[test]
fn request_command_surface_matches_handler_dispatch() -> Result<(), Box<dyn Error>> {
    let repo_root = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../..");
    let expected = expected_command_surface();
    let request_enum_commands = extract_enum_variant_commands(
        &repo_root.join("crates/rmux-proto/src/request.rs"),
        "pub enum Request",
    )?;
    let request_name_commands = extract_request_command_names(
        &repo_root.join("crates/rmux-proto/src/request.rs"),
        "match self",
    )?;
    let handler_commands = extract_match_variant_commands(
        &repo_root.join("crates/rmux-server/src/handler_dispatch.rs"),
        "match request",
        "Request::",
    )?;

    assert_eq!(expected.len(), COMMAND_SURFACE_COUNT);
    assert_eq!(filter_internal_commands(request_enum_commands), expected);
    assert_eq!(filter_internal_commands(request_name_commands), expected);
    assert_eq!(filter_internal_commands(handler_commands), expected);
    Ok(())
}

#[tokio::test]
async fn buffer_capture_and_scripting_requests_round_trip_over_real_socket(
) -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("request-buffer-capture-scripting");
    let handle = start_server(&harness).await?;
    let pane = PaneTarget::with_window(session_name("alpha"), 0, 0);
    let save_path = harness
        .socket_path()
        .parent()
        .expect("socket path should have a parent")
        .join("saved-buffer.txt");
    let load_path = harness
        .socket_path()
        .parent()
        .expect("socket path should have a parent")
        .join("loaded-buffer.txt");
    let paste_marker = "server_request_pasted_marker";
    let paste_command = format!("printf {paste_marker}");

    assert!(matches!(
        send_request(
            harness.socket_path(),
            &Request::NewSession(NewSessionRequest {
                session_name: session_name("alpha"),
                detached: true,
                size: Some(TerminalSize {
                    cols: 120,
                    rows: 40
                }),
                environment: None,
            }),
        )
        .await?,
        Response::NewSession(_)
    ));

    assert!(matches!(
        send_request(
            harness.socket_path(),
            &Request::SetBuffer(Box::new(SetBufferRequest {
                name: Some("empty".to_owned()),
                content: Vec::new(),
                append: false,
                new_name: None,
                set_clipboard: false,
                target_client: None,
            })),
        )
        .await?,
        Response::SetBuffer(_)
    ));
    let empty = send_request(
        harness.socket_path(),
        &Request::ShowBuffer(ShowBufferRequest {
            name: Some("empty".to_owned()),
        }),
    )
    .await?;
    assert!(matches!(empty, Response::Error(_)));

    assert!(matches!(
        send_request(
            harness.socket_path(),
            &Request::SetBuffer(Box::new(SetBufferRequest {
                name: Some("delete-me".to_owned()),
                content: b"x".to_vec(),
                append: false,
                new_name: None,
                set_clipboard: false,
                target_client: None,
            })),
        )
        .await?,
        Response::SetBuffer(_)
    ));

    assert!(matches!(
        send_request(
            harness.socket_path(),
            &Request::SetBuffer(Box::new(SetBufferRequest {
                name: Some("pastecmd".to_owned()),
                content: paste_command.clone().into_bytes(),
                append: false,
                new_name: None,
                set_clipboard: false,
                target_client: None,
            })),
        )
        .await?,
        Response::SetBuffer(_)
    ));

    let listed = send_request(
        harness.socket_path(),
        &Request::ListBuffers(ListBuffersRequest::default()),
    )
    .await?;
    let listed_stdout = std::str::from_utf8(
        listed
            .command_output()
            .expect("list-buffers returns command output")
            .stdout(),
    )?;
    assert!(!listed_stdout.contains("empty:"));
    assert!(listed_stdout.contains("delete-me:"));
    assert!(listed_stdout.contains("pastecmd:"));

    assert!(matches!(
        send_request(
            harness.socket_path(),
            &Request::SaveBuffer(SaveBufferRequest {
                path: save_path.to_string_lossy().into_owned(),
                cwd: None,
                name: Some("pastecmd".to_owned()),
                append: false,
            }),
        )
        .await?,
        Response::SaveBuffer(_)
    ));
    assert_eq!(fs::read_to_string(&save_path)?, paste_command);

    fs::write(&load_path, "loaded-over-socket")?;
    assert!(matches!(
        send_request(
            harness.socket_path(),
            &Request::LoadBuffer(Box::new(LoadBufferRequest {
                path: load_path.to_string_lossy().into_owned(),
                cwd: None,
                name: Some("loaded".to_owned()),
                set_clipboard: false,
                target_client: None,
            })),
        )
        .await?,
        Response::LoadBuffer(_)
    ));
    let loaded = send_request(
        harness.socket_path(),
        &Request::ShowBuffer(ShowBufferRequest {
            name: Some("loaded".to_owned()),
        }),
    )
    .await?;
    assert_eq!(
        loaded
            .command_output()
            .expect("show-buffer returns output")
            .stdout(),
        b"loaded-over-socket"
    );

    assert!(matches!(
        send_request(
            harness.socket_path(),
            &Request::PasteBuffer(Box::new(PasteBufferRequest {
                name: Some("pastecmd".to_owned()),
                target: pane.clone(),
                delete_after: false,
                separator: None,
                linefeed: false,
                raw: false,
                bracketed: false,
            })),
        )
        .await?,
        Response::PasteBuffer(_)
    ));
    assert!(matches!(
        send_request(
            harness.socket_path(),
            &Request::SendKeys(SendKeysRequest {
                target: pane.clone(),
                keys: vec!["Enter".to_owned()],
            }),
        )
        .await?,
        Response::SendKeys(_)
    ));
    let capture = wait_for_capture(harness.socket_path(), pane.clone(), paste_marker).await?;
    assert!(capture.contains(paste_marker));

    let captured = send_request(
        harness.socket_path(),
        &Request::CapturePane(Box::new(CapturePaneRequest {
            target: pane.clone(),
            start: None,
            end: None,
            print: false,
            buffer_name: Some("captured".to_owned()),
            alternate: false,
            escape_ansi: false,
            escape_sequences: false,
            include_format: false,
            hyperlinks: false,
            line_numbers: false,
            join_wrapped: false,
            use_mode_screen: false,
            preserve_trailing_spaces: false,
            do_not_trim_spaces: false,
            pending_input: false,
            quiet: false,
            start_is_absolute: false,
            end_is_absolute: false,
        })),
    )
    .await?;
    assert!(matches!(captured, Response::CapturePane(_)));
    let show_captured = send_request(
        harness.socket_path(),
        &Request::ShowBuffer(ShowBufferRequest {
            name: Some("captured".to_owned()),
        }),
    )
    .await?;
    assert!(std::str::from_utf8(
        show_captured
            .command_output()
            .expect("show-buffer returns output")
            .stdout(),
    )?
    .contains(paste_marker));

    assert!(matches!(
        send_request(
            harness.socket_path(),
            &Request::DeleteBuffer(DeleteBufferRequest {
                name: Some("delete-me".to_owned()),
            }),
        )
        .await?,
        Response::DeleteBuffer(_)
    ));
    let listed_after_delete = send_request(
        harness.socket_path(),
        &Request::ListBuffers(ListBuffersRequest::default()),
    )
    .await?;
    assert!(!std::str::from_utf8(
        listed_after_delete
            .command_output()
            .expect("list-buffers returns command output")
            .stdout(),
    )?
    .contains("delete-me:"));

    let display = send_request(
        harness.socket_path(),
        &Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(pane.clone())),
            print: true,
            message: Some("#{session_name}:#{pane_index}:#{missing}".to_owned()),
            empty_target_context: false,
        }),
    )
    .await?;
    assert_eq!(
        display
            .command_output()
            .expect("display-message -p returns command output")
            .stdout(),
        b"alpha:0:\n"
    );

    let shell = send_request(
        harness.socket_path(),
        &Request::RunShell(Box::new(RunShellRequest {
            command: "printf server-run-shell-output".to_owned(),
            arguments: Vec::new(),
            background: false,
            as_commands: false,
            show_stderr: true,
            delay_seconds: None,
            start_directory: None,
            target: None,
            source_depth: None,
        })),
    )
    .await?;
    match shell {
        Response::RunShell(response) => {
            assert_eq!(response.exit_status(), Some(0));
            let output = response
                .command_output()
                .expect("run-shell -E returns foreground command output");
            assert_eq!(
                output.stdout(),
                b"server-run-shell-output\n",
                "foreground run-shell -E should return stdout like tmux 3.7"
            );
        }
        other => panic!("expected run-shell response, got {other:?}"),
    }

    let if_shell = send_request(
        harness.socket_path(),
        &Request::IfShell(Box::new(IfShellRequest {
            condition: "#{pane_active}".to_owned(),
            format_mode: true,
            then_command: "set-buffer -b branch chosen".to_owned(),
            else_command: Some("set-buffer -b branch skipped".to_owned()),
            target: Some(Target::Pane(pane)),
            caller_cwd: None,
            background: false,
        })),
    )
    .await?;
    assert!(matches!(if_shell, Response::IfShell(_)));
    let branch = send_request(
        harness.socket_path(),
        &Request::ShowBuffer(ShowBufferRequest {
            name: Some("branch".to_owned()),
        }),
    )
    .await?;
    assert_eq!(
        branch
            .command_output()
            .expect("show-buffer returns output")
            .stdout(),
        b"chosen"
    );

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn rename_listing_and_wait_for_requests_round_trip_over_real_socket(
) -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("request-rename-list-wait");
    let handle = start_server(&harness).await?;
    let alpha = session_name("alpha");
    let gamma = session_name("gamma");

    assert!(matches!(
        send_request(
            harness.socket_path(),
            &Request::NewSession(NewSessionRequest {
                session_name: alpha.clone(),
                detached: true,
                size: Some(TerminalSize {
                    cols: 120,
                    rows: 40
                }),
                environment: None,
            }),
        )
        .await?,
        Response::NewSession(_)
    ));
    assert!(matches!(
        send_request(
            harness.socket_path(),
            &Request::SplitWindow(SplitWindowRequest {
                target: SplitWindowTarget::Session(alpha.clone()),
                direction: SplitDirection::Vertical,
                before: false,
                environment: None,
            }),
        )
        .await?,
        Response::SplitWindow(_)
    ));
    assert!(matches!(
        send_request(
            harness.socket_path(),
            &Request::NewWindow(Box::new(NewWindowRequest {
                target: alpha.clone(),
                name: Some("logs".to_owned()),
                detached: true,
                start_directory: None,
                environment: None,
                command: None,
                process_command: None,
                target_window_index: None,
                insert_at_target: false,
            })),
        )
        .await?,
        Response::NewWindow(_)
    ));

    let listed_before = send_request(
        harness.socket_path(),
        &Request::ListPanes(Box::new(ListPanesRequest {
            target: alpha.clone(),
            format: Some("#{session_name}:#{window_index}:#{pane_index}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
            target_window_index: None,
        })),
    )
    .await?;
    let before_lines = nonempty_lines(std::str::from_utf8(
        listed_before
            .command_output()
            .expect("list-panes returns command output")
            .stdout(),
    )?);
    assert_eq!(before_lines.len(), 3);
    assert!(before_lines.iter().all(|line| line.starts_with("alpha:")));

    let renamed = send_request(
        harness.socket_path(),
        &Request::RenameSession(RenameSessionRequest {
            target: alpha.clone(),
            new_name: gamma.clone(),
        }),
    )
    .await?;
    assert!(matches!(renamed, Response::RenameSession(_)));

    assert_eq!(
        send_request(
            harness.socket_path(),
            &Request::HasSession(HasSessionRequest {
                target: alpha.clone(),
            }),
        )
        .await?,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: false })
    );
    assert_eq!(
        send_request(
            harness.socket_path(),
            &Request::HasSession(HasSessionRequest {
                target: gamma.clone(),
            }),
        )
        .await?,
        Response::HasSession(rmux_proto::HasSessionResponse { exists: true })
    );

    let sessions = send_request(
        harness.socket_path(),
        &Request::ListSessions(ListSessionsRequest {
            format: Some(
                "#{session_name}:#{session_windows}:#{session_width}x#{session_height}".to_owned(),
            ),
            filter: None,
            sort_order: None,
            reversed: false,
        }),
    )
    .await?;
    assert_eq!(
        sessions
            .command_output()
            .expect("list-sessions returns command output")
            .stdout(),
        b"gamma:2:x\n"
    );

    let panes_after = send_request(
        harness.socket_path(),
        &Request::ListPanes(Box::new(ListPanesRequest {
            target: gamma.clone(),
            format: Some("#{session_name}:#{window_index}:#{pane_index}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
            target_window_index: None,
        })),
    )
    .await?;
    let after_lines = nonempty_lines(std::str::from_utf8(
        panes_after
            .command_output()
            .expect("list-panes returns command output")
            .stdout(),
    )?);
    assert_eq!(after_lines.len(), 3);
    assert!(after_lines.iter().all(|line| line.starts_with("gamma:")));
    assert!(after_lines.contains(&"gamma:1:0"));

    let ready_socket = harness.socket_path().to_path_buf();
    let ready_waiter = tokio::spawn(async move {
        let request = Request::WaitFor(WaitForRequest {
            channel: "ready".to_owned(),
            mode: WaitForMode::Wait,
        });
        send_request(&ready_socket, &request)
            .await
            .expect("ready waiter request should complete")
    });
    sleep(Duration::from_millis(50)).await;
    assert!(
        !ready_waiter.is_finished(),
        "plain wait-for should block until signalled"
    );
    assert!(matches!(
        send_request(
            harness.socket_path(),
            &Request::WaitFor(WaitForRequest {
                channel: "ready".to_owned(),
                mode: WaitForMode::Signal,
            }),
        )
        .await?,
        Response::WaitFor(_)
    ));
    assert!(matches!(ready_waiter.await?, Response::WaitFor(_)));

    assert!(matches!(
        send_request(
            harness.socket_path(),
            &Request::WaitFor(WaitForRequest {
                channel: "check".to_owned(),
                mode: WaitForMode::Lock,
            }),
        )
        .await?,
        Response::WaitFor(_)
    ));
    let gate_socket = harness.socket_path().to_path_buf();
    let lock_waiter = tokio::spawn(async move {
        let request = Request::WaitFor(WaitForRequest {
            channel: "check".to_owned(),
            mode: WaitForMode::Lock,
        });
        send_request(&gate_socket, &request)
            .await
            .expect("lock waiter request should complete")
    });
    sleep(Duration::from_millis(50)).await;
    assert!(
        !lock_waiter.is_finished(),
        "wait-for -L should block while the lock is held"
    );
    assert!(matches!(
        send_request(
            harness.socket_path(),
            &Request::WaitFor(WaitForRequest {
                channel: "check".to_owned(),
                mode: WaitForMode::Unlock,
            }),
        )
        .await?,
        Response::WaitFor(_)
    ));
    assert!(matches!(lock_waiter.await?, Response::WaitFor(_)));

    handle.shutdown().await?;
    Ok(())
}

fn expected_command_surface() -> BTreeSet<String> {
    COMMAND_SURFACE
        .iter()
        .map(|command| (*command).to_owned())
        .collect()
}

fn extract_enum_variant_commands(
    path: &Path,
    anchor: &str,
) -> Result<BTreeSet<String>, Box<dyn Error>> {
    let contents = fs::read_to_string(path)?;
    let block = extract_braced_block(&contents, anchor)?;
    let mut commands = BTreeSet::new();

    for line in block.lines() {
        let trimmed = line.trim();
        if trimmed.is_empty() || trimmed.starts_with('#') || trimmed.starts_with('/') {
            continue;
        }

        if let Some(variant) = extract_variant_name(trimmed) {
            commands.insert(camel_case_to_kebab(&variant));
        }
    }

    Ok(commands)
}

fn extract_request_command_names(
    path: &Path,
    anchor: &str,
) -> Result<BTreeSet<String>, Box<dyn Error>> {
    let contents = fs::read_to_string(path)?;
    let block = extract_braced_block(&contents, anchor)?;
    let mut commands = BTreeSet::new();
    let mut in_request_arm = false;

    for line in block.lines() {
        let trimmed = line.trim_start();
        if trimmed.starts_with("Self::") {
            in_request_arm = true;
        }
        if !in_request_arm {
            continue;
        }

        let Some(name_start) = trimmed.find('"') else {
            continue;
        };
        let remainder = &trimmed[name_start + 1..];
        let Some(name_end) = remainder.find('"') else {
            continue;
        };
        commands.insert(remainder[..name_end].to_owned());
        in_request_arm = false;
    }

    Ok(commands)
}

fn extract_match_variant_commands(
    path: &Path,
    anchor: &str,
    prefix: &str,
) -> Result<BTreeSet<String>, Box<dyn Error>> {
    let contents = fs::read_to_string(path)?;
    let block = extract_braced_block(&contents, anchor)?;
    let mut commands = BTreeSet::new();

    for line in block.lines() {
        let trimmed = line.trim_start();
        if !trimmed.starts_with(prefix) {
            continue;
        }

        if let Some(variant) = extract_prefixed_variant_name(trimmed, prefix) {
            commands.insert(camel_case_to_kebab(&variant));
        }
    }

    Ok(commands)
}

fn filter_internal_commands(commands: BTreeSet<String>) -> BTreeSet<String> {
    commands
        .into_iter()
        .filter(|command| !INTERNAL_REQUEST_COMMANDS.contains(&command.as_str()))
        .collect()
}

fn extract_braced_block<'a>(contents: &'a str, anchor: &str) -> Result<&'a str, Box<dyn Error>> {
    let anchor_offset = contents
        .find(anchor)
        .ok_or_else(|| format!("missing anchor `{anchor}`"))?;
    let brace_offset = contents[anchor_offset..]
        .find('{')
        .ok_or_else(|| format!("missing opening brace after `{anchor}`"))?;
    let body_start = anchor_offset + brace_offset + 1;
    let mut depth = 0usize;

    for (offset, character) in contents[body_start..].char_indices() {
        match character {
            '{' => depth += 1,
            '}' if depth == 0 => return Ok(&contents[body_start..body_start + offset]),
            '}' => depth -= 1,
            _ => {}
        }
    }

    Err(format!("unterminated block after `{anchor}`").into())
}

fn extract_variant_name(line: &str) -> Option<String> {
    let variant: String = line
        .chars()
        .take_while(|character| character.is_ascii_alphanumeric())
        .collect();
    (!variant.is_empty()).then_some(variant)
}

fn extract_prefixed_variant_name(line: &str, prefix: &str) -> Option<String> {
    let remainder = line.strip_prefix(prefix)?;
    extract_variant_name(remainder)
}

fn camel_case_to_kebab(value: &str) -> String {
    let mut output = String::new();

    for (index, character) in value.chars().enumerate() {
        if character.is_ascii_uppercase() {
            if index > 0 {
                output.push('-');
            }
            output.push(character.to_ascii_lowercase());
        } else {
            output.push(character);
        }
    }

    output
}

async fn wait_for_capture(
    socket_path: &Path,
    target: PaneTarget,
    marker: &str,
) -> Result<String, Box<dyn Error>> {
    for _ in 0..100 {
        let response = send_request(
            socket_path,
            &Request::CapturePane(Box::new(CapturePaneRequest {
                target: target.clone(),
                start: None,
                end: None,
                print: true,
                buffer_name: None,
                alternate: false,
                escape_ansi: false,
                escape_sequences: false,
                include_format: false,
                hyperlinks: false,
                line_numbers: false,
                join_wrapped: false,
                use_mode_screen: false,
                preserve_trailing_spaces: false,
                do_not_trim_spaces: false,
                pending_input: false,
                quiet: false,
                start_is_absolute: false,
                end_is_absolute: false,
            })),
        )
        .await?;
        let output = std::str::from_utf8(
            response
                .command_output()
                .expect("capture-pane -p returns command output")
                .stdout(),
        )?
        .to_owned();
        if output.contains(marker) {
            return Ok(output);
        }

        sleep(Duration::from_millis(20)).await;
    }

    Err(format!("capture-pane -p never surfaced marker {marker}").into())
}

fn nonempty_lines(output: &str) -> Vec<&str> {
    output.lines().filter(|line| !line.is_empty()).collect()
}

#[tokio::test]
async fn kill_server_request_shuts_down_server_and_cleans_socket() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("request-kill-server");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    match send_request(
        harness.socket_path(),
        &Request::KillServer(KillServerRequest),
    )
    .await
    {
        Ok(Response::KillServer(_)) => {}
        Ok(other) => panic!("unexpected kill-server response: {other:?}"),
        Err(error) => {
            let message = error.to_string();
            assert!(
                message.contains("connection closed")
                    || message.contains("UnexpectedEof")
                    || message.contains("reset")
                    || message.contains("broken pipe"),
                "unexpected kill-server transport error: {message}"
            );
        }
    }

    drop(handle);
    wait_for_socket_removal(&socket_path).await?;
    Ok(())
}
