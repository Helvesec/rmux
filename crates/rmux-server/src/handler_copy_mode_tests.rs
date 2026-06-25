use std::{
    fs,
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use super::super::RequestHandler;
use super::session_name;
use rmux_core::{input::InputParser, Screen};
use rmux_proto::{
    CapturePaneRequest, CopyModeRequest, ListPanesRequest, NewSessionExtRequest,
    OptionScopeSelector, PaneTarget, Request, Response, SendKeysExtRequest, SetOptionByNameRequest,
    SetOptionMode, ShowBufferRequest, TerminalSize,
};
use tokio::time::sleep;

fn capture_request(target: PaneTarget, use_mode_screen: bool) -> CapturePaneRequest {
    CapturePaneRequest {
        target,
        start: None,
        end: None,
        print: true,
        buffer_name: None,
        alternate: false,
        escape_ansi: false,
        escape_sequences: false,
        join_wrapped: false,
        use_mode_screen,
        preserve_trailing_spaces: false,
        do_not_trim_spaces: false,
        pending_input: false,
        quiet: false,
        start_is_absolute: false,
        end_is_absolute: false,
    }
}

async fn create_session(handler: &RequestHandler, name: &str, size: TerminalSize) -> PaneTarget {
    let session_name = session_name(name);
    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session_name.clone()),
            working_directory: None,
            detached: true,
            size: Some(size),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(quiet_copy_mode_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    assert!(matches!(response, Response::NewSession(_)));
    PaneTarget::with_window(session_name, 0, 0)
}

#[cfg(unix)]
fn quiet_copy_mode_command() -> Vec<String> {
    vec!["/bin/sh".to_owned(), "-c".to_owned(), "sleep 60".to_owned()]
}

#[cfg(windows)]
fn quiet_copy_mode_command() -> Vec<String> {
    let system_root =
        std::env::var_os("SystemRoot").unwrap_or_else(|| std::ffi::OsString::from(r"C:\Windows"));
    let cmd = PathBuf::from(system_root).join("System32").join("cmd.exe");
    vec![
        cmd.to_string_lossy().into_owned(),
        "/d".to_owned(),
        "/q".to_owned(),
        "/c".to_owned(),
        "ping -n 120 127.0.0.1 >NUL".to_owned(),
    ]
}

async fn replace_transcript_contents(
    handler: &RequestHandler,
    target: &PaneTarget,
    size: TerminalSize,
    content: &[u8],
) {
    let transcript = {
        let state = handler.state.lock().await;
        state
            .transcript_handle(target)
            .expect("session transcript must exist")
    };
    let history_limit = transcript
        .lock()
        .expect("pane transcript mutex must not be poisoned")
        .history_limit();
    let mut screen = Screen::new(size, history_limit);
    let mut parser = InputParser::new();
    parser.parse(content, &mut screen);
    transcript
        .lock()
        .expect("pane transcript mutex must not be poisoned")
        .set_screen_for_test(screen);
}

async fn wait_for_capture(
    handler: &RequestHandler,
    target: &PaneTarget,
    needle: &str,
    use_mode_screen: bool,
) -> String {
    for _ in 0..100 {
        let response = handler
            .handle(Request::CapturePane(Box::new(capture_request(
                target.clone(),
                use_mode_screen,
            ))))
            .await;
        let output = response
            .command_output()
            .expect("capture-pane returns command output");
        let text = String::from_utf8_lossy(output.stdout()).into_owned();
        if text.contains(needle) {
            return text;
        }
        sleep(Duration::from_millis(20)).await;
    }

    panic!("capture output never contained {needle}");
}

async fn enter_copy_mode(handler: &RequestHandler, target: &PaneTarget, page_up: bool) -> Response {
    handler
        .handle(Request::CopyMode(CopyModeRequest {
            target: Some(target.clone()),
            page_down: false,
            exit_on_scroll: false,
            hide_position: false,
            mouse_drag_start: false,
            cancel_mode: false,
            scrollbar_scroll: false,
            source: None,
            page_up,
        }))
        .await
}

async fn send_copy_mode_command(
    handler: &RequestHandler,
    target: &PaneTarget,
    tokens: &[&str],
) -> Response {
    send_copy_mode_command_values(
        handler,
        target,
        tokens.iter().map(|token| (*token).to_owned()).collect(),
    )
    .await
}

async fn send_copy_mode_command_values(
    handler: &RequestHandler,
    target: &PaneTarget,
    tokens: Vec<String>,
) -> Response {
    handler
        .handle(Request::SendKeysExt(SendKeysExtRequest {
            target: Some(target.clone()),
            keys: tokens,
            expand_formats: false,
            hex: false,
            literal: false,
            dispatch_key_table: false,
            copy_mode_command: true,
            forward_mouse_event: false,
            reset_terminal: false,
            repeat_count: None,
        }))
        .await
}

fn platform_copy_mode_arg(arg: &str) -> String {
    match arg {
        "cat >/dev/null" => crate::test_shell::stdin_discard_command(),
        _ => arg.to_owned(),
    }
}

fn unique_copy_pipe_output_path(label: &str) -> PathBuf {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock must be after Unix epoch")
        .as_nanos();
    std::env::temp_dir().join(format!(
        "rmux-copy-pipe-{label}-{}-{nanos}.txt",
        std::process::id()
    ))
}

#[cfg(unix)]
fn stdin_to_file_command(path: &Path) -> String {
    format!("cat > {}", crate::test_shell::sh_quote_path(path))
}

#[cfg(windows)]
fn stdin_to_file_command(path: &Path) -> String {
    let quoted_path = crate::test_shell::powershell_quote_path(path);
    crate::test_shell::powershell_encoded_command(&format!(
        "$inputStream=[Console]::OpenStandardInput(); \
         $output=[System.IO.File]::Create({quoted_path}); \
         try {{ $inputStream.CopyTo($output) }} finally {{ $output.Dispose() }}"
    ))
}

async fn set_copy_command(handler: &RequestHandler, command: String) {
    let response = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope: OptionScopeSelector::ServerGlobal,
            name: "copy-command".to_owned(),
            value: Some(command),
            mode: SetOptionMode::Replace,
            only_if_unset: false,
            unset: false,
            unset_pane_overrides: false,
            format: false,
            format_target: None,
        })))
        .await;
    assert!(
        matches!(response, Response::SetOptionByName(_)),
        "set-option copy-command returned {response:?}"
    );
}

async fn prepare_transfer_selection(handler: &RequestHandler, target: &PaneTarget) {
    let response = send_copy_mode_command(handler, target, &["select-line"]).await;
    assert!(matches!(response, Response::SendKeys(_)));
}

#[tokio::test]
async fn copy_mode_capture_uses_backing_screen_snapshot() {
    let handler = RequestHandler::new();
    let size = TerminalSize { cols: 24, rows: 3 };
    let target = create_session(&handler, "alpha", size).await;
    replace_transcript_contents(
        &handler,
        &target,
        size,
        b"line1\r\nline2\r\nline3\r\nline4\r\nline5\r\n",
    )
    .await;

    let response = enter_copy_mode(&handler, &target, true).await;
    assert_eq!(
        response,
        Response::CopyMode(rmux_proto::CopyModeResponse {
            target: target.clone(),
            active: true,
            view_mode: false,
        })
    );

    let mode_capture = wait_for_capture(&handler, &target, "line2", true).await;
    assert_eq!(mode_capture, "line1\nline2\nline3\n");
}

#[tokio::test]
async fn copy_mode_formats_report_live_state() {
    let handler = RequestHandler::new();
    let size = TerminalSize { cols: 40, rows: 4 };
    let target = create_session(&handler, "beta", size).await;
    replace_transcript_contents(
        &handler,
        &target,
        size,
        b"alpha beta gamma\r\nneedle here\r\nomega\r\n",
    )
    .await;

    assert!(matches!(
        enter_copy_mode(&handler, &target, false).await,
        Response::CopyMode(_)
    ));
    assert!(matches!(
        send_copy_mode_command(&handler, &target, &["search-backward", "--", "needle"]).await,
        Response::SendKeys(_)
    ));
    assert!(matches!(
        send_copy_mode_command(&handler, &target, &["select-word"]).await,
        Response::SendKeys(_)
    ));

    let listed = handler
        .handle(Request::ListPanes(ListPanesRequest {
            target: target.session_name().clone(),
            format: Some(
                "#{pane_in_mode} #{pane_mode} #{search_present} #{selection_present} #{copy_cursor_word}".to_owned(),
            ),
            target_window_index: None,
        }))
        .await;
    let output = listed
        .command_output()
        .expect("list-panes returns command output");
    let text = String::from_utf8_lossy(output.stdout());
    assert_eq!(text.as_ref(), "1 copy-mode 1 1 needle\n");
}

#[tokio::test]
async fn copy_mode_command_table_dispatches_all_tmux_commands() {
    const COMMANDS: &[(&str, &[&str])] = &[
        ("append-selection", &[]),
        ("append-selection-and-cancel", &[]),
        ("back-to-indentation", &[]),
        ("begin-selection", &[]),
        ("bottom-line", &[]),
        ("cancel", &[]),
        ("clear-selection", &[]),
        ("copy-end-of-line", &[]),
        ("copy-end-of-line-and-cancel", &[]),
        ("copy-pipe-end-of-line", &["cat >/dev/null"]),
        ("copy-pipe-end-of-line-and-cancel", &["cat >/dev/null"]),
        ("copy-line", &[]),
        ("copy-line-and-cancel", &[]),
        ("copy-pipe-line", &["cat >/dev/null"]),
        ("copy-pipe-line-and-cancel", &["cat >/dev/null"]),
        ("copy-pipe-no-clear", &["cat >/dev/null"]),
        ("copy-pipe", &["cat >/dev/null"]),
        ("copy-pipe-and-cancel", &["cat >/dev/null"]),
        ("copy-selection-no-clear", &[]),
        ("copy-selection", &[]),
        ("copy-selection-and-cancel", &[]),
        ("cursor-down", &[]),
        ("cursor-down-and-cancel", &[]),
        ("cursor-left", &[]),
        ("cursor-right", &[]),
        ("cursor-up", &[]),
        ("cursor-centre-vertical", &[]),
        ("cursor-centre-horizontal", &[]),
        ("end-of-buffer", &[]),
        ("end-of-line", &[]),
        ("goto-line", &["1"]),
        ("halfpage-down", &[]),
        ("halfpage-down-and-cancel", &[]),
        ("halfpage-up", &[]),
        ("history-bottom", &[]),
        ("history-top", &[]),
        ("jump-again", &[]),
        ("jump-backward", &["a"]),
        ("jump-forward", &["a"]),
        ("jump-reverse", &[]),
        ("jump-to-backward", &["a"]),
        ("jump-to-forward", &["a"]),
        ("jump-to-mark", &[]),
        ("next-prompt", &[]),
        ("previous-prompt", &[]),
        ("middle-line", &[]),
        ("next-matching-bracket", &[]),
        ("next-paragraph", &[]),
        ("next-space", &[]),
        ("next-space-end", &[]),
        ("next-word", &[]),
        ("next-word-end", &[]),
        ("other-end", &[]),
        ("page-down", &[]),
        ("page-down-and-cancel", &[]),
        ("page-up", &[]),
        ("pipe-no-clear", &["cat >/dev/null"]),
        ("pipe", &["cat >/dev/null"]),
        ("pipe-and-cancel", &["cat >/dev/null"]),
        ("previous-matching-bracket", &[]),
        ("previous-paragraph", &[]),
        ("previous-space", &[]),
        ("previous-word", &[]),
        ("rectangle-on", &[]),
        ("rectangle-off", &[]),
        ("rectangle-toggle", &[]),
        ("refresh-from-pane", &[]),
        ("scroll-bottom", &[]),
        ("scroll-down", &[]),
        ("scroll-down-and-cancel", &[]),
        ("scroll-exit-on", &[]),
        ("scroll-exit-off", &[]),
        ("scroll-exit-toggle", &[]),
        ("scroll-middle", &[]),
        ("scroll-to-mouse", &[]),
        ("scroll-top", &[]),
        ("scroll-up", &[]),
        ("search-again", &[]),
        ("search-backward", &["alpha"]),
        ("search-backward-text", &["alpha"]),
        ("search-backward-incremental", &["-:alpha"]),
        ("search-forward", &["alpha"]),
        ("search-forward-text", &["alpha"]),
        ("search-forward-incremental", &["+:alpha"]),
        ("search-reverse", &[]),
        ("select-line", &[]),
        ("select-word", &[]),
        ("selection-mode", &["word"]),
        ("set-mark", &[]),
        ("start-of-buffer", &[]),
        ("start-of-line", &[]),
        ("stop-selection", &[]),
        ("toggle-position", &[]),
        ("top-line", &[]),
    ];

    let handler = RequestHandler::new();
    let target = create_session(&handler, "gamma", TerminalSize { cols: 48, rows: 6 }).await;

    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 48, rows: 6 },
        b"(alpha) beta gamma\r\nword_two more words\r\nthird paragraph\r\n\r\nfourth line\r\nlast line\r\n",
    )
    .await;
    wait_for_capture(&handler, &target, "last line", false).await;

    for (command, args) in COMMANDS {
        assert!(matches!(
            enter_copy_mode(&handler, &target, false).await,
            Response::CopyMode(_)
        ));

        match *command {
            "append-selection"
            | "append-selection-and-cancel"
            | "copy-pipe-no-clear"
            | "copy-pipe"
            | "copy-pipe-and-cancel"
            | "copy-selection-no-clear"
            | "copy-selection"
            | "copy-selection-and-cancel"
            | "pipe-no-clear"
            | "pipe"
            | "pipe-and-cancel"
            | "stop-selection" => prepare_transfer_selection(&handler, &target).await,
            "other-end" => {
                prepare_transfer_selection(&handler, &target).await;
                let _ = send_copy_mode_command(&handler, &target, &["cursor-right"]).await;
            }
            "jump-again" | "jump-reverse" => {
                let _ =
                    send_copy_mode_command(&handler, &target, &["jump-forward", "--", "a"]).await;
            }
            "jump-to-mark" => {
                let _ = send_copy_mode_command(&handler, &target, &["set-mark"]).await;
                let _ = send_copy_mode_command(&handler, &target, &["cursor-down"]).await;
            }
            "search-again" | "search-reverse" => {
                let _ =
                    send_copy_mode_command(&handler, &target, &["search-backward", "--", "alpha"])
                        .await;
            }
            _ => {}
        }

        let mut tokens = vec![(*command).to_owned()];
        if !args.is_empty() {
            tokens.push("--".to_owned());
            tokens.extend(args.iter().map(|arg| platform_copy_mode_arg(arg)));
        }
        let response = send_copy_mode_command_values(&handler, &target, tokens).await;
        assert!(
            !matches!(response, Response::Error(_)),
            "{command} returned {response:?}"
        );
    }
}

#[tokio::test]
async fn copy_mode_copy_selection_and_cancel_writes_buffer() {
    let handler = RequestHandler::new();
    let target = create_session(&handler, "delta", TerminalSize { cols: 40, rows: 4 }).await;

    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 40, rows: 4 },
        b"alpha\r\nneedle value\r\nomega\r\n",
    )
    .await;
    wait_for_capture(&handler, &target, "needle", false).await;

    assert!(matches!(
        enter_copy_mode(&handler, &target, false).await,
        Response::CopyMode(_)
    ));
    assert!(matches!(
        send_copy_mode_command(&handler, &target, &["search-backward", "--", "needle"]).await,
        Response::SendKeys(_)
    ));
    assert!(matches!(
        send_copy_mode_command(&handler, &target, &["select-word"]).await,
        Response::SendKeys(_)
    ));
    assert!(matches!(
        send_copy_mode_command(&handler, &target, &["copy-selection-and-cancel"]).await,
        Response::SendKeys(_)
    ));

    let buffer = handler
        .handle(Request::ShowBuffer(ShowBufferRequest { name: None }))
        .await;
    let output = buffer.command_output().expect("show-buffer returns output");
    assert!(String::from_utf8_lossy(output.stdout()).contains("needle"));
}

#[tokio::test]
async fn copy_pipe_without_command_uses_copy_command_option() {
    let handler = RequestHandler::new();
    let target = create_session(&handler, "copy-command", TerminalSize { cols: 40, rows: 4 }).await;
    let output_path = unique_copy_pipe_output_path("fallback");
    set_copy_command(&handler, stdin_to_file_command(&output_path)).await;

    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 40, rows: 4 },
        b"alpha\r\nneedle fallback\r\nomega\r\n",
    )
    .await;
    wait_for_capture(&handler, &target, "needle fallback", false).await;

    assert!(matches!(
        enter_copy_mode(&handler, &target, false).await,
        Response::CopyMode(_)
    ));
    assert!(matches!(
        send_copy_mode_command(&handler, &target, &["search-backward", "--", "needle"]).await,
        Response::SendKeys(_)
    ));
    assert!(matches!(
        send_copy_mode_command(&handler, &target, &["select-line"]).await,
        Response::SendKeys(_)
    ));
    assert!(matches!(
        send_copy_mode_command(&handler, &target, &["copy-pipe-and-cancel"]).await,
        Response::SendKeys(_)
    ));

    let output = fs::read_to_string(&output_path).expect("copy-command should write selection");
    let _ = fs::remove_file(&output_path);
    assert!(output.contains("needle fallback"));
}

#[tokio::test]
async fn copy_pipe_explicit_command_overrides_copy_command_option() {
    let handler = RequestHandler::new();
    let target = create_session(
        &handler,
        "copy-command-explicit",
        TerminalSize { cols: 40, rows: 4 },
    )
    .await;
    let fallback_path = unique_copy_pipe_output_path("fallback-unused");
    let explicit_path = unique_copy_pipe_output_path("explicit");
    set_copy_command(&handler, stdin_to_file_command(&fallback_path)).await;

    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 40, rows: 4 },
        b"alpha\r\nneedle explicit\r\nomega\r\n",
    )
    .await;
    wait_for_capture(&handler, &target, "needle explicit", false).await;

    assert!(matches!(
        enter_copy_mode(&handler, &target, false).await,
        Response::CopyMode(_)
    ));
    assert!(matches!(
        send_copy_mode_command(&handler, &target, &["search-backward", "--", "needle"]).await,
        Response::SendKeys(_)
    ));
    assert!(matches!(
        send_copy_mode_command(&handler, &target, &["select-line"]).await,
        Response::SendKeys(_)
    ));
    let explicit_command = stdin_to_file_command(&explicit_path);
    assert!(matches!(
        send_copy_mode_command_values(
            &handler,
            &target,
            vec![
                "copy-pipe-and-cancel".to_owned(),
                "--".to_owned(),
                explicit_command,
            ],
        )
        .await,
        Response::SendKeys(_)
    ));

    let explicit_output =
        fs::read_to_string(&explicit_path).expect("explicit pipe command should write selection");
    let fallback_output = fs::read_to_string(&fallback_path).ok();
    let _ = fs::remove_file(&explicit_path);
    let _ = fs::remove_file(&fallback_path);
    assert!(explicit_output.contains("needle explicit"));
    assert!(
        fallback_output.is_none(),
        "copy-command fallback should not run when copy-pipe has an explicit command"
    );
}
