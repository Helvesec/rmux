use super::RequestHandler;
use rmux_core::Utf8Config;
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{
    ErrorResponse, NewSessionRequest, OptionName, Request, Response, RmuxError, ScopeSelector,
    SessionName, SetOptionByNameRequest, SetOptionMode, SetOptionRequest, TerminalSize,
    WindowTarget,
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

#[tokio::test]
async fn set_option_updates_the_store_and_session_values_override_global() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;

    assert_eq!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::Status,
                value: "off".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(rmux_proto::SetOptionResponse {
            scope: ScopeSelector::Global,
            option: OptionName::Status,
            mode: SetOptionMode::Replace,
        })
    );
    assert_eq!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Session(session_name("alpha")),
                option: OptionName::Status,
                value: "on".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(rmux_proto::SetOptionResponse {
            scope: ScopeSelector::Session(session_name("alpha")),
            option: OptionName::Status,
            mode: SetOptionMode::Replace,
        })
    );

    let state = handler.state.lock().await;
    assert_eq!(state.options.global_value(OptionName::Status), Some("off"));
    assert_eq!(
        state
            .options
            .resolve(Some(&session_name("alpha")), OptionName::Status),
        Some("on")
    );
    assert_eq!(
        state
            .options
            .resolve(Some(&session_name("beta")), OptionName::Status),
        Some("off")
    );
}

#[tokio::test]
async fn terminal_features_append_preserves_order_and_invalid_requests_fail_first() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;

    assert_eq!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::TerminalFeatures,
                value: "xterm*:RGB".to_owned(),
                mode: SetOptionMode::Append,
            }))
            .await,
        Response::SetOption(rmux_proto::SetOptionResponse {
            scope: ScopeSelector::Global,
            option: OptionName::TerminalFeatures,
            mode: SetOptionMode::Append,
        })
    );
    assert_eq!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::TerminalFeatures,
                value: "screen*:AX".to_owned(),
                mode: SetOptionMode::Append,
            }))
            .await,
        Response::SetOption(rmux_proto::SetOptionResponse {
            scope: ScopeSelector::Global,
            option: OptionName::TerminalFeatures,
            mode: SetOptionMode::Append,
        })
    );

    let scalar_append = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::Status,
            value: "off".to_owned(),
            mode: SetOptionMode::Append,
        }))
        .await;
    assert_eq!(
        scalar_append,
        Response::Error(rmux_proto::ErrorResponse {
            error: RmuxError::InvalidSetOption("status is not an array option".to_owned()),
        })
    );

    let invalid_scope = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(session_name("missing")),
            option: OptionName::TerminalFeatures,
            value: "rxvt*:ccolour".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert_eq!(
        invalid_scope,
        Response::Error(ErrorResponse {
            error: RmuxError::InvalidSetOption(
                "terminal-features is only supported at global scope".to_owned()
            ),
        })
    );

    let state = handler.state.lock().await;
    assert_eq!(
        state.options.global_value(OptionName::TerminalFeatures),
        Some(
            "xterm*:clipboard:ccolour:cstyle:focus:title,screen*:title,rxvt*:ignorefkeys,xterm*:RGB,screen*:AX"
        )
    );
    assert_eq!(state.options.global_value(OptionName::Status), None);
}

#[tokio::test]
async fn set_option_by_name_refreshes_existing_transcripts_for_server_utf8_options() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;
    let alpha = session_name("alpha");

    let before = {
        let state = handler.state.lock().await;
        state
            .transcript_utf8_config(&alpha, 0, 0)
            .expect("initial transcript exists")
    };

    assert_eq!(
        handler
            .handle(Request::SetOptionByName(SetOptionByNameRequest {
                scope: OptionScopeSelector::ServerGlobal,
                name: "variation-selector-always-wide".to_owned(),
                value: Some("off".to_owned()),
                mode: SetOptionMode::Replace,
                only_if_unset: false,
                unset: false,
                unset_pane_overrides: false,
                format: false,
                format_target: None,
            }))
            .await,
        Response::SetOptionByName(rmux_proto::SetOptionByNameResponse {
            scope: OptionScopeSelector::ServerGlobal,
            name: "variation-selector-always-wide".to_owned(),
            mode: SetOptionMode::Replace,
        })
    );

    let state = handler.state.lock().await;
    let after = state
        .transcript_utf8_config(&alpha, 0, 0)
        .expect("transcript still exists");
    let expected = Utf8Config::from_options(&state.options);

    assert_ne!(before, after);
    assert_eq!(after, expected);
}

#[tokio::test]
async fn pane_style_options_resolve_session_then_global_for_supported_variants() {
    let handler = RequestHandler::new();
    create_session(&handler, "alpha").await;
    create_session(&handler, "beta").await;
    let alpha_window = WindowTarget::with_window(session_name("alpha"), 0);

    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::PaneBorderStyle,
                value: "fg=colour1".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Global,
                option: OptionName::PaneActiveBorderStyle,
                value: "fg=colour2".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));
    assert!(matches!(
        handler
            .handle(Request::SetOption(SetOptionRequest {
                scope: ScopeSelector::Window(alpha_window),
                option: OptionName::PaneBorderStyle,
                value: "fg=colour3".to_owned(),
                mode: SetOptionMode::Replace,
            }))
            .await,
        Response::SetOption(_)
    ));

    let state = handler.state.lock().await;
    assert_eq!(
        state
            .options
            .resolve_for_window(&session_name("alpha"), 0, OptionName::PaneBorderStyle,),
        Some("fg=colour3")
    );
    assert_eq!(
        state.options.resolve_for_window(
            &session_name("alpha"),
            0,
            OptionName::PaneActiveBorderStyle,
        ),
        Some("fg=colour2")
    );
    assert_eq!(
        state.options.resolve(None, OptionName::DefaultTerminal),
        Some("tmux-256color")
    );
    assert_eq!(
        state
            .options
            .resolve_for_window(&session_name("beta"), 0, OptionName::PaneBorderStyle),
        Some("fg=colour1")
    );
    assert_eq!(
        state.options.resolve_for_window(
            &session_name("beta"),
            0,
            OptionName::PaneActiveBorderStyle,
        ),
        Some("fg=colour2")
    );
}

#[tokio::test]
async fn set_option_to_nonexistent_session_returns_session_not_found() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(session_name("missing")),
            option: OptionName::Status,
            value: "off".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;

    assert_eq!(
        response,
        Response::Error(ErrorResponse {
            error: RmuxError::SessionNotFound("missing".to_owned()),
        })
    );
}

#[tokio::test]
async fn set_option_append_empty_string_is_noop() {
    let handler = RequestHandler::new();

    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::TerminalFeatures,
            value: String::new(),
            mode: SetOptionMode::Append,
        }))
        .await;

    assert!(matches!(response, Response::SetOption(_)));

    let state = handler.state.lock().await;
    assert_eq!(
        state.options.global_value(OptionName::TerminalFeatures),
        None
    );
}
