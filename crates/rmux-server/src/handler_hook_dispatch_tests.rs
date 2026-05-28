use super::RequestHandler;
use rmux_proto::{
    HookLifecycle, HookName, NewSessionRequest, NewWindowRequest, Request, Response, ScopeSelector,
    SessionName, SetHookMutationRequest, TerminalSize,
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

#[tokio::test]
async fn appended_after_new_window_hooks_run_once_in_order() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;

    set_after_new_window_hook(&handler, "set-buffer -a -b hook first", false).await;
    set_after_new_window_hook(&handler, "set-buffer -a -b hook second", true).await;

    let response = handler
        .handle(Request::NewWindow(NewWindowRequest {
            target: session_name("alpha"),
            name: None,
            detached: false,
            start_directory: None,
            environment: None,
            command: None,
            target_window_index: None,
            insert_at_target: false,
        }))
        .await;
    assert!(matches!(response, Response::NewWindow(_)));

    let state = handler.state.lock().await;
    let (_, content) = state
        .buffers
        .show(Some("hook"))
        .expect("hook buffer exists");
    assert_eq!(String::from_utf8_lossy(content), "firstsecond");
}
