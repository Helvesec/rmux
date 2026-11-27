//! Sibling production callers, using only in-memory transports.
//! Expected fields and RPC order are transcribed from the unchanged d9cbeb3830 base.
use crate::transport::TransportClient;
#[cfg(not(windows))]
use crate::EnsureSession;
use crate::{
    Pane, PaneId, PaneRef, PaneRespawnOptions, ProcessCommandSpec, ProcessSpec, RmuxEndpoint,
    Session, SessionName, SplitDirection,
};
use rmux_proto::*;
use tokio::io::{AsyncReadExt, AsyncWriteExt, DuplexStream};

fn name() -> SessionName {
    SessionName::new(" unusual $@%[];=/é雪 ").unwrap()
}
fn strings(values: &[&str]) -> Vec<String> {
    values.iter().map(|s| (*s).into()).collect()
}
fn process_modes() -> Vec<(ProcessSpec, Option<ProcessCommand>)> {
    vec![
        (ProcessSpec::default(), None),
        (
            ProcessSpec::argv(["one $literal"]),
            Some(ProcessCommand::Argv(strings(&["one $literal"]))),
        ),
        (
            ProcessSpec::argv(["inert", "", " a b ", "é雪", "$X;=\\\n"]),
            Some(ProcessCommand::Argv(strings(&[
                "inert", "", " a b ", "é雪", "$X;=\\\n",
            ]))),
        ),
        (
            ProcessSpec::shell(" $X; 'é雪'\n\\ "),
            Some(ProcessCommand::Shell(" $X; 'é雪'\n\\ ".into())),
        ),
    ]
}

async fn exchange(stream: &mut DuplexStream, expected: Request, response: Response) {
    let expected_bytes = encode_frame(&expected).unwrap();
    let mut bytes = vec![0; expected_bytes.len()];
    stream.read_exact(&mut bytes).await.unwrap();
    assert_eq!(bytes, expected_bytes, "exact request bytes and ordering");
    let mut decoder = FrameDecoder::new();
    decoder.push_bytes(&bytes);
    assert_eq!(decoder.next_frame::<Request>().unwrap(), Some(expected));
    stream
        .write_all(&encode_frame(&response).unwrap())
        .await
        .unwrap();
}
async fn handshake(stream: &mut DuplexStream) {
    exchange(
        stream,
        Request::Handshake(HandshakeRequest::requiring([CAPABILITY_HANDSHAKE])),
        Response::Handshake(HandshakeResponse::current()),
    )
    .await;
}
async fn bounded(future: impl std::future::Future<Output = ()>) {
    tokio::time::timeout(std::time::Duration::from_secs(3), future)
        .await
        .unwrap();
}
fn session(transport: TransportClient) -> Session {
    Session::new(name(), RmuxEndpoint::Default, None, transport, false, None)
}
fn pane(transport: TransportClient) -> Pane {
    Pane::new(
        PaneRef::new(name(), u32::MAX, 0),
        RmuxEndpoint::Default,
        None,
        transport,
    )
}

#[cfg(not(windows))]
#[test]
fn borrowed_ensure_request_clones_process_without_consuming_the_builder() {
    let mut modes = process_modes();
    modes.extend([
        (
            ProcessSpec::argv(Vec::<String>::new()),
            Some(ProcessCommand::Argv(vec![])),
        ),
        (
            ProcessSpec::argv([""]),
            Some(ProcessCommand::Argv(strings(&[""]))),
        ),
        (
            ProcessSpec::shell(""),
            Some(ProcessCommand::Shell(String::new())),
        ),
    ]);
    for (mut process, expected_process) in modes {
        process.command = Some(vec![]);
        process.environment = Some(strings(&["", "A=B=C", "A="]));
        let ptr = match &process.process_command {
            Some(ProcessCommandSpec::Argv(v)) if !v.is_empty() => Some(v.as_ptr() as usize),
            Some(ProcessCommandSpec::Shell(s)) if !s.is_empty() => Some(s.as_ptr() as usize),
            _ => None,
        };
        let builder = EnsureSession::named(name())
            .working_directory("")
            .detached(false)
            .process(process)
            .group_target(name())
            .window_name(" window é ");
        for attach_if_exists in [false, true] {
            let first = builder.to_new_session_request(attach_if_exists);
            let second = builder.to_new_session_request(attach_if_exists);
            let expected = NewSessionExtRequest {
                session_name: Some(name()),
                working_directory: Some(String::new()),
                detached: false,
                size: None,
                environment: Some(strings(&["", "A=B=C", "A="])),
                group_target: Some(name()),
                attach_if_exists,
                detach_other_clients: false,
                kill_other_clients: false,
                flags: None,
                window_name: Some(" window é ".into()),
                print_session_info: false,
                print_format: None,
                command: Some(vec![]),
                process_command: expected_process.clone(),
                client_environment: None,
                skip_environment_update: false,
            };
            assert_eq!(first, expected);
            assert_eq!(second, expected);
            assert_eq!(
                encode_frame(&Request::NewSessionExt(Box::new(first.clone()))).unwrap(),
                encode_frame(&Request::NewSessionExt(Box::new(expected))).unwrap()
            );
            match (&first.process_command, &second.process_command, ptr) {
                (Some(ProcessCommand::Argv(a)), Some(ProcessCommand::Argv(b)), Some(ptr)) => {
                    assert_ne!(a.as_ptr() as usize, ptr);
                    assert_ne!(a.as_ptr(), b.as_ptr());
                }
                (Some(ProcessCommand::Shell(a)), Some(ProcessCommand::Shell(b)), Some(ptr)) => {
                    assert_ne!(a.as_ptr() as usize, ptr);
                    assert_ne!(a.as_ptr(), b.as_ptr());
                }
                _ => {}
            }
        }
    }
}

#[tokio::test]
async fn window_builder_preserves_process_modes_and_optional_fields() {
    for (process, expected_process) in process_modes() {
        for configured in [false, true] {
            let process = process.clone();
            let expected_process = expected_process.clone();
            let (client, mut server) = tokio::io::duplex(8192);
            let session = session(TransportClient::spawn(client));
            let mut builder = session.new_window_with();
            match process.process_command {
                Some(ProcessCommandSpec::Argv(argv)) => {
                    builder = builder.shell("overwritten").spawn(argv)
                }
                Some(ProcessCommandSpec::Shell(text)) => {
                    builder = builder.spawn(["overwritten"]).shell(text)
                }
                None => {}
            }
            if configured {
                builder = builder
                    .name("")
                    .detached(true)
                    .at_index(u32::MAX)
                    .insert(true)
                    .cwd("")
                    .env("A", "B=C")
                    .env("A", "");
            }
            let operation = async move {
                assert_eq!(builder.await.unwrap().target().window_index, u32::MAX);
            };
            let wire = async {
                if expected_process.is_some() {
                    handshake(&mut server).await;
                }
                exchange(
                    &mut server,
                    Request::NewWindow(Box::new(NewWindowRequest {
                        target: name(),
                        name: configured.then(String::new),
                        detached: configured,
                        environment: configured.then(|| strings(&["A=B=C", "A="])),
                        command: None,
                        process_command: expected_process,
                        start_directory: configured.then(|| "".into()),
                        target_window_index: configured.then_some(u32::MAX),
                        insert_at_target: configured,
                    })),
                    Response::NewWindow(NewWindowResponse {
                        target: WindowTarget::with_window(name(), u32::MAX),
                    }),
                )
                .await;
            };
            bounded(async {
                tokio::join!(operation, wire);
            })
            .await;
            drop(session);
            assert_eq!(server.read(&mut [0]).await.unwrap(), 0, "no extra RPC");
        }
    }
}

#[tokio::test]
async fn pane_split_builder_preserves_modes_directions_and_defaults() {
    for (direction, axis, before) in [
        (
            SplitDirection::Right,
            rmux_proto::SplitDirection::Horizontal,
            false,
        ),
        (
            SplitDirection::Left,
            rmux_proto::SplitDirection::Horizontal,
            true,
        ),
        (
            SplitDirection::Down,
            rmux_proto::SplitDirection::Vertical,
            false,
        ),
        (
            SplitDirection::Up,
            rmux_proto::SplitDirection::Vertical,
            true,
        ),
    ] {
        for (process, expected_process) in process_modes() {
            for configured in [false, true] {
                let process = process.clone();
                let expected_process = expected_process.clone();
                let (client, mut server) = tokio::io::duplex(8192);
                let pane = pane(TransportClient::spawn(client));
                let mut builder = pane.split_with(direction);
                match process.process_command {
                    Some(ProcessCommandSpec::Argv(argv)) => {
                        builder = builder.shell("overwritten").spawn(argv)
                    }
                    Some(ProcessCommandSpec::Shell(text)) => {
                        builder = builder.spawn(["overwritten"]).shell(text)
                    }
                    None => {}
                }
                if configured {
                    builder = builder
                        .cwd("")
                        .env("A", "B=C")
                        .env("A", "")
                        .keep_alive_on_exit(false);
                }
                let operation = async move {
                    assert_eq!(builder.await.unwrap().target().pane_index, u32::MAX);
                };
                let wire = async {
                    handshake(&mut server).await;
                    exchange(
                        &mut server,
                        Request::SplitWindowIdentity(Box::new(SplitWindowIdentityRequest {
                            action: SplitWindowTargetActionRequest {
                                target: Some(format!("{}:4294967295.0", name())),
                                direction: axis,
                                before,
                                environment: configured.then(|| strings(&["A=B=C", "A="])),
                                command: None,
                                process_command: expected_process,
                                start_directory: configured.then(|| "".into()),
                                keep_alive_on_exit: configured.then_some(false),
                                detached: false,
                                size: None,
                                preserve_zoom: false,
                                full_size: false,
                                stdin_payload: None,
                            },
                        })),
                        Response::SplitWindowIdentity(SplitWindowIdentityResponse {
                            pane: PaneTarget::with_window(name(), u32::MAX, u32::MAX),
                            pane_id: PaneId::new(42),
                        }),
                    )
                    .await;
                };
                bounded(async {
                    tokio::join!(operation, wire);
                })
                .await;
                drop(pane);
                assert_eq!(server.read(&mut [0]).await.unwrap(), 0, "no extra RPC");
            }
        }
    }
}

async fn lookup(stream: &mut DuplexStream, window: Option<u32>) {
    exchange(
        stream,
        Request::ListPanes(Box::new(ListPanesRequest {
            target: name(),
            target_window_index: window,
            format: Some("#{window_index}:#{pane_index}:#{pane_id}".into()),
            filter: None,
            sort_order: None,
            reversed: false,
        })),
        Response::ListPanes(ListPanesResponse {
            output: CommandOutput::from_stdout("4294967295:0:%42\n"),
        }),
    )
    .await;
}

#[tokio::test]
async fn pane_respawn_and_spawn_builders_preserve_process_payload_and_rpc_order() {
    for (process, expected_process) in process_modes() {
        for builder_mode in [false, true] {
            let process = process.clone();
            let expected_process = expected_process.clone();
            if builder_mode && expected_process.is_none() {
                continue;
            }
            let (client, mut server) = tokio::io::duplex(8192);
            let pane = pane(TransportClient::spawn(client));
            let operation = async {
                let result = if builder_mode {
                    let builder = match process.process_command {
                        Some(ProcessCommandSpec::Argv(argv)) => pane.spawn(argv),
                        Some(ProcessCommandSpec::Shell(text)) => pane.shell(text),
                        None => unreachable!(),
                    };
                    builder
                        .cwd("")
                        .env("A", "B=C")
                        .env("A", "")
                        .kill_existing(true)
                        .keep_alive_on_exit(false)
                        .await
                } else {
                    pane.respawn(PaneRespawnOptions {
                        kill: true,
                        start_directory: Some("".into()),
                        process: ProcessSpec {
                            command: Some(vec![]),
                            environment: Some(strings(&["A=B=C", "A="])),
                            ..process
                        },
                        keep_alive_on_exit: Some(false),
                    })
                    .await
                };
                assert_eq!(result.unwrap(), PaneRef::new(name(), u32::MAX, 0));
            };
            let wire = async {
                if builder_mode {
                    lookup(&mut server, Some(u32::MAX)).await;
                }
                handshake(&mut server).await;
                lookup(
                    &mut server,
                    if builder_mode { None } else { Some(u32::MAX) },
                )
                .await;
                exchange(
                    &mut server,
                    Request::PaneRespawn(Box::new(PaneRespawnRequest {
                        target: PaneTargetRef::by_id(name(), PaneId::new(42)),
                        kill: true,
                        start_directory: Some("".into()),
                        environment: Some(strings(&["A=B=C", "A="])),
                        command: (!builder_mode).then(Vec::new),
                        process_command: expected_process,
                        keep_alive_on_exit: Some(false),
                    })),
                    Response::RespawnPane(RespawnPaneResponse {
                        target: PaneTarget::with_window(name(), u32::MAX, 0),
                    }),
                )
                .await;
                if builder_mode {
                    lookup(&mut server, None).await;
                }
            };
            bounded(async {
                tokio::join!(operation, wire);
            })
            .await;
            drop(pane);
            assert_eq!(server.read(&mut [0]).await.unwrap(), 0, "no extra RPC");
        }
    }
}

#[tokio::test]
async fn empty_process_builder_rejections_remain_local() {
    for mode in 0..3 {
        let (client, mut server) = tokio::io::duplex(8192);
        let transport = TransportClient::spawn(client);
        let session = session(transport.clone());
        let pane = pane(transport);
        let window = session.new_window_with();
        let split = pane.split_with(SplitDirection::Right);
        let (window, split, spawn) = match mode {
            0 => (
                window.spawn(Vec::<String>::new()),
                split.spawn(Vec::<String>::new()),
                pane.spawn(Vec::<String>::new()),
            ),
            1 => (
                window.spawn(["", "arg"]),
                split.spawn(["", "arg"]),
                pane.spawn(["", "arg"]),
            ),
            _ => (window.shell(""), split.shell(""), pane.shell("")),
        };
        for error in [
            window.await.unwrap_err(),
            split.await.unwrap_err(),
            spawn.await.unwrap_err(),
        ] {
            assert!(
                matches!(error, crate::RmuxError::SpawnFailed { message } if message == PROCESS_COMMAND_EMPTY_MESSAGE)
            );
        }
        drop(session);
        drop(pane);
        bounded(async {
            assert_eq!(
                server.read(&mut [0]).await.unwrap(),
                0,
                "no RPC on invalid builders"
            );
        })
        .await;
    }
}
