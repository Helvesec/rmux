use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::RequestHandler;
use rmux_core::{input::InputParser, GridRenderOptions, Screen, ScreenCaptureRange};
use rmux_proto::types::OptionScopeSelector;
#[cfg(unix)]
use rmux_proto::ListBuffersRequest;
use rmux_proto::{
    CapturePaneRequest, CapturePaneTargetActionRequest, CopyModeRequest, LoadBufferRequest,
    NewSessionRequest, OptionName, PaneOptionSetRequest, PaneTarget, PaneTargetRef, Request,
    Response, SaveBufferRequest, ScopeSelector, SendKeysRequest, SetBufferRequest,
    SetOptionByNameRequest, SetOptionMode, SetOptionRequest, ShowBufferRequest, SplitDirection,
    SplitWindowRequest, SplitWindowTarget, TerminalSize, WindowTarget,
};
use tokio::time::sleep;

#[path = "handler_capture_tests/hlink1.rs"]
mod hlink1;

#[path = "handler_capture_tests/capture_format.rs"]
mod capture_format;

#[path = "handler_capture_tests/capture_format_neighbors.rs"]
mod capture_format_neighbors;

#[path = "handler_capture_tests/capture_format_empty_print.rs"]
mod capture_format_empty_print;

static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

fn session_name(value: &str) -> rmux_proto::SessionName {
    rmux_proto::SessionName::new(value).expect("valid session name")
}

fn capture_pane_request(
    target: PaneTarget,
    start: Option<i64>,
    end: Option<i64>,
    print: bool,
    buffer_name: Option<&str>,
) -> CapturePaneRequest {
    CapturePaneRequest {
        target,
        start,
        end,
        print,
        buffer_name: buffer_name.map(str::to_owned),
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
    }
}

fn capture_stdout(response: Response) -> Vec<u8> {
    let Response::CapturePane(response) = response else {
        panic!("expected capture-pane response, got {response:?}");
    };
    response
        .command_output()
        .expect("capture-pane -p returns command output")
        .stdout()
        .to_vec()
}

fn set_buffer_request(name: &str, content: &[u8]) -> SetBufferRequest {
    SetBufferRequest {
        name: Some(name.to_owned()),
        content: content.to_vec(),
        append: false,
        new_name: None,
        set_clipboard: false,
        target_client: None,
    }
}

fn load_buffer_request(
    path: &std::path::Path,
    cwd: Option<std::path::PathBuf>,
    name: &str,
) -> LoadBufferRequest {
    LoadBufferRequest {
        path: path.display().to_string(),
        cwd,
        name: Some(name.to_owned()),
        set_clipboard: false,
        target_client: None,
    }
}

fn save_buffer_request(
    path: &std::path::Path,
    cwd: Option<std::path::PathBuf>,
    name: &str,
) -> SaveBufferRequest {
    SaveBufferRequest {
        path: path.display().to_string(),
        cwd,
        name: Some(name.to_owned()),
        append: false,
    }
}

async fn create_session(handler: &RequestHandler, name: &str) {
    create_session_with_size(handler, name, TerminalSize { cols: 80, rows: 24 }).await;
}

async fn create_session_with_size(handler: &RequestHandler, name: &str, size: TerminalSize) {
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name(name),
            detached: true,
            size: Some(size),
            environment: None,
        }))
        .await;

    crate::session_fixture::assert_new_session(&response);
}

#[tokio::test]
async fn target_action_capture_resolves_raw_target_server_side() {
    let handler = RequestHandler::new();
    create_session_with_size(&handler, "alpha", TerminalSize { cols: 20, rows: 4 }).await;
    let target = PaneTarget::with_window(session_name("alpha"), 0, 0);
    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 20, rows: 4 },
        b"target-capture",
    )
    .await;

    let response = handler
        .handle(Request::CapturePaneTargetAction(Box::new(
            CapturePaneTargetActionRequest {
                target: Some("alpha:0.0".to_owned()),
                start: Some(0),
                end: Some(0),
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
            },
        )))
        .await;
    let Response::CapturePane(response) = response else {
        panic!("expected capture-pane response, got {response:?}");
    };
    let output = response
        .command_output()
        .expect("capture-pane -p returns command output");
    assert_eq!(output.stdout(), b"target-capture\n");
}

#[tokio::test]
async fn direct_and_target_action_capture_join_stop_at_active_alternate_boundary() {
    let handler = RequestHandler::new();
    create_session_with_size(&handler, "alpha", TerminalSize { cols: 8, rows: 2 }).await;
    let target = PaneTarget::with_window(session_name("alpha"), 0, 0);
    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 8, rows: 2 },
        b"abcdefghijkl\r\n\x1b[?1049h\x1b[HVIM",
    )
    .await;

    let mut direct = capture_pane_request(target, None, None, true, None);
    direct.join_wrapped = true;
    direct.start_is_absolute = true;
    let direct = handler.handle(Request::CapturePane(Box::new(direct))).await;
    assert_eq!(capture_stdout(direct), b"abcdefgh\nVIM\n\n");

    let target_action = handler
        .handle(Request::CapturePaneTargetAction(Box::new(
            CapturePaneTargetActionRequest {
                target: Some("alpha:0.0".to_owned()),
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
                join_wrapped: true,
                use_mode_screen: false,
                preserve_trailing_spaces: false,
                do_not_trim_spaces: false,
                pending_input: false,
                quiet: false,
                start_is_absolute: true,
                end_is_absolute: false,
            },
        )))
        .await;
    assert_eq!(capture_stdout(target_action), b"abcdefgh\nVIM\n\n");

    let queued = handler
        .parse_control_commands("capture-pane -pJ -S - -t alpha:0.0")
        .await
        .expect("queued capture-pane parses");
    let queued = handler
        .execute_parsed_commands_for_test(std::process::id(), queued)
        .await
        .expect("queued capture-pane executes");
    assert_eq!(queued.stdout(), b"abcdefgh\nVIM\n\n");
}

#[tokio::test]
async fn formatted_join_capture_stops_at_all_active_alternate_boundaries() {
    let handler = RequestHandler::new();

    for mode in [47, 1047, 1049] {
        for marked in [false, true] {
            let name = format!("formatted-boundary-{mode}-{marked}");
            create_session_with_size(&handler, &name, TerminalSize { cols: 8, rows: 2 }).await;
            let target = PaneTarget::with_window(session_name(&name), 0, 0);
            let prompt_mark = if marked { "\x1b]133;A\x07" } else { "" };
            let output_mark = if marked { "\x1b]133;C\x07" } else { "" };
            let content = format!(
                "{prompt_mark}\x1b[31mabcdefghijkl\x1b[0m\r\n\
                 \x1b[?{mode}h\x1b[H{output_mark}\x1b[32mVIM\x1b[0m"
            );
            replace_transcript_contents(
                &handler,
                &target,
                TerminalSize { cols: 8, rows: 2 },
                content.as_bytes(),
            )
            .await;

            let mut request = capture_pane_request(target, None, None, true, None);
            request.start_is_absolute = true;
            request.include_format = true;
            request.join_wrapped = true;
            let response = handler
                .handle(Request::CapturePane(Box::new(request)))
                .await;
            let expected = if marked {
                b"PW abcdefgh\nO VIM\n- \n".as_slice()
            } else {
                b"W abcdefgh\n- VIM\n- \n".as_slice()
            };
            assert_eq!(
                capture_stdout(response),
                expected,
                "mode={mode}, marked={marked}"
            );
        }
    }
}

#[tokio::test]
async fn formatted_join_capture_boundary_covers_ranges_ansi_saved_and_copy_mode() {
    let handler = RequestHandler::new();
    create_session_with_size(
        &handler,
        "formatted-boundary-neighbours",
        TerminalSize { cols: 8, rows: 2 },
    )
    .await;
    let target = PaneTarget::with_window(session_name("formatted-boundary-neighbours"), 0, 0);
    replace_transcript_contents(
        &handler,
        &target,
        TerminalSize { cols: 8, rows: 2 },
        b"\x1b]133;A\x07\x1b[31mabcdefghijkl\x1b[0m\r\n\
          \x1b[?1049h\x1b[H\x1b]133;C\x07\x1b[32mVIM\x1b[0m",
    )
    .await;

    let mut ansi = capture_pane_request(target.clone(), None, None, true, None);
    ansi.start_is_absolute = true;
    ansi.include_format = true;
    ansi.join_wrapped = true;
    ansi.escape_ansi = true;
    let response = handler.handle(Request::CapturePane(Box::new(ansi))).await;
    assert_eq!(
        capture_stdout(response),
        b"PW \x1b[31mabcdefgh\x1b[39m\nO \x1b[32mVIM\x1b[39m\n- \n"
    );

    let mut history_only = capture_pane_request(target.clone(), Some(-1), Some(-1), true, None);
    history_only.include_format = true;
    history_only.join_wrapped = true;
    let response = handler
        .handle(Request::CapturePane(Box::new(history_only)))
        .await;
    assert_eq!(capture_stdout(response), b"PW abcdefgh\n");

    let mut alternate_only = capture_pane_request(target.clone(), Some(0), Some(0), true, None);
    alternate_only.include_format = true;
    alternate_only.join_wrapped = true;
    let response = handler
        .handle(Request::CapturePane(Box::new(alternate_only)))
        .await;
    assert_eq!(capture_stdout(response), b"O VIM\n");

    let mut crossing = capture_pane_request(target.clone(), Some(-1), Some(0), true, None);
    crossing.include_format = true;
    crossing.join_wrapped = true;
    let response = handler
        .handle(Request::CapturePane(Box::new(crossing)))
        .await;
    assert_eq!(capture_stdout(response), b"PW abcdefgh\nO VIM\n");

    let mut saved = capture_pane_request(target.clone(), Some(0), Some(1), true, None);
    saved.alternate = true;
    saved.include_format = true;
    saved.join_wrapped = true;
    let response = handler.handle(Request::CapturePane(Box::new(saved))).await;
    assert_eq!(capture_stdout(response), b"- ijkl\n- \n");

    let response = handler
        .handle(Request::CopyMode(CopyModeRequest {
            target: Some(target.clone()),
            page_down: false,
            exit_on_scroll: false,
            hide_position: false,
            mouse_drag_start: false,
            cancel_mode: false,
            scrollbar_scroll: false,
            source: None,
            page_up: true,
        }))
        .await;
    assert!(matches!(response, Response::CopyMode(_)));

    let mut mode_capture = capture_pane_request(target, Some(0), Some(1), true, None);
    mode_capture.include_format = true;
    mode_capture.join_wrapped = true;
    mode_capture.use_mode_screen = true;
    let response = handler
        .handle(Request::CapturePane(Box::new(mode_capture)))
        .await;
    assert_eq!(capture_stdout(response), b"PW abcdefgh\nO VIM\n");
}

#[tokio::test]
async fn named_capture_buffer_keeps_dch_field_boundary_like_tmux_3_7b() {
    let handler = RequestHandler::new();
    let size = TerminalSize { cols: 12, rows: 8 };
    create_session_with_size(&handler, "mutations", size).await;
    let target = PaneTarget::with_window(session_name("mutations"), 0, 0);
    replace_transcript_contents(
        &handler,
        &target,
        size,
        b"ABCDEFGHIJKLmnopqrstuvwx012345678\r\nNXT\r\nEND\
          \x1b[r\x1b[2;1H\x1b[99P",
    )
    .await;

    let mut capture = capture_pane_request(target, None, None, false, Some("mutation-consumer"));
    capture.join_wrapped = true;
    let response = handler
        .handle(Request::CapturePane(Box::new(capture)))
        .await;
    let Response::CapturePane(response) = response else {
        panic!("expected capture-pane buffer response, got {response:?}");
    };
    assert_eq!(response.buffer_name.as_deref(), Some("mutation-consumer"));

    let shown = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("mutation-consumer".to_owned()),
        }))
        .await;
    let bytes = shown
        .command_output()
        .expect("show-buffer returns captured bytes")
        .stdout()
        .to_vec();
    let nonempty = bytes
        .split(|byte| *byte == b'\n')
        .filter(|field| !field.is_empty())
        .collect::<Vec<_>>();

    assert_eq!(
        nonempty,
        [
            b"ABCDEFGHIJKL".as_slice(),
            b"012345678".as_slice(),
            b"NXT".as_slice(),
            b"END".as_slice(),
        ]
    );
}

async fn replace_transcript_contents(
    handler: &RequestHandler,
    target: &PaneTarget,
    size: TerminalSize,
    content: &[u8],
) {
    // The pane runs a real producer. Until its startup output has landed, this
    // synthetic Screen can still be overwritten, and the loss only becomes
    // observable later - on Windows the alternate screen loses its OSC133 mark
    // and content by the time copy mode reads it. Wait for observable startup
    // first, exactly as handler_copy_mode_tests does before the same
    // replacement. This settles the producer; it does not silence it. On Unix
    // `RequestHandler::new()` already runs the quiet child; Windows keeps its
    // real shell here.
    handler
        .wait_for_pane_startup_to_finish_for_test(target)
        .await;
    let transcript = {
        let state = handler.state.lock().await;
        state
            .transcript_handle(target)
            .expect("session transcript must exist")
    };
    let history_limit = transcript.lock().history_limit();
    let mut screen = Screen::new(size, history_limit);
    let mut parser = InputParser::new();
    parser.parse(content, &mut screen);
    transcript.lock().set_screen_for_test(screen);
}

async fn send_marker(handler: &RequestHandler, target: PaneTarget, marker: &str) {
    let response = handler
        .handle(Request::SendKeys(SendKeysRequest {
            target,
            keys: vec![marker_print_command(marker), "Enter".to_owned()],
        }))
        .await;

    assert!(matches!(response, Response::SendKeys(_)));
}

#[cfg(unix)]
fn marker_print_command(marker: &str) -> String {
    format!("printf '{marker}\\n'")
}

#[cfg(windows)]
fn marker_print_command(marker: &str) -> String {
    format!("echo {marker}")
}

async fn wait_for_capture(handler: &RequestHandler, target: PaneTarget, marker: &str) -> Vec<u8> {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut last_stdout = Vec::new();
    while Instant::now() < deadline {
        let response = handler
            .handle(Request::CapturePane(Box::new(capture_pane_request(
                target.clone(),
                None,
                None,
                true,
                None,
            ))))
            .await;

        let output = response
            .command_output()
            .expect("capture-pane -p returns command output");
        last_stdout = output.stdout().to_vec();
        if String::from_utf8_lossy(output.stdout()).contains(marker) {
            return output.stdout().to_vec();
        }

        sleep(Duration::from_millis(20)).await;
    }

    panic!(
        "capture output never contained marker {marker}; last stdout: {:?}",
        String::from_utf8_lossy(&last_stdout)
    );
}

#[tokio::test]
async fn capture_pane_prints_transcript_without_creating_buffer() {
    let handler = RequestHandler::new();
    let target = PaneTarget::with_window(session_name("alpha"), 0, 0);
    let marker = "handler_capture_print_marker";

    create_session(&handler, "alpha").await;
    send_marker(&handler, target.clone(), marker).await;

    let output = wait_for_capture(&handler, target, marker).await;
    assert!(String::from_utf8_lossy(&output).contains(marker));

    let show = handler
        .handle(Request::ShowBuffer(ShowBufferRequest { name: None }))
        .await;
    assert!(matches!(show, Response::Error(_)));
}

#[tokio::test]
async fn capture_pane_writes_named_buffer() {
    let handler = RequestHandler::new();
    let target = PaneTarget::with_window(session_name("alpha"), 0, 0);
    let marker = "handler_capture_buffer_marker";

    create_session(&handler, "alpha").await;
    send_marker(&handler, target.clone(), marker).await;
    wait_for_capture(&handler, target.clone(), marker).await;

    let capture = handler
        .handle(Request::CapturePane(Box::new(capture_pane_request(
            target,
            None,
            None,
            false,
            Some("capture-buffer"),
        ))))
        .await;
    match capture {
        Response::CapturePane(response) => {
            assert_eq!(response.buffer_name.as_deref(), Some("capture-buffer"));
            assert!(response.command_output().is_none());
        }
        other => panic!("expected capture response, got {other:?}"),
    }

    let show = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("capture-buffer".to_owned()),
        }))
        .await;
    let output = show.command_output().expect("show-buffer returns output");
    assert!(String::from_utf8_lossy(output.stdout()).contains(marker));
}

#[tokio::test]
async fn capture_pane_do_not_trim_uses_tmux_cell_capacity() {
    let handler = RequestHandler::new();
    let target = PaneTarget::with_window(session_name("capacity"), 0, 0);
    let size = TerminalSize { cols: 20, rows: 6 };

    create_session_with_size(&handler, "capacity", size).await;
    replace_transcript_contents(&handler, &target, size, b"a\r\nabcde\r\nabcdefghij\r\n").await;

    let mut request = capture_pane_request(target, None, None, true, None);
    request.do_not_trim_spaces = true;
    let response = handler
        .handle(Request::CapturePane(Box::new(request)))
        .await;
    let output = response
        .command_output()
        .expect("capture-pane -Np returns command output");
    let output = String::from_utf8(output.stdout().to_vec()).expect("capture output is utf-8");

    assert_eq!(output, "a    \nabcde     \nabcdefghij          \n\n\n\n");
}

#[tokio::test]
async fn alternate_screen_off_keeps_program_output_on_main_screen() {
    let handler = RequestHandler::new();
    let target = PaneTarget::with_window(session_name("altscreen"), 0, 0);
    create_session_with_size(&handler, "altscreen", TerminalSize { cols: 20, rows: 5 }).await;

    let response = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope: OptionScopeSelector::WindowGlobal,
            name: "alternate-screen".to_owned(),
            value: Some("off".to_owned()),
            mode: SetOptionMode::Replace,
            only_if_unset: false,
            unset: false,
            unset_pane_overrides: false,
            format: false,
            format_target: None,
        })))
        .await;
    assert!(matches!(response, Response::SetOptionByName(_)));

    let transcript = {
        let state = handler.state.lock().await;
        state
            .transcript_handle(&target)
            .expect("session transcript must exist")
    };
    let output = {
        let mut transcript = transcript.lock();
        transcript.append_bytes(b"\x1b[2J\x1b[H\x1b[?1049hALTLINE\r\n\x1b[?1049lMAINLINE\r\n");
        assert!(!transcript.is_alternate());
        transcript.capture_main(ScreenCaptureRange::default(), GridRenderOptions::default())
    };
    let output = String::from_utf8(output).expect("capture output is utf8");
    assert!(output.contains("ALTLINE"), "{output:?}");
    assert!(output.contains("MAINLINE"), "{output:?}");
}

#[tokio::test]
async fn load_buffer_reads_server_file() {
    let handler = RequestHandler::new();
    let path = temp_path("load-success");
    std::fs::write(&path, b"loaded data").expect("write input");

    let response = handler
        .handle(Request::LoadBuffer(Box::new(load_buffer_request(
            &path, None, "loaded",
        ))))
        .await;
    match response {
        Response::LoadBuffer(response) => assert_eq!(response.buffer_name, "loaded"),
        other => panic!("expected load-buffer response, got {other:?}"),
    }

    let show = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("loaded".to_owned()),
        }))
        .await;
    assert_eq!(
        show.command_output()
            .expect("show-buffer returns output")
            .stdout(),
        b"loaded data"
    );

    let _ = std::fs::remove_file(path);
}

#[cfg(unix)]
#[tokio::test]
async fn load_buffer_waiting_on_fifo_does_not_block_other_requests() {
    let handler = RequestHandler::new();
    let path = temp_path("load-fifo");
    let output = std::process::Command::new("mkfifo")
        .arg(&path)
        .output()
        .expect("run mkfifo");
    assert!(
        output.status.success(),
        "mkfifo failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let writer_path = path.clone();
    let writer = std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(2));
        std::fs::write(writer_path, b"fifo data").expect("write fifo");
    });
    let load_handler = handler.clone();
    let load_path = path.clone();
    let load = tokio::spawn(async move {
        load_handler
            .handle(Request::LoadBuffer(Box::new(load_buffer_request(
                &load_path, None, "fifo",
            ))))
            .await
    });

    let concurrent_response = tokio::time::timeout(Duration::from_millis(500), async {
        tokio::task::yield_now().await;
        handler
            .handle(Request::ListBuffers(ListBuffersRequest::default()))
            .await
    })
    .await
    .expect("a blocked FIFO read must not stall unrelated daemon requests");
    assert!(matches!(concurrent_response, Response::ListBuffers(_)));

    let load_response = load.await.expect("load-buffer task should finish");
    assert!(matches!(load_response, Response::LoadBuffer(_)));
    writer.join().expect("FIFO writer should finish");
    let _ = std::fs::remove_file(path);
}

#[tokio::test]
async fn load_buffer_failure_does_not_mutate_existing_buffer() {
    let handler = RequestHandler::new();
    let missing_path = temp_path("load-missing");

    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            "stable",
            b"original",
        ))))
        .await;

    let response = handler
        .handle(Request::LoadBuffer(Box::new(load_buffer_request(
            &missing_path,
            None,
            "stable",
        ))))
        .await;
    assert!(matches!(response, Response::Error(_)));

    let show = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("stable".to_owned()),
        }))
        .await;
    assert_eq!(
        show.command_output()
            .expect("show-buffer returns output")
            .stdout(),
        b"original"
    );
}

#[tokio::test]
async fn load_buffer_resolves_relative_path_against_request_cwd() {
    let handler = RequestHandler::new();
    let root = temp_path("load-relative-root");
    let nested_dir = root.join("nested");
    std::fs::create_dir_all(&nested_dir).expect("create nested dir");
    std::fs::write(nested_dir.join("input.txt"), b"relative data").expect("write input");

    let response = handler
        .handle(Request::LoadBuffer(Box::new(load_buffer_request(
            &std::path::Path::new("nested").join("input.txt"),
            Some(root.clone()),
            "loaded",
        ))))
        .await;
    match response {
        Response::LoadBuffer(response) => assert_eq!(response.buffer_name, "loaded"),
        other => panic!("expected load-buffer response, got {other:?}"),
    }

    let show = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("loaded".to_owned()),
        }))
        .await;
    assert_eq!(
        show.command_output()
            .expect("show-buffer returns output")
            .stdout(),
        b"relative data"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn save_buffer_writes_server_file() {
    let handler = RequestHandler::new();
    let path = temp_path("save-success");

    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            "saved", b"save me",
        ))))
        .await;

    let response = handler
        .handle(Request::SaveBuffer(save_buffer_request(
            &path, None, "saved",
        )))
        .await;
    match response {
        Response::SaveBuffer(response) => assert_eq!(response.buffer_name, "saved"),
        other => panic!("expected save-buffer response, got {other:?}"),
    }
    assert_eq!(std::fs::read(&path).expect("read saved file"), b"save me");

    let _ = std::fs::remove_file(path);
}

#[cfg(unix)]
#[tokio::test]
async fn save_buffer_waiting_on_fifo_does_not_block_other_requests() {
    for append in [false, true] {
        let handler = RequestHandler::new();
        let path = temp_path(if append {
            "save-append-fifo"
        } else {
            "save-overwrite-fifo"
        });
        let output = std::process::Command::new("mkfifo")
            .arg(&path)
            .output()
            .expect("run mkfifo");
        assert!(
            output.status.success(),
            "mkfifo failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );

        handler
            .handle(Request::SetBuffer(Box::new(set_buffer_request(
                "saved",
                b"fifo data",
            ))))
            .await;

        let reader_path = path.clone();
        let reader = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_secs(2));
            std::fs::read(reader_path).expect("read fifo")
        });
        let save_handler = handler.clone();
        let save_path = path.clone();
        let save = tokio::spawn(async move {
            let mut request = save_buffer_request(&save_path, None, "saved");
            request.append = append;
            save_handler.handle(Request::SaveBuffer(request)).await
        });

        let concurrent_response = tokio::time::timeout(Duration::from_millis(500), async {
            tokio::task::yield_now().await;
            handler
                .handle(Request::ListBuffers(ListBuffersRequest::default()))
                .await
        })
        .await
        .expect("a blocked FIFO write must not stall unrelated daemon requests");
        assert!(matches!(concurrent_response, Response::ListBuffers(_)));

        let save_response = save.await.expect("save-buffer task should finish");
        assert!(matches!(save_response, Response::SaveBuffer(_)));
        assert_eq!(
            reader.join().expect("FIFO reader should finish"),
            b"fifo data"
        );
        let _ = std::fs::remove_file(path);
    }
}

#[tokio::test]
async fn save_buffer_resolves_relative_path_against_request_cwd() {
    let handler = RequestHandler::new();
    let root = temp_path("save-relative-root");
    let nested_dir = root.join("nested");
    std::fs::create_dir_all(&nested_dir).expect("create nested dir");

    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            "saved",
            b"relative save",
        ))))
        .await;

    let response = handler
        .handle(Request::SaveBuffer(save_buffer_request(
            &std::path::Path::new("nested").join("output.txt"),
            Some(root.clone()),
            "saved",
        )))
        .await;
    match response {
        Response::SaveBuffer(response) => assert_eq!(response.buffer_name, "saved"),
        other => panic!("expected save-buffer response, got {other:?}"),
    }
    assert_eq!(
        std::fs::read(nested_dir.join("output.txt")).expect("read saved file"),
        b"relative save"
    );

    let _ = std::fs::remove_dir_all(root);
}

#[tokio::test]
async fn save_buffer_failure_does_not_mutate_existing_buffer() {
    let handler = RequestHandler::new();
    let path = temp_path("missing-parent").join("out.txt");

    handler
        .handle(Request::SetBuffer(Box::new(set_buffer_request(
            "stable",
            b"original",
        ))))
        .await;

    let response = handler
        .handle(Request::SaveBuffer(save_buffer_request(
            &path, None, "stable",
        )))
        .await;
    assert!(matches!(response, Response::Error(_)));

    let show = handler
        .handle(Request::ShowBuffer(ShowBufferRequest {
            name: Some("stable".to_owned()),
        }))
        .await;
    assert_eq!(
        show.command_output()
            .expect("show-buffer returns output")
            .stdout(),
        b"original"
    );
}

fn temp_path(label: &str) -> std::path::PathBuf {
    let unique_id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "rmux-handler-{label}-{}-{unique_id}",
        std::process::id()
    ))
}

// ---------------------------------------------------------------------------
// `scroll-on-clear` option effect.
//
// These fixtures prove the option mutation reaches the live pane screen: a
// whole-screen erase either moves the used visible rows into scrollback ("on",
// the default) or discards them in place ("off"). The discriminator is the
// history delta plus whether the erased marker survives a capture that spans
// history, exactly the pair tmux 3.7b separates.
//
// Every pane runs the quiet `default-command` producer so the only bytes in the
// transcript are the ones the fixture injects; the producer is still settled
// with `wait_for_pane_startup_to_finish_for_test` before the first injection.
// The screen is never replaced with `set_screen_for_test` here, because that
// would install a fresh `Screen` and silently drop the pushed policy.
// ---------------------------------------------------------------------------

/// Pane height used by the `scroll-on-clear` fixtures.
const SCROLL_ON_CLEAR_ROWS: u16 = 6;
/// Rows the seed writes, i.e. the exact number of used rows an "on" erase moves
/// into scrollback.
const SCROLL_ON_CLEAR_SEEDED_ROWS: usize = 3;

#[derive(Debug)]
struct ScrollOnClearObservation {
    history_delta: usize,
    visible: String,
    full: String,
}

/// Blanks every row with `EL`, then writes three marker rows at the top.
///
/// `EL` and absolute cursor addressing are deliberately used instead of `ED 2`:
/// the seed must not itself consult the policy under test, and it must leave a
/// used-row count that does not depend on whatever the pane held before.
fn scroll_on_clear_seed_bytes(marker: &str) -> Vec<u8> {
    let mut bytes = String::new();
    for row in 1..=SCROLL_ON_CLEAR_ROWS {
        bytes.push_str(&format!("\x1b[{row};1H\x1b[K"));
    }
    for (offset, suffix) in ["A", "B", "C"].iter().enumerate() {
        bytes.push_str(&format!("\x1b[{};1H{marker}{suffix}", offset + 1));
    }
    bytes.into_bytes()
}

async fn scroll_on_clear_transcript(
    handler: &RequestHandler,
    target: &PaneTarget,
) -> crate::pane_transcript::SharedPaneTranscript {
    let state = handler.state.lock().await;
    state
        .transcript_handle(target)
        .expect("pane transcript must exist")
}

async fn scroll_on_clear_append(handler: &RequestHandler, target: &PaneTarget, bytes: &[u8]) {
    let transcript = scroll_on_clear_transcript(handler, target).await;
    transcript.lock().append_bytes(bytes);
}

async fn scroll_on_clear_history_size(handler: &RequestHandler, target: &PaneTarget) -> usize {
    let transcript = scroll_on_clear_transcript(handler, target).await;
    let size = transcript.lock().history_size();
    size
}

async fn scroll_on_clear_capture(
    handler: &RequestHandler,
    target: &PaneTarget,
    start: Option<i64>,
    end: Option<i64>,
) -> String {
    let response = handler
        .handle(Request::CapturePane(Box::new(capture_pane_request(
            target.clone(),
            start,
            end,
            true,
            None,
        ))))
        .await;
    String::from_utf8(capture_stdout(response)).expect("capture output is utf8")
}

async fn scroll_on_clear_set(
    handler: &RequestHandler,
    scope: OptionScopeSelector,
    value: Option<&str>,
    unset: bool,
) {
    let response = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope,
            name: "scroll-on-clear".to_owned(),
            value: value.map(str::to_owned),
            mode: SetOptionMode::Replace,
            only_if_unset: false,
            unset,
            unset_pane_overrides: false,
            format: false,
            format_target: None,
        })))
        .await;
    assert!(
        matches!(response, Response::SetOptionByName(_)),
        "{response:?}"
    );
}

/// Creates a detached session whose panes run the quiet stdin-discard command,
/// and returns the settled first pane.
async fn scroll_on_clear_session(handler: &RequestHandler, name: &str) -> PaneTarget {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::DefaultCommand,
            value: crate::test_shell::stdin_discard_command(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
    create_session_with_size(
        handler,
        name,
        TerminalSize {
            cols: 40,
            rows: SCROLL_ON_CLEAR_ROWS,
        },
    )
    .await;
    let target = PaneTarget::with_window(session_name(name), 0, 0);
    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    target
}

async fn scroll_on_clear_split(handler: &RequestHandler, target: &PaneTarget) -> PaneTarget {
    let response = handler
        .handle(Request::SplitWindow(SplitWindowRequest {
            target: SplitWindowTarget::Pane(target.clone()),
            // Side-by-side keeps both panes at the full fixture height.
            direction: SplitDirection::Horizontal,
            before: false,
            environment: None,
        }))
        .await;
    let pane = match response {
        Response::SplitWindow(response) => response.pane,
        response => panic!("expected split-window success, got {response:?}"),
    };
    handler
        .wait_for_pane_startup_to_finish_for_test(&pane)
        .await;
    pane
}

/// Seeds three marker rows, issues `ED 2`, and reports what the erase did.
async fn scroll_on_clear_erase_and_observe(
    handler: &RequestHandler,
    target: &PaneTarget,
    marker: &str,
) -> ScrollOnClearObservation {
    scroll_on_clear_append(handler, target, &scroll_on_clear_seed_bytes(marker)).await;
    let seeded = scroll_on_clear_capture(handler, target, None, None).await;
    assert!(
        seeded.contains(&format!("{marker}A")) && seeded.contains(&format!("{marker}C")),
        "seed must be visible before the erase: {seeded:?}"
    );
    let before = scroll_on_clear_history_size(handler, target).await;
    scroll_on_clear_append(handler, target, b"\x1b[2J").await;
    let after = scroll_on_clear_history_size(handler, target).await;
    let visible = scroll_on_clear_capture(handler, target, None, None).await;
    let full = scroll_on_clear_capture(handler, target, Some(-(after as i64)), None).await;
    ScrollOnClearObservation {
        history_delta: after.saturating_sub(before),
        visible,
        full,
    }
}

fn assert_scroll_on_clear_kept(observation: &ScrollOnClearObservation, marker: &str, label: &str) {
    assert_eq!(
        observation.history_delta, SCROLL_ON_CLEAR_SEEDED_ROWS,
        "{label}: the used rows must reach scrollback: {observation:?}"
    );
    assert!(
        !observation.visible.contains(marker),
        "{label}: the viewport must be blank after the erase: {observation:?}"
    );
    assert!(
        observation.full.contains(&format!("{marker}A"))
            && observation.full.contains(&format!("{marker}C")),
        "{label}: a capture spanning history must still hold the rows: {observation:?}"
    );
}

fn assert_scroll_on_clear_discarded(
    observation: &ScrollOnClearObservation,
    marker: &str,
    label: &str,
) {
    assert_eq!(
        observation.history_delta, 0,
        "{label}: history must not grow: {observation:?}"
    );
    assert!(
        !observation.visible.contains(marker),
        "{label}: the viewport must be blank after the erase: {observation:?}"
    );
    assert!(
        !observation.full.contains(marker),
        "{label}: a capture spanning history must not hold the rows: {observation:?}"
    );
}

#[tokio::test]
async fn scroll_on_clear_off_at_window_global_discards_the_erased_rows() {
    let handler = RequestHandler::new();
    let target = scroll_on_clear_session(&handler, "soc-wg-off").await;

    scroll_on_clear_set(
        &handler,
        OptionScopeSelector::WindowGlobal,
        Some("off"),
        false,
    )
    .await;
    let observation = scroll_on_clear_erase_and_observe(&handler, &target, "ZQA").await;
    assert_scroll_on_clear_discarded(&observation, "ZQA", "window-global off");

    // Pre-existing history must survive an "off" erase untouched: seed a row,
    // scroll it into scrollback with "on", switch to "off" and erase again.
    scroll_on_clear_set(
        &handler,
        OptionScopeSelector::WindowGlobal,
        Some("on"),
        false,
    )
    .await;
    let kept = scroll_on_clear_erase_and_observe(&handler, &target, "ZQB").await;
    assert_scroll_on_clear_kept(&kept, "ZQB", "window-global back on");

    scroll_on_clear_set(
        &handler,
        OptionScopeSelector::WindowGlobal,
        Some("off"),
        false,
    )
    .await;
    let discarded = scroll_on_clear_erase_and_observe(&handler, &target, "ZQC").await;
    assert_scroll_on_clear_discarded(&discarded, "ZQC", "window-global off again");
    assert!(
        discarded.full.contains("ZQBA"),
        "the earlier scrollback rows must survive an off erase: {discarded:?}"
    );
}

#[tokio::test]
async fn scroll_on_clear_default_and_explicit_on_keep_the_erased_rows() {
    let handler = RequestHandler::new();
    let target = scroll_on_clear_session(&handler, "soc-wg-on").await;

    // No mutation at all: the table default is "on".
    let defaulted = scroll_on_clear_erase_and_observe(&handler, &target, "ZRA").await;
    assert_scroll_on_clear_kept(&defaulted, "ZRA", "table default");

    scroll_on_clear_set(
        &handler,
        OptionScopeSelector::WindowGlobal,
        Some("on"),
        false,
    )
    .await;
    let explicit = scroll_on_clear_erase_and_observe(&handler, &target, "ZRB").await;
    assert_scroll_on_clear_kept(&explicit, "ZRB", "window-global on");
}

#[tokio::test]
async fn scroll_on_clear_pane_scope_overrides_window_scope() {
    let handler = RequestHandler::new();
    let first = scroll_on_clear_session(&handler, "soc-scope").await;
    let second = scroll_on_clear_split(&handler, &first).await;

    // Window scope applies to every pane that has no override.
    scroll_on_clear_set(
        &handler,
        OptionScopeSelector::Window(WindowTarget::with_window(
            session_name("soc-scope"),
            first.window_index(),
        )),
        Some("off"),
        false,
    )
    .await;
    let inherited = scroll_on_clear_erase_and_observe(&handler, &second, "ZSA").await;
    assert_scroll_on_clear_discarded(&inherited, "ZSA", "window scope reaches an unset pane");

    // The pane-local value wins over the window value.
    scroll_on_clear_set(
        &handler,
        OptionScopeSelector::Pane(first.clone()),
        Some("on"),
        false,
    )
    .await;
    let overridden = scroll_on_clear_erase_and_observe(&handler, &first, "ZSB").await;
    assert_scroll_on_clear_kept(&overridden, "ZSB", "pane override beats window");
    let still_window = scroll_on_clear_erase_and_observe(&handler, &second, "ZSC").await;
    assert_scroll_on_clear_discarded(&still_window, "ZSC", "sibling keeps the window value");

    // `set -p -u` drops the pane override and falls back to the window value.
    scroll_on_clear_set(
        &handler,
        OptionScopeSelector::Pane(first.clone()),
        None,
        true,
    )
    .await;
    let fallback = scroll_on_clear_erase_and_observe(&handler, &first, "ZSD").await;
    assert_scroll_on_clear_discarded(&fallback, "ZSD", "unset pane falls back to window");
}

#[tokio::test]
async fn scroll_on_clear_hot_change_on_a_live_pane_takes_effect() {
    let handler = RequestHandler::new();
    let target = scroll_on_clear_session(&handler, "soc-hot").await;

    scroll_on_clear_set(
        &handler,
        OptionScopeSelector::Pane(target.clone()),
        Some("on"),
        false,
    )
    .await;
    let first = scroll_on_clear_erase_and_observe(&handler, &target, "ZTA").await;
    assert_scroll_on_clear_kept(&first, "ZTA", "hot change: on");

    scroll_on_clear_set(
        &handler,
        OptionScopeSelector::Pane(target.clone()),
        Some("off"),
        false,
    )
    .await;
    let second = scroll_on_clear_erase_and_observe(&handler, &target, "ZTB").await;
    assert_scroll_on_clear_discarded(&second, "ZTB", "hot change: off");

    scroll_on_clear_set(
        &handler,
        OptionScopeSelector::Pane(target.clone()),
        Some("on"),
        false,
    )
    .await;
    let third = scroll_on_clear_erase_and_observe(&handler, &target, "ZTC").await;
    assert_scroll_on_clear_kept(&third, "ZTC", "hot change: back on");

    // The pane was never respawned, so the first "on" rows are still in history.
    assert!(
        third.full.contains("ZTAA"),
        "the live pane kept its scrollback across the hot changes: {third:?}"
    );
}

#[tokio::test]
async fn scroll_on_clear_two_panes_in_one_window_stay_isolated() {
    let handler = RequestHandler::new();
    let keeper = scroll_on_clear_session(&handler, "soc-isolated").await;
    let discarder = scroll_on_clear_split(&handler, &keeper).await;

    scroll_on_clear_set(
        &handler,
        OptionScopeSelector::Pane(keeper.clone()),
        Some("on"),
        false,
    )
    .await;
    scroll_on_clear_set(
        &handler,
        OptionScopeSelector::Pane(discarder.clone()),
        Some("off"),
        false,
    )
    .await;

    // Identical byte streams, one window, opposite outcomes.
    let seed = scroll_on_clear_seed_bytes("ZUA");
    scroll_on_clear_append(&handler, &keeper, &seed).await;
    scroll_on_clear_append(&handler, &discarder, &seed).await;
    let keeper_before = scroll_on_clear_history_size(&handler, &keeper).await;
    let discarder_before = scroll_on_clear_history_size(&handler, &discarder).await;
    scroll_on_clear_append(&handler, &keeper, b"\x1b[2J").await;
    scroll_on_clear_append(&handler, &discarder, b"\x1b[2J").await;
    let keeper_after = scroll_on_clear_history_size(&handler, &keeper).await;
    let discarder_after = scroll_on_clear_history_size(&handler, &discarder).await;

    assert_eq!(
        keeper_after - keeper_before,
        SCROLL_ON_CLEAR_SEEDED_ROWS,
        "the on pane keeps its rows"
    );
    assert_eq!(
        discarder_after, discarder_before,
        "the off pane discards its rows"
    );
    let keeper_full =
        scroll_on_clear_capture(&handler, &keeper, Some(-(keeper_after as i64)), None).await;
    let discarder_full =
        scroll_on_clear_capture(&handler, &discarder, Some(-(discarder_after as i64)), None).await;
    assert!(keeper_full.contains("ZUAA"), "{keeper_full:?}");
    assert!(!discarder_full.contains("ZUA"), "{discarder_full:?}");
}

#[tokio::test]
async fn scroll_on_clear_pane_created_after_the_option_inherits_it() {
    let handler = RequestHandler::new();
    let first = scroll_on_clear_session(&handler, "soc-inherit").await;

    scroll_on_clear_set(
        &handler,
        OptionScopeSelector::WindowGlobal,
        Some("off"),
        false,
    )
    .await;
    // Created only now, so the value can only arrive through the spawn seeding.
    let spawned = scroll_on_clear_split(&handler, &first).await;
    let observation = scroll_on_clear_erase_and_observe(&handler, &spawned, "ZVA").await;
    assert_scroll_on_clear_discarded(&observation, "ZVA", "pane spawned after the mutation");
}

#[tokio::test]
async fn scroll_on_clear_sdk_pane_option_reaches_the_pane_screen() {
    let handler = RequestHandler::new();
    let target = scroll_on_clear_session(&handler, "soc-sdk").await;

    let response = handler
        .handle(Request::PaneOptionSet(PaneOptionSetRequest {
            target: PaneTargetRef::slot(target.clone()),
            name: "scroll-on-clear".to_owned(),
            value: Some("off".to_owned()),
            mode: SetOptionMode::Replace,
            unset: false,
        }))
        .await;
    assert!(
        matches!(response, Response::PaneOptionSet(_)),
        "{response:?}"
    );
    let observation = scroll_on_clear_erase_and_observe(&handler, &target, "ZWA").await;
    assert_scroll_on_clear_discarded(&observation, "ZWA", "sdk pane option set");

    let response = handler
        .handle(Request::PaneOptionSet(PaneOptionSetRequest {
            target: PaneTargetRef::slot(target.clone()),
            name: "scroll-on-clear".to_owned(),
            value: Some("on".to_owned()),
            mode: SetOptionMode::Replace,
            unset: false,
        }))
        .await;
    assert!(
        matches!(response, Response::PaneOptionSet(_)),
        "{response:?}"
    );
    let restored = scroll_on_clear_erase_and_observe(&handler, &target, "ZWB").await;
    assert_scroll_on_clear_kept(&restored, "ZWB", "sdk pane option back on");
}
