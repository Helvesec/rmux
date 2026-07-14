use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

use super::RequestHandler;
use rmux_core::{input::InputParser, Screen};
use rmux_proto::types::OptionScopeSelector;
#[cfg(unix)]
use rmux_proto::ListBuffersRequest;
use rmux_proto::{
    CapturePaneRequest, CapturePaneTargetActionRequest, LoadBufferRequest, NewSessionRequest,
    PaneTarget, Request, Response, SaveBufferRequest, SendKeysRequest, SetBufferRequest,
    SetOptionByNameRequest, SetOptionMode, ShowBufferRequest, TerminalSize,
};
use tokio::time::sleep;

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

    assert!(matches!(response, Response::NewSession(_)));
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

async fn append_to_transcript(handler: &RequestHandler, target: &PaneTarget, bytes: &[u8]) {
    let transcript = {
        let state = handler.state.lock().await;
        state
            .transcript_handle(target)
            .expect("session transcript must exist")
    };
    transcript
        .lock()
        .expect("pane transcript mutex must not be poisoned")
        .append_bytes(bytes);
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

    append_to_transcript(
        &handler,
        &target,
        b"\x1b[2J\x1b[H\x1b[?1049hALTLINE\r\n\x1b[?1049lMAINLINE\r\n",
    )
    .await;

    let response = handler
        .handle(Request::CapturePane(Box::new(capture_pane_request(
            target, None, None, true, None,
        ))))
        .await;
    let output = response
        .command_output()
        .expect("capture-pane -p returns command output");
    let output = String::from_utf8(output.stdout().to_vec()).expect("capture output is utf8");
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
