//! D1 R5: input queued behind a released binding, through the client's real input entries.
//!
//! A released binding is a binding whose client input path a popup has released: here F10 runs
//! a blocking run-shell and an independent CLI popup, opened and closed during it, releases it.
//! Every test uses its own client pid, so a barrier armed by one test cannot stop another.

use std::path::PathBuf;

use rmux_proto::{NewSessionRequest, PaneTarget, Request, SessionName, TerminalSize};
use tokio::sync::mpsc;
use tokio::time::{sleep, timeout, Duration};

use super::super::attach_support::test_barrier::{self, Point};
use super::super::RequestHandler;
use super::completion_tests::{
    blocking_job, cli_command, exit_status, release_path, wait_for_file,
};

const F9: &[u8] = b"\x1b[20~";
/// 'é' cut after its first byte.
const PARTIAL_CHAR: &[u8] = b"\xc3";
const LAST_BYTE: &[u8] = b"\xa9";
const WHOLE_CHAR: &[u8] = "é".as_bytes();
const F10: &[u8] = b"\x1b[21~";

pub(super) struct Scene {
    pub(super) handler: RequestHandler,
    pub(super) pid: u32,
    pub(super) pane: PaneTarget,
    _control_rx: mpsc::UnboundedReceiver<crate::pane_io::AttachControl>,
}

pub(super) async fn scene(name: &str, pid: u32) -> Scene {
    let handler = RequestHandler::new();
    let session = SessionName::new(name).expect("valid session name");
    let created = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&created);
    let (control_tx, control_rx) = mpsc::unbounded_channel();
    handler
        .register_attach(pid, session.clone(), control_tx)
        .await;
    handler
        .handle_attached_resize(pid, TerminalSize { cols: 80, rows: 24 })
        .await
        .expect("the client takes a real size");
    Scene {
        handler,
        pid,
        pane: PaneTarget::new(session, 0),
        _control_rx: control_rx,
    }
}

impl Scene {
    pub(super) async fn bind(&self, key: &str, commands: &str) {
        let bound = self
            .handler
            .handle(cli_command(format!("bind-key -n {key} {{ {commands} }}")))
            .await;
        assert_eq!(exit_status(&bound), None, "{bound:?}");
    }

    pub(super) async fn overlay_active(&self) -> bool {
        self.handler.overlay_active(self.pid).await
    }

    pub(super) async fn wait_for_overlay(&self, active: bool) {
        timeout(Duration::from_secs(5), async {
            while self.overlay_active().await != active {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the overlay reaches the expected state");
    }

    pub(super) async fn released_work(&self) -> usize {
        self.handler
            .active_attach
            .lock()
            .await
            .attached_input_waits
            .released_work_count(self.pid)
    }

    pub(super) async fn queued_frames(&self) -> Option<usize> {
        self.handler
            .active_attach
            .lock()
            .await
            .attached_input_waits
            .queued_frame_count(self.pid)
    }

    pub(super) async fn wait_for_released_work(&self, count: usize) {
        timeout(Duration::from_secs(10), async {
            while self.released_work().await != count {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the released work reaches the expected count");
    }

    pub(super) async fn capture_pane(&self) {
        self.handler
            .state
            .lock()
            .await
            .start_pane_input_capture_for_test(&self.pane);
    }

    pub(super) async fn pane_received(&self, bytes: &[u8]) -> bool {
        self.handler
            .state
            .lock()
            .await
            .pane_input_capture_for_test(&self.pane)
            .is_some_and(|captured| captured.windows(bytes.len()).any(|window| window == bytes))
    }

    pub(super) async fn pane_captured(&self) -> Vec<u8> {
        self.handler
            .state
            .lock()
            .await
            .pane_input_capture_for_test(&self.pane)
            .unwrap_or_default()
    }

    pub(super) async fn wait_for_pane(&self, bytes: &[u8]) {
        timeout(Duration::from_secs(10), async {
            while !self.pane_received(bytes).await {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the pane receives the input");
    }

    pub(super) async fn type_live(&self, bytes: &[u8]) {
        timeout(
            Duration::from_secs(5),
            self.handler
                .handle_attached_live_input_for_test(self.pid, bytes),
        )
        .await
        .expect("the client's input path is not held")
        .expect("the input is handled");
    }

    /// F10 runs a blocking run-shell; an independent CLI popup opened and closed during it releases
    /// the client's input path, so the binding goes on as released work. Returns the file that
    /// ends it and the file it touches when it ends.
    pub(super) async fn released_binding(&self, label: &str) -> (PathBuf, PathBuf) {
        let release = release_path(&format!("{label}-binding"));
        let end = release_path(&format!("{label}-binding-end"));
        self.bind(
            "F10",
            &format!(
                "run-shell \"while [ ! -e {} ]; do sleep 0.02; done; touch {}\"",
                release.display(),
                end.display()
            ),
        )
        .await;
        let input = tokio::spawn({
            let handler = self.handler.clone();
            let pid = self.pid;
            async move { handler.handle_attached_live_input_for_test(pid, F10).await }
        });
        sleep(Duration::from_millis(200)).await;
        let job_release = release_path(&format!("{label}-releasing-popup"));
        let popup = tokio::spawn({
            let handler = self.handler.clone();
            let command = format!(
                "display-popup -c {} -w 20 -h 5 -E \"{}\"",
                self.pid,
                blocking_job(&job_release, 0)
            );
            async move { handler.handle(cli_command(command)).await }
        });
        self.wait_for_overlay(true).await;
        timeout(Duration::from_secs(5), input)
            .await
            .expect("the popup releases the input path")
            .expect("the input task")
            .expect("the key is handled");
        std::fs::write(&job_release, b"go").expect("close the releasing popup");
        timeout(Duration::from_secs(10), popup)
            .await
            .expect("the releasing popup's CLI completes")
            .expect("the CLI task");
        let _ = std::fs::remove_file(&job_release);
        assert_eq!(
            self.released_work().await,
            1,
            "the binding runs as released work"
        );
        (release, end)
    }
}

#[tokio::test]
async fn the_last_binding_end_between_the_queue_decision_and_the_queue_strands_nothing() {
    let scene = scene("d1-r5-end-before-queue", 974_401).await;
    let (binding_release, binding_end) = scene.released_binding("r5-end-before-queue").await;
    scene.capture_pane().await;

    // The frame is found ordinary while the binding still runs ...
    let barrier = test_barrier::arm(Point::QueueDecision, scene.pid);
    let input = tokio::spawn({
        let handler = scene.handler.clone();
        let pid = scene.pid;
        async move { handler.handle_attached_live_input_for_test(pid, b"q").await }
    });
    timeout(Duration::from_secs(5), barrier.reached())
        .await
        .expect("the frame reaches the queue decision");
    // ... and the last released binding ends before the frame is queued.
    std::fs::write(&binding_release, b"go").expect("end the binding");
    wait_for_file(&binding_end).await;
    scene.wait_for_released_work(0).await;
    barrier.resume();
    timeout(Duration::from_secs(5), input)
        .await
        .expect("the input call returns")
        .expect("the input task")
        .expect("the key is handled");

    scene.wait_for_pane(b"q").await;
    assert_eq!(
        scene.queued_frames().await,
        None,
        "no frame is left queued without a binding to replay it"
    );
    for path in [&binding_release, &binding_end] {
        let _ = std::fs::remove_file(path);
    }
}

#[tokio::test]
async fn a_frame_queued_before_the_last_binding_end_is_replayed_by_that_end() {
    let scene = scene("d1-r5-queue-before-end", 974_402).await;
    let (binding_release, binding_end) = scene.released_binding("r5-queue-before-end").await;
    scene.capture_pane().await;

    let barrier = test_barrier::arm(Point::QueueDecision, scene.pid);
    let input = tokio::spawn({
        let handler = scene.handler.clone();
        let pid = scene.pid;
        async move { handler.handle_attached_live_input_for_test(pid, b"q").await }
    });
    timeout(Duration::from_secs(5), barrier.reached())
        .await
        .expect("the frame reaches the queue decision");
    barrier.resume();
    timeout(Duration::from_secs(5), input)
        .await
        .expect("the input call returns")
        .expect("the input task")
        .expect("the key is handled");
    assert_eq!(scene.queued_frames().await, Some(1), "the frame is queued");
    assert!(!scene.pane_received(b"q").await);

    std::fs::write(&binding_release, b"go").expect("end the binding");
    scene.wait_for_pane(b"q").await;
    assert!(binding_end.exists(), "q reached the pane after the binding");
    assert_eq!(scene.queued_frames().await, None);
    for path in [&binding_release, &binding_end] {
        let _ = std::fs::remove_file(path);
    }
}

/// Queues the first byte of 'é' and F9 (a binding that shows a popup) in one frame, then the
/// character's last byte; the replay of F9 releases a new binding with the partial character
/// still retained. (An ESC-based partial sequence no longer serves: admission keeps it pending with
/// the client, R7.)
async fn queue_new_binding_then_partial_key(scene: &Scene, label: &str) -> (PathBuf, PathBuf) {
    let second_job = release_path(&format!("{label}-second-job"));
    let second_end = release_path(&format!("{label}-second-end"));
    scene
        .bind(
            "F9",
            &format!(
                "display-popup -w 20 -h 5 -E \"{}\" ; run-shell \"touch {}\"",
                blocking_job(&second_job, 0),
                second_end.display()
            ),
        )
        .await;
    let mut frame = PARTIAL_CHAR.to_vec();
    frame.extend_from_slice(F9);
    scene.type_live(&frame).await;
    scene.type_live(LAST_BYTE).await;
    assert_eq!(
        scene.queued_frames().await,
        Some(2),
        "both frames are queued"
    );
    (second_job, second_end)
}

#[tokio::test]
async fn a_replay_paused_by_a_new_released_binding_resumes_with_its_retained_input() {
    let scene = scene("d1-r5-pause", 974_403).await;
    let (binding_release, binding_end) = scene.released_binding("r5-pause").await;
    let (second_job, second_end) = queue_new_binding_then_partial_key(&scene, "r5-pause").await;
    scene.capture_pane().await;

    std::fs::write(&binding_release, b"go").expect("end the first binding");
    // The replay runs F9, whose popup releases the replay in turn: it pauses with the partial
    // sequence put back in front of the queue.
    scene.wait_for_overlay(true).await;
    scene.wait_for_released_work(1).await;
    assert_eq!(scene.queued_frames().await, Some(2));
    assert!(!scene.pane_received(LAST_BYTE).await);

    std::fs::write(&second_job, b"go").expect("close the new binding's popup");
    scene.wait_for_pane(WHOLE_CHAR).await;
    assert!(
        second_end.exists(),
        "the new binding ended before its queue was replayed"
    );
    assert_eq!(scene.queued_frames().await, None);
    for path in [&binding_release, &binding_end, &second_job, &second_end] {
        let _ = std::fs::remove_file(path);
    }
}

#[tokio::test]
async fn a_new_released_binding_ending_at_the_replay_pause_point_strands_nothing() {
    let scene = scene("d1-r5-pause-race", 974_404).await;
    let (binding_release, binding_end) = scene.released_binding("r5-pause-race").await;
    let (second_job, second_end) =
        queue_new_binding_then_partial_key(&scene, "r5-pause-race").await;
    scene.capture_pane().await;

    let barrier = test_barrier::arm(Point::ReplayStep, scene.pid);
    std::fs::write(&binding_release, b"go").expect("end the first binding");
    timeout(Duration::from_secs(10), barrier.reached())
        .await
        .expect("the replay reaches its step after F9");
    // The new binding's popup is up; it ends right where the replay decides whether to pause.
    scene.wait_for_overlay(true).await;
    std::fs::write(&second_job, b"go").expect("close the new binding's popup");
    wait_for_file(&second_end).await;
    scene.wait_for_released_work(0).await;
    barrier.resume();

    scene.wait_for_pane(WHOLE_CHAR).await;
    let captured = scene.pane_captured().await;
    assert_eq!(
        captured
            .iter()
            .filter(|byte| **byte == LAST_BYTE[0])
            .count(),
        1,
        "the retained byte and the last one arrive together, as one 'é': {captured:?}"
    );
    assert_eq!(scene.queued_frames().await, None);
    for path in [&binding_release, &binding_end, &second_job, &second_end] {
        let _ = std::fs::remove_file(path);
    }
}
