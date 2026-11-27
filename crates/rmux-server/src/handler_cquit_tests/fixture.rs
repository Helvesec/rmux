//! The panes these guards measure on, made quiet before anything is injected.
//!
//! A pane opened with the default interactive shell is not settled when its
//! startup hook ends: on Windows the shell writes its own prompt, and a prompt
//! seventy-three columns wide wraps the first injected line - `CQUIT-O` then
//! `UTSIDE` - so a capture that looks for that word does not find it. The guards
//! were reading a shell, not their own producer.
//!
//! This is the acquired CF1/MF1 recipe, taken from
//! `handler_display_message_tests/terminal_mode_flags.rs`: a producer that
//! announces itself once with a short marker and then reads its input to nothing
//! for good, pinned as `default-command` BEFORE each creation, and waited for
//! twice - the pane's own startup, then the marker really consumed into its
//! transcript. Both waits are bounded polls of observed state; neither sleeps for
//! a fixed time and neither looks at anything a guard measures.

use super::{capture, new_session, split, RequestHandler};
use rmux_proto::{
    OptionName, PaneTarget, Request, Response, ScopeSelector, SessionName, SetOptionMode,
    SetOptionRequest,
};
use tokio::time::{sleep, Duration, Instant};

/// Announces itself once, then goes quiet for good.
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

/// Waits until the marker has really been consumed into that pane's transcript.
///
/// The startup hook ends when a starting pane's pending input is drained and
/// says nothing about its stdout having been read, so what answers here is the
/// pane's own capture.
async fn wait_for_producer_marker(handler: &RequestHandler, name: &str, pane: u32, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(15);
    loop {
        if capture(handler, &format!("{name}:0.{pane}"))
            .await
            .contains(marker)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for pane {name}:0.{pane} to consume its producer marker {marker}"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

/// A session whose first pane holds this fixture's own producer, settled.
pub(super) async fn new_quiet_session(handler: &RequestHandler, name: &str) -> SessionName {
    let marker = format!("CQ5RDY-{name}-0");
    pin_quiet_producer(handler, &marker).await;
    let session = new_session(handler, name).await;
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(session.clone(), 0, 0))
        .await;
    wait_for_producer_marker(handler, name, 0, &marker).await;
    session
}

/// The same, for the pane a split opens beside `target`.
pub(super) async fn split_quiet(handler: &RequestHandler, target: &PaneTarget, pane: u32) {
    let name = target.session_name().as_str().to_owned();
    let marker = format!("CQ5RDY-{name}-{pane}");
    pin_quiet_producer(handler, &marker).await;
    split(handler, target).await;
    handler
        .wait_for_pane_startup_to_finish_for_test(&PaneTarget::with_window(
            target.session_name().clone(),
            target.window_index(),
            pane,
        ))
        .await;
    wait_for_producer_marker(handler, &name, pane, &marker).await;
}
