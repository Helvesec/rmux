//! D1 R7: input that waits behind a released binding keeps its timing and its continuity across the
//! binding's end: a cut key sequence keeps its escape-time deadline, a bracketed paste stays open
//! until its own end, a replayed key that releases a new binding holds the rest of its frame, and
//! a surface a frame's own binding leaves up does not take the rest of that frame, nor does one
//! opened between two parts of a frame. A popup closing with its job closes when the job exits, so
//! the keys typed after that reach what is under it.

use rmux_proto::SessionName;
use tokio::time::{sleep, timeout, Duration};

use super::super::attach_support::test_barrier::{self, Point};
use super::completion_tests::{blocking_job, cli_command, exit_status, release_path};
use super::queued_continuity_tests::{bind_popup_then_end, C_G, F5};
use super::queued_destination_tests::{completes, open_through_cli, remove};
use super::queued_focus_tests::{focus_scene, type_live_retaining};
use super::queued_input_tests::{scene, Scene};

/// The escape time runs out on the input the client retains: the real escape-flush entry.
pub(super) async fn escape_time_flushes(scene: &Scene, pending: &mut Vec<u8>) {
    timeout(
        Duration::from_secs(5),
        scene
            .handler
            .flush_attached_pending_escape_input(scene.pid, pending),
    )
    .await
    .expect("the client's input path is not held")
    .expect("the escape time flushes the retained input");
}

/// Binds S-Enter to record its presses; returns the record.
async fn record_shift_enter(scene: &Scene, label: &str) -> std::path::PathBuf {
    let presses = release_path(&format!("{label}-s-enter"));
    scene
        .bind(
            "S-Enter",
            &format!("run-shell \"echo S-Enter >> {}\"", presses.display()),
        )
        .await;
    presses
}

const CUT_SHIFT_ENTER: &[u8] = b"\x1b[13;2";

#[tokio::test]
async fn a_cut_key_sequence_the_escape_time_finalises_does_not_join_the_key_after_it() {
    let scene = scene("d1-r7-cut-key-expired", 974_701).await;
    let presses = record_shift_enter(&scene, "r7-cut-key-expired").await;

    // Control: the same input while nothing waits.
    scene.capture_pane().await;
    let mut pending = Vec::new();
    type_live_retaining(&scene, &mut pending, CUT_SHIFT_ENTER).await;
    escape_time_flushes(&scene, &mut pending).await;
    type_live_retaining(&scene, &mut pending, b"u").await;
    scene.wait_for_pane(b"u").await;
    sleep(Duration::from_millis(100)).await;
    let live = scene.pane_captured().await;
    assert!(!presses.exists(), "no S-Enter across the escape time, live");

    let (binding_release, binding_end) = scene.released_binding("r7-cut-key-expired").await;
    scene.capture_pane().await;
    type_live_retaining(&scene, &mut pending, CUT_SHIFT_ENTER).await;
    assert_eq!(pending, CUT_SHIFT_ENTER, "the cut sequence stays pending");
    assert_eq!(scene.queued_frames().await, None, "nothing is queued yet");
    escape_time_flushes(&scene, &mut pending).await;
    assert!(pending.is_empty());
    type_live_retaining(&scene, &mut pending, b"u").await;
    assert_eq!(
        scene.queued_frames().await,
        Some(2),
        "the finalised sequence and u wait"
    );
    sleep(Duration::from_millis(200)).await;
    assert!(
        scene.pane_captured().await.is_empty(),
        "nothing before the binding ends"
    );

    std::fs::write(&binding_release, b"go").expect("end the binding");
    scene.wait_for_pane(b"u").await;
    sleep(Duration::from_millis(100)).await;
    assert!(binding_end.exists());
    assert_eq!(
        scene.pane_captured().await,
        live,
        "the same as without waiting"
    );
    assert!(!presses.exists(), "no S-Enter across the escape time");
    remove(&[&binding_release, &binding_end, &presses]);
}

#[tokio::test]
async fn a_cut_key_sequence_completed_before_the_escape_time_stays_one_key() {
    let scene = scene("d1-r7-cut-key-completed", 974_702).await;
    let presses = record_shift_enter(&scene, "r7-cut-key-completed").await;
    let (binding_release, binding_end) = scene.released_binding("r7-cut-key-completed").await;
    scene.capture_pane().await;
    let mut pending = Vec::new();
    type_live_retaining(&scene, &mut pending, CUT_SHIFT_ENTER).await;
    type_live_retaining(&scene, &mut pending, b"u").await;
    assert!(pending.is_empty());
    assert_eq!(scene.queued_frames().await, Some(1), "S-Enter waits");
    sleep(Duration::from_millis(200)).await;
    assert!(!presses.exists(), "S-Enter waits for the binding");

    std::fs::write(&binding_release, b"go").expect("end the binding");
    timeout(Duration::from_secs(10), async {
        while !presses.exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("S-Enter runs its binding after the binding ends");
    assert!(binding_end.exists());
    assert!(
        scene.pane_captured().await.is_empty(),
        "S-Enter is its binding's"
    );
    remove(&[&binding_release, &binding_end, &presses]);
}

const FOCUS_OUT: &[u8] = b"\x1b[O";

/// A scene whose pane asks for focus reports and bracketed paste, with M-y bound to record its
/// presses.
pub(super) async fn paste_scene(name: &str, pid: u32, label: &str) -> (Scene, std::path::PathBuf) {
    let scene = focus_scene(name, pid).await;
    scene
        .handler
        .state
        .lock()
        .await
        .append_bytes_to_pane_transcript_for_test(
            &SessionName::new(name).expect("valid session name"),
            0,
            0,
            b"\x1b[?2004h",
        )
        .expect("the pane asks for bracketed paste");
    let presses = release_path(&format!("{label}-m-y"));
    scene
        .bind(
            "M-y",
            &format!("run-shell \"echo M-y >> {}\"", presses.display()),
        )
        .await;
    (scene, presses)
}

/// Types `opening` - the start of a paste - while a released binding runs, ends the binding, then
/// types the rest of the paste as `later` frames: the pane gets the real focus-out, then the whole
/// paste, bound key and literal report included, and nothing else.
async fn paste_across_the_binding_end(
    name: &str,
    pid: u32,
    opening: &[u8],
    later: &[&[u8]],
    pasted: &[u8],
) {
    let (scene, presses) = paste_scene(name, pid, name).await;
    let (binding_release, binding_end) = scene.released_binding(name).await;
    scene.capture_pane().await;
    scene.type_live(FOCUS_OUT).await;
    scene.wait_for_pane(FOCUS_OUT).await;
    let mut pending = Vec::new();
    type_live_retaining(&scene, &mut pending, opening).await;
    assert_eq!(
        pending, opening,
        "the open paste stays pending with the client"
    );

    // The binding ends between the fragments, well within the escape time.
    std::fs::write(&binding_release, b"go").expect("end the binding");
    scene.wait_for_released_work(0).await;
    timeout(Duration::from_secs(10), async {
        while !binding_end.exists() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the binding ends");
    assert_eq!(scene.queued_frames().await, None);
    assert_eq!(
        scene.pane_captured().await,
        FOCUS_OUT,
        "the binding's end closes no paste"
    );

    for frame in later {
        type_live_retaining(&scene, &mut pending, frame).await;
    }
    assert!(pending.is_empty());
    scene.wait_for_pane(pasted).await;
    sleep(Duration::from_millis(100)).await;
    assert_eq!(scene.pane_captured().await, [FOCUS_OUT, pasted].concat());
    assert!(!presses.exists(), "a bound key inside the paste stays text");
    remove(&[&binding_release, &binding_end, &presses]);
}

#[tokio::test]
async fn a_paste_open_when_the_binding_ends_stays_open_for_its_next_fragments() {
    paste_across_the_binding_end(
        "d1-r7-paste-open-drain",
        974_711,
        b"\x1b[200~ab",
        &[b"c\x1b[Id\x1by\x1b[20", b"1~"],
        b"\x1b[200~abc\x1b[Id\x1by\x1b[201~",
    )
    .await;
}

#[tokio::test]
async fn a_lone_paste_start_when_the_binding_ends_stays_open() {
    paste_across_the_binding_end(
        "d1-r7-paste-marker-drain",
        974_712,
        b"\x1b[200~",
        &[b"x\x1by", b"z\x1b[201~"],
        b"\x1b[200~x\x1byz\x1b[201~",
    )
    .await;
}

#[tokio::test]
async fn a_paste_the_escape_time_finalises_behind_a_binding_ends_as_it_does_live() {
    let name = "d1-r7-paste-expired";
    let (scene, presses) = paste_scene(name, 974_713, name).await;
    // Control: the open paste the escape time finalises while nothing waits.
    scene.capture_pane().await;
    let mut pending = Vec::new();
    type_live_retaining(&scene, &mut pending, b"\x1b[200~ab").await;
    escape_time_flushes(&scene, &mut pending).await;
    scene.wait_for_pane(b"ab").await;
    sleep(Duration::from_millis(100)).await;
    let live = scene.pane_captured().await;

    let (binding_release, binding_end) = scene.released_binding(name).await;
    scene.capture_pane().await;
    type_live_retaining(&scene, &mut pending, b"\x1b[200~ab").await;
    assert_eq!(
        scene.queued_frames().await,
        None,
        "the open paste stays pending"
    );
    escape_time_flushes(&scene, &mut pending).await;
    assert_eq!(
        scene.queued_frames().await,
        Some(1),
        "the finalised paste waits"
    );
    // The finalised paste is over: a real report after it acts at once.
    type_live_retaining(&scene, &mut pending, FOCUS_OUT).await;
    scene.wait_for_pane(FOCUS_OUT).await;
    sleep(Duration::from_millis(200)).await;
    assert_eq!(scene.pane_captured().await, FOCUS_OUT);

    std::fs::write(&binding_release, b"go").expect("end the binding");
    scene.wait_for_pane(b"ab").await;
    sleep(Duration::from_millis(100)).await;
    assert!(binding_end.exists());
    assert_eq!(
        scene.pane_captured().await,
        [FOCUS_OUT, live.as_slice()].concat(),
        "finalised as it is live"
    );
    remove(&[&binding_release, &binding_end, &presses]);
}

/// Queues `frame` - a key whose binding shows a popup and waits for it, then ordinary keys - behind
/// a released binding B0; once B0 ends, the replayed key releases its own binding B1. Nothing of
/// the rest of the frame reaches the pane while B1's popup is up; all of it does, exactly, after B1.
async fn replayed_frame_behind_its_new_binding(
    name: &str,
    pid: u32,
    key: &str,
    frame: &[u8],
    after: &[u8],
) {
    let scene = scene(name, pid).await;
    let (job, end) = bind_popup_then_end(&scene, key, name).await;
    let (binding_release, binding_end) = scene.released_binding(name).await;
    scene.capture_pane().await;
    scene.type_live(frame).await;
    assert!(
        scene.queued_frames().await.is_some(),
        "the frame waits behind B0"
    );

    std::fs::write(&binding_release, b"go").expect("end B0");
    scene.wait_for_overlay(true).await;
    assert_eq!(
        scene.released_work().await,
        1,
        "the replayed key released B1"
    );
    sleep(Duration::from_millis(300)).await;
    assert!(
        scene.pane_captured().await.is_empty(),
        "the rest of the frame does not reach the pane while B1 runs"
    );
    assert!(
        scene.queued_frames().await.is_some(),
        "the rest of the frame waits in the queue, not in B1's popup"
    );

    std::fs::write(&job, b"go").expect("close B1's popup");
    scene.wait_for_pane(after).await;
    sleep(Duration::from_millis(100)).await;
    assert!(end.exists(), "B1 ended before the rest of its frame");
    assert!(binding_end.exists());
    assert_eq!(scene.pane_captured().await, after);
    assert_eq!(scene.queued_frames().await, None);
    remove(&[&binding_release, &binding_end, &job, &end]);
}

#[tokio::test]
async fn a_replayed_decoded_key_releasing_a_binding_holds_the_rest_of_its_frame() {
    let frame = [F5, b"x"].concat();
    replayed_frame_behind_its_new_binding("d1-r7-replay-event", 974_721, "F5", &frame, b"x").await;
}

#[tokio::test]
async fn a_replayed_key_releasing_a_binding_holds_the_rest_of_its_frame() {
    replayed_frame_behind_its_new_binding("d1-r7-replay-raw", 974_722, "C-g", &[C_G, b'x'], b"x")
        .await;
}

#[tokio::test]
async fn every_replayed_piece_after_a_key_releasing_a_binding_waits_in_order() {
    let keys: Vec<u8> = (0..1100).map(|index| b'a' + (index % 26) as u8).collect();
    let frame = [&[C_G][..], &keys].concat();
    replayed_frame_behind_its_new_binding("d1-r7-replay-large", 974_723, "C-g", &frame, &keys)
        .await;
}

/// A frame's part closed the surface that took the frame and went to wait behind the binding; a
/// popup opened before the next part is decided does not take that part, which waits after the
/// first, as tmux classifies every key of a read before anything else runs.
#[tokio::test]
async fn a_surface_opened_between_two_parts_of_a_frame_does_not_take_the_later_part() {
    let scene = scene("d1-r7-part-waits", 974_741).await;
    let (binding_release, binding_end) = scene.released_binding("r7-part-waits").await;
    scene.capture_pane().await;
    let menu = open_through_cli(
        &scene,
        format!(
            "display-menu -c {} -x 0 -y 0 -T Menu Nothing n {{ display-message nothing }}",
            scene.pid
        ),
    );
    scene.wait_for_overlay(true).await;

    // q closes the menu and x waits; the frame stops before its decoded C-a.
    let next_part = test_barrier::arm(Point::NextPart, scene.pid);
    let typing = tokio::spawn({
        let handler = scene.handler.clone();
        let pid = scene.pid;
        async move {
            handler
                .handle_attached_live_input_for_test(pid, b"qx\x1b[97;5u")
                .await
        }
    });
    timeout(Duration::from_secs(5), next_part.reached())
        .await
        .expect("the frame reaches its decoded C-a");
    assert!(!scene.overlay_active().await, "q closed the menu");
    assert_eq!(scene.queued_frames().await, Some(1), "x waits");
    let job = release_path("r7-part-waits-job");
    let popup = open_through_cli(
        &scene,
        format!(
            "display-popup -c {} -w 20 -h 5 -E \"{}\"",
            scene.pid,
            blocking_job(&job, 0)
        ),
    );
    scene.wait_for_overlay(true).await;
    next_part.resume();
    timeout(Duration::from_secs(5), typing)
        .await
        .expect("the client's input path is not held")
        .expect("the input task")
        .expect("the frame is handled");
    assert_eq!(
        scene.queued_frames().await,
        Some(2),
        "C-a waits after x instead of going to the popup"
    );

    std::fs::write(&binding_release, b"go").expect("end the binding");
    scene.wait_for_pane(b"x\x01").await;
    assert!(binding_end.exists());
    assert_eq!(scene.pane_captured().await, b"x\x01", "x, then C-a");
    assert!(scene.overlay_active().await, "they went past the popup");
    std::fs::write(&job, b"go").expect("close the popup");
    completes(popup).await;
    completes(menu).await;
    remove(&[&binding_release, &binding_end, &job]);
}

/// tmux 3.7b delivered the key typed 0-40 ms after a `-E` popup's job exited to the pane and the
/// command reported the job's status; RMUX's waiter looked for the exit every 50 ms, so until then
/// the key went to the dead job and was lost.
#[tokio::test]
async fn a_popup_closing_with_its_job_closes_when_the_job_exits() {
    // Only the end of the job's output can wake the exit waiter within the test's bound.
    super::popup_job::POPUP_EXIT_POLL_FOR_TEST.set(Some(Duration::from_secs(60)));
    let scene = scene("d1-r7-popup-close", 974_731).await;
    let release = release_path("r7-popup-close");
    let job = format!(
        "sh -c 'while [ ! -e {} ]; do sleep 0.02; done; exit 3'",
        release.display()
    );
    let command = format!("display-popup -c {} -w 20 -h 5 -E \"{job}\"", scene.pid);
    let waiting = tokio::spawn({
        let handler = scene.handler.clone();
        async move { handler.handle(cli_command(command)).await }
    });
    scene.wait_for_overlay(true).await;
    scene.capture_pane().await;

    std::fs::write(&release, b"go").expect("release the popup job");
    let response = timeout(Duration::from_secs(5), waiting)
        .await
        .expect("the popup closes when its job exits, not at the waiter's next look")
        .expect("the command task");
    let _ = std::fs::remove_file(&release);
    assert_eq!(exit_status(&response), Some(3), "the job's status");
    assert!(!scene.overlay_active().await, "-E closed the popup");
    scene.type_live(b"q").await;
    scene.wait_for_pane(b"q").await;
}
