//! `#{scroll_region_upper}` and `#{scroll_region_lower}` describe the DECSTBM
//! region of the *targeted* pane's own terminal screen.
//!
//! Every expectation below is a literal measured on the pinned tmux 3.7b before
//! the correction existed, with the same fixture: a 24-row window holding two
//! tiled panes of different heights, 16 rows over 7. There, the two panes report
//! `0..15` and `0..6`; a `CSI 3;7 r` written by pane A's own child moves A to
//! `2..6` and leaves B alone; a `CSI r` from that same child brings A back to
//! `0..15`. The same values come back through `display-message -a` and through a
//! `P` loop in each of those states.

use super::*;
use crate::pane_io::publish_pane_bytes_for_test;
use rmux_core::command_parser::CommandParser;

async fn queue(handler: &RequestHandler, command: &str) -> String {
    let output = handler
        .execute_parsed_commands_for_test(
            std::process::id(),
            CommandParser::new()
                .parse(command)
                .expect("the command parses"),
        )
        .await
        .expect("the command succeeds");
    String::from_utf8(output.stdout().to_vec())
        .expect("stdout is utf-8")
        .trim_end_matches('\n')
        .to_owned()
}

/// The oracle's fixture: two tiled panes of different heights in one window.
async fn tiled_pair(name: &str) -> (RequestHandler, SessionName) {
    let handler = RequestHandler::new();
    let session = session_name(name);
    crate::session_fixture::assert_new_session(
        &handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
    );
    queue(&handler, &format!("split-window -v -l 7 -t {name}:0.0")).await;
    (handler, session)
}

/// Moves the region the way a real pane does: the bytes go through the same
/// publication path a reader uses, so the screen under test is the live one.
async fn feed(handler: &RequestHandler, session: &SessionName, pane: u32, bytes: &[u8]) {
    let (output, transcript) = {
        let state = handler.state.lock().await;
        (
            state
                .pane_output_for_target(session, 0, pane)
                .expect("the pane has an output"),
            state
                .transcript_handle(&PaneTarget::with_window(session.clone(), 0, pane))
                .expect("the pane has a transcript"),
        )
    };
    publish_pane_bytes_for_test(&transcript, &output, bytes.to_vec());
}

async fn region(handler: &RequestHandler, name: &str, pane: u32) -> String {
    queue(
        handler,
        &format!(
            "display-message -p -t {name}:0.{pane} 'height=#{{pane_height}} upper=#{{scroll_region_upper}} lower=#{{scroll_region_lower}}'"
        ),
    )
    .await
}

async fn inventory_bounds(handler: &RequestHandler, name: &str, pane: u32) -> Vec<String> {
    let rendered = queue(
        handler,
        &format!("display-message -a -p -t {name}:0.{pane}"),
    )
    .await;
    let mut lines: Vec<String> = rendered
        .lines()
        .filter(|line| line.starts_with("scroll_region_"))
        .map(str::to_owned)
        .collect();
    lines.sort();
    lines
}

#[tokio::test]
async fn a_tiled_pane_reports_its_own_bounds_not_its_window_height() {
    let (handler, _session) = tiled_pair("srf-initial").await;
    assert_eq!(
        queue(
            &handler,
            "display-message -p -t srf-initial:0 '#{window_height}'"
        )
        .await,
        "24",
        "the fixture's window is the oracle's"
    );
    assert_eq!(
        region(&handler, "srf-initial", 0).await,
        "height=16 upper=0 lower=15"
    );
    assert_eq!(
        region(&handler, "srf-initial", 1).await,
        "height=7 upper=0 lower=6"
    );
}

#[tokio::test]
async fn a_decstbm_from_the_pane_moves_only_that_pane() {
    let (handler, session) = tiled_pair("srf-decstbm").await;
    feed(&handler, &session, 0, b"\x1b[3;7r").await;
    assert_eq!(
        region(&handler, "srf-decstbm", 0).await,
        "height=16 upper=2 lower=6"
    );
    assert_eq!(
        region(&handler, "srf-decstbm", 1).await,
        "height=7 upper=0 lower=6",
        "the neighbour keeps its own region"
    );
}

#[tokio::test]
async fn the_reset_sequence_returns_the_pane_to_its_own_height() {
    let (handler, session) = tiled_pair("srf-reset").await;
    feed(&handler, &session, 0, b"\x1b[3;7r").await;
    assert_eq!(
        region(&handler, "srf-reset", 0).await,
        "height=16 upper=2 lower=6"
    );
    feed(&handler, &session, 0, b"\x1b[r").await;
    assert_eq!(
        region(&handler, "srf-reset", 0).await,
        "height=16 upper=0 lower=15",
        "the reset returns the bounds to the pane's own height, not the window's"
    );
    assert_eq!(
        region(&handler, "srf-reset", 1).await,
        "height=7 upper=0 lower=6"
    );
}

#[tokio::test]
async fn each_target_and_the_pane_loop_carry_their_own_region() {
    let (handler, session) = tiled_pair("srf-target").await;
    feed(&handler, &session, 0, b"\x1b[3;7r").await;
    // The split selected pane 1, so reading pane 0's region can only be right if
    // the pane the target resolved to is the one that was read.
    assert_eq!(
        queue(
            &handler,
            "display-message -p -t srf-target:0 '#{pane_index}'"
        )
        .await,
        "1"
    );
    assert_eq!(
        queue(
            &handler,
            "display-message -p -t srf-target:0.0 '#{scroll_region_upper}-#{scroll_region_lower}'"
        )
        .await,
        "2-6"
    );
    assert_eq!(
        queue(
            &handler,
            "display-message -p -t srf-target:0.1 '#{scroll_region_upper}-#{scroll_region_lower}'"
        )
        .await,
        "0-6"
    );
    assert_eq!(
        queue(
            &handler,
            "display-message -p -t srf-target:0 '#{P:#{pane_index}:#{scroll_region_upper}-#{scroll_region_lower} }'"
        )
        .await,
        "0:2-6 1:0-6 "
    );
}

#[tokio::test]
async fn the_display_message_inventory_carries_the_same_bounds() {
    let (handler, session) = tiled_pair("srf-all").await;
    assert_eq!(
        inventory_bounds(&handler, "srf-all", 0).await,
        ["scroll_region_lower=15", "scroll_region_upper=0"]
    );
    feed(&handler, &session, 0, b"\x1b[3;7r").await;
    assert_eq!(
        inventory_bounds(&handler, "srf-all", 0).await,
        ["scroll_region_lower=6", "scroll_region_upper=2"]
    );
    assert_eq!(
        inventory_bounds(&handler, "srf-all", 1).await,
        ["scroll_region_lower=6", "scroll_region_upper=0"]
    );
}
