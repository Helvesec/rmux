use std::fs;
use std::time::{SystemTime, UNIX_EPOCH};

use super::RequestHandler;
use rmux_core::LifecycleEvent;
use rmux_proto::{
    ErrorResponse, HookLifecycle, HookName, NewSessionExtRequest, NewSessionRequest,
    NewWindowRequest, OptionName, Request, Response, RmuxError, ScopeSelector, SessionName,
    SetEnvironmentRequest, SetHookRequest, SetOptionMode, SetOptionRequest, ShowEnvironmentRequest,
    ShowOptionsRequest, TerminalSize,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
}

fn temp_path(label: &str) -> std::path::PathBuf {
    let stamp = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("current time after epoch")
        .as_nanos();
    std::env::temp_dir().join(format!("rmux-{label}-{stamp}-{}", std::process::id()))
}

async fn create_session(handler: &RequestHandler, name: &str) {
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name(name),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;

    assert!(matches!(response, Response::NewSession(_)));
}

#[tokio::test]
async fn global_environment_applies_to_initial_panes_created_after_mutation() {
    let handler = RequestHandler::new();
    let variable_name = "RMUX_TEST_GLOBAL";

    assert_eq!(
        handler
            .handle(Request::SetEnvironment(Box::new(SetEnvironmentRequest {
                scope: ScopeSelector::Global,
                name: variable_name.to_owned(),
                value: "screen".to_owned(),
                mode: None,
                hidden: false,
                format: false,
            })))
            .await,
        Response::SetEnvironment(rmux_proto::SetEnvironmentResponse {
            scope: ScopeSelector::Global,
            name: variable_name.to_owned(),
        })
    );

    create_session(&handler, "alpha").await;

    let state = handler.state.lock().await;
    let pane_zero = state
        .pane_profile(&session_name("alpha"), 0)
        .expect("pane 0 profile exists");
    assert_eq!(pane_zero.environment_value(variable_name), Some("screen"));
}

#[tokio::test]
async fn default_terminal_applies_to_initial_panes_and_yields_to_explicit_term() {
    let handler = RequestHandler::new();

    assert_eq!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::DefaultTerminal,
                value: "tmux-256color".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(rmux_proto::SetOptionResponse {
            scope: ScopeSelector::Global,
            option: OptionName::DefaultTerminal,
            mode: SetOptionMode::Replace,
        })
    );

    create_session(&handler, "alpha").await;

    {
        let state = handler.state.lock().await;
        let pane_zero = state
            .pane_profile(&session_name("alpha"), 0)
            .expect("pane 0 profile exists");
        assert_eq!(pane_zero.environment_value("TERM"), Some("tmux-256color"));
    }

    assert_eq!(
        handler
            .handle(Request::SetEnvironment(Box::new(SetEnvironmentRequest {
                scope: ScopeSelector::Session(session_name("alpha")),
                name: "TERM".to_owned(),
                value: "screen-256color".to_owned(),
                mode: None,
                hidden: false,
                format: false,
            })))
            .await,
        Response::SetEnvironment(rmux_proto::SetEnvironmentResponse {
            scope: ScopeSelector::Session(session_name("alpha")),
            name: "TERM".to_owned(),
        })
    );

    let split = handler
        .handle(Request::SplitWindow(rmux_proto::SplitWindowRequest {
            target: rmux_proto::SplitWindowTarget::Session(session_name("alpha")),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)));

    let state = handler.state.lock().await;
    let pane_one = state
        .pane_profile(&session_name("alpha"), 1)
        .expect("pane 1 profile exists");
    assert_eq!(pane_one.environment_value("TERM"), Some("tmux-256color"));
    drop(state);

    let split = handler
        .handle(Request::SplitWindow(rmux_proto::SplitWindowRequest {
            target: rmux_proto::SplitWindowTarget::Session(session_name("alpha")),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,
            environment: Some(vec!["TERM=screen-256color".to_owned()]),
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)));

    let state = handler.state.lock().await;
    let pane_two = state
        .pane_profile(&session_name("alpha"), 2)
        .expect("pane 2 profile exists");
    assert_eq!(pane_two.environment_value("TERM"), Some("screen-256color"));
}

#[tokio::test]
async fn environment_mutations_apply_only_to_future_panes_and_session_values_win() {
    let handler = RequestHandler::new();
    let variable_name = "RMUX_TEST_SESSION_VALUE";
    create_session(&handler, "alpha").await;

    {
        let state = handler.state.lock().await;
        let pane_zero = state
            .pane_profile(&session_name("alpha"), 0)
            .expect("pane 0 profile exists");
        assert_eq!(pane_zero.environment_value(variable_name), None);
    }

    assert_eq!(
        handler
            .handle(Request::SetEnvironment(Box::new(SetEnvironmentRequest {
                scope: ScopeSelector::Global,
                name: variable_name.to_owned(),
                value: "screen".to_owned(),
                mode: None,
                hidden: false,
                format: false,
            })))
            .await,
        Response::SetEnvironment(rmux_proto::SetEnvironmentResponse {
            scope: ScopeSelector::Global,
            name: variable_name.to_owned(),
        })
    );
    {
        let state = handler.state.lock().await;
        let pane_zero = state
            .pane_profile(&session_name("alpha"), 0)
            .expect("pane 0 profile exists");
        assert_eq!(pane_zero.environment_value(variable_name), None);
        assert_eq!(
            state
                .environment
                .resolve(Some(&session_name("alpha")), variable_name),
            Some("screen")
        );
    }

    let first_split = handler
        .handle(Request::SplitWindow(rmux_proto::SplitWindowRequest {
            target: rmux_proto::SplitWindowTarget::Session(session_name("alpha")),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(first_split, Response::SplitWindow(_)));
    {
        let state = handler.state.lock().await;
        let pane_one = state
            .pane_profile(&session_name("alpha"), 1)
            .expect("pane 1 profile exists");
        assert_eq!(pane_one.environment_value(variable_name), Some("screen"));
    }

    assert_eq!(
        handler
            .handle(Request::SetEnvironment(Box::new(SetEnvironmentRequest {
                scope: ScopeSelector::Session(session_name("alpha")),
                name: variable_name.to_owned(),
                value: "tmux-256color".to_owned(),
                mode: None,
                hidden: false,
                format: false,
            })))
            .await,
        Response::SetEnvironment(rmux_proto::SetEnvironmentResponse {
            scope: ScopeSelector::Session(session_name("alpha")),
            name: variable_name.to_owned(),
        })
    );
    {
        let state = handler.state.lock().await;
        let pane_one = state
            .pane_profile(&session_name("alpha"), 1)
            .expect("pane 1 profile exists");
        assert_eq!(pane_one.environment_value(variable_name), Some("screen"));
        assert_eq!(
            state
                .environment
                .resolve(Some(&session_name("alpha")), variable_name),
            Some("tmux-256color")
        );
    }

    let second_split = handler
        .handle(Request::SplitWindow(rmux_proto::SplitWindowRequest {
            target: rmux_proto::SplitWindowTarget::Session(session_name("alpha")),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(second_split, Response::SplitWindow(_)));

    let state = handler.state.lock().await;
    let pane_two = state
        .pane_profile(&session_name("alpha"), 2)
        .expect("pane 2 profile exists");
    assert_eq!(
        pane_two.environment_value(variable_name),
        Some("tmux-256color")
    );
}

#[tokio::test]
async fn set_hook_updates_the_store_and_one_shot_hooks_are_consumed_on_attach() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;

    assert_eq!(
        handler
            .handle(Request::SetHook(SetHookRequest {
                scope: ScopeSelector::Session(session_name("alpha")),
                hook: HookName::ClientAttached,
                command: "printf attached".to_owned(),
                lifecycle: HookLifecycle::OneShot,
            }))
            .await,
        Response::SetHook(rmux_proto::SetHookResponse {
            scope: ScopeSelector::Session(session_name("alpha")),
            hook: HookName::ClientAttached,
            lifecycle: HookLifecycle::OneShot,
        })
    );

    {
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .hooks
                .session_command(&session_name("alpha"), HookName::ClientAttached),
            Some("printf attached")
        );
    }

    let outcome = handler
        .dispatch(
            std::process::id(),
            Request::AttachSession(rmux_proto::AttachSessionRequest {
                target: session_name("alpha"),
            }),
        )
        .await;
    assert!(matches!(outcome.response, Response::AttachSession(_)));

    let attach = outcome.attach.expect("attach upgrade");
    let _attach_id = handler
        .register_attach(std::process::id(), session_name("alpha"), attach.control_tx)
        .await;
    let queued = {
        let mut state = handler.state.lock().await;
        super::prepare_lifecycle_event(
            &mut state,
            &LifecycleEvent::ClientAttached {
                session_name: session_name("alpha"),
                client_name: None,
            },
        )
    };
    handler.dispatch_lifecycle_hook(queued).await;

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .hooks
            .session_command(&session_name("alpha"), HookName::ClientAttached),
        None
    );
}

#[tokio::test]
async fn session_closed_hooks_fire_before_session_scope_is_removed() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;

    assert!(matches!(
        handler
            .handle(Request::SetHook(SetHookRequest {
                scope: ScopeSelector::Session(session_name("alpha")),
                hook: HookName::SessionClosed,
                command: "if-shell -F '#{==:#{hook_session_name},alpha}' 'set-buffer -b closed ok' 'set-buffer -b closed bad'".to_owned(),
                lifecycle: HookLifecycle::Persistent,
            }))
            .await,
        Response::SetHook(_)
    ));

    assert_eq!(
        handler
            .handle(Request::KillSession(rmux_proto::KillSessionRequest {
                target: session_name("alpha"),
                kill_all_except_target: false,
                clear_alerts: false,
            }))
            .await,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );

    let state = handler.state.lock().await;
    let (_, content) = state
        .buffers
        .show(Some("closed"))
        .expect("closed buffer exists");
    assert_eq!(String::from_utf8_lossy(content), "ok");
}

#[tokio::test]
async fn pane_exited_hooks_fire_with_removed_pane_context_before_pane_hooks_are_cleared() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;

    let split = handler
        .handle(Request::SplitWindow(rmux_proto::SplitWindowRequest {
            target: rmux_proto::SplitWindowTarget::Session(session_name("alpha")),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(split, Response::SplitWindow(_)));

    let pane_target = rmux_proto::PaneTarget::with_window(session_name("alpha"), 0, 1);
    let (pane_id, window_id) = {
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(&session_name("alpha"))
            .expect("alpha session exists");
        let window = session.window_at(0).expect("window 0 exists");
        let pane = window.pane(1).expect("pane 1 exists");
        (pane.id().as_u32(), window.id().as_u32())
    };

    assert!(matches!(
        handler
            .handle(Request::SetHook(SetHookRequest {
                scope: ScopeSelector::Pane(pane_target.clone()),
                hook: HookName::PaneExited,
                command: format!(
                    "if-shell -F '#{{==:#{{hook_pane}} #{{hook_window}},%{pane_id} @{window_id}}}' 'set-buffer -b exited ok' 'set-buffer -b exited bad'"
                ),
                lifecycle: HookLifecycle::Persistent,
            }))
            .await,
        Response::SetHook(_)
    ));

    assert!(matches!(
        handler
            .handle(Request::KillPane(rmux_proto::KillPaneRequest {
                target: pane_target,
                kill_all_except: false,
            }))
            .await,
        Response::KillPane(_)
    ));

    let state = handler.state.lock().await;
    let (_, content) = state
        .buffers
        .show(Some("exited"))
        .expect("exited buffer exists");
    assert_eq!(String::from_utf8_lossy(content), "ok");
}

#[tokio::test]
async fn window_unlinked_hooks_keep_removed_window_name_and_id() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;

    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name("alpha"),
            name: Some("logs".to_owned()),
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        })))
        .await;
    let Response::NewWindow(success) = response else {
        panic!("new-window should succeed");
    };
    let window_id = {
        let state = handler.state.lock().await;
        let session = state
            .sessions
            .session(&session_name("alpha"))
            .expect("alpha session exists");
        session
            .window_at(success.target.window_index())
            .expect("logs window exists")
            .id()
            .as_u32()
    };

    assert!(matches!(
        handler
            .handle(Request::SetHook(SetHookRequest {
                scope: ScopeSelector::Global,
                hook: HookName::WindowUnlinked,
                command: format!(
                    "if-shell -F '#{{==:#{{hook_window_name}} #{{hook_window}},logs @{window_id}}}' 'set-buffer -b unlinked ok' 'set-buffer -b unlinked bad'"
                ),
                lifecycle: HookLifecycle::Persistent,
            }))
            .await,
        Response::SetHook(_)
    ));

    assert!(matches!(
        handler
            .handle(Request::KillWindow(rmux_proto::KillWindowRequest {
                target: success.target.clone(),
                kill_all_others: false,
            }))
            .await,
        Response::KillWindow(_)
    ));

    let state = handler.state.lock().await;
    let (_, content) = state
        .buffers
        .show(Some("unlinked"))
        .expect("unlinked buffer exists");
    assert_eq!(String::from_utf8_lossy(content), "ok");
}

#[tokio::test]
async fn self_unsetting_hook_payloads_are_normalized_to_one_shot_shell_commands() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;

    assert_eq!(
        handler
            .handle(Request::SetHook(SetHookRequest {
                scope: ScopeSelector::Session(session_name("alpha")),
                hook: HookName::ClientAttached,
                command: format!(
                    "run-shell {}; set-hook -u -t alpha client-attached",
                    shell_quote_str("printf attached > /tmp/rmux-hook")
                ),
                lifecycle: HookLifecycle::Persistent,
            }))
            .await,
        Response::SetHook(rmux_proto::SetHookResponse {
            scope: ScopeSelector::Session(session_name("alpha")),
            hook: HookName::ClientAttached,
            lifecycle: HookLifecycle::OneShot,
        })
    );

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .hooks
            .session_command(&session_name("alpha"), HookName::ClientAttached),
        Some("printf attached > /tmp/rmux-hook")
    );
    assert_eq!(
        state
            .hooks
            .session_lifecycle(&session_name("alpha"), HookName::ClientAttached),
        Some(HookLifecycle::OneShot)
    );
}

#[tokio::test]
async fn session_scoped_mutations_require_live_sessions_and_are_cleared_on_kill() {
    let handler = RequestHandler::new();

    let missing_environment = handler
        .handle(Request::SetEnvironment(Box::new(SetEnvironmentRequest {
            scope: ScopeSelector::Session(session_name("missing")),
            name: "TERM".to_owned(),
            value: "screen".to_owned(),
            mode: None,
            hidden: false,
            format: false,
        })))
        .await;
    assert_eq!(
        missing_environment,
        Response::Error(ErrorResponse {
            error: RmuxError::SessionNotFound("missing".to_owned()),
        })
    );

    create_session(&handler, "alpha").await;
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Session(session_name("alpha")),
                option: OptionName::Status,
                value: "off".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SetEnvironment(Box::new(SetEnvironmentRequest {
                scope: ScopeSelector::Session(session_name("alpha")),
                name: "TERM".to_owned(),
                value: "screen".to_owned(),
                mode: None,
                hidden: false,
                format: false,
            })))
            .await,
        Response::SetEnvironment(_)
    ));
    assert_eq!(
        handler
            .handle(Request::KillSession(rmux_proto::KillSessionRequest {
                target: session_name("alpha"),
                kill_all_except_target: false,
                clear_alerts: false,
            }))
            .await,
        Response::KillSession(rmux_proto::KillSessionResponse { existed: true })
    );

    create_session(&handler, "alpha").await;
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .options
                .resolve(Some(&session_name("alpha")), OptionName::Status),
            Some("on")
        );
        assert_eq!(
            state
                .environment
                .resolve(Some(&session_name("alpha")), "TERM"),
            None
        );
        let pane_zero = state
            .pane_profile(&session_name("alpha"), 0)
            .expect("pane 0 profile exists");
        assert_eq!(pane_zero.environment_value("TERM"), Some("tmux-256color"));
    }
}

#[tokio::test]
async fn after_show_options_runs_without_triggering_nested_notify_hooks() {
    let handler = RequestHandler::new();

    assert!(matches!(
        handler
            .handle(Request::SetHook(SetHookRequest {
                scope: ScopeSelector::Global,
                hook: HookName::AfterShowOptions,
                command: "if-shell -F '#{==:#{hook},after-show-options}' 'set-buffer -b observed ok' 'set-buffer -b observed bad'".to_owned(),
                lifecycle: HookLifecycle::Persistent,
            }))
            .await,
        Response::SetHook(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SetHook(SetHookRequest {
                scope: ScopeSelector::Global,
                hook: HookName::PasteBufferChanged,
                command: "set-buffer -b recursive fired".to_owned(),
                lifecycle: HookLifecycle::OneShot,
            }))
            .await,
        Response::Error(_)
    ));

    assert!(matches!(
        handler
            .handle(Request::ShowOptions(ShowOptionsRequest {
                scope: rmux_proto::OptionScopeSelector::SessionGlobal,
                name: None,
                value_only: false,
                include_inherited: true,
                quiet: false,
            }))
            .await,
        Response::ShowOptions(_)
    ));

    let state = handler.state.lock().await;
    let (_, content) = state
        .buffers
        .show(Some("observed"))
        .expect("observed buffer exists");
    assert_eq!(String::from_utf8_lossy(content), "ok");
    assert_eq!(
        state.hooks.global_command(HookName::PasteBufferChanged),
        None
    );
}

#[tokio::test]
async fn split_window_runs_after_hook_once() {
    let handler = RequestHandler::new();
    let output_path = temp_path("after-split-window");
    let shell_command = append_x_command(&output_path);
    create_session(&handler, "alpha").await;

    assert!(matches!(
        handler
            .handle(Request::SetHook(SetHookRequest {
                scope: ScopeSelector::Global,
                hook: HookName::AfterSplitWindow,
                command: format!("run-shell {}", shell_quote_str(&shell_command)),
                lifecycle: HookLifecycle::Persistent,
            }))
            .await,
        Response::SetHook(_)
    ));

    let response = handler
        .handle(Request::SplitWindow(rmux_proto::SplitWindowRequest {
            target: rmux_proto::SplitWindowTarget::Session(session_name("alpha")),
            direction: rmux_proto::SplitDirection::Vertical,
            before: false,
            environment: None,
        }))
        .await;
    assert!(matches!(response, Response::SplitWindow(_)));

    assert_eq!(
        fs::read_to_string(&output_path).expect("split hook output exists"),
        "x"
    );
    let _ = fs::remove_file(output_path);
}

#[tokio::test]
async fn window_linked_hooks_receive_session_and_window_format_context() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;

    assert!(matches!(
        handler
            .handle(Request::SetHook(SetHookRequest {
                scope: ScopeSelector::Global,
                hook: HookName::WindowLinked,
                command: "if-shell -F '#{==:#{hook_session} #{hook_window},$0 @1}' 'set-buffer -b linked ok' 'set-buffer -b linked bad'".to_owned(),
                lifecycle: HookLifecycle::Persistent,
            }))
            .await,
        Response::SetHook(_)
    ));

    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name("alpha"),
            name: None,
            detached: false,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        })))
        .await;
    let Response::NewWindow(success) = response else {
        panic!("new-window should succeed");
    };

    let state = handler.state.lock().await;
    assert!(
        state
            .sessions
            .session(&session_name("alpha"))
            .and_then(|session| session.window_at(success.target.window_index()))
            .is_some(),
        "new window exists"
    );
    let (_, content) = state
        .buffers
        .show(Some("linked"))
        .expect("linked buffer exists");
    assert_eq!(String::from_utf8_lossy(content), "ok");
}

#[tokio::test]
async fn hook_commands_do_not_pre_expand_set_buffer_arguments() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;

    assert!(matches!(
        handler
            .handle(Request::SetHook(SetHookRequest {
                scope: ScopeSelector::Global,
                hook: HookName::AfterNewWindow,
                command: "set-buffer -b hook '#{hook} #{hook_window_name}'".to_owned(),
                lifecycle: HookLifecycle::Persistent,
            }))
            .await,
        Response::SetHook(_)
    ));

    assert!(matches!(
        handler
            .handle(Request::NewWindow(Box::new(NewWindowRequest {
                target: session_name("alpha"),
                name: Some("hooked".to_owned()),
                detached: false,
                start_directory: None,
                environment: None,
                command: None,
                process_command: None,
                target_window_index: None,
                insert_at_target: false,
            })))
            .await,
        Response::NewWindow(_)
    ));

    let state = handler.state.lock().await;
    let (_, content) = state
        .buffers
        .show(Some("hook"))
        .expect("hook buffer exists");
    assert_eq!(
        String::from_utf8_lossy(content),
        "#{hook} #{hook_window_name}"
    );
}

#[tokio::test]
async fn spawned_pane_environment_contains_pane_id_with_percent_prefix() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;

    let state = handler.state.lock().await;
    let pane_zero = state
        .pane_profile(&session_name("alpha"), 0)
        .expect("pane 0 profile exists");
    let rmux_pane = pane_zero.environment_value("RMUX_PANE");
    assert!(
        rmux_pane.is_some(),
        "RMUX_PANE must be set in spawned pane environment"
    );
    let rmux_pane = rmux_pane.expect("RMUX_PANE is set");
    assert_eq!(pane_zero.environment_value("TMUX_PANE"), Some(rmux_pane));
    assert!(
        rmux_pane.starts_with('%'),
        "RMUX_PANE must start with %: got {rmux_pane}"
    );
    let id_part = &rmux_pane[1..];
    assert!(
        id_part.parse::<u32>().is_ok(),
        "RMUX_PANE must be %<id>: got {rmux_pane}"
    );
}

#[tokio::test]
async fn spawned_pane_environment_contains_mux_socket_pid_session_format() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;

    let state = handler.state.lock().await;
    let pane_zero = state
        .pane_profile(&session_name("alpha"), 0)
        .expect("pane 0 profile exists");
    let rmux_value = pane_zero.environment_value("RMUX").expect("RMUX is set");
    assert_eq!(pane_zero.environment_value("TMUX"), Some(rmux_value));
    let parts: Vec<_> = rmux_value.split(',').collect();
    assert_eq!(
        parts.len(),
        3,
        "RMUX must be <socket>,<pid>,<session_id>: got {rmux_value}"
    );
    assert!(
        parts[1].parse::<u32>().is_ok(),
        "RMUX pid must be numeric: got {}",
        parts[1]
    );
    assert!(
        parts[2].parse::<u32>().is_ok(),
        "RMUX session_id must be numeric: got {}",
        parts[2]
    );
}

#[tokio::test]
async fn environment_override_layering_session_then_override_then_rmux_pane() {
    let handler = RequestHandler::new();

    assert!(matches!(
        handler
            .handle(Request::SetEnvironment(Box::new(SetEnvironmentRequest {
                scope: ScopeSelector::Global,
                name: "MY_VAR".to_owned(),
                value: "global".to_owned(),
                mode: None,
                hidden: false,
                format: false,
            })))
            .await,
        Response::SetEnvironment(_)
    ));

    // Create session with -e overrides: MY_VAR should be overridden by -e.
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: Some(vec!["MY_VAR=override".to_owned()]),
        }))
        .await;
    assert!(matches!(response, Response::NewSession(_)));

    let state = handler.state.lock().await;
    let pane_zero = state
        .pane_profile(&session_name("alpha"), 0)
        .expect("pane 0 profile exists");

    // -e overrides must beat session/global environment.
    assert_eq!(pane_zero.environment_value("MY_VAR"), Some("override"));
    // RMUX_PANE must still be present despite -e.
    assert!(pane_zero.environment_value("RMUX_PANE").is_some());
    drop(state);

    let shown = handler
        .handle(Request::ShowEnvironment(ShowEnvironmentRequest {
            scope: ScopeSelector::Session(session_name("alpha")),
            name: Some("MY_VAR".to_owned()),
            hidden: false,
            shell_format: false,
        }))
        .await;
    let output = shown
        .command_output()
        .expect("show-environment should expose new-session -e");
    assert_eq!(output.stdout(), b"MY_VAR=override\n");
}

#[tokio::test]
async fn new_session_ext_client_environment_respects_tmux_precedence() {
    let handler = RequestHandler::new();
    let session = session_name("client-env");

    for name in ["RMUX_GLOBAL_ENV_SENTINEL", "RMUX_OVERRIDE_ENV_SENTINEL"] {
        assert!(matches!(
            handler
                .handle(Request::SetEnvironment(Box::new(SetEnvironmentRequest {
                    scope: ScopeSelector::Global,
                    name: name.to_owned(),
                    value: "from-server".to_owned(),
                    mode: None,
                    hidden: false,
                    format: false,
                })))
                .await,
            Response::SetEnvironment(_)
        ));
    }

    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: Some(vec![
                "RMUX_OVERRIDE_ENV_SENTINEL=from-explicit".to_owned(),
                "RMUX_CLIENT_ENV_SENTINEL=from-explicit".to_owned(),
            ]),
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: None,
            client_environment: Some(vec![
                "PATH=/tmp/rmux-client-bin:/usr/bin".to_owned(),
                "SSH_AUTH_SOCK=/tmp/rmux-client-agent.sock".to_owned(),
                "RMUX_GLOBAL_ENV_SENTINEL=from-client".to_owned(),
                "RMUX_OVERRIDE_ENV_SENTINEL=from-client".to_owned(),
                "RMUX_CLIENT_ENV_SENTINEL=from-client".to_owned(),
                "RMUX_CLIENT_ONLY_ENV_SENTINEL=from-client".to_owned(),
            ]),
            skip_environment_update: false,
        })))
        .await;

    assert!(matches!(response, Response::NewSession(_)));

    let state = handler.state.lock().await;
    let pane_zero = state
        .pane_profile(&session, 0)
        .expect("pane 0 profile exists");
    assert_eq!(
        pane_zero.environment_value("PATH"),
        Some("/tmp/rmux-client-bin:/usr/bin")
    );
    assert_eq!(
        pane_zero.environment_value("SSH_AUTH_SOCK"),
        Some("/tmp/rmux-client-agent.sock")
    );
    assert_eq!(
        pane_zero.environment_value("RMUX_GLOBAL_ENV_SENTINEL"),
        Some("from-server")
    );
    assert_eq!(
        pane_zero.environment_value("RMUX_OVERRIDE_ENV_SENTINEL"),
        Some("from-explicit")
    );
    assert_eq!(
        pane_zero.environment_value("RMUX_CLIENT_ENV_SENTINEL"),
        Some("from-explicit")
    );
    assert_eq!(
        pane_zero.environment_value("RMUX_CLIENT_ONLY_ENV_SENTINEL"),
        Some("from-client")
    );
    drop(state);

    let shown = handler
        .handle(Request::ShowEnvironment(ShowEnvironmentRequest {
            scope: ScopeSelector::Session(session.clone()),
            name: Some("RMUX_CLIENT_ONLY_ENV_SENTINEL".to_owned()),
            hidden: false,
            shell_format: false,
        }))
        .await;
    assert!(matches!(shown, Response::Error(_)));
}

#[tokio::test]
async fn new_session_ext_skip_environment_update_keeps_client_spawn_environment() {
    let handler = RequestHandler::new();
    let session = session_name("skip-client-env");

    let response = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: None,
            client_environment: Some(vec![
                "PATH=/tmp/rmux-client-bin:/usr/bin".to_owned(),
                "RMUX_CLIENT_ONLY_ENV_SENTINEL=from-client".to_owned(),
            ]),
            skip_environment_update: true,
        })))
        .await;

    assert!(matches!(response, Response::NewSession(_)));

    let state = handler.state.lock().await;
    let pane_zero = state
        .pane_profile(&session, 0)
        .expect("pane 0 profile exists");
    assert_eq!(
        pane_zero.environment_value("RMUX_CLIENT_ONLY_ENV_SENTINEL"),
        Some("from-client")
    );
    drop(state);

    let shown = handler
        .handle(Request::ShowEnvironment(ShowEnvironmentRequest {
            scope: ScopeSelector::Session(session),
            name: Some("PATH".to_owned()),
            hidden: false,
            shell_format: false,
        }))
        .await;
    assert!(matches!(shown, Response::Error(_)));
}

fn shell_quote_str(value: &str) -> String {
    crate::test_shell::command_quote(value)
}

#[cfg(unix)]
fn append_x_command(path: &std::path::Path) -> String {
    format!(
        "printf x >> {}",
        shell_quote_str(&path.display().to_string())
    )
}

#[cfg(windows)]
fn append_x_command(path: &std::path::Path) -> String {
    crate::test_shell::powershell_encoded_command(&format!(
        "Add-Content -NoNewline -LiteralPath {} -Value 'x'",
        crate::test_shell::powershell_quote_path(path)
    ))
}
