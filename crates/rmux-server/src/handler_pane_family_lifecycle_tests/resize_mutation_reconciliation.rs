use super::inactive_winlink_resize::{
    assert_window_and_pty_size, create_sized_session, link_window, register_sized_attach,
    select_window, set_window_size_policy, LARGE_SIZE, SMALL_SIZE,
};
use super::RequestHandler;
use rmux_core::LifecycleEvent;
use rmux_proto::{
    HookLifecycle, HookName, KillWindowRequest, LinkWindowRequest, NewWindowRequest,
    OptionScopeSelector, RenameSessionRequest, Request, Response, ScopeSelector, SetHookRequest,
    SetOptionByNameRequest, SetOptionMode, SwapWindowRequest, WindowTarget,
};

fn window_size_option_request(scope: OptionScopeSelector, value: &str) -> Request {
    Request::SetOptionByName(Box::new(SetOptionByNameRequest {
        scope,
        name: "window-size".to_owned(),
        value: Some(value.to_owned()),
        mode: SetOptionMode::Replace,
        only_if_unset: false,
        unset: false,
        unset_pane_overrides: false,
        format: false,
        format_target: None,
    }))
}

async fn receive_window_resized(
    events: &mut tokio::sync::broadcast::Receiver<super::super::QueuedLifecycleEvent>,
) -> super::super::QueuedLifecycleEvent {
    loop {
        let event = tokio::time::timeout(std::time::Duration::from_secs(1), events.recv())
            .await
            .expect("window-resized lifecycle event arrives")
            .expect("lifecycle sender remains open");
        if matches!(event.event, LifecycleEvent::WindowResized { .. }) {
            return event;
        }
    }
}

async fn create_detached_window(
    handler: &RequestHandler,
    session_name: &rmux_proto::SessionName,
    window_index: u32,
) {
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session_name.clone(),
            name: Some(format!("window-{window_index}")),
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: Some(window_index),
            insert_at_target: false,
        })))
        .await;
    assert!(matches!(response, Response::NewWindow(_)), "{response:?}");
}

async fn create_linked_inactive_window_fixture(
    label: &str,
    first_attach_pid: u32,
) -> (
    RequestHandler,
    rmux_proto::SessionName,
    rmux_proto::SessionName,
    tokio::sync::mpsc::UnboundedReceiver<crate::pane_io::AttachControl>,
    tokio::sync::mpsc::UnboundedReceiver<crate::pane_io::AttachControl>,
) {
    let handler = RequestHandler::new();
    let source = create_sized_session(&handler, &format!("{label}-source"), LARGE_SIZE).await;
    let alias = create_sized_session(&handler, &format!("{label}-alias"), LARGE_SIZE).await;
    create_detached_window(&handler, &source, 1).await;
    link_window(
        &handler,
        WindowTarget::with_window(source.clone(), 1),
        WindowTarget::with_window(alias.clone(), 1),
    )
    .await;
    select_window(&handler, WindowTarget::with_window(alias.clone(), 1)).await;
    handler.wait_for_initial_panes_for_test().await;

    let source_rx = register_sized_attach(&handler, first_attach_pid, &source, SMALL_SIZE).await;
    let alias_rx = register_sized_attach(&handler, first_attach_pid + 1, &alias, LARGE_SIZE).await;

    assert_window_and_pty_size(&handler, &source, 0, SMALL_SIZE).await;
    assert_window_and_pty_size(&handler, &source, 1, LARGE_SIZE).await;
    assert_window_and_pty_size(&handler, &alias, 1, LARGE_SIZE).await;

    (handler, source, alias, source_rx, alias_rx)
}

#[tokio::test]
async fn resize_mutation_kill_window_reconciles_the_new_active_window() {
    let handler = RequestHandler::new();
    let session = create_sized_session(&handler, "resize-kill-active", LARGE_SIZE).await;
    create_detached_window(&handler, &session, 1).await;
    handler.wait_for_initial_panes_for_test().await;
    let _attach_rx = register_sized_attach(&handler, 8_101, &session, SMALL_SIZE).await;

    assert_window_and_pty_size(&handler, &session, 0, SMALL_SIZE).await;
    assert_window_and_pty_size(&handler, &session, 1, LARGE_SIZE).await;

    let response = handler
        .handle(Request::KillWindow(KillWindowRequest {
            target: WindowTarget::with_window(session.clone(), 0),
            kill_all_others: false,
        }))
        .await;
    assert!(matches!(response, Response::KillWindow(_)), "{response:?}");

    {
        let state = handler.state.lock().await;
        let surviving_session = state.sessions.session(&session).expect("session survives");
        assert_eq!(surviving_session.active_window_index(), 1);
    }
    assert_window_and_pty_size(&handler, &session, 1, SMALL_SIZE).await;
}

#[tokio::test]
async fn resize_mutation_kill_window_reconciles_each_linked_session_new_active_runtime() {
    let handler = RequestHandler::new();
    let source = create_sized_session(&handler, "resize-kill-link-source", LARGE_SIZE).await;
    let alias = create_sized_session(&handler, "resize-kill-link-alias", SMALL_SIZE).await;
    create_detached_window(&handler, &source, 1).await;
    link_window(
        &handler,
        WindowTarget::with_window(source.clone(), 0),
        WindowTarget::with_window(alias.clone(), 1),
    )
    .await;
    select_window(&handler, WindowTarget::with_window(alias.clone(), 1)).await;
    handler.wait_for_initial_panes_for_test().await;

    let _source_rx = register_sized_attach(&handler, 8_151, &source, SMALL_SIZE).await;
    let _alias_rx = register_sized_attach(&handler, 8_152, &alias, LARGE_SIZE).await;
    set_window_size_policy(&handler, &source, 0, "smallest").await;

    assert_window_and_pty_size(&handler, &source, 0, SMALL_SIZE).await;
    assert_window_and_pty_size(&handler, &alias, 1, SMALL_SIZE).await;
    assert_window_and_pty_size(&handler, &source, 1, LARGE_SIZE).await;
    assert_window_and_pty_size(&handler, &alias, 0, SMALL_SIZE).await;

    let response = handler
        .handle(Request::KillWindow(KillWindowRequest {
            target: WindowTarget::with_window(source.clone(), 0),
            kill_all_others: false,
        }))
        .await;
    assert!(matches!(response, Response::KillWindow(_)), "{response:?}");

    {
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&source)
                .expect("source survives")
                .active_window_index(),
            1
        );
        let alias_session = state
            .sessions
            .session(&alias)
            .expect("linked session survives");
        assert_eq!(alias_session.active_window_index(), 0);
        assert!(
            alias_session.window_at(1).is_none(),
            "kill-window removes every true link occurrence of the killed window"
        );
    }
    assert_window_and_pty_size(&handler, &source, 1, SMALL_SIZE).await;
    assert_window_and_pty_size(&handler, &alias, 0, LARGE_SIZE).await;
}

#[tokio::test]
async fn resize_mutation_link_window_reconciles_previous_active_aggressive_window() {
    let handler = RequestHandler::new();
    let owner = create_sized_session(&handler, "resize-link-old-owner", LARGE_SIZE).await;
    let target = create_sized_session(&handler, "resize-link-target", SMALL_SIZE).await;
    let incoming = create_sized_session(&handler, "resize-link-incoming", LARGE_SIZE).await;

    link_window(
        &handler,
        WindowTarget::with_window(owner.clone(), 0),
        WindowTarget::with_window(target.clone(), 1),
    )
    .await;
    select_window(&handler, WindowTarget::with_window(target.clone(), 1)).await;
    handler.wait_for_initial_panes_for_test().await;

    let _owner_rx = register_sized_attach(&handler, 8_171, &owner, LARGE_SIZE).await;
    let _target_rx = register_sized_attach(&handler, 8_172, &target, SMALL_SIZE).await;
    set_window_size_policy(&handler, &owner, 0, "smallest").await;
    let response = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope: OptionScopeSelector::Window(WindowTarget::with_window(owner.clone(), 0)),
            name: "aggressive-resize".to_owned(),
            value: Some("on".to_owned()),
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
        "{response:?}"
    );
    handler
        .reconcile_attached_session_size_and_emit(&owner)
        .await
        .expect("initial aggressive window size reconciles");
    assert_window_and_pty_size(&handler, &owner, 0, SMALL_SIZE).await;
    assert_window_and_pty_size(&handler, &target, 1, SMALL_SIZE).await;

    let response = handler
        .handle(Request::LinkWindow(LinkWindowRequest {
            source: WindowTarget::with_window(incoming, 0),
            target: WindowTarget::with_window(target.clone(), 2),
            after: false,
            before: false,
            kill_destination: false,
            detached: false,
        }))
        .await;
    assert!(matches!(response, Response::LinkWindow(_)), "{response:?}");

    {
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&target)
                .expect("target survives")
                .active_window_index(),
            2
        );
    }
    assert_window_and_pty_size(&handler, &owner, 0, LARGE_SIZE).await;
    assert_window_and_pty_size(&handler, &target, 1, LARGE_SIZE).await;
}

#[tokio::test]
async fn resize_mutation_swap_window_reconciles_the_new_active_linked_family() {
    let (handler, source, alias, _source_rx, _alias_rx) =
        create_linked_inactive_window_fixture("resize-swap-active", 8_201).await;

    set_window_size_policy(&handler, &source, 1, "smallest").await;
    let response = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope: OptionScopeSelector::Window(WindowTarget::with_window(source.clone(), 1)),
            name: "aggressive-resize".to_owned(),
            value: Some("on".to_owned()),
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
        "{response:?}"
    );
    assert_window_and_pty_size(&handler, &source, 1, LARGE_SIZE).await;
    assert_window_and_pty_size(&handler, &alias, 1, LARGE_SIZE).await;

    let response = handler
        .handle(Request::SwapWindow(SwapWindowRequest {
            source: WindowTarget::with_window(source.clone(), 0),
            target: WindowTarget::with_window(source.clone(), 1),
            detached: false,
        }))
        .await;
    assert!(matches!(response, Response::SwapWindow(_)), "{response:?}");

    {
        let state = handler.state.lock().await;
        let source_session = state.sessions.session(&source).expect("source survives");
        let alias_session = state.sessions.session(&alias).expect("alias survives");
        assert_eq!(source_session.active_window_index(), 0);
        assert_eq!(alias_session.active_window_index(), 1);
        assert_eq!(
            source_session
                .window_at(0)
                .expect("source alias exists")
                .id(),
            alias_session
                .window_at(1)
                .expect("linked alias exists")
                .id()
        );
    }
    assert_window_and_pty_size(&handler, &source, 0, SMALL_SIZE).await;
    assert_window_and_pty_size(&handler, &alias, 1, SMALL_SIZE).await;
}

#[tokio::test]
async fn resize_mutation_window_options_reconcile_the_exact_linked_family() {
    let (handler, source, alias, _source_rx, _alias_rx) =
        create_linked_inactive_window_fixture("resize-option-exact", 8_301).await;

    set_window_size_policy(&handler, &source, 1, "smallest").await;
    assert_window_and_pty_size(&handler, &source, 0, SMALL_SIZE).await;
    assert_window_and_pty_size(&handler, &source, 1, SMALL_SIZE).await;
    assert_window_and_pty_size(&handler, &alias, 1, SMALL_SIZE).await;

    let response = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope: OptionScopeSelector::Window(WindowTarget::with_window(source.clone(), 1)),
            name: "aggressive-resize".to_owned(),
            value: Some("on".to_owned()),
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
        "{response:?}"
    );

    assert_window_and_pty_size(&handler, &source, 0, SMALL_SIZE).await;
    assert_window_and_pty_size(&handler, &source, 1, LARGE_SIZE).await;
    assert_window_and_pty_size(&handler, &alias, 1, LARGE_SIZE).await;
}

#[tokio::test]
async fn resize_option_reconciliation_follows_a_renamed_session_identity() {
    let (handler, source, alias, _source_rx, _alias_rx) =
        create_linked_inactive_window_fixture("resize-option-rename", 8_351).await;

    set_window_size_policy(&handler, &source, 1, "smallest").await;
    assert_window_and_pty_size(&handler, &source, 1, SMALL_SIZE).await;
    assert_window_and_pty_size(&handler, &alias, 1, SMALL_SIZE).await;

    let renamed = rmux_proto::SessionName::new("resize-option-rename-renamed")
        .expect("valid renamed session name");
    let state_guard = handler.state.lock().await;
    let mut option_future = Box::pin(handler.handle(Request::SetOptionByName(Box::new(
        SetOptionByNameRequest {
            scope: OptionScopeSelector::Window(WindowTarget::with_window(source.clone(), 1)),
            name: "aggressive-resize".to_owned(),
            value: Some("on".to_owned()),
            mode: SetOptionMode::Replace,
            only_if_unset: false,
            unset: false,
            unset_pane_overrides: false,
            format: false,
            format_target: None,
        },
    ))));
    tokio::select! {
        biased;
        response = &mut option_future => {
            panic!("set-option bypassed the held state lock: {response:?}");
        }
        () = tokio::task::yield_now() => {}
    }

    let mut rename_future =
        Box::pin(handler.handle(Request::RenameSession(RenameSessionRequest {
            target: source.clone(),
            new_name: renamed.clone(),
        })));
    tokio::select! {
        biased;
        response = &mut rename_future => {
            panic!("rename-session bypassed the held state lock: {response:?}");
        }
        () = tokio::task::yield_now() => {}
    }

    drop(state_guard);
    let (option_response, rename_response) = tokio::join!(option_future, rename_future);
    assert!(
        matches!(option_response, Response::SetOptionByName(_)),
        "{option_response:?}"
    );
    assert!(
        matches!(rename_response, Response::RenameSession(_)),
        "{rename_response:?}"
    );

    assert_window_and_pty_size(&handler, &renamed, 1, LARGE_SIZE).await;
    assert_window_and_pty_size(&handler, &alias, 1, LARGE_SIZE).await;
}

#[tokio::test]
async fn resize_option_reconciliation_retries_a_rename_after_stable_selection() {
    let (handler, source, alias, _source_rx, _alias_rx) =
        create_linked_inactive_window_fixture("resize-option-selected-rename", 8_401).await;

    set_window_size_policy(&handler, &source, 1, "smallest").await;
    assert_window_and_pty_size(&handler, &source, 1, SMALL_SIZE).await;
    assert_window_and_pty_size(&handler, &alias, 1, SMALL_SIZE).await;

    let renamed = rmux_proto::SessionName::new("resize-option-selected-rename-renamed")
        .expect("valid renamed session name");
    let pause = handler.install_attached_size_selection_pause();
    let set_option = handler.handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
        scope: OptionScopeSelector::Window(WindowTarget::with_window(source.clone(), 1)),
        name: "aggressive-resize".to_owned(),
        value: Some("on".to_owned()),
        mode: SetOptionMode::Replace,
        only_if_unset: false,
        unset: false,
        unset_pane_overrides: false,
        format: false,
        format_target: None,
    })));
    let rename_after_selection = async {
        pause.reached.notified().await;
        let response = handler
            .handle(Request::RenameSession(RenameSessionRequest {
                target: source.clone(),
                new_name: renamed.clone(),
            }))
            .await;
        pause.release.notify_one();
        response
    };
    let (option_response, rename_response) = tokio::join!(set_option, rename_after_selection);
    assert!(
        matches!(option_response, Response::SetOptionByName(_)),
        "{option_response:?}"
    );
    assert!(
        matches!(rename_response, Response::RenameSession(_)),
        "{rename_response:?}"
    );

    assert_window_and_pty_size(&handler, &renamed, 1, LARGE_SIZE).await;
    assert_window_and_pty_size(&handler, &alias, 1, LARGE_SIZE).await;
}

#[tokio::test]
async fn window_global_resize_reconciliation_follows_a_rename_after_selection() {
    let handler = RequestHandler::new();
    let session = create_sized_session(&handler, "resize-option-global-rename", LARGE_SIZE).await;
    handler.wait_for_initial_panes_for_test().await;
    let _small_rx = register_sized_attach(&handler, 8_451, &session, SMALL_SIZE).await;
    let _large_rx = register_sized_attach(&handler, 8_452, &session, LARGE_SIZE).await;

    let response = handler
        .handle(window_size_option_request(
            OptionScopeSelector::WindowGlobal,
            "smallest",
        ))
        .await;
    assert!(
        matches!(response, Response::SetOptionByName(_)),
        "{response:?}"
    );
    assert_window_and_pty_size(&handler, &session, 0, SMALL_SIZE).await;

    let renamed = rmux_proto::SessionName::new("resize-option-global-renamed")
        .expect("valid renamed session name");
    let pause = handler.install_attached_size_selection_pause();
    let set_global = handler.handle(window_size_option_request(
        OptionScopeSelector::WindowGlobal,
        "largest",
    ));
    let rename_after_selection = async {
        pause.reached.notified().await;
        let response = handler
            .handle(Request::RenameSession(RenameSessionRequest {
                target: session.clone(),
                new_name: renamed.clone(),
            }))
            .await;
        pause.release.notify_one();
        response
    };
    let (option_response, rename_response) = tokio::join!(set_global, rename_after_selection);
    assert!(
        matches!(option_response, Response::SetOptionByName(_)),
        "{option_response:?}"
    );
    assert!(
        matches!(rename_response, Response::RenameSession(_)),
        "{rename_response:?}"
    );
    assert_window_and_pty_size(&handler, &renamed, 0, LARGE_SIZE).await;
}

#[tokio::test]
async fn session_resize_reconciliation_follows_a_rename_after_selection() {
    let handler = RequestHandler::new();
    let session = create_sized_session(&handler, "resize-option-session-rename", LARGE_SIZE).await;
    handler.wait_for_initial_panes_for_test().await;
    let _small_rx = register_sized_attach(&handler, 8_471, &session, SMALL_SIZE).await;
    let _large_rx = register_sized_attach(&handler, 8_472, &session, LARGE_SIZE).await;

    let response = handler
        .handle(window_size_option_request(
            OptionScopeSelector::Session(session.clone()),
            "smallest",
        ))
        .await;
    assert!(
        matches!(response, Response::SetOptionByName(_)),
        "{response:?}"
    );
    assert_window_and_pty_size(&handler, &session, 0, SMALL_SIZE).await;

    let renamed = rmux_proto::SessionName::new("resize-option-session-renamed")
        .expect("valid renamed session name");
    let pause = handler.install_attached_size_selection_pause();
    let set_session = handler.handle(window_size_option_request(
        OptionScopeSelector::Session(session.clone()),
        "largest",
    ));
    let rename_after_selection = async {
        pause.reached.notified().await;
        let response = handler
            .handle(Request::RenameSession(RenameSessionRequest {
                target: session.clone(),
                new_name: renamed.clone(),
            }))
            .await;
        pause.release.notify_one();
        response
    };
    let (option_response, rename_response) = tokio::join!(set_session, rename_after_selection);
    assert!(
        matches!(option_response, Response::SetOptionByName(_)),
        "{option_response:?}"
    );
    assert!(
        matches!(rename_response, Response::RenameSession(_)),
        "{rename_response:?}"
    );
    assert_window_and_pty_size(&handler, &renamed, 0, LARGE_SIZE).await;
}

#[tokio::test]
async fn resize_event_keeps_its_exact_identity_across_post_apply_rename() {
    let handler = RequestHandler::new();
    let session = create_sized_session(&handler, "resize-event-stable", LARGE_SIZE).await;
    handler.wait_for_initial_panes_for_test().await;
    let _small_rx = register_sized_attach(&handler, 8_501, &session, SMALL_SIZE).await;
    let _large_rx = register_sized_attach(&handler, 8_502, &session, LARGE_SIZE).await;
    set_window_size_policy(&handler, &session, 0, "smallest").await;

    let hook_response = handler
        .handle(Request::SetHook(SetHookRequest {
            scope: ScopeSelector::Window(WindowTarget::with_window(session.clone(), 0)),
            hook: HookName::WindowResized,
            command: "rename-window resize-hook-ran".to_owned(),
            lifecycle: HookLifecycle::OneShot,
        }))
        .await;
    assert!(
        matches!(hook_response, Response::SetHook(_)),
        "{hook_response:?}"
    );
    let mut events = handler.subscribe_lifecycle_events();
    let pause = handler.install_window_lifecycle_emit_pause();
    let resize = handler.handle(window_size_option_request(
        OptionScopeSelector::Window(WindowTarget::with_window(session.clone(), 0)),
        "largest",
    ));
    let renamed = rmux_proto::SessionName::new("resize-event-stable-renamed")
        .expect("valid renamed session name");
    let rename_after_apply = async {
        pause.reached.notified().await;
        let response = handler
            .handle(Request::RenameSession(RenameSessionRequest {
                target: session.clone(),
                new_name: renamed.clone(),
            }))
            .await;
        pause.release.notify_one();
        response
    };
    let (resize_response, rename_response) = tokio::join!(resize, rename_after_apply);
    assert!(
        matches!(resize_response, Response::SetOptionByName(_)),
        "{resize_response:?}"
    );
    assert!(
        matches!(rename_response, Response::RenameSession(_)),
        "{rename_response:?}"
    );

    let event = receive_window_resized(&mut events).await;
    assert_eq!(event.hooks.len(), 1, "one-shot hook is captured once");
    handler.dispatch_lifecycle_hook(event).await;
    {
        let state = handler.state.lock().await;
        assert_eq!(
            state
                .sessions
                .session(&renamed)
                .expect("renamed session survives")
                .window_at(0)
                .expect("resized window survives")
                .name(),
            Some("resize-hook-ran")
        );
    }

    let response = handler
        .handle(window_size_option_request(
            OptionScopeSelector::Window(WindowTarget::with_window(renamed, 0)),
            "smallest",
        ))
        .await;
    assert!(
        matches!(response, Response::SetOptionByName(_)),
        "{response:?}"
    );
    let second_event = receive_window_resized(&mut events).await;
    assert!(
        second_event.hooks.is_empty(),
        "the prepared one-shot hook must not be emitted twice"
    );
}

#[tokio::test]
async fn hooks_disabled_resize_preserves_the_one_shot_hook() {
    let handler = RequestHandler::new();
    let session = create_sized_session(&handler, "resize-hook-disabled", LARGE_SIZE).await;
    handler.wait_for_initial_panes_for_test().await;
    let _small_rx = register_sized_attach(&handler, 8_551, &session, SMALL_SIZE).await;
    let _large_rx = register_sized_attach(&handler, 8_552, &session, LARGE_SIZE).await;
    set_window_size_policy(&handler, &session, 0, "smallest").await;

    let target = WindowTarget::with_window(session.clone(), 0);
    let hook_response = handler
        .handle(Request::SetHook(SetHookRequest {
            scope: ScopeSelector::Window(target.clone()),
            hook: HookName::WindowResized,
            command: "display-message preserved-one-shot".to_owned(),
            lifecycle: HookLifecycle::OneShot,
        }))
        .await;
    assert!(
        matches!(hook_response, Response::SetHook(_)),
        "{hook_response:?}"
    );
    let mut events = handler.subscribe_lifecycle_events();

    let response = crate::hook_runtime::with_hook_execution(Vec::new(), async {
        handler
            .handle(window_size_option_request(
                OptionScopeSelector::Window(target.clone()),
                "largest",
            ))
            .await
    })
    .await;
    assert!(
        matches!(response, Response::SetOptionByName(_)),
        "{response:?}"
    );

    let response = handler
        .handle(window_size_option_request(
            OptionScopeSelector::Window(target.clone()),
            "smallest",
        ))
        .await;
    assert!(
        matches!(response, Response::SetOptionByName(_)),
        "{response:?}"
    );
    let preserved_event = receive_window_resized(&mut events).await;
    assert_eq!(
        preserved_event.hooks.len(),
        1,
        "hooks-disabled resize must not consume the one-shot hook"
    );

    let response = handler
        .handle(window_size_option_request(
            OptionScopeSelector::Window(target),
            "largest",
        ))
        .await;
    assert!(
        matches!(response, Response::SetOptionByName(_)),
        "{response:?}"
    );
    let consumed_event = receive_window_resized(&mut events).await;
    assert!(
        consumed_event.hooks.is_empty(),
        "one-shot hook must be consumed by the first enabled resize"
    );
}
