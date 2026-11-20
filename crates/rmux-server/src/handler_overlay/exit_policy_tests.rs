//! C4: repeated `-E` keeps a `display-popup` popup open after a nonzero job exit.
//!
//! Measured on tmux 3.7b with a real attached client: tmux counts every `-E`, so `-EE`, `-E -E`,
//! `-EBE`, `-E -B -E` and `-EEE` keep the popup after the job exits with 3 while one `-E` closes
//! it. The kept popup swallows ordinary keys, C-c closes it, and only then does the waiting
//! command complete with the job's status.

use tokio::time::{sleep, timeout, Duration};

use super::super::RequestHandler;
use super::completion_tests::{
    blocking_job, cli_command, exit_status, release_path, scene, wait_for_popup, ATTACH_PID,
};
use super::state::ClientOverlayState;

/// `Some(true)` once the open popup's job has ended, `None` when no popup is open.
async fn popup_job_ended(handler: &RequestHandler) -> Option<bool> {
    let active_attach = handler.active_attach.lock().await;
    match active_attach.by_pid.get(&ATTACH_PID)?.overlay.as_ref() {
        Some(ClientOverlayState::Popup(popup)) => Some(popup.job.is_none()),
        _ => None,
    }
}

async fn popup_is_kept_after_a_nonzero_exit_until_ctrl_c(label: &str, flags: &str) {
    let (handler, _control_rx) = scene(label).await;
    let release = release_path(label);
    let command = format!(
        "display-popup -c {ATTACH_PID} -w 20 -h 5 {flags} \"{}\"",
        blocking_job(&release, 3)
    );
    let waiting = tokio::spawn({
        let handler = handler.clone();
        async move { handler.handle(cli_command(command)).await }
    });
    wait_for_popup(&handler).await;

    std::fs::write(&release, b"go").expect("release the popup job");
    let after_exit = timeout(Duration::from_secs(10), async {
        loop {
            match popup_job_ended(&handler).await {
                Some(false) => sleep(Duration::from_millis(10)).await,
                ended => return ended,
            }
        }
    })
    .await
    .expect("the popup job exits");
    let _ = std::fs::remove_file(&release);
    assert_eq!(
        after_exit,
        Some(true),
        "{flags}: the popup stays open after its job exits with 3"
    );
    sleep(Duration::from_millis(200)).await;
    assert!(
        !waiting.is_finished(),
        "{flags}: the command waits while the popup stays open"
    );

    handler
        .handle_attached_live_input_for_test(ATTACH_PID, b"p")
        .await
        .expect("the key is handled");
    assert_eq!(
        popup_job_ended(&handler).await,
        Some(true),
        "{flags}: an ordinary key does not close the popup"
    );

    handler
        .handle_attached_live_input_for_test(ATTACH_PID, b"\x03")
        .await
        .expect("C-c is handled");
    let response = timeout(Duration::from_secs(10), waiting)
        .await
        .expect("C-c closes the popup and completes the command")
        .expect("the command task");
    assert_eq!(
        exit_status(&response),
        Some(3),
        "{flags}: the job's status is the command's status"
    );
    assert!(!handler.overlay_active(ATTACH_PID).await);
}

#[tokio::test]
async fn repeated_exit_flags_keep_the_popup_after_a_nonzero_exit_until_ctrl_c() {
    for (label, flags) in [
        ("c4-grouped", "-EE"),
        ("c4-separated", "-E -E"),
        ("c4-grouped-apart", "-EBE"),
        ("c4-separated-apart", "-E -B -E"),
        ("c4-grouped-triple", "-EEE"),
    ] {
        popup_is_kept_after_a_nonzero_exit_until_ctrl_c(label, flags).await;
    }
}

#[tokio::test]
async fn repeated_exit_flags_still_close_the_popup_on_a_zero_exit() {
    let (handler, _control_rx) = scene("c4-grouped-zero").await;
    let release = release_path("c4-grouped-zero");
    let command = format!(
        "display-popup -c {ATTACH_PID} -w 20 -h 5 -EE \"{}\"",
        blocking_job(&release, 0)
    );
    let waiting = tokio::spawn({
        let handler = handler.clone();
        async move { handler.handle(cli_command(command)).await }
    });
    wait_for_popup(&handler).await;

    std::fs::write(&release, b"go").expect("release the popup job");
    let response = timeout(Duration::from_secs(10), waiting)
        .await
        .expect("the command completes once the job exits with 0")
        .expect("the command task");
    let _ = std::fs::remove_file(&release);
    assert_eq!(
        exit_status(&response),
        None,
        "the zero exit closes the popup and the command succeeds"
    );
    assert!(!handler.overlay_active(ATTACH_PID).await);
}
