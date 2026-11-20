use super::*;
use rmux_proto::{
    encode_frame, ErrorResponse, FrameDecoder, ListPanesResponse, PaneTarget, Request,
    ResizePaneResponse, ResolveTargetResponse, Response, RmuxError, SessionName, Target,
};
use std::io::{Read, Write};
use std::os::unix::net::{UnixListener, UnixStream};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);

fn resize_args(arguments: &[&str]) -> ResizePaneArgs {
    let cli = crate::cli_args::parse(
        ["rmux", "resize-pane"]
            .into_iter()
            .chain(arguments.iter().copied()),
    )
    .expect("resize-pane arguments");
    let Some(crate::cli_args::Command::ResizePane(args)) = cli.command else {
        panic!("resize-pane command")
    };
    args
}

fn pane(index: u32) -> PaneTarget {
    PaneTarget::with_window(SessionName::new("compat").unwrap(), 0, index)
}

fn accept(listener: &UnixListener) -> (UnixStream, FrameDecoder) {
    let (stream, _) = listener.accept().unwrap();
    stream
        .set_read_timeout(Some(std::time::Duration::from_secs(5)))
        .unwrap();
    (stream, FrameDecoder::new())
}

fn read_request(stream: &mut UnixStream, decoder: &mut FrameDecoder) -> Request {
    let mut bytes = [0; 8192];
    loop {
        if let Some(request) = decoder.next_frame().unwrap() {
            return request;
        }
        let count = stream.read(&mut bytes).unwrap();
        assert!(count > 0, "client closed before the expected request");
        decoder.push_bytes(&bytes[..count]);
    }
}

fn respond(stream: &mut UnixStream, response: Response) {
    stream.write_all(&encode_frame(&response).unwrap()).unwrap();
}

fn exchange<F>(arguments: &[&str], serve: F) -> Result<i32, ExitFailure>
where
    F: FnOnce(UnixListener) + Send + 'static,
{
    exchange_parsed(resize_args(arguments), serve)
}

fn exchange_parsed<F>(args: ResizePaneArgs, serve: F) -> Result<i32, ExitFailure>
where
    F: FnOnce(UnixListener) + Send + 'static,
{
    let path = std::env::temp_dir().join(format!(
        "frs1-r2-{}-{}.sock",
        std::process::id(),
        NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
    ));
    let listener = UnixListener::bind(&path).unwrap();
    let server = std::thread::spawn(move || serve(listener));
    let result = run_resize_pane(args, &path);
    let server_result = server.join();
    std::fs::remove_file(path).unwrap();
    server_result.unwrap();
    result
}

fn send_decode(stream: &mut UnixStream) {
    respond(
        stream,
        Response::Error(ErrorResponse {
            error: RmuxError::Decode("legacy peer has no target action variant".into()),
        }),
    );
}

fn receive_percent_target_action(listener: &UnixListener) -> (UnixStream, FrameDecoder) {
    let (mut stream, mut decoder) = accept(listener);
    let Request::ResolveTarget(request) = read_request(&mut stream, &mut decoder) else {
        panic!("expected initial target resolution")
    };
    assert_eq!(request.target.as_deref(), Some("compat:0.1"));
    respond(
        &mut stream,
        Response::ResolveTarget(ResolveTargetResponse {
            target: Target::Pane(pane(1)),
        }),
    );

    assert!(matches!(
        read_request(&mut stream, &mut decoder),
        Request::ListPanes(_)
    ));
    respond(
        &mut stream,
        Response::ListPanes(ListPanesResponse {
            output: CommandOutput::from_stdout("1\t0\t80\t24\t%41\n"),
        }),
    );

    let Request::ResizePaneTargetAction(request) = read_request(&mut stream, &mut decoder) else {
        panic!("expected percent target action")
    };
    assert_eq!(request.target.as_deref(), Some("%41"));
    assert_eq!(
        request.adjustment,
        ResizePaneAdjustment::AbsoluteWidth { columns: 40 }
    );
    (stream, decoder)
}

fn receive_reconnected_client(
    first_stream: &mut UnixStream,
    listener: &UnixListener,
) -> (UnixStream, FrameDecoder) {
    let mut byte = [0];
    assert_eq!(
        first_stream.read(&mut byte).unwrap(),
        0,
        "the refused connection must be abandoned"
    );
    accept(listener)
}

fn accept_before_deadline(listener: &UnixListener) -> Option<(UnixStream, FrameDecoder)> {
    listener.set_nonblocking(true).unwrap();
    let deadline = std::time::Instant::now() + std::time::Duration::from_millis(500);
    loop {
        match listener.accept() {
            Ok((stream, _)) => {
                stream.set_nonblocking(false).unwrap();
                stream
                    .set_read_timeout(Some(std::time::Duration::from_secs(5)))
                    .unwrap();
                return Some((stream, FrameDecoder::new()));
            }
            Err(error)
                if error.kind() == std::io::ErrorKind::WouldBlock
                    && std::time::Instant::now() < deadline =>
            {
                std::thread::sleep(std::time::Duration::from_millis(5));
            }
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => return None,
            Err(error) => panic!("accept: {error}"),
        }
    }
}

fn finish_without_another_connection(stream: &mut UnixStream, listener: &UnixListener) {
    let mut byte = [0];
    assert_eq!(stream.read(&mut byte).unwrap(), 0);
    listener.set_nonblocking(true).unwrap();
    assert!(matches!(
        listener.accept(),
        Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
    ));
}

#[test]
fn frs1_r2_percent_decode_falls_back_using_the_frozen_pane_id() {
    let result = exchange(&["-t", "compat:0.1", "-x", "50%"], |listener| {
        let (mut first_stream, _) = receive_percent_target_action(&listener);
        send_decode(&mut first_stream);
        let (mut stream, mut decoder) = receive_reconnected_client(&mut first_stream, &listener);

        let Request::ResolveTarget(request) = read_request(&mut stream, &mut decoder) else {
            panic!("expected stable-id resolution before legacy resize")
        };
        assert_eq!(request.target.as_deref(), Some("%41"));
        respond(
            &mut stream,
            Response::ResolveTarget(ResolveTargetResponse {
                target: Target::Pane(pane(2)),
            }),
        );

        let Request::ResizePane(request) = read_request(&mut stream, &mut decoder) else {
            panic!("expected legacy resize")
        };
        assert_eq!(request.target, pane(2));
        assert_eq!(
            request.adjustment,
            ResizePaneAdjustment::AbsoluteWidth { columns: 40 }
        );
        respond(
            &mut stream,
            Response::ResizePane(ResizePaneResponse {
                target: request.target,
                adjustment: request.adjustment,
            }),
        );
    });

    assert_eq!(result.unwrap(), 0);
}

#[test]
fn frs1_r2_decode_then_close_allows_known_safe_fallback() {
    let result = exchange(&["-t", "compat:0.1", "-x", "50%"], |listener| {
        let (mut stream, _) = receive_percent_target_action(&listener);
        send_decode(&mut stream);
        stream.shutdown(std::net::Shutdown::Both).unwrap();
        drop(stream);

        let Some((mut stream, mut decoder)) = accept_before_deadline(&listener) else {
            return;
        };
        let Request::ResolveTarget(request) = read_request(&mut stream, &mut decoder) else {
            panic!("expected stable ID lookup on the new connection")
        };
        assert_eq!(request.target.as_deref(), Some("%41"));
        respond(
            &mut stream,
            Response::ResolveTarget(ResolveTargetResponse {
                target: Target::Pane(pane(2)),
            }),
        );
        let Request::ResizePane(request) = read_request(&mut stream, &mut decoder) else {
            panic!("expected one legacy resize")
        };
        assert_eq!(request.target, pane(2));
        assert_eq!(
            request.adjustment,
            ResizePaneAdjustment::AbsoluteWidth { columns: 40 }
        );
        respond(
            &mut stream,
            Response::ResizePane(ResizePaneResponse {
                target: request.target,
                adjustment: request.adjustment,
            }),
        );
    });

    assert_eq!(result.unwrap(), 0);
}

#[test]
fn frs1_r2_missing_frozen_id_stops_before_legacy_mutation() {
    let result = exchange(&["-t", "compat:0.1", "-x", "50%"], |listener| {
        let (mut first_stream, _) = receive_percent_target_action(&listener);
        send_decode(&mut first_stream);
        let (mut stream, mut decoder) = receive_reconnected_client(&mut first_stream, &listener);
        let Request::ResolveTarget(request) = read_request(&mut stream, &mut decoder) else {
            panic!("expected stable ID lookup on the new connection")
        };
        assert_eq!(request.target.as_deref(), Some("%41"));
        respond(
            &mut stream,
            Response::Error(ErrorResponse {
                error: RmuxError::InvalidTarget {
                    value: "%41".into(),
                    reason: "can't find pane: %41".into(),
                },
            }),
        );
        finish_without_another_connection(&mut stream, &listener);
    });

    assert!(result.is_err());
}

#[test]
fn frs1_r2_non_decode_refusal_does_not_retry() {
    let result = exchange(&["-t", "compat:0.1", "-x", "50%"], |listener| {
        let (mut stream, _) = receive_percent_target_action(&listener);
        respond(
            &mut stream,
            Response::Error(ErrorResponse {
                error: RmuxError::Server("client is read-only".into()),
            }),
        );
        finish_without_another_connection(&mut stream, &listener);
    });

    assert!(result.is_err());
}

#[test]
fn frs1_r2_percent_eof_never_retries() {
    let result = exchange(&["-t", "compat:0.1", "-x", "50%"], |listener| {
        let (mut stream, _) = receive_percent_target_action(&listener);
        stream.shutdown(std::net::Shutdown::Write).unwrap();
        let mut byte = [0];
        assert_eq!(stream.read(&mut byte).unwrap(), 0);
        listener.set_nonblocking(true).unwrap();
        assert!(matches!(
            listener.accept(),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock
        ));
    });

    assert!(result.is_err());
}

#[test]
fn frs1_r2_non_percent_keeps_its_explicit_decode_fallback() {
    let result = exchange(&["-t", "compat:0.1", "-x", "40"], |listener| {
        let (mut stream, mut decoder) = accept(&listener);
        assert!(matches!(
            read_request(&mut stream, &mut decoder),
            Request::ResizePaneTargetAction(_)
        ));
        send_decode(&mut stream);
        drop(stream);

        let (mut stream, mut decoder) = accept(&listener);
        let Request::ResolveTarget(request) = read_request(&mut stream, &mut decoder) else {
            panic!("expected legacy target resolution")
        };
        assert_eq!(request.target.as_deref(), Some("compat:0.1"));
        respond(
            &mut stream,
            Response::ResolveTarget(ResolveTargetResponse {
                target: Target::Pane(pane(1)),
            }),
        );
        let Request::ResizePane(request) = read_request(&mut stream, &mut decoder) else {
            panic!("expected legacy resize")
        };
        assert_eq!(request.target, pane(1));
        assert_eq!(
            request.adjustment,
            ResizePaneAdjustment::AbsoluteWidth { columns: 40 }
        );
        respond(
            &mut stream,
            Response::ResizePane(ResizePaneResponse {
                target: request.target,
                adjustment: request.adjustment,
            }),
        );
    });

    assert_eq!(result.unwrap(), 0);
}

fn mode_command(command: &str, flags: &[&str], width: &str) -> ResizePaneArgs {
    let mut arguments = vec![command, "-t", "compat:0.0", "-x", width, "-y", "bad"];
    arguments.extend_from_slice(flags);
    let cli = if command == "resize-pane" {
        crate::cli_args::parse(std::iter::once("rmux").chain(arguments.iter().copied()))
    } else {
        let canonical = rmux_core::command_parser::parse_command_arguments(&arguments)
            .unwrap()
            .to_tmux_reparse_string();
        assert!(canonical.starts_with("resize-pane "));
        crate::cli_args::parse_with_runtime_command_groups(
            std::iter::once("rmux").chain(arguments.iter().copied()),
            &[crate::cli_args::RuntimeCommandGroup::Canonical(canonical)],
        )
    }
    .unwrap();
    let Some(crate::cli_args::Command::ResizePane(args)) = cli.command else {
        panic!("canonical resize command")
    };
    args
}

fn mode_exchange(command: &str, flags: &[&str], width: &str, mode: Option<ResizePaneAdjustment>) {
    let args = mode_command(command, flags, width);
    let percent = width.ends_with('%');
    let expected_width = if percent { 20 } else { 10 };
    let result = exchange_parsed(args, move |listener| {
        // The endpoint decodes real production requests and applies their geometry
        // to the real process-free core model. It never runs a daemon or a PTY.
        let mut session = rmux_core::Session::new(
            SessionName::new("compat").unwrap(),
            rmux_proto::TerminalSize { cols: 80, rows: 12 },
        );
        session
            .split_pane_with_direction(0, rmux_proto::SplitDirection::Vertical)
            .unwrap();
        let before = session.window_at(0).unwrap().clone();
        let ids = before.panes().iter().map(|p| p.id()).collect::<Vec<_>>();
        assert_eq!(before.pane(0).unwrap().geometry().cols(), 40);
        assert_ne!(before.pane(0).unwrap().geometry().cols(), expected_width);
        let canonical = before.layout_dump();
        let (mut stream, mut decoder) = accept(&listener);
        let mut resolutions = 0;
        let mut listings = 0;
        loop {
            match read_request(&mut stream, &mut decoder) {
                Request::ResolveTarget(request) => {
                    resolutions += 1;
                    assert_eq!(resolutions, 1);
                    assert_eq!(request.target.as_deref(), Some("compat:0.0"));
                    respond(
                        &mut stream,
                        Response::ResolveTarget(ResolveTargetResponse {
                            target: Target::Pane(pane(0)),
                        }),
                    );
                }
                Request::ListPanes(_) => {
                    listings += 1;
                    assert!(percent);
                    assert_eq!(listings, 1);
                    respond(
                        &mut stream,
                        Response::ListPanes(ListPanesResponse {
                            output: CommandOutput::from_stdout("0\t0\t80\t12\t%41\n"),
                        }),
                    );
                }
                Request::ResizePaneTargetAction(request) => {
                    assert_eq!(
                        request.target.as_deref(),
                        Some(if percent { "%41" } else { "compat:0.0" })
                    );
                    assert_eq!(
                        Some(request.adjustment),
                        mode,
                        "mode must precede deferred height"
                    );
                    session.resize_pane(0, request.adjustment).unwrap();
                    let after = session.window_at(0).unwrap();
                    assert_eq!(
                        after.panes().iter().map(|p| p.id()).collect::<Vec<_>>(),
                        ids
                    );
                    if request.adjustment == ResizePaneAdjustment::Zoom {
                        assert!(after.is_zoomed());
                        session.toggle_zoom_in_window(0, 0).unwrap();
                        assert_eq!(session.window_at(0).unwrap().layout_dump(), canonical);
                        assert_eq!(
                            session
                                .window_at(0)
                                .unwrap()
                                .panes()
                                .iter()
                                .map(|p| p.geometry())
                                .collect::<Vec<_>>(),
                            before
                                .panes()
                                .iter()
                                .map(|p| p.geometry())
                                .collect::<Vec<_>>()
                        );
                    } else {
                        assert_eq!(request.adjustment, ResizePaneAdjustment::TrimBelow);
                        assert_eq!(
                            after, &before,
                            "trim dispatch must not mutate canonical geometry or zoom"
                        );
                    }
                    respond(
                        &mut stream,
                        Response::ResizePane(ResizePaneResponse {
                            target: pane(0),
                            adjustment: request.adjustment,
                        }),
                    );
                    break;
                }
                Request::ResizePaneAxes(request) => {
                    assert!(mode.is_none(), "ordinary axes must not bypass -T/-Z");
                    assert_eq!(request.target, pane(0));
                    assert_eq!(request.columns, Some(expected_width));
                    assert_eq!(
                        request.rows,
                        Err(RmuxError::Server("height invalid".into()))
                    );
                    session
                        .resize_pane(
                            0,
                            ResizePaneAdjustment::AbsoluteWidth {
                                columns: request.columns.unwrap(),
                            },
                        )
                        .unwrap();
                    let after = session.window_at(0).unwrap();
                    assert_eq!(after.pane(0).unwrap().geometry().cols(), expected_width);
                    assert_eq!(
                        after.pane(0).unwrap().geometry().rows(),
                        before.pane(0).unwrap().geometry().rows()
                    );
                    assert_eq!(
                        after.panes().iter().map(|p| p.id()).collect::<Vec<_>>(),
                        ids
                    );
                    assert!(!after.is_zoomed());
                    respond(
                        &mut stream,
                        Response::Error(ErrorResponse {
                            error: request.rows.unwrap_err(),
                        }),
                    );
                    break;
                }
                other => panic!("unexpected request {other:?}"),
            }
        }
        assert_eq!(resolutions, usize::from(percent || mode.is_none()));
        assert_eq!(listings, usize::from(percent));
        finish_without_another_connection(&mut stream, &listener);
    });
    if mode.is_some() {
        assert_eq!(result.unwrap(), 0);
    } else {
        let error = result.unwrap_err();
        assert_eq!(error.exit_code(), 1);
        assert_eq!(error.message(), "height invalid");
    }
}

#[test]
fn l1_040_full_and_canonical_alias_modes_precede_deferred_height() {
    // tmux3.7b L1-040: modes succeed without an axis commit; -T wins both flags.
    for command in ["resize-pane", "resizep"] {
        for width in ["10", "25%"] {
            for (flags, adjustment) in [
                (&["-Z"][..], ResizePaneAdjustment::Zoom),
                (&["-T"][..], ResizePaneAdjustment::TrimBelow),
                (&["-Z", "-T"][..], ResizePaneAdjustment::TrimBelow),
                (&["-T", "-Z"][..], ResizePaneAdjustment::TrimBelow),
            ] {
                mode_exchange(command, flags, width, Some(adjustment));
            }
        }
    }
}

#[test]
fn l1_040_no_mode_commits_width_before_height_error() {
    for command in ["resize-pane", "resizep"] {
        for width in ["10", "25%"] {
            mode_exchange(command, &[], width, None);
        }
    }
}
