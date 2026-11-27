//! Issue #36 (Helvesec/rmux), also problem 2 of issue #35: with two clients
//! attached to two different sessions of one daemon, a split triggered from
//! client A must land in A's session even when client B is the most recently
//! active client, and the reverse for B. The split reaches the queue through
//! both attached-key paths: a prefix binding and the `:` command prompt.

use super::*;

// This module's own pid block, distinct from every sibling module's.
const CLIENT_A_PID: u32 = 94_361;
const CLIENT_B_PID: u32 = 94_362;

async fn pane_count(handler: &RequestHandler, session: &SessionName) -> usize {
    active_panes(handler, session).await.lines().count()
}

async fn wait_for_pane_counts(
    handler: &RequestHandler,
    sessions: (&SessionName, &SessionName),
    expected: (usize, usize),
    step: &str,
) {
    let deadline = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let counts = (
            pane_count(handler, sessions.0).await,
            pane_count(handler, sessions.1).await,
        );
        if counts == expected {
            return;
        }
        assert!(
            tokio::time::Instant::now() < deadline,
            "issue #36 {step}: expected (A, B) pane counts {expected:?}, got {counts:?}"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

#[tokio::test]
async fn issue_36_split_from_an_attached_client_stays_in_that_clients_session() {
    let handler = RequestHandler::new();
    let alpha = session_name("issue36-a");
    let beta = session_name("issue36-b");
    let _control_a = create_attached_session(&handler, CLIENT_A_PID, &alpha).await;
    let _control_b = create_attached_session(&handler, CLIENT_B_PID, &beta).await;
    wait_for_pane_counts(&handler, (&alpha, &beta), (1, 1), "initial layout").await;

    // B attached last and types last: it is the most recent client when A
    // presses its split binding.
    handler
        .handle_attached_live_input_for_test(CLIENT_B_PID, b" ")
        .await
        .expect("client B input");

    handler
        .handle_attached_live_input_for_test(CLIENT_A_PID, b"\x02%")
        .await
        .expect("prefix % from client A");
    wait_for_pane_counts(&handler, (&alpha, &beta), (2, 1), "prefix % from A").await;

    handler
        .handle_attached_live_input_for_test(CLIENT_A_PID, b"\x02:split-window -v\r")
        .await
        .expect("command-prompt split-window from client A");
    wait_for_pane_counts(&handler, (&alpha, &beta), (3, 1), "prompt split from A").await;

    handler
        .handle_attached_live_input_for_test(CLIENT_B_PID, b"\x02\"")
        .await
        .expect("prefix \" from client B");
    wait_for_pane_counts(&handler, (&alpha, &beta), (3, 2), "prefix \" from B").await;
}
