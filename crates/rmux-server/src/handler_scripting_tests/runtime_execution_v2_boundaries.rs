use super::*;

#[tokio::test]
async fn legacy_c2s131_full_inventory_generic_command_remains_unsupported() {
    let handler = RequestHandler::new();
    let Request::RuntimeCommandExecution(mut request) =
        execution_request(&["display-message", "-p", "capabilities"])
    else {
        unreachable!()
    };
    request.handshake.required_capabilities = advertised(&handler).await;
    let bytes = rmux_proto::encode_frame(&Request::RuntimeCommandExecution(request)).unwrap();
    assert_eq!(&bytes[6..10], &131_u32.to_le_bytes());
    assert_eq!(
        execution_result(
            handler
                .handle(rmux_proto::decode_frame(&bytes).unwrap())
                .await
        ),
        RuntimeCommandExecutionResponse::Unsupported(RmuxError::Server(
            "new-pane requires an explicit floating creation offer".to_owned()
        ))
    );
}

#[tokio::test]
async fn legacy_c2s131_keeps_every_marker_combination_and_fallback_boundary() {
    let handler = RequestHandler::new();
    for mask in 0_u8..8 {
        for args in [
            &["display-message", "-p", "ok"][..],
            &["{"],
            &[
                "set-environment",
                "-g",
                "V2_EFFECT",
                "bad",
                ";",
                "display-message",
                "-p",
                "ok",
            ],
            &["V2_EFFECT=bad", "display-message", "-p", "ok"],
        ] {
            let mut request = RuntimeCommandExecutionRequest::new(
                args.iter().map(|s| (*s).to_owned()).collect(),
                None,
            );
            for (bit, intent) in SPECIFIC.into_iter().enumerate() {
                if mask & (1 << bit) != 0 {
                    request
                        .handshake
                        .required_capabilities
                        .push(intent.required_capability().unwrap().to_owned());
                }
            }
            let result = execution_result(
                handler
                    .handle(Request::RuntimeCommandExecution(Box::new(request)))
                    .await,
            );
            if args.contains(&";") || (mask == 0 && args[0] != "display-message") {
                assert_eq!(
                    result,
                    RuntimeCommandExecutionResponse::Fallback,
                    "mask={mask} args={args:?}"
                );
            } else if mask == 0 {
                complete(result);
            } else {
                assert!(
                    matches!(result, RuntimeCommandExecutionResponse::Unsupported(_)),
                    "mask={mask} args={args:?}: {result:?}"
                );
            }
            assert_eq!(
                handler
                    .state
                    .lock()
                    .await
                    .environment
                    .global_value("V2_EFFECT"),
                None
            );
        }
    }
}

#[tokio::test]
async fn v2_specific_ordinary_batches_assignments_and_parse_errors_never_fallback() {
    let handler = setup().await;
    await_runtime_birth_identities(&handler).await;
    let before = snapshot(&handler).await;
    for intent in SPECIFIC {
        for args in [
            &["{"][..],
            &[
                "V2_EFFECT=bad",
                "new-pane",
                "-d",
                "-t",
                "v2:0.0",
                "-x30",
                "-y8",
                "-X3",
                "-Y4",
            ],
            &[
                "set-environment",
                "-g",
                "V2_EFFECT",
                "bad",
                ";",
                "display-message",
                "-p",
                "ok",
            ],
        ] {
            assert!(matches!(
                send(&handler, offer(args, intent)).await,
                RuntimeCommandExecutionResponse::Unsupported(_)
            ));
            unchanged(&handler, &before).await;
        }
    }
    assert_eq!(
        send(&handler, offer(&["{"], RuntimeCommandIntent::GenericCli)).await,
        RuntimeCommandExecutionResponse::Fallback
    );
    frs1::cleanup(&handler).await;
}

#[tokio::test]
async fn v2_unknown_capability_and_wire_window_precede_parsing_and_effects() {
    let handler = setup().await;
    await_runtime_birth_identities(&handler).await;
    let before = snapshot(&handler).await;
    for args in [&["{"][..], &["set-environment", "-g", "V2_EFFECT", "bad"]] {
        let mut request = offer(args, RuntimeCommandIntent::GenericCli);
        request
            .handshake
            .required_capabilities
            .push("future.execution".to_owned());
        assert!(
            matches!(send(&handler, request).await, RuntimeCommandExecutionResponse::Unsupported(RmuxError::UnsupportedCapability { feature, .. }) if feature == "future.execution")
        );
        let mut request = offer(args, RuntimeCommandIntent::GenericCli);
        request.handshake.minimum_wire_version = 10;
        request.handshake.maximum_wire_version = 10;
        assert!(matches!(
            send(&handler, request).await,
            RuntimeCommandExecutionResponse::Unsupported(RmuxError::UnsupportedWireVersion { .. })
        ));
        unchanged(&handler, &before).await;
    }
    frs1::cleanup(&handler).await;
}

#[tokio::test]
async fn v2_generic_preserves_live_aliases_literal_argv_and_geometry_resize() {
    let handler = setup().await;
    for value in ["first", "second"] {
        set_alias(&handler, &format!("probe=display-message -p {value}")).await;
        assert!(
            matches!(send(&handler, offer(&["probe"], RuntimeCommandIntent::GenericCli)).await,
            RuntimeCommandExecutionResponse::Complete { stdout, error: None, exit_status: 0, .. } if stdout == format!("{value}\n").as_bytes())
        );
    }
    for value in ["", "space ; $HOME \\ ' \"\n\r\t", "-X9"] {
        assert!(
            matches!(send(&handler, offer(&["display-message", "-p", "--", value], RuntimeCommandIntent::GenericCli)).await,
            RuntimeCommandExecutionResponse::Complete { stdout, error: None, exit_status: 0, .. } if stdout == format!("{value}\n").as_bytes())
        );
    }
    complete(
        send(
            &handler,
            offer(
                &["resize-pane", "-t", "v2:0.1", "-x25", "-y7"],
                RuntimeCommandIntent::GenericCli,
            ),
        )
        .await,
    );
    let window = frs1::window(&handler, &session_name("v2")).await;
    assert_eq!(
        frs1::floating_rect(&window, 1),
        rmux_core::FloatingPaneRect::new(3, 4, 25, 7).unwrap()
    );
    frs1::cleanup(&handler).await;
}

#[tokio::test]
async fn v2_read_only_alias_mutation_is_terminal_and_reading_still_works() {
    use crate::server_access::{apply_access_policy, current_owner_uid, AccessMode};
    let handler = RequestHandler::new();
    let uid = current_owner_uid()
        .checked_add(1)
        .filter(|uid| *uid != 0)
        .unwrap_or(1);
    let pid = 831_135;
    handler
        .set_test_access_mode_for_uid(uid, AccessMode::ReadOnly)
        .unwrap();
    let admission = handler
        .server_access_admission_for_peer(&rmux_ipc::PeerIdentity {
            pid,
            uid,
            user: rmux_os::identity::UserIdentity::Uid(uid),
        })
        .unwrap();
    let _scope = handler.begin_detached_requester_access(pid, admission);
    set_alias(&handler, "probe=set-environment -g V2_EFFECT forbidden").await;
    for (args, denied) in [
        (&["probe"][..], true),
        (&["show-options", "-gv", "status"], false),
    ] {
        let mut offer = offer(args, RuntimeCommandIntent::GenericCli);
        offer.handshake.required_capabilities = advertised(&handler).await;
        let request =
            apply_access_policy(Request::RuntimeCommandExecutionV2(Box::new(offer)), false)
                .unwrap();
        let result = execution_result(
            handler
                .dispatch_for_connection(pid, u64::from(pid), request)
                .await
                .response,
        );
        if denied {
            assert!(
                matches!(result, RuntimeCommandExecutionResponse::Complete { error: Some(RmuxError::Server(message)), exit_status: 1, .. } if message == "client is read-only")
            );
        } else {
            complete(result);
        }
        assert_eq!(
            handler
                .state
                .lock()
                .await
                .environment
                .global_value("V2_EFFECT"),
            None
        );
    }
}

#[tokio::test]
async fn v2_post_execution_error_remains_complete_after_a_committed_mutation() {
    let handler = RequestHandler::new();
    let result = send(&handler, offer(&["if-shell", "-F", "1", "set-environment -g V2_EFFECT committed ; display-message -p before-error ; run-shell 'exit 17'"], RuntimeCommandIntent::GenericCli)).await;
    assert!(
        matches!(result, RuntimeCommandExecutionResponse::Complete { exit_status: 17, ref stdout, .. } if stdout.starts_with(b"before-error\n")),
        "{result:?}"
    );
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .environment
            .global_value("V2_EFFECT"),
        Some("committed")
    );
}
