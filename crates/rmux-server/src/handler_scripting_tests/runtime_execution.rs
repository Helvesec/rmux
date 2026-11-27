use super::*;
use rmux_proto::{RmuxError, RuntimeCommandExecutionRequest, RuntimeCommandExecutionResponse};

#[path = "runtime_execution_v2.rs"]
mod v2;

#[path = "runtime_execution_contexts.rs"]
mod contexts;

#[path = "runtime_execution_entries.rs"]
mod entries;

#[path = "fmp1.rs"]
mod fmp1;
#[path = "fmp1_rights.rs"]
mod fmp1_rights;
#[path = "fnp1.rs"]
mod fnp1;
#[path = "fpc1.rs"]
mod fpc1;
#[path = "fpc1_junction.rs"]
mod fpc1_junction;
#[path = "fpc1_marked_source.rs"]
mod fpc1_marked_source;
#[path = "fpc1_marked_source_boundaries.rs"]
mod fpc1_marked_source_boundaries;
#[path = "fpc1_r1.rs"]
mod fpc1_r1;
#[path = "fpc1_requester_control.rs"]
mod fpc1_requester_control;
#[path = "frs1.rs"]
mod frs1;
#[path = "frs1_r1_n1.rs"]
mod frs1_r1_n1;
#[path = "frs1_sibling_producers.rs"]
mod frs1_sibling_producers;
#[path = "frs1_sibling_source_boundaries.rs"]
mod frs1_sibling_source_boundaries;
#[path = "runtime_execution_panes.rs"]
mod panes;
#[path = "source_input_marked_context.rs"]
mod source_input_marked_context;

fn execution_request(arguments: &[&str]) -> Request {
    Request::RuntimeCommandExecution(Box::new(RuntimeCommandExecutionRequest::new(
        arguments.iter().map(|s| (*s).to_owned()).collect(),
        None,
    )))
}

fn execution_result(response: Response) -> RuntimeCommandExecutionResponse {
    let Response::RuntimeCommandExecution(result) = response else {
        panic!("execution response: {response:?}")
    };
    *result
}

async fn set_alias(handler: &RequestHandler, value: &str) {
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::CommandAlias,
                value: value.to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
}

#[tokio::test]
async fn runtime_execution_preserves_literal_argv_and_empty_operands() {
    let handler = RequestHandler::new();
    for literal in [
        "space ; dollar $HOME slash\\ quote' double\"\n\r\t",
        "",
        "-tfoo",
    ] {
        let result = execution_result(
            handler
                .handle(execution_request(&["display-message", "-p", "--", literal]))
                .await,
        );
        assert_eq!(
            result,
            RuntimeCommandExecutionResponse::Complete {
                command_name: "display-message".to_owned(),
                stdout: format!("{literal}\n").into_bytes(),
                error: None,
                exit_status: 0,
            }
        );
    }
}

#[tokio::test]
async fn runtime_execution_resolves_changed_aliases_under_current_parser_state() {
    let handler = RequestHandler::new();
    for text in ["first", "second"] {
        set_alias(
            &handler,
            &format!("list-sessions=display-message -p {text}"),
        )
        .await;
        assert_eq!(
            execution_result(handler.handle(execution_request(&["list-sessions"])).await),
            RuntimeCommandExecutionResponse::Complete {
                command_name: "display-message".to_owned(),
                stdout: format!("{text}\n").into_bytes(),
                error: None,
                exit_status: 0,
            }
        );
    }
}

#[tokio::test]
async fn runtime_execution_defers_compound_assignment_and_startup_aliases_before_effects() {
    let handler = RequestHandler::new();
    for alias in [
        "list-sessions=R2_ASSIGNMENT=bad display-message -p assigned",
        "list-sessions=set-environment -g R2_GROUP bad ; display-message -p grouped",
        "list-sessions=new-session -d -s r2-unexpected",
        "list-sessions=kill-server",
        "list-sessions=source-file missing.conf",
        "list-sessions=display-message -p -Z invalid",
    ] {
        set_alias(&handler, alias).await;
        assert_eq!(
            execution_result(handler.handle(execution_request(&["list-sessions"])).await),
            RuntimeCommandExecutionResponse::Fallback,
            "{alias}"
        );
        let state = handler.state.lock().await;
        assert_eq!(state.environment.global_value("R2_ASSIGNMENT"), None);
        assert_eq!(state.environment.global_value("R2_GROUP"), None);
        assert!(state.sessions.is_empty());
    }
}

#[tokio::test]
async fn runtime_execution_negotiates_before_parsing_or_mutation() {
    let handler = RequestHandler::new();
    let Request::RuntimeCommandExecution(mut request) =
        execution_request(&["set-environment", "-g", "R2_NEGOTIATION", "bad"])
    else {
        unreachable!()
    };
    request
        .handshake
        .required_capabilities
        .push("future.execution".to_owned());
    assert!(
        matches!(execution_result(handler.handle(Request::RuntimeCommandExecution(request)).await),
        RuntimeCommandExecutionResponse::Unsupported(RmuxError::UnsupportedCapability { feature, .. }) if feature == "future.execution")
    );
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .environment
            .global_value("R2_NEGOTIATION"),
        None
    );
}

#[tokio::test]
async fn runtime_execution_uses_live_aliases_while_config_loading_without_waiting_on_itself() {
    let handler = RequestHandler::new();
    let guard = handler.start_config_loading();
    set_alias(&handler, "list-sessions=display-message -p loading").await;
    let result = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        handler.handle(execution_request(&["list-sessions"])),
    )
    .await
    .expect("config child must not deadlock");
    assert!(
        matches!(execution_result(result), RuntimeCommandExecutionResponse::Complete { stdout, .. } if stdout == b"loading\n")
    );
    drop(guard);
    set_alias(&handler, "list-sessions=display-message -p loaded").await;
    assert!(
        matches!(execution_result(handler.handle(execution_request(&["list-sessions"])).await), RuntimeCommandExecutionResponse::Complete { stdout, .. } if stdout == b"loaded\n")
    );
}

#[tokio::test]
async fn runtime_execution_preserves_read_only_authority_through_aliases() {
    use crate::server_access::{apply_access_policy, current_owner_uid, AccessMode};
    use rmux_ipc::PeerIdentity;
    use rmux_os::identity::UserIdentity;

    let handler = RequestHandler::new();
    let uid = current_owner_uid().wrapping_add(31_131).max(1);
    let pid = 831_131;
    handler
        .set_test_access_mode_for_uid(uid, AccessMode::ReadOnly)
        .unwrap();
    let admission = handler
        .server_access_admission_for_peer(&PeerIdentity {
            pid,
            uid,
            user: UserIdentity::Uid(uid),
        })
        .unwrap();
    let _scope = handler.begin_detached_requester_access(pid, admission);
    set_alias(
        &handler,
        "list-sessions=set-environment -g R2_READONLY forbidden",
    )
    .await;
    let request = apply_access_policy(execution_request(&["list-sessions"]), false).unwrap();
    let response = handler
        .dispatch_for_connection(pid, u64::from(pid), request)
        .await
        .response;
    assert!(
        matches!(execution_result(response), RuntimeCommandExecutionResponse::Complete {
        error: Some(RmuxError::Server(message)), exit_status: 1, ..
    } if message == "client is read-only")
    );
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .environment
            .global_value("R2_READONLY"),
        None
    );
    let request =
        apply_access_policy(execution_request(&["show-options", "-gv", "status"]), false).unwrap();
    let response = handler
        .dispatch_for_connection(pid, u64::from(pid), request)
        .await
        .response;
    assert!(
        matches!(execution_result(response), RuntimeCommandExecutionResponse::Complete {
        stdout, error: None, exit_status: 0, ..
    } if stdout == b"on\n")
    );
}

async fn run_hook_probe_queue(handler: &RequestHandler, commands: &str) -> Vec<u8> {
    let parsed = handler
        .parse_command_string_one_group(commands)
        .await
        .unwrap();
    handler
        .execute_parsed_commands_for_test(std::process::id(), parsed)
        .await
        .unwrap()
        .stdout()
        .to_vec()
}

#[tokio::test]
async fn runtime_execution_preserves_direct_hook_metadata_and_nested_queue_metadata() {
    let handler = RequestHandler::new();
    run_hook_probe_queue(&handler,
        "set-hook -g after-set-option 'set-option -gF @r2hook \"#{hook_arguments}|#{hook_argument_0}|#{hook_flag_g}\"'",
    ).await;
    // The established direct request omits parsed argv in after-hook formats.
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::Status,
                value: "off".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    let direct = run_hook_probe_queue(&handler, "show-options -gqv @r2hook").await;
    assert_eq!(direct, b"||\n");
    let result = execution_result(
        handler
            .handle(execution_request(&["set-option", "-g", "status", "on"]))
            .await,
    );
    assert!(matches!(
        result,
        RuntimeCommandExecutionResponse::Complete {
            error: None,
            exit_status: 0,
            ..
        }
    ));
    assert_eq!(
        run_hook_probe_queue(&handler, "show-options -gqv @r2hook").await,
        direct
    );

    // Existing queue entry points still expose their own arguments.
    run_hook_probe_queue(&handler, "set-option -g status off").await;
    let queued = run_hook_probe_queue(&handler, "show-options -gqv @r2hook").await;
    assert_ne!(queued, direct);
    for alias in [
        "list-sessions=if-shell -F 1 'set-option -g status off'",
        "list-sessions=run-shell -C 'set-option -g status off'",
    ] {
        set_alias(&handler, alias).await;
        let result = execution_result(handler.handle(execution_request(&["list-sessions"])).await);
        assert!(
            matches!(
                result,
                RuntimeCommandExecutionResponse::Complete {
                    error: None,
                    exit_status: 0,
                    ..
                }
            ),
            "{alias}: {result:?}"
        );
        assert_eq!(
            run_hook_probe_queue(&handler, "show-options -gqv @r2hook").await,
            queued,
            "{alias}"
        );
    }
}

#[tokio::test]
async fn runtime_execution_uses_the_same_capabilities_as_the_handshake() {
    let handler = RequestHandler::new();
    let Response::Handshake(advertised) = handler
        .handle(Request::Handshake(rmux_proto::HandshakeRequest::current()))
        .await
    else {
        panic!("current handshake");
    };
    let mut offer = rmux_proto::RuntimeCommandExecutionV2Request::new(
        ["display-message", "-p", "capabilities"]
            .into_iter()
            .map(str::to_owned)
            .collect(),
        None,
        rmux_proto::RuntimeCommandIntent::GenericCli,
    );
    offer.handshake.required_capabilities = advertised.capabilities;
    assert!(matches!(
        execution_result(
            handler
                .handle(Request::RuntimeCommandExecutionV2(Box::new(offer)))
                .await
        ),
        RuntimeCommandExecutionResponse::Complete {
            error: None,
            exit_status: 0,
            ..
        }
    ));
}

#[tokio::test]
async fn runtime_execution_shutdown_bounds_resolved_foreground_shells() {
    use std::future::Future;
    use std::task::Poll;
    use tokio::time::{advance, timeout, Duration};

    tokio::time::pause();
    for (alias, grace) in [
        ("list-sessions=run-shell -d20", Duration::from_secs(5)),
        ("list-sessions=run-shell -E -d20", Duration::from_secs(5)),
        (
            "list-sessions=if-shell -F 1 'run-shell -d20'",
            Duration::ZERO,
        ),
    ] {
        let handler = RequestHandler::new();
        set_alias(&handler, alias).await;
        // Delay-only shells start no OS child. The native marker fixture covers
        // actual process cleanup; this exercises the actual admitted runtime.
        let mut execution = Box::pin(handler.handle(execution_request(&["list-sessions"])));
        std::future::poll_fn(|cx| {
            assert!(execution.as_mut().poll(cx).is_pending(), "setup: {alias}");
            Poll::Ready(())
        })
        .await;
        handler.close_normal_request_admission();
        if !grace.is_zero() {
            std::future::poll_fn(|cx| {
                assert!(execution.as_mut().poll(cx).is_pending(), "grace: {alias}");
                Poll::Ready(())
            })
            .await;
            advance(grace - Duration::from_millis(1)).await;
            std::future::poll_fn(|cx| {
                assert!(
                    execution.as_mut().poll(cx).is_pending(),
                    "full grace: {alias}"
                );
                Poll::Ready(())
            })
            .await;
            advance(Duration::from_millis(1)).await;
        }
        let result = timeout(Duration::from_millis(100), execution)
            .await
            .expect("resolved foreground must release shutdown within its existing grace");
        assert!(
            matches!(execution_result(result), RuntimeCommandExecutionResponse::Complete {
            exit_status: 1, error: Some(RmuxError::Server(message)), ..
        } if message.contains("interrupted by server shutdown")),
            "{alias}"
        );
    }
}

#[tokio::test]
async fn runtime_execution_keeps_command_queues_and_short_mutations_on_the_offer() {
    let handler = RequestHandler::new();
    for alias in [
        "list-sessions=set-environment -g R2_SHORT admitted",
        "list-sessions=run-shell -C 'set-environment -g R2_SHORT admitted'",
        "list-sessions=if-shell -F 1 'set-environment -g R2_SHORT admitted'",
    ] {
        set_alias(&handler, alias).await;
        let result = execution_result(handler.handle(execution_request(&["list-sessions"])).await);
        assert!(
            matches!(
                result,
                RuntimeCommandExecutionResponse::Complete {
                    error: None,
                    exit_status: 0,
                    ..
                }
            ),
            "{alias}: {result:?}"
        );
        assert_eq!(
            handler
                .state
                .lock()
                .await
                .environment
                .global_value("R2_SHORT"),
            Some("admitted")
        );
    }
}

#[tokio::test]
async fn runtime_execution_shutdown_drains_admitted_run_shell_command_mutations() {
    use std::future::Future;
    use std::task::Poll;
    use tokio::time::{advance, Duration};

    tokio::time::pause();
    let handler = RequestHandler::new();
    set_alias(
        &handler,
        "list-sessions=run-shell -C 'run-shell -d20 ; set-environment -g R2_DRAIN committed'",
    )
    .await;
    let mut execution = Box::pin(handler.handle(execution_request(&["list-sessions"])));
    std::future::poll_fn(|cx| {
        assert!(
            execution.as_mut().poll(cx).is_pending(),
            "setup reaches the queue delay"
        );
        Poll::Ready(())
    })
    .await;
    handler.close_normal_request_admission();
    advance(Duration::from_secs(6)).await;
    std::future::poll_fn(|cx| {
        assert!(
            execution.as_mut().poll(cx).is_pending(),
            "admitted -C mutation must drain"
        );
        Poll::Ready(())
    })
    .await;
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .environment
            .global_value("R2_DRAIN"),
        None
    );
    advance(Duration::from_secs(14)).await;
    assert!(matches!(
        execution_result(execution.await),
        RuntimeCommandExecutionResponse::Complete {
            error: None,
            exit_status: 0,
            ..
        }
    ));
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .environment
            .global_value("R2_DRAIN"),
        Some("committed")
    );
}
