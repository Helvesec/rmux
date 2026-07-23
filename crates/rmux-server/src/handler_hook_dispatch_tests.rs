use super::pane_group_transfer_tests::create_grouped_session;
use super::RequestHandler;
use crate::daemon::ShutdownHandle;
use rmux_proto::{
    HookLifecycle, HookName, LinkWindowRequest, NewSessionRequest, NewWindowRequest, Request,
    ResizeWindowRequest, Response, ScopeSelector, SessionName, SetHookMutationRequest,
    ShowOptionsRequest, TerminalSize, WindowTarget,
};

fn session_name(value: &str) -> SessionName {
    SessionName::new(value).expect("valid session name")
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

async fn set_after_new_window_hook(handler: &RequestHandler, command: &str, append: bool) {
    let response = handler
        .handle(Request::SetHookMutation(SetHookMutationRequest {
            scope: ScopeSelector::Global,
            hook: HookName::AfterNewWindow,
            command: Some(command.to_owned()),
            lifecycle: HookLifecycle::Persistent,
            append,
            unset: false,
            run_immediately: false,
            index: None,
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)));
}

async fn set_window_resized_hook(handler: &RequestHandler, command: &str) {
    let response = handler
        .handle(Request::SetHookMutation(SetHookMutationRequest {
            scope: ScopeSelector::Global,
            hook: HookName::WindowResized,
            command: Some(command.to_owned()),
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: false,
            run_immediately: false,
            index: None,
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)));
}

async fn set_hook(handler: &RequestHandler, scope: ScopeSelector, hook: HookName, command: &str) {
    let response = handler
        .handle(Request::SetHookMutation(SetHookMutationRequest {
            scope,
            hook,
            command: Some(command.to_owned()),
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: false,
            run_immediately: false,
            index: None,
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)), "{response:?}");
}

async fn append_hook(
    handler: &RequestHandler,
    scope: ScopeSelector,
    hook: HookName,
    command: &str,
) {
    let response = handler
        .handle(Request::SetHookMutation(SetHookMutationRequest {
            scope,
            hook,
            command: Some(command.to_owned()),
            lifecycle: HookLifecycle::Persistent,
            append: true,
            unset: false,
            run_immediately: false,
            index: None,
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)), "{response:?}");
}

async fn create_window(handler: &RequestHandler, session: &SessionName) -> WindowTarget {
    let response = handler
        .handle(Request::NewWindow(Box::new(NewWindowRequest {
            target: session.clone(),
            name: None,
            detached: true,
            start_directory: None,
            environment: None,
            command: None,
            process_command: None,
            target_window_index: None,
            insert_at_target: false,
        })))
        .await;
    let Response::NewWindow(response) = response else {
        panic!("new-window should succeed: {response:?}");
    };
    response.target
}

async fn buffer_text(handler: &RequestHandler, name: &str) -> Option<String> {
    let state = handler.state.lock().await;
    state
        .buffers
        .show(Some(name))
        .ok()
        .map(|(_, bytes)| String::from_utf8_lossy(bytes).into_owned())
}

#[tokio::test]
async fn appended_after_new_window_hooks_run_once_in_order() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;

    set_after_new_window_hook(&handler, "set-buffer -a -b hook first", false).await;
    set_after_new_window_hook(&handler, "set-buffer -a -b hook second", true).await;

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
    assert!(matches!(response, Response::NewWindow(_)));

    let state = handler.state.lock().await;
    let (_, content) = state
        .buffers
        .show(Some("hook"))
        .expect("hook buffer exists");
    assert_eq!(String::from_utf8_lossy(content), "firstsecond");
}

#[tokio::test]
async fn after_new_window_runs_distinct_kill_window_lifecycle_hooks_in_tmux_order() {
    let handler = RequestHandler::new();
    let alpha = session_name("nested-lifecycle-alpha");
    let beta = session_name("nested-lifecycle-beta");
    create_session(&handler, alpha.as_str()).await;
    create_session(&handler, beta.as_str()).await;
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::WindowUnlinked,
        "set-buffer -a -b nested-lifecycle W",
    )
    .await;
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::SessionClosed,
        "set-buffer -a -b nested-lifecycle S",
    )
    .await;
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::AfterNewWindow,
        "kill-window -t nested-lifecycle-alpha:0",
    )
    .await;

    let _ = create_window(&handler, &beta).await;

    assert_eq!(
        buffer_text(&handler, "nested-lifecycle").await.as_deref(),
        Some("WS")
    );
    assert!(handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .is_none());
}

#[tokio::test]
async fn same_after_hook_does_not_reenter_when_its_command_creates_a_window() {
    let handler = RequestHandler::new();
    let alpha = session_name("same-after-hook");
    create_session(&handler, alpha.as_str()).await;
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::AfterNewWindow,
        "set-buffer -a -b same-after-hook A",
    )
    .await;
    append_hook(
        &handler,
        ScopeSelector::Global,
        HookName::AfterNewWindow,
        "if-shell -F '#{==:#{session_windows},2}' 'new-window -d -t same-after-hook'",
    )
    .await;

    let _ = create_window(&handler, &alpha).await;

    assert_eq!(
        buffer_text(&handler, "same-after-hook").await.as_deref(),
        Some("A")
    );
    assert_eq!(
        handler
            .state
            .lock()
            .await
            .sessions
            .session(&alpha)
            .expect("session survives")
            .windows()
            .len(),
        3
    );
}

#[tokio::test]
async fn lifecycle_hook_commands_cannot_enqueue_a_second_lifecycle_generation() {
    let handler = RequestHandler::new();
    let alpha = session_name("bounded-lifecycle-alpha");
    let beta = session_name("bounded-lifecycle-beta");
    create_session(&handler, alpha.as_str()).await;
    create_session(&handler, beta.as_str()).await;
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::WindowUnlinked,
        "set-buffer -a -b bounded-lifecycle W",
    )
    .await;
    append_hook(
        &handler,
        ScopeSelector::Global,
        HookName::WindowUnlinked,
        "kill-window -t bounded-lifecycle-beta:1",
    )
    .await;
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::SessionClosed,
        "set-buffer -a -b bounded-lifecycle S",
    )
    .await;
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::AfterNewWindow,
        "kill-window -t bounded-lifecycle-alpha:0",
    )
    .await;

    let _ = create_window(&handler, &beta).await;

    assert_eq!(
        buffer_text(&handler, "bounded-lifecycle").await.as_deref(),
        Some("WS")
    );
    let state = handler.state.lock().await;
    let beta_session = state.sessions.session(&beta).expect("beta survives");
    assert_eq!(beta_session.windows().len(), 1);
    assert!(beta_session.window_at(0).is_some());
}

#[tokio::test]
async fn explicitly_run_hook_allows_one_same_lifecycle_generation() {
    let handler = RequestHandler::new();
    let alpha = session_name("same-lifecycle-alpha");
    let beta = session_name("same-lifecycle-beta");
    create_session(&handler, alpha.as_str()).await;
    create_session(&handler, beta.as_str()).await;
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::WindowUnlinked,
        "set-buffer -a -b same-lifecycle X",
    )
    .await;
    append_hook(
        &handler,
        ScopeSelector::Global,
        HookName::WindowUnlinked,
        "kill-window -t same-lifecycle-alpha:0",
    )
    .await;
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::SessionClosed,
        "set-buffer -a -b same-lifecycle S",
    )
    .await;

    let response = handler
        .handle(Request::SetHookMutation(SetHookMutationRequest {
            scope: ScopeSelector::Global,
            hook: HookName::WindowUnlinked,
            command: None,
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: false,
            run_immediately: true,
            index: None,
        }))
        .await;

    assert!(matches!(response, Response::SetHook(_)));
    assert_eq!(
        buffer_text(&handler, "same-lifecycle").await.as_deref(),
        Some("XXS")
    );
    assert!(handler
        .state
        .lock()
        .await
        .sessions
        .session(&alpha)
        .is_none());
}

#[tokio::test]
async fn command_error_hook_does_not_reenter_from_a_hook_command_failure() {
    let handler = RequestHandler::new();
    let alpha = session_name("nested-command-error");
    create_session(&handler, alpha.as_str()).await;
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::CommandError,
        "set-buffer -a -b nested-command-error E",
    )
    .await;
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::AfterNewWindow,
        "set-buffer -a -b nested-command-error A",
    )
    .await;
    append_hook(
        &handler,
        ScopeSelector::Global,
        HookName::AfterNewWindow,
        "kill-window -t missing-nested-hook:0",
    )
    .await;

    let _ = create_window(&handler, &alpha).await;

    assert_eq!(
        buffer_text(&handler, "nested-command-error")
            .await
            .as_deref(),
        Some("A")
    );
}

#[tokio::test]
async fn session_scoped_after_hook_can_run_a_distinct_session_lifecycle_hook() {
    let handler = RequestHandler::new();
    let beta = session_name("session-scoped-nested-hook");
    create_session(&handler, beta.as_str()).await;
    set_hook(
        &handler,
        ScopeSelector::Session(beta.clone()),
        HookName::WindowUnlinked,
        "set-buffer -a -b session-scoped-nested-hook W",
    )
    .await;
    set_hook(
        &handler,
        ScopeSelector::Session(beta.clone()),
        HookName::AfterNewWindow,
        "kill-window -t session-scoped-nested-hook:1",
    )
    .await;

    let _ = create_window(&handler, &beta).await;

    assert_eq!(
        buffer_text(&handler, "session-scoped-nested-hook")
            .await
            .as_deref(),
        Some("W")
    );
    let state = handler.state.lock().await;
    let session = state.sessions.session(&beta).expect("session survives");
    assert_eq!(session.windows().len(), 1);
    assert!(session.window_at(0).is_some());
}

#[tokio::test]
async fn nested_linked_last_window_hooks_keep_tmux_family_order() {
    let handler = RequestHandler::new();
    let alpha = session_name("nested-linked-alpha");
    let beta = session_name("nested-linked-beta");
    let trigger = session_name("nested-linked-trigger");
    create_session(&handler, alpha.as_str()).await;
    create_session(&handler, beta.as_str()).await;
    create_session(&handler, trigger.as_str()).await;
    assert!(matches!(
        handler
            .handle(Request::LinkWindow(LinkWindowRequest {
                source: WindowTarget::with_window(alpha.clone(), 0),
                target: WindowTarget::with_window(beta.clone(), 1),
                after: false,
                before: false,
                kill_destination: false,
                detached: true,
            }))
            .await,
        Response::LinkWindow(_)
    ));
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::WindowUnlinked,
        "set-buffer -a -b nested-linked W",
    )
    .await;
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::SessionClosed,
        "set-buffer -a -b nested-linked S",
    )
    .await;
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::AfterNewWindow,
        "kill-window -t nested-linked-alpha:0",
    )
    .await;

    let _ = create_window(&handler, &trigger).await;

    assert_eq!(
        buffer_text(&handler, "nested-linked").await.as_deref(),
        Some("WSW")
    );
}

#[tokio::test]
async fn nested_grouped_last_window_hooks_keep_tmux_family_order() {
    let handler = RequestHandler::new();
    let alpha = session_name("nested-grouped-alpha");
    let trigger = session_name("nested-grouped-trigger");
    create_session(&handler, alpha.as_str()).await;
    let beta = create_grouped_session(&handler, "nested-grouped-beta", &alpha).await;
    create_session(&handler, trigger.as_str()).await;
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::WindowUnlinked,
        "set-buffer -a -b nested-grouped W",
    )
    .await;
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::SessionClosed,
        "set-buffer -a -b nested-grouped S",
    )
    .await;
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::AfterNewWindow,
        "kill-window -t nested-grouped-alpha:0",
    )
    .await;

    let _ = create_window(&handler, &trigger).await;

    assert_eq!(
        buffer_text(&handler, "nested-grouped").await.as_deref(),
        Some("WSSW")
    );
    let state = handler.state.lock().await;
    assert!(state.sessions.session(&alpha).is_none());
    assert!(state.sessions.session(&beta).is_none());
}

#[tokio::test]
async fn nested_last_session_kill_preserves_exit_empty_shutdown() {
    let handler = RequestHandler::new();
    let (shutdown_handle, shutdown_rx) = ShutdownHandle::new();
    handler.install_shutdown_handle(shutdown_handle);
    let alpha = session_name("nested-shutdown-alpha");
    create_session(&handler, alpha.as_str()).await;
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::WindowUnlinked,
        "set-buffer -a -b nested-shutdown W",
    )
    .await;
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::SessionClosed,
        "set-buffer -a -b nested-shutdown S",
    )
    .await;
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::AfterShowOptions,
        "kill-window -t nested-shutdown-alpha:0",
    )
    .await;

    let response = handler
        .handle(Request::ShowOptions(ShowOptionsRequest {
            scope: rmux_proto::OptionScopeSelector::SessionGlobal,
            name: None,
            value_only: false,
            include_inherited: true,
            quiet: false,
            include_hooks: false,
        }))
        .await;

    assert!(matches!(response, Response::ShowOptions(_)));
    assert_eq!(
        buffer_text(&handler, "nested-shutdown").await.as_deref(),
        Some("WS")
    );
    assert!(handler.state.lock().await.sessions.is_empty());
    tokio::time::timeout(std::time::Duration::from_millis(50), shutdown_rx)
        .await
        .expect("the nested last-session kill must request exit-empty shutdown")
        .expect("shutdown receiver should complete cleanly");
}

#[tokio::test]
async fn window_resized_hook_runs_after_resize_window() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;
    set_window_resized_hook(&handler, "set-buffer -b resized yes").await;

    let response = handler
        .handle(Request::ResizeWindow(ResizeWindowRequest {
            target: WindowTarget::with_window(session_name("alpha"), 0),
            width: Some(90),
            height: Some(24),
            adjustment: None,
        }))
        .await;
    assert!(matches!(response, Response::ResizeWindow(_)));

    let state = handler.state.lock().await;
    let (_, content) = state
        .buffers
        .show(Some("resized"))
        .expect("window-resized hook buffer exists");
    assert_eq!(String::from_utf8_lossy(content), "yes");
}
