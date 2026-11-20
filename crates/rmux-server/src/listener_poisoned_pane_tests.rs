//! Listener-level coverage for a request that names a pane whose transcript
//! is poisoned: the client reads a framed error and the same connection keeps
//! serving later requests, instead of seeing the connection close.

use rmux_proto::{
    CapturePaneRequest, DisplayMessageRequest, ErrorResponse, ListSessionsRequest, PaneTarget,
    RmuxError, Target,
};

use super::connection_test_support::{
    connected_streams, finish_connection, read_test_response, spawn_connection, start_quiet_pane,
    write_test_request,
};
use super::*;

fn capture_pane(target: PaneTarget) -> Request {
    Request::CapturePane(Box::new(CapturePaneRequest {
        target,
        start: None,
        end: None,
        print: true,
        buffer_name: None,
        alternate: false,
        escape_ansi: false,
        escape_sequences: false,
        include_format: false,
        hyperlinks: false,
        line_numbers: false,
        join_wrapped: false,
        use_mode_screen: false,
        preserve_trailing_spaces: false,
        do_not_trim_spaces: false,
        pending_input: false,
        quiet: false,
        start_is_absolute: false,
        end_is_absolute: false,
    }))
}

#[tokio::test]
async fn poisoned_pane_request_is_answered_and_the_connection_stays_served() -> io::Result<()> {
    let handler = Arc::new(RequestHandler::new());
    let poisoned = start_quiet_pane(&handler, "poisoned-conn").await;
    let healthy = start_quiet_pane(&handler, "healthy-conn").await;
    let displayed = handler
        .handle(Request::DisplayMessage(DisplayMessageRequest {
            target: Some(Target::Pane(poisoned.clone())),
            print: true,
            message: Some("#{pane_id}".to_owned()),
            empty_target_context: false,
        }))
        .await;
    let pane_id = String::from_utf8_lossy(
        displayed
            .command_output()
            .expect("display-message -p prints the pane id")
            .stdout(),
    )
    .trim_end()
    .to_owned();
    handler.poison_pane_transcript_for_test(&poisoned).await;

    // The server owner on every platform: a uid on Unix, a SID on Windows.
    let peer = PeerIdentity {
        pid: std::process::id(),
        uid: crate::server_access::current_owner_uid(),
        user: rmux_os::identity::IdentityResolver::current()?,
    };
    let (server, mut client) = connected_streams("poisoned-pane-request").await?;
    let (_shutdown_tx, connection) = spawn_connection(&handler, peer, server);

    write_test_request(&mut client, capture_pane(poisoned)).await?;
    assert_eq!(
        read_test_response(&mut client).await?,
        Response::Error(ErrorResponse {
            error: RmuxError::Message(format!("pane {pane_id} unavailable")),
        })
    );

    write_test_request(&mut client, capture_pane(healthy)).await?;
    assert!(matches!(
        read_test_response(&mut client).await?,
        Response::CapturePane(_)
    ));
    write_test_request(
        &mut client,
        Request::ListSessions(ListSessionsRequest {
            format: Some("#{session_name}".to_owned()),
            filter: None,
            sort_order: None,
            reversed: false,
        }),
    )
    .await?;
    let Response::ListSessions(sessions) = read_test_response(&mut client).await? else {
        panic!("list-sessions must succeed on the same connection");
    };
    assert_eq!(
        String::from_utf8_lossy(sessions.output.stdout()),
        "healthy-conn\npoisoned-conn\n"
    );

    drop(client);
    finish_connection(connection).await
}
