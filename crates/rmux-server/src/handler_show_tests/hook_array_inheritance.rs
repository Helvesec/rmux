use super::{create_session, session_name, RequestHandler};
use rmux_core::HookScopeIdentity;
use rmux_proto::{
    HookLifecycle, HookName, OptionScopeSelector, PaneTarget, Request, Response, ScopeSelector,
    SetHookMutationRequest, ShowOptionsRequest, WindowTarget,
};

async fn set_hook(
    handler: &RequestHandler,
    scope: ScopeSelector,
    hook: HookName,
    index: u32,
    value: &str,
) {
    let response = handler
        .handle(Request::SetHookMutation(SetHookMutationRequest {
            scope,
            hook,
            command: Some(format!("display-message {value}")),
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: false,
            run_immediately: false,
            index: Some(index),
        }))
        .await;
    assert!(matches!(response, Response::SetHook(_)), "{response:?}");
}

async fn show(
    handler: &RequestHandler,
    scope: OptionScopeSelector,
    name: Option<String>,
    inherited: bool,
    value_only: bool,
) -> Vec<u8> {
    let response = handler
        .handle(Request::ShowOptions(ShowOptionsRequest {
            scope,
            name,
            value_only,
            include_inherited: inherited,
            quiet: false,
            include_hooks: true,
        }))
        .await;
    response
        .command_output()
        .expect("hook output")
        .stdout()
        .to_vec()
}

#[tokio::test]
async fn show_options_indexed_session_hook_resolves_array_before_index() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;
    let scope = OptionScopeSelector::Session(session_name("alpha"));
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::AfterNewWindow,
        2,
        "global-value",
    )
    .await;
    set_hook(
        &handler,
        ScopeSelector::Session(session_name("alpha")),
        HookName::AfterNewWindow,
        1,
        "local-value",
    )
    .await;
    assert_eq!(
        show(
            &handler,
            scope.clone(),
            Some("after-new-window[2]".into()),
            true,
            true
        )
        .await,
        b""
    );
    assert_eq!(
        show(
            &handler,
            scope.clone(),
            Some("after-new-window[2]".into()),
            true,
            false
        )
        .await,
        b"after-new-window[2] \n"
    );
    assert_eq!(
        show(
            &handler,
            scope.clone(),
            Some("after-new-window[1]".into()),
            true,
            true
        )
        .await,
        b"display-message local-value\n"
    );
    assert_eq!(
        show(
            &handler,
            scope.clone(),
            Some("after-new-window".into()),
            true,
            false
        )
        .await,
        b"after-new-window[1] display-message local-value\n"
    );
    assert_eq!(
        show(
            &handler,
            scope.clone(),
            Some("after-new-window[2]".into()),
            false,
            false
        )
        .await,
        b""
    );
    let listed = String::from_utf8(show(&handler, scope, None, true, false).await).unwrap();
    assert!(listed.contains("after-new-window[1] display-message local-value\n"));
    assert!(!listed.contains("global-value"));
    let mut state = handler.state.lock().await;
    let mut legacy = state.hooks.clone();
    assert_eq!(
        legacy
            .dispatch(
                &ScopeSelector::Session(session_name("alpha")),
                HookName::AfterNewWindow
            )
            .iter()
            .map(|d| d.command())
            .collect::<Vec<_>>(),
        ["display-message local-value"]
    );
    assert_eq!(
        state
            .hooks
            .dispatch_with_identity(
                &HookScopeIdentity::Session(session_name("alpha")),
                HookName::AfterNewWindow
            )
            .iter()
            .map(|d| d.command())
            .collect::<Vec<_>>(),
        ["display-message local-value"]
    );
}

#[tokio::test]
async fn show_options_indexed_window_and_pane_hooks_resolve_nearest_array() {
    for pane in [false, true] {
        let handler = RequestHandler::new();
        create_session(&handler, "alpha").await;
        let window = WindowTarget::with_window(session_name("alpha"), 0);
        let target = PaneTarget::with_window(session_name("alpha"), 0, 0);
        let hook = if pane {
            HookName::PaneExited
        } else {
            HookName::WindowResized
        };
        let name = hook.to_string();
        let scope = if pane {
            OptionScopeSelector::Pane(target.clone())
        } else {
            OptionScopeSelector::Window(window.clone())
        };
        set_hook(&handler, ScopeSelector::Global, hook, 2, "global-value").await;
        assert_eq!(
            show(
                &handler,
                scope.clone(),
                Some(format!("{name}[2]")),
                true,
                false
            )
            .await,
            format!("{name}[2]* display-message global-value\n").as_bytes()
        );
        set_hook(
            &handler,
            ScopeSelector::Window(window.clone()),
            hook,
            1,
            "window-value",
        )
        .await;
        assert_eq!(
            show(
                &handler,
                scope.clone(),
                Some(format!("{name}[2]")),
                true,
                true
            )
            .await,
            b""
        );
        assert_eq!(
            show(
                &handler,
                scope.clone(),
                Some(format!("{name}[2]")),
                true,
                false
            )
            .await,
            format!("{name}[2]{} \n", if pane { "*" } else { "" }).as_bytes()
        );
        assert_eq!(
            show(&handler, scope.clone(), Some(name.clone()), true, false).await,
            format!(
                "{name}[1]{} display-message window-value\n",
                if pane { "*" } else { "" }
            )
            .as_bytes()
        );
        if pane {
            set_hook(&handler, ScopeSelector::Pane(target), hook, 3, "pane-value").await;
            assert_eq!(
                show(
                    &handler,
                    scope.clone(),
                    Some(format!("{name}[1]")),
                    true,
                    true
                )
                .await,
                b""
            );
            assert_eq!(
                show(
                    &handler,
                    scope.clone(),
                    Some(format!("{name}[1]")),
                    true,
                    false
                )
                .await,
                format!("{name}[1] \n").as_bytes()
            );
            assert_eq!(
                show(
                    &handler,
                    scope.clone(),
                    Some(format!("{name}[3]")),
                    false,
                    true
                )
                .await,
                b"display-message pane-value\n"
            );
        }
    }
}

#[tokio::test]
async fn show_options_indexed_session_without_local_hook_keeps_inherited_markers() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;
    let scope = OptionScopeSelector::Session(session_name("alpha"));
    set_hook(
        &handler,
        ScopeSelector::Global,
        HookName::AfterNewWindow,
        2,
        "global-value",
    )
    .await;
    assert_eq!(
        show(
            &handler,
            scope.clone(),
            Some("after-new-window[2]".into()),
            true,
            false
        )
        .await,
        b"after-new-window[2]* display-message global-value\n"
    );
    assert_eq!(
        show(
            &handler,
            scope.clone(),
            Some("after-new-window[2]".into()),
            true,
            true
        )
        .await,
        b"display-message global-value\n"
    );
    assert_eq!(
        show(
            &handler,
            scope,
            Some("after-new-window[2]".into()),
            false,
            true
        )
        .await,
        b""
    );
}
