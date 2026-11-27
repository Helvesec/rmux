//! `#{pane_tabs}` lists the columns where the *targeted* pane really holds a tab
//! stop.
//!
//! Every expectation below is a literal measured on the pinned tmux 3.7b before
//! this correction existed, with the same fixture: a 24-row, 80-column window
//! split vertically so both panes keep the full width, the neighbour active and
//! the measured pane not. There, the same producer goes through four states —
//! the default `8,16,24,32,40,48,56,64,72`, then nothing at all after `CSI 3 g`,
//! then `0,5,13` after three `ESC H` at base-0 columns 0, 5 and 13, then `0,13`
//! once the stop at 5 is cleared with `CSI 0 g`. The separator is a comma and the
//! columns are 0-based.

use super::*;
use crate::pane_io::publish_pane_bytes_for_test;
use rmux_core::command_parser::CommandParser;

/// The default grid of a fresh 80-column pane, as tmux reports it and as this
/// server's own `Screen` builds it.
const DEFAULT_STOPS: &str = "8,16,24,32,40,48,56,64,72";

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

/// The oracle's fixture: two 80-column panes, the second one active.
async fn measured_pair(name: &str) -> (RequestHandler, SessionName) {
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

/// Publishes real bytes through the same path a pane reader uses, so the bitmap
/// under test is the live one and not a value the test computed.
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

async fn tabs(handler: &RequestHandler, name: &str, pane: u32) -> String {
    queue(
        handler,
        &format!("display-message -p -t {name}:0.{pane} 'tabs=[#{{pane_tabs}}]'"),
    )
    .await
}

#[tokio::test]
async fn clearing_every_stop_leaves_the_format_empty() {
    let (handler, session) = measured_pair("ptab-clear").await;
    assert_eq!(
        tabs(&handler, "ptab-clear", 0).await,
        format!("tabs=[{DEFAULT_STOPS}]"),
        "the fixture starts on the measured default grid"
    );
    feed(&handler, &session, 0, b"\x1b[3g").await;
    assert_eq!(
        tabs(&handler, "ptab-clear", 0).await,
        "tabs=[]",
        "a pane that cleared every stop reports nothing, not the default grid"
    );
}

#[tokio::test]
async fn the_set_stops_are_listed_from_column_zero_and_a_cleared_one_disappears() {
    let (handler, session) = measured_pair("ptab-hts").await;
    feed(&handler, &session, 0, b"\x1b[3g").await;
    // CUP is 1-based, so base-0 columns 0, 5 and 13 are 1, 6 and 14.
    feed(
        &handler,
        &session,
        0,
        b"\x1b[1;1H\x1bH\x1b[1;6H\x1bH\x1b[1;14H\x1bH",
    )
    .await;
    assert_eq!(
        tabs(&handler, "ptab-hts", 0).await,
        "tabs=[0,5,13]",
        "column 0 is listed like any other, and the separator is a comma"
    );
    feed(&handler, &session, 0, b"\x1b[1;6H\x1b[0g").await;
    assert_eq!(
        tabs(&handler, "ptab-hts", 0).await,
        "tabs=[0,13]",
        "clearing the stop under the cursor removes exactly that column"
    );
}

#[tokio::test]
async fn the_targeted_pane_its_neighbour_the_inventory_and_the_loop_agree() {
    let (handler, session) = measured_pair("ptab-target").await;
    feed(&handler, &session, 0, b"\x1b[3g").await;
    feed(
        &handler,
        &session,
        0,
        b"\x1b[1;1H\x1bH\x1b[1;6H\x1bH\x1b[1;14H\x1bH",
    )
    .await;
    feed(&handler, &session, 0, b"\x1b[1;6H\x1b[0g").await;

    // The split selected pane 1, so pane 0's stops can only be right if the pane
    // the target resolved to is the one that was read.
    assert_eq!(
        queue(
            &handler,
            "display-message -p -t ptab-target:0 '#{pane_index}'"
        )
        .await,
        "1"
    );
    assert_eq!(tabs(&handler, "ptab-target", 0).await, "tabs=[0,13]");
    assert_eq!(
        tabs(&handler, "ptab-target", 1).await,
        format!("tabs=[{DEFAULT_STOPS}]"),
        "the neighbour received no mutation and keeps its own grid"
    );

    let inventory = queue(&handler, "display-message -a -p -t ptab-target:0.0").await;
    assert!(
        inventory.lines().any(|line| line == "pane_tabs=0,13"),
        "the inventory carries the same stops: {inventory}"
    );
    let neighbour = queue(&handler, "display-message -a -p -t ptab-target:0.1").await;
    assert!(
        neighbour
            .lines()
            .any(|line| line == format!("pane_tabs={DEFAULT_STOPS}")),
        "and the neighbour's inventory its own: {neighbour}"
    );

    assert_eq!(
        queue(
            &handler,
            "display-message -p -t ptab-target:0 '#{P:#{pane_index}=#{pane_tabs} }'"
        )
        .await,
        format!("0=0,13 1={DEFAULT_STOPS} ")
    );
}
