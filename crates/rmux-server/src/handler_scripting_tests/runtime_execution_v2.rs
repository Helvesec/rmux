use super::*;
use rmux_proto::{RuntimeCommandExecutionV2Request, RuntimeCommandIntent};

const NEW: &[&str] = &[
    "new-pane", "-d", "-t", "v2:0.0", "-x30", "-y8", "-X3", "-Y4",
];
const MOVE: &[&str] = &["resize-pane", "-t", "v2:0.1", "-X12", "-Y7"];
const PLACE: &[&str] = &["set-pane-placement", "-t", "v2:0.0", "floating"];
const SPECIFIC: [RuntimeCommandIntent; 3] = [
    RuntimeCommandIntent::NewFloatingPane,
    RuntimeCommandIntent::MoveFloatingPane,
    RuntimeCommandIntent::SetPanePlacement,
];

#[path = "runtime_execution_v2_boundaries.rs"]
mod boundaries;

fn offer(arguments: &[&str], intent: RuntimeCommandIntent) -> RuntimeCommandExecutionV2Request {
    RuntimeCommandExecutionV2Request::new(
        arguments.iter().map(|s| (*s).to_owned()).collect(),
        None,
        intent,
    )
}

async fn advertised(handler: &RequestHandler) -> Vec<String> {
    let Response::Handshake(response) = handler
        .handle(Request::Handshake(rmux_proto::HandshakeRequest::current()))
        .await
    else {
        panic!("handshake response");
    };
    response.capabilities
}

async fn send(
    handler: &RequestHandler,
    offer: RuntimeCommandExecutionV2Request,
) -> RuntimeCommandExecutionResponse {
    // Exercise the serialized declaration, not just a field in an in-memory DTO.
    let bytes =
        rmux_proto::encode_frame(&Request::RuntimeCommandExecutionV2(Box::new(offer))).unwrap();
    assert_eq!(&bytes[6..10], &135_u32.to_le_bytes());
    execution_result(
        handler
            .handle(rmux_proto::decode_frame(&bytes).unwrap())
            .await,
    )
}

fn complete(result: RuntimeCommandExecutionResponse) {
    assert!(
        matches!(
            result,
            RuntimeCommandExecutionResponse::Complete {
                error: None,
                exit_status: 0,
                ..
            }
        ),
        "{result:?}"
    );
}

async fn setup() -> RequestHandler {
    let (handler, _) = frs1::setup("v2").await;
    run_hook_probe_queue(
        &handler,
        "set-hook -g command-error 'set-environment -g V2_HOOK fired'",
    )
    .await;
    run_hook_probe_queue(
        &handler,
        "set-hook -g window-layout-changed 'set-environment -g V2_HOOK fired'",
    )
    .await;
    handler
}

async fn snapshot(handler: &RequestHandler) -> rmux_proto::RmuxWindowSnapshot {
    let response = handler
        .handle(Request::ExportWindowSnapshot(
            rmux_proto::ExportWindowSnapshotRequest {
                target: rmux_proto::WindowTarget::with_window(session_name("v2"), 0),
            },
        ))
        .await;
    let Response::ExportWindowSnapshot(response) = response else {
        panic!("{response:?}")
    };
    response.snapshot
}

/// Snapshots require every pane's runtime birth identity. Detached startup may
/// still be pending, so await that precondition without disabling it.
async fn await_runtime_birth_identities(handler: &RequestHandler) {
    tokio::time::timeout(std::time::Duration::from_secs(10), async {
        loop {
            let ready = {
                let state = handler.state.lock().await;
                let window = state
                    .sessions
                    .session(&session_name("v2"))
                    .unwrap()
                    .window_at(0)
                    .unwrap();
                window.panes().iter().all(|pane| {
                    state
                        .pane_lifecycle(pane.id())
                        .and_then(|lifecycle| lifecycle.snapshot_incarnation())
                        .is_some()
                })
            };
            if ready {
                break;
            }
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture requires all pane runtime birth identities before snapshot");
}

async fn unchanged(handler: &RequestHandler, before: &rmux_proto::RmuxWindowSnapshot) {
    assert_eq!(
        &snapshot(handler).await,
        before,
        "pane identities, layout and placement unchanged"
    );
    let state = handler.state.lock().await;
    assert_eq!(state.environment.global_value("V2_EFFECT"), None);
    assert_eq!(
        state.environment.global_value("V2_HOOK"),
        None,
        "no execution or error hook"
    );
}

#[tokio::test]
async fn v2_m01_generic_accepts_the_complete_handshake_inventory() {
    let handler = RequestHandler::new();
    let mut request = offer(
        &["display-message", "-p", "capabilities"],
        RuntimeCommandIntent::GenericCli,
    );
    request.handshake.required_capabilities = advertised(&handler).await;
    assert_eq!(
        send(&handler, request).await,
        RuntimeCommandExecutionResponse::Complete {
            command_name: "display-message".to_owned(),
            stdout: b"capabilities\n".to_vec(),
            error: None,
            exit_status: 0,
        }
    );
}

#[tokio::test]
async fn v2_m02_generic_full_inventory_refuses_every_protected_kind_without_effects() {
    let handler = setup().await;
    await_runtime_birth_identities(&handler).await;
    let before = snapshot(&handler).await;
    for args in [NEW, MOVE, PLACE] {
        for alias in [false, true] {
            set_alias(&handler, &format!("probe={}", args.join(" "))).await;
            let mut request = offer(
                if alias { &["probe"] } else { args },
                RuntimeCommandIntent::GenericCli,
            );
            request.handshake.required_capabilities = advertised(&handler).await;
            assert!(matches!(
                send(&handler, request).await,
                RuntimeCommandExecutionResponse::Unsupported(_)
            ));
            unchanged(&handler, &before).await;
        }
    }
    frs1::cleanup(&handler).await;
}

#[tokio::test]
async fn v2_m03_declared_creation_succeeds_directly_and_through_alias() {
    let handler = setup().await;
    for full in [false, true] {
        for alias in [false, true] {
            set_alias(&handler, &format!("probe={}", NEW.join(" "))).await;
            let before = frs1::window(&handler, &session_name("v2")).await;
            let mut request = offer(
                if alias { &["probe"] } else { NEW },
                RuntimeCommandIntent::NewFloatingPane,
            );
            if full {
                request.handshake.required_capabilities = advertised(&handler).await;
            }
            complete(send(&handler, request).await);
            let after = frs1::window(&handler, &session_name("v2")).await;
            assert_eq!(after.panes().len(), before.panes().len() + 1);
            assert_eq!(
                after.floating_stack().placements().len(),
                before.floating_stack().placements().len() + 1
            );
            for pane in before.panes() {
                assert_eq!(after.pane(pane.index()).unwrap().id(), pane.id());
                assert_eq!(
                    after.pane(pane.index()).unwrap().geometry(),
                    pane.geometry()
                );
            }
        }
    }
    frs1::cleanup(&handler).await;
}

#[tokio::test]
async fn v2_m04_changed_specific_alias_refuses_minimal_and_full_inventory_without_effects() {
    for intent in SPECIFIC {
        for full in [false, true] {
            let handler = setup().await;
            let args = match intent {
                RuntimeCommandIntent::NewFloatingPane => NEW,
                RuntimeCommandIntent::MoveFloatingPane => MOVE,
                RuntimeCommandIntent::SetPanePlacement => PLACE,
                RuntimeCommandIntent::GenericCli => unreachable!(),
            };
            set_alias(&handler, &format!("probe={}", args.join(" "))).await;
            let mut request = offer(&["probe"], intent);
            if full {
                request.handshake.required_capabilities = advertised(&handler).await;
            }
            complete(send(&handler, request.clone()).await);
            run_hook_probe_queue(&handler, "set-environment -gu V2_HOOK").await;
            let before = snapshot(&handler).await;
            set_alias(&handler, "probe=set-environment -g V2_EFFECT changed").await;
            assert!(matches!(
                send(&handler, request).await,
                RuntimeCommandExecutionResponse::Unsupported(_)
            ));
            unchanged(&handler, &before).await;
            frs1::cleanup(&handler).await;
        }
    }
}

#[tokio::test]
async fn v2_m05_declared_move_and_placement_execute_their_matching_kind() {
    for full in [false, true] {
        for alias in [false, true] {
            let handler = setup().await;
            let before = frs1::window(&handler, &session_name("v2")).await;
            for (intent, args) in [
                (RuntimeCommandIntent::MoveFloatingPane, MOVE),
                (RuntimeCommandIntent::SetPanePlacement, PLACE),
            ] {
                set_alias(&handler, &format!("probe={}", args.join(" "))).await;
                let mut request = offer(if alias { &["probe"] } else { args }, intent);
                if full {
                    request.handshake.required_capabilities = advertised(&handler).await;
                }
                complete(send(&handler, request).await);
            }
            let after = frs1::window(&handler, &session_name("v2")).await;
            assert_eq!(
                frs1::floating_rect(&after, 1),
                rmux_core::FloatingPaneRect::new(12, 7, 30, 8).unwrap()
            );
            assert!(after
                .floating_stack()
                .placement(before.pane(0).unwrap().id())
                .is_some());
            assert_eq!(
                after
                    .panes()
                    .iter()
                    .map(rmux_core::Pane::id)
                    .collect::<Vec<_>>(),
                before
                    .panes()
                    .iter()
                    .map(rmux_core::Pane::id)
                    .collect::<Vec<_>>()
            );
            frs1::cleanup(&handler).await;
        }
    }
}

#[tokio::test]
async fn v2_m06_specific_intent_refuses_other_kinds_without_effects() {
    let handler = setup().await;
    await_runtime_birth_identities(&handler).await;
    let before = snapshot(&handler).await;
    for (index, intent) in SPECIFIC.into_iter().enumerate() {
        for (other, args) in [
            NEW,
            MOVE,
            PLACE,
            &["set-environment", "-g", "V2_EFFECT", "bad"],
        ]
        .into_iter()
        .enumerate()
        {
            if index == other {
                continue;
            }
            let mut request = offer(args, intent);
            request.handshake.required_capabilities = advertised(&handler).await;
            assert!(matches!(
                send(&handler, request).await,
                RuntimeCommandExecutionResponse::Unsupported(_)
            ));
            unchanged(&handler, &before).await;
        }
    }
    frs1::cleanup(&handler).await;
}

#[tokio::test]
async fn v2_m07_missing_intent_capability_refuses_before_parsing_or_effects() {
    use std::{
        future::{poll_fn, Future},
        task::Poll,
    };
    let handler = setup().await;
    await_runtime_birth_identities(&handler).await;
    let before = snapshot(&handler).await;
    for (intent, matching) in SPECIFIC.into_iter().zip([NEW, MOVE, PLACE]) {
        for args in [
            matching,
            &["{"],
            &["set-environment", "-g", "V2_EFFECT", "bad"],
        ] {
            let mut request = offer(args, intent);
            request
                .handshake
                .required_capabilities
                .retain(|cap| Some(cap.as_str()) != intent.required_capability());
            // Parsing first would wait on this state lock. Require an immediate
            // refusal while it is held, not merely an error with chosen text.
            let guard = handler.state.lock().await;
            // dispatch_captured ends at admission; handle() also runs the
            // unrelated applied-resize backstop after the response is ready.
            let pid = std::process::id();
            let mut response = Box::pin(handler.dispatch_captured(
                pid,
                u64::from(pid),
                Request::RuntimeCommandExecutionV2(Box::new(request.clone())),
            ));
            let (outcome, hooks) = poll_fn(|cx| match response.as_mut().poll(cx) {
                Poll::Ready(response) => Poll::Ready(response),
                Poll::Pending => panic!("missing capability reached parsing/state access"),
            })
            .await;
            drop(guard);
            assert!(hooks.is_empty());
            let expected =
                RuntimeCommandExecutionResponse::Unsupported(RmuxError::Server(format!(
                    "missing declared operation capability: {}",
                    intent.required_capability().unwrap(),
                )));
            assert_eq!(execution_result(outcome.response), expected);
            // Also run the ordinary wire/dispatch/hook path after releasing the
            // lock, proving the same refusal and no externally visible effects.
            assert_eq!(send(&handler, request).await, expected);
            unchanged(&handler, &before).await;
        }
    }
    frs1::cleanup(&handler).await;
}

#[tokio::test]
async fn v2_m08_protected_batches_refuse_before_the_first_effect_for_every_intent() {
    let handler = setup().await;
    await_runtime_birth_identities(&handler).await;
    let before = snapshot(&handler).await;
    for intent in std::iter::once(RuntimeCommandIntent::GenericCli).chain(SPECIFIC) {
        for args in [NEW, MOVE, PLACE] {
            let mut batch = vec!["set-environment", "-g", "V2_EFFECT", "bad", ";"];
            batch.extend_from_slice(args);
            let mut request = offer(&batch, intent);
            request.handshake.required_capabilities = advertised(&handler).await;
            assert!(matches!(
                send(&handler, request).await,
                RuntimeCommandExecutionResponse::Unsupported(_)
            ));
            unchanged(&handler, &before).await;
        }
    }
    frs1::cleanup(&handler).await;
}
