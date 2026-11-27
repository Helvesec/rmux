//! show-hooks and the hook lines of show-options print a hook as the command
//! list tmux 3.7b parsed (`cmd_list_print`), not the text it was given.

use super::{create_session, session_name, RequestHandler};
use rmux_proto::{
    HookLifecycle, HookName, OptionScopeSelector, Request, Response, ScopeSelector,
    SetHookMutationRequest, ShowHooksRequest, ShowOptionsRequest,
};

async fn shown_hook_line(handler: &RequestHandler, scope: ScopeSelector, command: &str) -> String {
    let response = handler
        .handle(Request::SetHookMutation(SetHookMutationRequest {
            scope: scope.clone(),
            hook: HookName::AfterNewWindow,
            command: Some(command.to_owned()),
            lifecycle: HookLifecycle::Persistent,
            append: false,
            unset: false,
            run_immediately: false,
            index: None,
        }))
        .await;
    assert!(
        matches!(response, Response::SetHook(_)),
        "{command:?}: {response:?}"
    );
    let response = handler
        .handle(Request::ShowHooks(ShowHooksRequest {
            scope,
            window: false,
            pane: false,
            hook: Some(HookName::AfterNewWindow),
        }))
        .await;
    let output = response
        .command_output()
        .expect("show-hooks should return command output");
    String::from_utf8(output.stdout().to_vec()).expect("utf8 output")
}

#[tokio::test]
async fn show_hooks_prints_the_parsed_hook_command_like_tmux() {
    // tmux 3.7b (private socket): set-hook -g / -t alpha
    // after-new-window '<command>', then show-hooks [-g | -t alpha] after-new-window.
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;

    for scope in [
        ScopeSelector::Global,
        ScopeSelector::Session(session_name("alpha")),
    ] {
        for (command, shown) in [
            ("list-sessions -F x --", "list-sessions -F x"),
            ("list-sessions --", "list-sessions"),
            ("display-message -p -- hello", "display-message -p hello"),
            ("new-window -n foo -d", "new-window -d -n foo"),
            ("select-pane -t:.+", "select-pane -t :.+"),
            (
                "if-shell true { list-sessions -F x -- }",
                "if-shell true { list-sessions -F x }",
            ),
        ] {
            assert_eq!(
                shown_hook_line(&handler, scope.clone(), command).await,
                format!("after-new-window[0] {shown}\n"),
                "{scope:?} {command:?}"
            );
        }
    }
}

#[tokio::test]
async fn show_options_prints_a_hook_value_like_show_hooks() {
    // tmux 3.7b: after set-hook -g after-new-window 'list-sessions -F x --',
    // show-options -g after-new-window prints `after-new-window[0] list-sessions -F x`
    // and show-options -gv after-new-window prints `list-sessions -F x`.
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;
    shown_hook_line(&handler, ScopeSelector::Global, "list-sessions -F x --").await;

    for (value_only, shown) in [
        (false, "after-new-window[0] list-sessions -F x\n"),
        (true, "list-sessions -F x\n"),
    ] {
        let response = handler
            .handle(Request::ShowOptions(ShowOptionsRequest {
                scope: OptionScopeSelector::SessionGlobal,
                name: Some("after-new-window".to_owned()),
                value_only,
                include_inherited: false,
                quiet: false,
                include_hooks: false,
            }))
            .await;
        let output = response
            .command_output()
            .expect("show-options should return command output");
        assert_eq!(
            std::str::from_utf8(output.stdout()).expect("utf8 output"),
            shown,
            "value_only={value_only}"
        );
    }
}
