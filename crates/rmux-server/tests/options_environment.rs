#![cfg(unix)]

use std::error::Error;

mod common;

use common::{send_request, session_name, start_server, TestHarness};
use rmux_proto::{
    NewSessionRequest, OptionName, Request, Response, RmuxError, ScopeSelector,
    SetEnvironmentRequest, SetOptionMode, SetOptionRequest,
};

#[tokio::test]
async fn set_option_round_trips_and_invalid_variants_fail_cleanly() -> Result<(), Box<dyn Error>> {
    let harness = TestHarness::new("set-option");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let created = send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: None,
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let global_status = send_request(
        &socket_path,
        &Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::Status,
            value: "off".to_owned(),
            mode: SetOptionMode::Replace,
        }),
    )
    .await?;
    assert_eq!(
        global_status,
        Response::SetOption(rmux_proto::SetOptionResponse {
            scope: ScopeSelector::Global,
            option: OptionName::Status,
            mode: SetOptionMode::Replace,
        })
    );

    let session_status = send_request(
        &socket_path,
        &Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(session_name("alpha")),
            option: OptionName::Status,
            value: "on".to_owned(),
            mode: SetOptionMode::Replace,
        }),
    )
    .await?;
    assert_eq!(
        session_status,
        Response::SetOption(rmux_proto::SetOptionResponse {
            scope: ScopeSelector::Session(session_name("alpha")),
            option: OptionName::Status,
            mode: SetOptionMode::Replace,
        })
    );

    let scalar_append = send_request(
        &socket_path,
        &Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::Status,
            value: "off".to_owned(),
            mode: SetOptionMode::Append,
        }),
    )
    .await?;
    assert_eq!(
        scalar_append,
        Response::Error(rmux_proto::ErrorResponse {
            error: RmuxError::InvalidSetOption("status is not an array option".to_owned()),
        })
    );

    let invalid_scope = send_request(
        &socket_path,
        &Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Session(session_name("alpha")),
            option: OptionName::TerminalFeatures,
            value: "xterm*:RGB".to_owned(),
            mode: SetOptionMode::Replace,
        }),
    )
    .await?;
    assert_eq!(
        invalid_scope,
        Response::Error(rmux_proto::ErrorResponse {
            error: RmuxError::InvalidSetOption(
                "terminal-features is only supported at global scope".to_owned()
            ),
        })
    );

    handle.shutdown().await?;
    Ok(())
}

#[tokio::test]
async fn set_environment_round_trips_and_requires_existing_sessions() -> Result<(), Box<dyn Error>>
{
    let harness = TestHarness::new("set-environment");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let created = send_request(
        &socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: session_name("alpha"),
            detached: true,
            size: None,
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));

    let global = send_request(
        &socket_path,
        &Request::SetEnvironment(Box::new(SetEnvironmentRequest {
            scope: ScopeSelector::Global,
            name: "TERM".to_owned(),
            value: "screen".to_owned(),
            mode: None,
            hidden: false,
            format: false,
        })),
    )
    .await?;
    assert_eq!(
        global,
        Response::SetEnvironment(rmux_proto::SetEnvironmentResponse {
            scope: ScopeSelector::Global,
            name: "TERM".to_owned(),
        })
    );

    let session = send_request(
        &socket_path,
        &Request::SetEnvironment(Box::new(SetEnvironmentRequest {
            scope: ScopeSelector::Session(session_name("alpha")),
            name: "TERM".to_owned(),
            value: "tmux-256color".to_owned(),
            mode: None,
            hidden: false,
            format: false,
        })),
    )
    .await?;
    assert_eq!(
        session,
        Response::SetEnvironment(rmux_proto::SetEnvironmentResponse {
            scope: ScopeSelector::Session(session_name("alpha")),
            name: "TERM".to_owned(),
        })
    );

    let missing_session = send_request(
        &socket_path,
        &Request::SetEnvironment(Box::new(SetEnvironmentRequest {
            scope: ScopeSelector::Session(session_name("missing")),
            name: "TERM".to_owned(),
            value: "screen".to_owned(),
            mode: None,
            hidden: false,
            format: false,
        })),
    )
    .await?;
    assert_eq!(
        missing_session,
        Response::Error(rmux_proto::ErrorResponse {
            error: RmuxError::SessionNotFound("missing".to_owned()),
        })
    );

    handle.shutdown().await?;
    Ok(())
}
