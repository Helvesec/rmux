//! D1 R5: focus reports act as they arrive while the ordinary input around them waits behind a
//! released binding, however the transport cuts them, through the client's real live and
//! escape-flush entries.

use rmux_proto::SessionName;
use tokio::time::{sleep, timeout, Duration};

use super::completion_tests::{cli_command, exit_status};
use super::queued_input_tests::{scene, Scene};

const FOCUS_IN: &[u8] = b"\x1b[I";
const FOCUS_OUT: &[u8] = b"\x1b[O";

/// A scene whose pane asks for focus reports, with focus-events on.
pub(super) async fn focus_scene(name: &str, pid: u32) -> Scene {
    let scene = scene(name, pid).await;
    let focus = scene
        .handler
        .handle(cli_command("set-option -g focus-events on".to_owned()))
        .await;
    assert_eq!(exit_status(&focus), None, "{focus:?}");
    scene
        .handler
        .state
        .lock()
        .await
        .append_bytes_to_pane_transcript_for_test(
            &SessionName::new(name).expect("valid session name"),
            0,
            0,
            b"\x1b[?1004h",
        )
        .expect("the pane asks for focus reports");
    scene
}

/// Types `bytes` through the live entry, with the input the client retains across its frames.
pub(super) async fn type_live_retaining(scene: &Scene, pending: &mut Vec<u8>, bytes: &[u8]) {
    timeout(
        Duration::from_secs(5),
        scene
            .handler
            .handle_attached_live_input(scene.pid, pending, bytes),
    )
    .await
    .expect("the client's input path is not held")
    .expect("the input is handled");
}

/// The key has not reached the pane a while after it was typed.
async fn still_waits(scene: &Scene, key: &[u8]) {
    sleep(Duration::from_millis(200)).await;
    assert!(
        !scene.pane_received(key).await,
        "the ordinary key still waits behind the binding"
    );
}

/// The position of `bytes` in what the pane received.
async fn received_at(scene: &Scene, bytes: &[u8]) -> usize {
    scene
        .pane_captured()
        .await
        .windows(bytes.len())
        .position(|window| window == bytes)
        .expect("the pane received the bytes")
}

fn remove(paths: &[&std::path::PathBuf]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

#[tokio::test]
async fn focus_reports_mixed_with_keys_act_during_the_binding_and_the_keys_after_it() {
    let scene = focus_scene("d1-r5-focus-mixed", 974_411).await;
    let (binding_release, binding_end) = scene.released_binding("r5-focus-mixed").await;
    scene.capture_pane().await;

    scene.type_live(b"\x1b[Oq").await;
    scene.wait_for_pane(FOCUS_OUT).await;
    scene.type_live(b"x\x1b[I").await;
    scene.wait_for_pane(FOCUS_IN).await;
    assert!(
        !binding_end.exists(),
        "both reports acted during the binding"
    );
    assert_eq!(scene.queued_frames().await, Some(2), "q and x are queued");
    still_waits(&scene, b"q").await;
    still_waits(&scene, b"x").await;

    std::fs::write(&binding_release, b"go").expect("end the binding");
    scene.wait_for_pane(b"qx").await;
    assert!(binding_end.exists());
    assert!(received_at(&scene, FOCUS_IN).await < received_at(&scene, b"qx").await);
    assert_eq!(scene.queued_frames().await, None);
    remove(&[&binding_release, &binding_end]);
}

#[tokio::test]
async fn a_focus_report_cut_by_the_transport_acts_during_the_binding() {
    let scene = focus_scene("d1-r5-focus-cut", 974_412).await;
    let (binding_release, binding_end) = scene.released_binding("r5-focus-cut").await;
    scene.capture_pane().await;
    let mut pending = Vec::new();

    type_live_retaining(&scene, &mut pending, b"\x1b").await;
    type_live_retaining(&scene, &mut pending, b"[").await;
    assert_eq!(pending, b"\x1b[", "the start of a report is retained");
    assert_eq!(scene.queued_frames().await, None, "nothing waits yet");
    type_live_retaining(&scene, &mut pending, b"O").await;
    scene.wait_for_pane(FOCUS_OUT).await;
    assert!(pending.is_empty());

    type_live_retaining(&scene, &mut pending, b"\x1b[").await;
    type_live_retaining(&scene, &mut pending, b"Iq").await;
    scene.wait_for_pane(FOCUS_IN).await;
    assert!(pending.is_empty());
    assert!(
        !binding_end.exists(),
        "both reports acted during the binding"
    );
    assert_eq!(scene.queued_frames().await, Some(1), "q is queued");
    still_waits(&scene, b"q").await;

    std::fs::write(&binding_release, b"go").expect("end the binding");
    scene.wait_for_pane(b"q").await;
    assert!(binding_end.exists());
    assert!(received_at(&scene, FOCUS_IN).await < received_at(&scene, b"q").await);
    remove(&[&binding_release, &binding_end]);
}

#[tokio::test]
async fn a_focus_report_completing_a_prefix_retained_before_the_binding_acts_during_it() {
    let scene = focus_scene("d1-r5-focus-prefix", 974_413).await;
    let mut pending = Vec::new();
    type_live_retaining(&scene, &mut pending, b"\x1b[").await;
    assert_eq!(
        pending, b"\x1b[",
        "the client retains the start of a sequence"
    );
    let (binding_release, binding_end) = scene.released_binding("r5-focus-prefix").await;
    scene.capture_pane().await;

    type_live_retaining(&scene, &mut pending, b"Oq").await;
    scene.wait_for_pane(FOCUS_OUT).await;
    assert!(pending.is_empty());
    assert!(!binding_end.exists(), "the report acted during the binding");
    assert_eq!(scene.queued_frames().await, Some(1), "q is queued");
    still_waits(&scene, b"q").await;

    std::fs::write(&binding_release, b"go").expect("end the binding");
    scene.wait_for_pane(b"q").await;
    assert!(binding_end.exists());
    remove(&[&binding_release, &binding_end]);
}

#[tokio::test]
async fn the_start_of_a_report_flushed_by_the_escape_time_waits_behind_the_binding() {
    let scene = focus_scene("d1-r5-focus-flush", 974_414).await;
    let (binding_release, binding_end) = scene.released_binding("r5-focus-flush").await;
    scene.capture_pane().await;
    let mut pending = Vec::new();

    type_live_retaining(&scene, &mut pending, b"\x1b[").await;
    assert_eq!(scene.queued_frames().await, None, "nothing waits yet");
    timeout(
        Duration::from_secs(5),
        scene
            .handler
            .flush_attached_pending_escape_input(scene.pid, &mut pending),
    )
    .await
    .expect("the client's input path is not held")
    .expect("the escape time flushes the retained input");
    assert!(pending.is_empty());
    assert_eq!(
        scene.queued_frames().await,
        Some(1),
        "no report came: the flushed M-[ is ordinary input and waits"
    );
    still_waits(&scene, b"\x1b[").await;

    std::fs::write(&binding_release, b"go").expect("end the binding");
    scene.wait_for_pane(b"\x1b[").await;
    assert!(binding_end.exists());
    assert_eq!(scene.queued_frames().await, None);
    remove(&[&binding_release, &binding_end]);
}
