//! `send-keys -R`, `scroll-on-clear` and the alternate screen.
//!
//! `send-keys -R` never writes to the child: `SendKeysExt(reset_terminal)` goes
//! through `reset_pane_terminal_state`, which re-injects `ESC c` into the pane
//! transcript and publishes `TerminalReset`. That injection reaches the real
//! `RIS` dispatch, so the reset is a whole-screen erase on the principal screen
//! and must honour `scroll-on-clear` exactly as tmux 3.7b's
//! `screen_write_reset` -> `screen_write_clearscreen` does.
//!
//! These fixtures pin that server caller: "on" moves the used visible rows into
//! scrollback and leaves the earlier history alone, "off" destroys them in
//! place and still leaves the earlier history alone, and neither sends a single
//! byte to the child.
//!
//! The pane runs the quiet command and is settled with
//! `wait_for_pane_startup_to_finish_for_test`, so the only transcript content is
//! what the seed injects. The seed itself uses absolute cursor addressing plus
//! `EL` and index-driven scrolling, never `ED 2`: it must not consult the policy
//! under test. The pane `Screen` is never replaced, because installing a fresh
//! one would silently drop the pushed policy.

use super::*;
use rmux_proto::types::OptionScopeSelector;
use rmux_proto::{CapturePaneRequest, SetOptionByNameRequest};

/// Pane geometry used by these fixtures.
const RIS_ROWS: u16 = 6;
const RIS_COLS: u16 = 40;
/// Rows the seed scrolls into scrollback before the reset.
const RIS_HISTORY_ROWS: usize = 2;
/// Used visible rows at the moment of the reset, i.e. the exact number an "on"
/// reset has to move into scrollback.
const RIS_VISIBLE_ROWS: usize = 2;

#[derive(Debug)]
struct RisResetObservation {
    history_before: usize,
    history_after: usize,
    visible: String,
    full: String,
    child_input: Option<Vec<u8>>,
}

/// Bytes that leave `RIS_HISTORY_ROWS` marker rows in scrollback and
/// `RIS_VISIBLE_ROWS` marker rows used at the top of the viewport.
///
/// Every row is first blanked with `EL` at an absolute address, the two history
/// rows are written and then scrolled off the top with two line feeds issued
/// from the bottom row, and only then are the two visible rows written. No step
/// uses `ED 2`, so the seed cannot itself exercise `scroll-on-clear`.
fn ris_seed_bytes(marker: &str) -> Vec<u8> {
    let mut bytes = String::new();
    for row in 1..=RIS_ROWS {
        bytes.push_str(&format!("\x1b[{row};1H\x1b[K"));
    }
    bytes.push_str(&format!("\x1b[1;1H{marker}OLD1"));
    bytes.push_str(&format!("\x1b[2;1H{marker}OLD2"));
    bytes.push_str(&format!("\x1b[{RIS_ROWS};1H"));
    for _ in 0..RIS_HISTORY_ROWS {
        bytes.push_str("\r\n");
    }
    bytes.push_str(&format!("\x1b[1;1H{marker}VIS1"));
    bytes.push_str(&format!("\x1b[2;1H{marker}VIS2"));
    bytes.into_bytes()
}

async fn ris_transcript(
    handler: &RequestHandler,
    target: &PaneTarget,
) -> crate::pane_transcript::SharedPaneTranscript {
    let state = handler.state.lock().await;
    state
        .transcript_handle(target)
        .expect("pane transcript must exist")
}

async fn ris_append(handler: &RequestHandler, target: &PaneTarget, bytes: &[u8]) {
    let transcript = ris_transcript(handler, target).await;
    transcript.lock().append_bytes(bytes);
}

async fn ris_history_size(handler: &RequestHandler, target: &PaneTarget) -> usize {
    let transcript = ris_transcript(handler, target).await;
    let size = transcript.lock().history_size();
    size
}

async fn ris_capture(
    handler: &RequestHandler,
    target: &PaneTarget,
    start: Option<i64>,
    end: Option<i64>,
) -> String {
    let response = handler
        .handle(Request::CapturePane(Box::new(CapturePaneRequest {
            target: target.clone(),
            start,
            end,
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
        })))
        .await;
    let Response::CapturePane(response) = response else {
        panic!("expected capture-pane response, got {response:?}");
    };
    let stdout = response
        .command_output()
        .expect("capture-pane -p returns command output")
        .stdout()
        .to_vec();
    String::from_utf8(stdout).expect("capture output is utf8")
}

async fn ris_scroll_on_clear_set(handler: &RequestHandler, value: &str) {
    let response = handler
        .handle(Request::SetOptionByName(Box::new(SetOptionByNameRequest {
            scope: OptionScopeSelector::WindowGlobal,
            name: "scroll-on-clear".to_owned(),
            value: Some(value.to_owned()),
            mode: SetOptionMode::Replace,
            only_if_unset: false,
            unset: false,
            unset_pane_overrides: false,
            format: false,
            format_target: None,
        })))
        .await;
    assert!(
        matches!(response, Response::SetOptionByName(_)),
        "{response:?}"
    );
}

/// Creates a detached session whose single pane runs the quiet command at the
/// fixture geometry, and returns the settled pane.
async fn ris_session(handler: &RequestHandler, name: &str) -> PaneTarget {
    let session = session_name(name);
    let created = handler
        .handle(Request::NewSessionExt(Box::new(NewSessionExtRequest {
            session_name: Some(session.clone()),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize {
                cols: RIS_COLS,
                rows: RIS_ROWS,
            }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: Some(quiet_pane_command()),
            process_command: None,
            client_environment: None,
            skip_environment_update: false,
        })))
        .await;
    crate::session_fixture::assert_new_session(&created);
    let target = PaneTarget::new(session, 0);
    handler
        .wait_for_pane_startup_to_finish_for_test(&target)
        .await;
    target
}

/// Seeds the pane, issues the real `send-keys -R` request and reports what the
/// reset did to history, to the viewport and to the child's input.
async fn ris_reset_and_observe(
    handler: &RequestHandler,
    target: &PaneTarget,
    marker: &str,
) -> RisResetObservation {
    ris_append(handler, target, &ris_seed_bytes(marker)).await;
    let seeded = ris_capture(handler, target, None, None).await;
    assert!(
        seeded.contains(&format!("{marker}VIS1")) && seeded.contains(&format!("{marker}VIS2")),
        "the used rows must be visible before the reset: {seeded:?}"
    );
    assert!(
        !seeded.contains(&format!("{marker}OLD1")),
        "the history rows must already have scrolled off: {seeded:?}"
    );

    let history_before = ris_history_size(handler, target).await;
    assert_eq!(
        history_before, RIS_HISTORY_ROWS,
        "the seed must leave exactly the earlier history: {seeded:?}"
    );

    {
        let state = handler.state.lock().await;
        state.start_pane_input_capture_for_test(target);
    }

    ris_send_keys_reset(handler, target).await;

    let child_input = {
        let state = handler.state.lock().await;
        state.pane_input_capture_for_test(target)
    };
    let history_after = ris_history_size(handler, target).await;
    let visible = ris_capture(handler, target, None, None).await;
    let full = ris_capture(handler, target, Some(-(history_after as i64)), None).await;
    RisResetObservation {
        history_before,
        history_after,
        visible,
        full,
        child_input,
    }
}

/// Issues the real `send-keys -R` request: `SendKeysExt` with
/// `reset_terminal`, no keys at all.
async fn ris_send_keys_reset(handler: &RequestHandler, target: &PaneTarget) {
    let response = handler
        .handle(Request::SendKeysExt(SendKeysExtRequest {
            target: Some(target.clone()),
            keys: vec![],
            expand_formats: false,
            hex: false,
            literal: false,
            dispatch_key_table: false,
            copy_mode_command: false,
            forward_mouse_event: false,
            reset_terminal: true,
            repeat_count: None,
        }))
        .await;
    assert_eq!(
        response,
        Response::SendKeys(SendKeysResponse { key_count: 0 })
    );
}

fn assert_ris_reset_sent_nothing_to_the_child(observation: &RisResetObservation, label: &str) {
    assert_eq!(
        observation.child_input,
        Some(Vec::new()),
        "{label}: send-keys -R must be injected into the transcript, not written to the child: \
         {observation:?}"
    );
}

#[tokio::test]
async fn send_keys_reset_on_principal_with_scroll_on_clear_on_keeps_the_used_rows() {
    let handler = RequestHandler::new();
    let target = ris_session(&handler, "ris-soc-on").await;
    // `on` is the constructed default, so setting it alone would pass even if
    // the option never reached the live pane. Drive the pane to `off` first so
    // the `on` below has to travel the server resolution path to take effect.
    ris_scroll_on_clear_set(&handler, "off").await;
    ris_scroll_on_clear_set(&handler, "on").await;

    let observation = ris_reset_and_observe(&handler, &target, "ZT").await;

    assert_eq!(
        observation.history_after - observation.history_before,
        RIS_VISIBLE_ROWS,
        "on: the used visible rows must reach scrollback: {observation:?}"
    );
    assert!(
        !observation.visible.contains("ZTVIS1") && !observation.visible.contains("ZTVIS2"),
        "on: the viewport must be blank after the reset: {observation:?}"
    );
    assert!(
        observation.full.contains("ZTVIS1") && observation.full.contains("ZTVIS2"),
        "on: a capture spanning history must still hold the reset rows: {observation:?}"
    );
    assert!(
        observation.full.contains("ZTOLD1") && observation.full.contains("ZTOLD2"),
        "on: the earlier history must survive the reset: {observation:?}"
    );
    assert_ris_reset_sent_nothing_to_the_child(&observation, "on");
}

#[tokio::test]
async fn send_keys_reset_on_principal_with_scroll_on_clear_off_destroys_the_used_rows() {
    let handler = RequestHandler::new();
    let target = ris_session(&handler, "ris-soc-off").await;
    ris_scroll_on_clear_set(&handler, "off").await;

    let observation = ris_reset_and_observe(&handler, &target, "ZU").await;

    assert_eq!(
        observation.history_after, observation.history_before,
        "off: history must not grow: {observation:?}"
    );
    assert!(
        !observation.visible.contains("ZUVIS1") && !observation.visible.contains("ZUVIS2"),
        "off: the viewport must be blank after the reset: {observation:?}"
    );
    assert!(
        !observation.full.contains("ZUVIS1") && !observation.full.contains("ZUVIS2"),
        "off: a capture spanning history must not hold the reset rows: {observation:?}"
    );
    assert!(
        observation.full.contains("ZUOLD1") && observation.full.contains("ZUOLD2"),
        "off: the earlier history must survive the reset untouched: {observation:?}"
    );
    assert_ris_reset_sent_nothing_to_the_child(&observation, "off");
}

#[tokio::test]
async fn send_keys_reset_in_the_alternate_screen_keeps_the_principal_screen() {
    // R3510 M5 through the command entry, measured on tmux 3.7b: `send-keys
    // -R` runs `input_reset` -> `screen_write_reset`, which stays on the
    // alternate screen, so `?1049l` later restores the principal rows.
    let handler = RequestHandler::new();
    let target = ris_session(&handler, "ris-alt").await;
    ris_append(&handler, &target, &ris_seed_bytes("ZA")).await;
    let history_before = ris_history_size(&handler, &target).await;
    assert_eq!(history_before, RIS_HISTORY_ROWS);
    ris_append(&handler, &target, b"\x1b[?1049hZAALT").await;

    ris_send_keys_reset(&handler, &target).await;

    let transcript = ris_transcript(&handler, &target).await;
    assert!(
        transcript.lock().is_alternate(),
        "the reset must stay on the alternate screen"
    );
    let visible = ris_capture(&handler, &target, None, None).await;
    assert!(
        !visible.contains("ZAALT"),
        "the alternate rows are blanked: {visible:?}"
    );

    ris_append(&handler, &target, b"\x1b[?1049l").await;

    assert!(!transcript.lock().is_alternate());
    assert_eq!(
        ris_history_size(&handler, &target).await,
        history_before,
        "the principal history neither grows nor shrinks"
    );
    let full = ris_capture(&handler, &target, Some(-(history_before as i64)), None).await;
    for row in ["ZAOLD1", "ZAOLD2", "ZAVIS1", "ZAVIS2"] {
        assert!(full.contains(row), "{row} must survive the reset: {full:?}");
    }
}
