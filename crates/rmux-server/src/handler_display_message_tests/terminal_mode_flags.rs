//! `#{insert_flag}`, `#{keypad_cursor_flag}`, `#{keypad_flag}` and
//! `#{origin_flag}` each follow their own bit of the *targeted* pane's terminal
//! screen.
//!
//! Every expectation below is a literal measured on the acquired tmux 3.7b
//! before this correction existed, with the same fixture: an 80×24 window split
//! vertically, the neighbour active and the measured pane not. There the same
//! producer walks six states — `0000`, then `1000`, `0100`, `0010`, `0001` and
//! back to `0000` — through five payloads it writes itself, while the neighbour
//! stays at `0000` in every reading: direct, the four single lines of the `-a`
//! inventory, and the `P` loop.

use super::*;
use crate::pane_io::publish_pane_bytes_for_test;
use rmux_core::command_parser::CommandParser;
use rmux_core::formats::{FormatContext, FormatVariables};
use tokio::time::{sleep, Instant};

use crate::format_runtime::RuntimeFormatContext;

/// The four names, in the order the fixture reads them.
const NAMES: [&str; 4] = [
    "insert_flag",
    "keypad_cursor_flag",
    "keypad_flag",
    "origin_flag",
];

/// The five payloads, exactly as the oracle's producer wrote them.
const SET_INSERT: &[u8] = b"\x1b[4h";
const CLEAR_INSERT_SET_KCURSOR: &[u8] = b"\x1b[4l\x1b[?1h";
const CLEAR_KCURSOR_SET_KEYPAD: &[u8] = b"\x1b[?1l\x1b=";
const CLEAR_KEYPAD_SET_ORIGIN: &[u8] = b"\x1b>\x1b[?6h";
const CLEAR_ORIGIN: &[u8] = b"\x1b[?6l";

pub(super) async fn queue(handler: &RequestHandler, command: &str) -> String {
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

/// A producer that announces itself once and then goes quiet for good, so
/// nothing it writes can reach the transcript while the fixture measures. This
/// is the acquired CF1 R2 recipe.
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

pub(super) async fn pin_quiet_producer(handler: &RequestHandler, marker: &str) {
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
/// pane's transcript: the startup hook ends when a starting pane's pending INPUT
/// is drained and says nothing about its stdout having been read. A bounded poll
/// of observed state, never a fixed sleep.
pub(super) async fn wait_for_producer_marker(
    handler: &RequestHandler,
    name: &str,
    pane: u32,
    marker: &str,
) {
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

/// The oracle's fixture: two panes, the second one active, each with a producer
/// this test owns and has seen settle.
pub(super) async fn measured_pair(name: &str) -> (RequestHandler, SessionName) {
    let handler = RequestHandler::new();
    let session = session_name(name);
    let marker_a = format!("MF1RDY-{name}-A");
    let marker_b = format!("MF1RDY-{name}-B");

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

    pin_quiet_producer(&handler, &marker_b).await;
    queue(&handler, &format!("split-window -v -l 7 -t {name}:0.0")).await;
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(session.clone(), 0, 1))
        .await;
    wait_for_producer_marker(&handler, name, 1, &marker_b).await;

    (handler, session)
}

/// Publishes a payload through the same path a pane reader uses.
pub(super) async fn feed(handler: &RequestHandler, session: &SessionName, pane: u32, bytes: &[u8]) {
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

async fn direct(handler: &RequestHandler, name: &str, pane: u32) -> String {
    queue(
        handler,
        &format!(
            "display-message -p -t {name}:0.{pane} 'i=#{{insert_flag}};kc=#{{keypad_cursor_flag}};kp=#{{keypad_flag}};o=#{{origin_flag}}'"
        ),
    )
    .await
}

/// The four inventory lines of one pane, each of which must appear exactly once.
async fn inventory(handler: &RequestHandler, name: &str, pane: u32) -> Vec<String> {
    let rendered = queue(
        handler,
        &format!("display-message -a -p -t {name}:0.{pane}"),
    )
    .await;
    NAMES
        .iter()
        .map(|wanted| {
            let lines: Vec<&str> = rendered
                .lines()
                .filter(|line| line.starts_with(&format!("{wanted}=")))
                .collect();
            assert_eq!(lines.len(), 1, "one {wanted} line expected in {rendered}");
            lines[0].to_owned()
        })
        .collect()
}

fn expected_inventory(bits: &str) -> Vec<String> {
    NAMES
        .iter()
        .zip(bits.chars())
        .map(|(name, bit)| format!("{name}={bit}"))
        .collect()
}

async fn pane_loop(handler: &RequestHandler, name: &str) -> String {
    queue(
        handler,
        &format!(
            "display-message -p -t {name}:0 '#{{P:#{{pane_index}}=#{{insert_flag}}#{{keypad_cursor_flag}}#{{keypad_flag}}#{{origin_flag}} }}'"
        ),
    )
    .await
}

#[tokio::test]
async fn each_mode_moves_its_own_flag_and_leaves_the_other_three_alone() {
    let (handler, session) = measured_pair("mf1-transitions").await;
    let name = "mf1-transitions";

    assert_eq!(direct(&handler, name, 0).await, "i=0;kc=0;kp=0;o=0");
    feed(&handler, &session, 0, SET_INSERT).await;
    assert_eq!(
        direct(&handler, name, 0).await,
        "i=1;kc=0;kp=0;o=0",
        "insert mode alone"
    );
    feed(&handler, &session, 0, CLEAR_INSERT_SET_KCURSOR).await;
    assert_eq!(
        direct(&handler, name, 0).await,
        "i=0;kc=1;kp=0;o=0",
        "application cursor keys alone"
    );
    feed(&handler, &session, 0, CLEAR_KCURSOR_SET_KEYPAD).await;
    assert_eq!(
        direct(&handler, name, 0).await,
        "i=0;kc=0;kp=1;o=0",
        "application keypad alone"
    );
    feed(&handler, &session, 0, CLEAR_KEYPAD_SET_ORIGIN).await;
    assert_eq!(
        direct(&handler, name, 0).await,
        "i=0;kc=0;kp=0;o=1",
        "origin mode alone"
    );
    feed(&handler, &session, 0, CLEAR_ORIGIN).await;
    assert_eq!(
        direct(&handler, name, 0).await,
        "i=0;kc=0;kp=0;o=0",
        "and every bit is back down"
    );
}

#[tokio::test]
async fn the_targeted_pane_its_neighbour_the_inventory_and_the_loop_agree() {
    let (handler, session) = measured_pair("mf1-target").await;
    let name = "mf1-target";
    feed(&handler, &session, 0, SET_INSERT).await;
    feed(&handler, &session, 0, CLEAR_INSERT_SET_KCURSOR).await;

    // The split selected pane 1, so pane 0's bits can only be right if the pane
    // the target resolved to is the one that was read.
    assert_eq!(
        queue(
            &handler,
            "display-message -p -t mf1-target:0 '#{pane_index}'"
        )
        .await,
        "1"
    );
    assert_eq!(direct(&handler, name, 0).await, "i=0;kc=1;kp=0;o=0");
    assert_eq!(
        direct(&handler, name, 1).await,
        "i=0;kc=0;kp=0;o=0",
        "the neighbour received no payload and keeps every bit down"
    );
    assert_eq!(
        inventory(&handler, name, 0).await,
        expected_inventory("0100")
    );
    assert_eq!(
        inventory(&handler, name, 1).await,
        expected_inventory("0000")
    );
    assert_eq!(pane_loop(&handler, name).await, "0=0100 1=0000 ");
}

#[tokio::test]
async fn the_four_names_are_empty_without_a_pane_context() {
    // tmux returns NULL for each of the four callbacks when the format tree has
    // no pane (`format.c:1818-1827`, `1832-1841`, `1844-1853`, `1999-2008`),
    // which the registered names render as the empty value. A context with no
    // state and no pane must give that, and never the constant `0` the old
    // grouped branch returned whatever the pane did. This is the callback
    // contract plus this server's existing fallback; no contextless CLI cell is
    // claimed for it.
    let context = RuntimeFormatContext::new(FormatContext::new());

    for name in NAMES {
        assert_eq!(
            context.format_value_by_name(name).as_deref(),
            Some(""),
            "{name} without a pane context"
        );
    }
}
