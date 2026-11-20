use super::*;
use rmux_os::identity::IdentityResolver;
use rmux_proto::request::AttachSessionExt3Request;
use rmux_proto::{
    ClientTerminalContext, ControlMode, ControlModeRequest, KillSessionRequest, OptionName,
    ScopeSelector, SetBufferRequest, SetOptionMode, SetOptionRequest, ShowBufferRequest,
};

const PROGRESS_TIMEOUT: Duration = Duration::from_secs(2);

fn peer(slot: u32) -> PeerIdentity {
    let uid = crate::server_access::current_owner_uid();
    PeerIdentity {
        pid: std::process::id().wrapping_add(48_019 + slot),
        uid,
        user: IdentityResolver::current().expect("native owner identity"),
    }
}

pub(crate) async fn set_title_option(handler: &RequestHandler, option: OptionName, value: &str) {
    let result = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option,
            value: value.to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(result, Response::SetOption(_)), "{result:?}");
}

pub(crate) async fn read_until(client: &mut TestClientStream, needle: &[u8]) {
    tokio::time::timeout(PROGRESS_TIMEOUT, async {
        let mut received = Vec::new();
        let mut buffer = [0_u8; 1024];
        loop {
            let count = client.read(&mut buffer).await.expect("live peer reads");
            assert!(count > 0, "peer closes before its output marker");
            received.extend_from_slice(&buffer[..count]);
            if received.windows(needle.len()).any(|part| part == needle) {
                break;
            }
            assert!(received.len() < 64 * 1024, "marker precedes bulk output");
        }
    })
    .await
    .expect("live peer makes progress while the other response is stalled");
}

pub(crate) async fn attach_peer(
    handler: &Arc<RequestHandler>,
    target: &PaneTarget,
    label: &str,
) -> (
    TestClientStream,
    watch::Sender<()>,
    tokio::task::JoinHandle<io::Result<()>>,
) {
    let (server, mut attached) = connected_streams(label).await.unwrap();
    let (attach_shutdown, attach_task) = spawn_connection(handler, peer(1), server);
    write_test_request(
        &mut attached,
        Request::AttachSessionExt3(Box::new(AttachSessionExt3Request {
            target: Some(target.session_name().clone()),
            target_spec: None,
            detach_other_clients: false,
            kill_other_clients: false,
            read_only: false,
            skip_environment_update: true,
            flags: None,
            working_directory: None,
            client_terminal: ClientTerminalContext {
                terminal_features: vec!["title".to_owned()],
                utf8: true,
            },
            client_size: Some(TerminalSize { cols: 80, rows: 24 }),
            attach_capabilities: Vec::new(),
        })),
    )
    .await
    .unwrap();
    let response = read_response_leaving_raw_bytes(&mut attached)
        .await
        .unwrap();
    assert!(
        matches!(response, Response::AttachSession(_)),
        "{response:?}"
    );
    read_until(&mut attached, b"ISOLATION_READY").await;

    (attached, attach_shutdown, attach_task)
}

#[tokio::test]
async fn slow_reader_isolation_sdk_and_control_writes_keep_an_attached_client_live() {
    for mode in [
        None,
        Some(ControlMode::Plain),
        Some(ControlMode::ControlControl),
    ] {
        let handler = Arc::new(RequestHandler::new());
        let target = start_quiet_pane_sized(
            &handler,
            "reader-isolation",
            TerminalSize { cols: 80, rows: 24 },
        )
        .await;
        set_title_option(&handler, OptionName::SetTitlesString, "ISOLATION_READY").await;
        set_title_option(&handler, OptionName::SetTitles, "on").await;
        set_title_option(&handler, OptionName::StatusInterval, "0").await;
        let result = handler
            .handle(Request::SetBuffer(Box::new(SetBufferRequest {
                name: Some("bulk".to_owned()),
                content: vec![b'x'; 2 * 1024 * 1024],
                append: false,
                new_name: None,
                set_clipboard: false,
                target_client: None,
            })))
            .await;
        assert!(matches!(result, Response::SetBuffer(_)), "{result:?}");

        let (mut attached, attach_shutdown, attach_task) =
            attach_peer(&handler, &target, "isolation-attached").await;

        let (server, mut slow) = connected_streams("isolation-slow").await.unwrap();
        let (slow_shutdown, slow_task) = spawn_connection(&handler, peer(2), server);
        if let Some(mode) = mode {
            write_test_request(
                &mut slow,
                Request::ControlMode(ControlModeRequest {
                    mode,
                    client_terminal: ClientTerminalContext::default(),
                    initial_command_count: 0,
                }),
            )
            .await
            .unwrap();
            let response = read_response_leaving_raw_bytes(&mut slow).await.unwrap();
            assert!(matches!(response, Response::ControlMode(_)), "{response:?}");
            slow.write_all(b"show-buffer -b bulk\nset-buffer -b after-stall reached\n")
                .await
                .unwrap();
        } else {
            // Mio may accept one complete named-pipe write into its owned
            // overlapped buffer before the peer reads it. The second response
            // must wait for that operation, so the following mutation proves
            // a stalled writer on both native transports.
            for _ in 0..2 {
                write_test_request(
                    &mut slow,
                    Request::ShowBuffer(ShowBufferRequest {
                        name: Some("bulk".to_owned()),
                    }),
                )
                .await
                .unwrap();
            }
            write_test_request(
                &mut slow,
                Request::SetBuffer(Box::new(SetBufferRequest {
                    name: Some("after-stall".to_owned()),
                    content: b"reached".to_vec(),
                    append: false,
                    new_name: None,
                    set_clipboard: false,
                    target_client: None,
                })),
            )
            .await
            .unwrap();
        }
        read_until(&mut slow, &[b'x'; 32]).await;
        // The response exceeds both native transports' buffers. Leaving its
        // remainder unread must keep the next request behind the stalled write.
        for title in ["ISOLATION_ONE", "ISOLATION_TWO"] {
            let pending = handler
                .handle(Request::ShowBuffer(ShowBufferRequest {
                    name: Some("after-stall".to_owned()),
                }))
                .await;
            assert!(
                matches!(pending, Response::Error(_)),
                "write must still be stalled: {pending:?}"
            );
            tokio::time::timeout(
                PROGRESS_TIMEOUT,
                set_title_option(&handler, OptionName::SetTitlesString, title),
            )
            .await
            .expect("attached state mutation is independent of the stalled peer");
            read_until(&mut attached, title.as_bytes()).await;
            assert!(!slow_task.is_finished(), "slow peer remains connected");
        }

        drop(slow);
        drop(attached);
        let _ = slow_shutdown.send(());
        let _ = attach_shutdown.send(());
        tokio::time::timeout(Duration::from_secs(6), async {
            finish_connection(slow_task).await.unwrap();
            finish_connection(attach_task).await.unwrap();
        })
        .await
        .expect("both native connections retire");
        let response = handler
            .handle(Request::KillSession(KillSessionRequest {
                target: target.session_name().clone(),
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }))
            .await;
        assert!(matches!(response, Response::KillSession(_)), "{response:?}");
    }
}

#[tokio::test]
async fn slow_reader_isolation_blocking_hook_keeps_an_attached_client_live() {
    use rmux_proto::{
        HookLifecycle, HookName, SetHookMutationRequest, WaitForMode, WaitForRequest,
    };

    let handler = Arc::new(RequestHandler::new());
    let target = start_quiet_pane_sized(
        &handler,
        "hook-isolation",
        TerminalSize { cols: 80, rows: 24 },
    )
    .await;
    set_title_option(&handler, OptionName::SetTitlesString, "ISOLATION_READY").await;
    set_title_option(&handler, OptionName::SetTitles, "on").await;
    set_title_option(&handler, OptionName::StatusInterval, "0").await;
    let (mut attached, attach_shutdown, attach_task) =
        attach_peer(&handler, &target, "hook-isolation-attached").await;
    let mut hook = SetHookMutationRequest {
        scope: ScopeSelector::Global,
        hook: HookName::AfterNewWindow,
        command: Some("wait-for isolation-hook".to_owned()),
        lifecycle: HookLifecycle::Persistent,
        append: false,
        unset: false,
        run_immediately: false,
        index: None,
    };
    let result = handler.handle(Request::SetHookMutation(hook.clone())).await;
    assert!(matches!(result, Response::SetHook(_)), "{result:?}");
    hook.command = None;
    hook.run_immediately = true;
    let (server, mut blocked) = connected_streams("hook-isolation-blocked").await.unwrap();
    let (hook_shutdown, hook_task) = spawn_connection(&handler, peer(2), server);
    write_test_request(&mut blocked, Request::SetHookMutation(hook))
        .await
        .unwrap();
    tokio::time::timeout(PROGRESS_TIMEOUT, async {
        while handler.wait_for_counts("isolation-hook").0 != 1 {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("the actual hook reaches its blocking wait");
    for title in ["HOOK_ISOLATION_ONE", "HOOK_ISOLATION_TWO"] {
        tokio::time::timeout(
            PROGRESS_TIMEOUT,
            set_title_option(&handler, OptionName::SetTitlesString, title),
        )
        .await
        .expect("attached mutation progresses during a blocking hook");
        read_until(&mut attached, title.as_bytes()).await;
        assert_eq!(handler.wait_for_counts("isolation-hook").0, 1);
        assert!(!hook_task.is_finished());
    }
    let result = handler
        .handle(Request::WaitFor(WaitForRequest {
            channel: "isolation-hook".to_owned(),
            mode: WaitForMode::Signal,
        }))
        .await;
    assert!(matches!(result, Response::WaitFor(_)), "{result:?}");
    let result = tokio::time::timeout(PROGRESS_TIMEOUT, read_test_response(&mut blocked))
        .await
        .expect("released hook answers")
        .unwrap();
    assert!(matches!(result, Response::SetHook(_)), "{result:?}");
    drop(blocked);
    drop(attached);
    let _ = hook_shutdown.send(());
    let _ = attach_shutdown.send(());
    tokio::time::timeout(Duration::from_secs(6), async {
        finish_connection(hook_task).await.unwrap();
        finish_connection(attach_task).await.unwrap();
    })
    .await
    .expect("hook and attach connections retire");
    let result = handler
        .handle(Request::KillSession(KillSessionRequest {
            target: target.session_name().clone(),
            kill_all_except_target: false,
            clear_alerts: false,
            kill_group: false,
        }))
        .await;
    assert!(matches!(result, Response::KillSession(_)), "{result:?}");
}
