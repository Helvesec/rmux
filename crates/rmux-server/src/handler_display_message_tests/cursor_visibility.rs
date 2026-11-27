//! `#{cursor_flag}` follows the `MODE_CURSOR` bit of the *targeted* pane's own
//! terminal screen.
//!
//! Both expectations below are literals measured on the acquired tmux 3.7b
//! before this correction existed, with the same fixture: an 80×24 window split
//! vertically, the neighbour active and the measured pane not. There the same
//! producer gives `flag=[1]` by default, `flag=[0]` after one six-byte
//! `ESC [ ? 2 5 l`, and `flag=[1]` again after one six-byte `ESC [ ? 2 5 h`,
//! while the neighbour stays at `1` throughout — in the direct reading, in the
//! `-a` inventory's single `cursor_flag` line and in the `P` loop.

use super::*;
use crate::pane_io::publish_pane_bytes_for_test;
use rmux_core::command_parser::CommandParser;
use tokio::time::{sleep, Instant};

/// A producer that announces itself once and then goes quiet for good: it writes
/// one marker and afterwards only discards its stdin, so it stays alive and
/// never writes into the transcript again.
///
/// The fixture used to leave both panes on the interactive default shell. Such a
/// shell keeps writing on its own — a prompt, and on Windows whatever ConPTY
/// emits behind it — into the very transcript this test then injects into, and
/// the pane reader publishes those bytes through the same path `feed` uses. A
/// late `CSI ? 25 h` from that writer restores `MODE_CURSOR`, which is what the
/// Windows run saw at the `P` assertion. The exact bytes of that run were not
/// retained, so this comment claims no more than that: the fixture did not own
/// its second writer, and now it does.
fn quiet_producer_command(marker: &str) -> String {
    #[cfg(unix)]
    {
        format!("printf '{marker}\\n'; cat >/dev/null")
    }
    #[cfg(windows)]
    {
        crate::test_shell::powershell_encoded_command(&format!(
            "[Console]::Out.Write('{marker}' + [char]13 + [char]10); \
             $inputStream=[Console]::OpenStandardInput(); \
             $inputStream.CopyTo([System.IO.Stream]::Null)"
        ))
    }
}

/// Pins that producer as the default command, before anything is created.
async fn pin_quiet_producer(handler: &RequestHandler, marker: &str) {
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::DefaultCommand,
            value: quiet_producer_command(marker),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

/// Waits until the producer's own marker has really been consumed into the
/// pane's transcript.
///
/// `wait_for_pane_startup_to_finish_for_test` is a necessary first step but not
/// a sufficient one: it ends when the pending INPUT of a starting pane has been
/// drained, which says nothing about the pane's stdout having been read. Seeing
/// the marker the producer itself wrote does say it. The wait is a bounded poll
/// of observed state, never a fixed sleep, and it fails loudly on timeout.
async fn wait_for_producer_marker(handler: &RequestHandler, name: &str, pane: u32, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        let captured = queue(handler, &format!("capture-pane -p -t {name}:0.{pane}")).await;
        if captured.contains(marker) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for pane {name}:0.{pane} to consume its producer marker {marker}"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

/// The pane still has its producer, and that producer is still alive.
async fn assert_producer_still_alive(handler: &RequestHandler, name: &str, pane: u32) {
    assert_eq!(
        queue(
            handler,
            &format!("display-message -p -t {name}:0.{pane} 'dead=#{{pane_dead}}'")
        )
        .await,
        "dead=0",
        "the quiet producer of {name}:0.{pane} must still be alive"
    );
}

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

/// The oracle's fixture: two panes, the second one active — and, unlike R1, a
/// producer this test owns in each of them.
///
/// Each pane is pinned to its own quiet producer BEFORE it exists, its startup
/// is awaited through the existing hook, and then its producer's own marker is
/// waited for until it has actually been consumed into that pane's transcript.
/// Only then does the caller inject anything.
async fn measured_pair(name: &str) -> (RequestHandler, SessionName) {
    let handler = RequestHandler::new();
    let session = session_name(name);
    let marker_a = format!("CF1RDY-{name}-A");
    let marker_b = format!("CF1RDY-{name}-B");

    pin_quiet_producer(&handler, &marker_a).await;
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
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(session.clone(), 0, 0))
        .await;
    wait_for_producer_marker(&handler, name, 0, &marker_a).await;
    assert_producer_still_alive(&handler, name, 0).await;

    pin_quiet_producer(&handler, &marker_b).await;
    queue(&handler, &format!("split-window -v -l 7 -t {name}:0.0")).await;
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(session.clone(), 0, 1))
        .await;
    wait_for_producer_marker(&handler, name, 1, &marker_b).await;
    assert_producer_still_alive(&handler, name, 1).await;

    (handler, session)
}

/// Publishes the six bytes through the same path a pane reader uses, so the bit
/// under test is moved by the pane's own output.
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

async fn flag(handler: &RequestHandler, name: &str, pane: u32) -> String {
    queue(
        handler,
        &format!("display-message -p -t {name}:0.{pane} 'flag=[#{{cursor_flag}}]'"),
    )
    .await
}

#[tokio::test]
async fn hiding_and_showing_the_cursor_moves_the_flag() {
    let (handler, session) = measured_pair("cf1-hide").await;
    assert_eq!(
        flag(&handler, "cf1-hide", 0).await,
        "flag=[1]",
        "the fixture starts on the measured default"
    );
    feed(&handler, &session, 0, b"\x1b[?25l").await;
    assert_eq!(
        flag(&handler, "cf1-hide", 0).await,
        "flag=[0]",
        "a pane that hid its cursor reports 0, not a constant"
    );
    feed(&handler, &session, 0, b"\x1b[?25h").await;
    assert_eq!(
        flag(&handler, "cf1-hide", 0).await,
        "flag=[1]",
        "and showing it again brings the bit back"
    );
    assert_producer_still_alive(&handler, "cf1-hide", 0).await;
    assert_producer_still_alive(&handler, "cf1-hide", 1).await;
}

#[tokio::test]
async fn the_targeted_pane_its_neighbour_the_inventory_and_the_loop_agree() {
    let (handler, session) = measured_pair("cf1-target").await;
    feed(&handler, &session, 0, b"\x1b[?25l").await;

    // The split selected pane 1, so pane 0's flag can only be right if the pane
    // the target resolved to is the one that was read.
    assert_eq!(
        queue(
            &handler,
            "display-message -p -t cf1-target:0 '#{pane_index}'"
        )
        .await,
        "1"
    );
    assert_eq!(flag(&handler, "cf1-target", 0).await, "flag=[0]");
    assert_eq!(
        flag(&handler, "cf1-target", 1).await,
        "flag=[1]",
        "the neighbour received no mutation and keeps its cursor visible"
    );

    let inventory = queue(&handler, "display-message -a -p -t cf1-target:0.0").await;
    let lines: Vec<&str> = inventory
        .lines()
        .filter(|line| line.starts_with("cursor_flag="))
        .collect();
    assert_eq!(lines, ["cursor_flag=0"], "in {inventory}");
    let neighbour = queue(&handler, "display-message -a -p -t cf1-target:0.1").await;
    let neighbour_lines: Vec<&str> = neighbour
        .lines()
        .filter(|line| line.starts_with("cursor_flag="))
        .collect();
    assert_eq!(neighbour_lines, ["cursor_flag=1"], "in {neighbour}");

    assert_eq!(
        queue(
            &handler,
            "display-message -p -t cf1-target:0 '#{P:#{pane_index}=#{cursor_flag} }'"
        )
        .await,
        "0=0 1=1 "
    );
    assert_producer_still_alive(&handler, "cf1-target", 0).await;
    assert_producer_still_alive(&handler, "cf1-target", 1).await;
}
