//! D1 R6: input that waits behind a released binding keeps what made it distinct when it arrived -
//! an Escape the escape time already finalised, a bracketed paste cut across frames, the rest of a
//! frame after a surface closed or a binding was released, and decoded keyboard events.

use rmux_proto::SessionName;
use tokio::time::{sleep, Duration};

use super::completion_tests::{blocking_job, release_path};
use super::queued_destination_tests::{completes, open_through_cli, queue_escape, remove};
use super::queued_focus_tests::{focus_scene, type_live_retaining};
use super::queued_input_tests::{scene, Scene};

const FOCUS_OUT: &[u8] = b"\x1b[O";

/// A scene whose pane asks for focus reports and bracketed paste, focused out once for real, so a
/// focus-in report is a change it receives.
async fn paste_scene(name: &str, pid: u32) -> Scene {
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
    scene
}

/// Types the frames of one paste while a released binding runs, then checks that nothing but the
/// real focus reports reached the pane before the binding ends, and the whole paste after it.
async fn paste_behind_binding(
    scene: &Scene,
    label: &str,
    frames: &[&[u8]],
    before_end: &[u8],
    pasted: &[u8],
) {
    let (binding_release, binding_end) = scene.released_binding(label).await;
    scene.capture_pane().await;
    scene.type_live(FOCUS_OUT).await;
    let mut pending = Vec::new();
    for frame in frames {
        type_live_retaining(scene, &mut pending, frame).await;
    }
    assert!(pending.is_empty(), "nothing of the paste is left retained");
    sleep(Duration::from_millis(300)).await;
    assert_eq!(
        scene.pane_captured().await,
        before_end,
        "only the real focus reports act during the binding"
    );

    std::fs::write(&binding_release, b"go").expect("end the binding");
    scene.wait_for_pane(pasted).await;
    assert!(binding_end.exists());
    let captured = scene.pane_captured().await;
    assert_eq!(
        &captured[before_end.len()..],
        pasted,
        "the paste arrives whole, its text included"
    );
    remove(&[&binding_release, &binding_end]);
}

#[tokio::test]
async fn an_escape_the_escape_time_finalised_stays_apart_from_the_key_after_it() {
    let scene = scene("d1-r6-escape-finalised", 974_601).await;
    let meta_x = release_path("r6-escape-finalised-m-x");
    scene
        .bind("M-x", &format!("run-shell \"touch {}\"", meta_x.display()))
        .await;
    let (binding_release, binding_end) = scene.released_binding("r6-escape-finalised").await;
    scene.capture_pane().await;

    // The escape time finalises Escape through the real flush entry; x arrives after that, while
    // the binding still runs.
    queue_escape(&scene).await;
    scene.type_live(b"x").await;
    assert_eq!(scene.queued_frames().await, Some(2), "Escape and x wait");
    sleep(Duration::from_millis(200)).await;
    assert!(
        scene.pane_captured().await.is_empty(),
        "nothing reaches the pane before the binding ends"
    );

    std::fs::write(&binding_release, b"go").expect("end the binding");
    scene.wait_for_pane(b"x").await;
    assert!(binding_end.exists());
    let captured = scene.pane_captured().await;
    assert_eq!(captured, b"\x1bx", "Escape then x, as two keys");
    assert!(!meta_x.exists(), "Escape and x were not merged into M-x");
    assert_eq!(scene.queued_frames().await, None);
    remove(&[&binding_release, &binding_end, &meta_x]);
}

#[tokio::test]
async fn a_focus_report_in_a_later_frame_of_a_queued_paste_stays_pasted_text() {
    let scene = paste_scene("d1-r6-paste-text", 974_611).await;
    paste_behind_binding(
        &scene,
        "r6-paste-text",
        &[b"\x1b[200~ab", b"c\x1b[Id", b"e\x1b[201~"],
        FOCUS_OUT,
        b"\x1b[200~abc\x1b[Ide\x1b[201~",
    )
    .await;
}

#[tokio::test]
async fn paste_delimiters_and_text_cut_across_frames_keep_the_paste_and_the_focus_after_it() {
    let scene = paste_scene("d1-r6-paste-cut", 974_612).await;
    paste_behind_binding(
        &scene,
        "r6-paste-cut",
        &[b"\x1b[20", b"0~ab\x1b", b"[Icd\x1b[2", b"01~\x1b[I"],
        b"\x1b[O\x1b[I",
        b"\x1b[200~ab\x1b[Icd\x1b[201~",
    )
    .await;
}

#[tokio::test]
async fn decoded_keyboard_events_keep_their_identity_past_a_popup_opened_since() {
    let scene = scene("d1-r6-acquired-replay", 974_621).await;
    let presses = release_path("r6-acquired-replay-presses");
    for key in ["S-Enter", "C-Enter"] {
        scene
            .bind(
                key,
                &format!("run-shell \"echo {key} >> {}\"", presses.display()),
            )
            .await;
    }
    let (binding_release, binding_end) = scene.released_binding("r6-acquired-replay").await;
    scene.capture_pane().await;
    // S-Enter and C-Enter as CSI u: keys that have no legacy encoding of their own.
    scene.type_live(b"\x1b[13;2u\x1b[13;5u").await;
    assert_eq!(scene.queued_frames().await, Some(1), "the events wait");

    let job = release_path("r6-acquired-replay-job");
    let popup = open_through_cli(
        &scene,
        format!(
            "display-popup -c {} -w 20 -h 5 -E \"{}\"",
            scene.pid,
            blocking_job(&job, 0)
        ),
    );
    scene.wait_for_overlay(true).await;
    sleep(Duration::from_millis(200)).await;
    assert!(!presses.exists(), "no binding runs before the binding ends");

    std::fs::write(&binding_release, b"go").expect("end the binding");
    tokio::time::timeout(Duration::from_secs(10), async {
        while std::fs::read_to_string(&presses).map_or(0, |text| text.lines().count()) < 2 {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("both keys run their own bindings");
    assert_eq!(
        std::fs::read_to_string(&presses).expect("the presses"),
        "S-Enter\nC-Enter\n",
        "each event keeps its key and its order"
    );
    assert!(binding_end.exists());
    assert!(
        scene.overlay_active().await,
        "the popup opened since stays up"
    );
    let captured = scene.pane_captured().await;
    assert!(
        !captured.contains(&b'\r') && !captured.contains(&b'\n'),
        "no legacy Enter reaches the pane: {captured:?}"
    );

    std::fs::write(&job, b"go").expect("close the popup");
    completes(popup).await;
    remove(&[&binding_release, &binding_end, &presses, &job]);
}

/// Opens a CLI menu on the client while its released binding runs, then types `frame` into it.
async fn frame_into_menu(
    scene: &Scene,
    frame: &[u8],
) -> tokio::task::JoinHandle<rmux_proto::Response> {
    let menu = open_through_cli(
        scene,
        format!(
            "display-menu -c {} -x 0 -y 0 -T Menu Nothing n {{ display-message nothing }}",
            scene.pid
        ),
    );
    scene.wait_for_overlay(true).await;
    scene.type_live(frame).await;
    scene.wait_for_overlay(false).await;
    menu
}

#[tokio::test]
async fn the_rest_of_a_frame_after_a_menu_closes_waits_events_included() {
    let scene = scene("d1-r6-rest-events", 974_631).await;
    let (binding_release, binding_end) = scene.released_binding("r6-rest-events").await;
    scene.capture_pane().await;
    // q closes the menu; x and a decoded C-a follow in the same frame.
    let menu = frame_into_menu(&scene, b"qx\x1b[97;5u").await;
    assert_eq!(
        scene.queued_frames().await,
        Some(2),
        "x and the event wait as two units"
    );
    sleep(Duration::from_millis(300)).await;
    assert!(
        scene.pane_captured().await.is_empty(),
        "nothing of the frame reaches the pane while the binding runs"
    );

    std::fs::write(&binding_release, b"go").expect("end the binding");
    scene.wait_for_pane(b"x\x01").await;
    assert!(binding_end.exists());
    assert_eq!(scene.pane_captured().await, b"x\x01", "x, then C-a");
    completes(menu).await;
    remove(&[&binding_release, &binding_end]);
}

#[tokio::test]
async fn bytes_after_a_decoded_event_that_closes_a_menu_wait_behind_the_binding() {
    let scene = scene("d1-r6-event-closes", 974_632).await;
    let (binding_release, binding_end) = scene.released_binding("r6-event-closes").await;
    scene.capture_pane().await;
    // q arrives decoded (CSI u) and closes the menu; x follows as raw bytes.
    let menu = frame_into_menu(&scene, b"\x1b[113ux").await;
    assert_eq!(scene.queued_frames().await, Some(1), "x waits");
    sleep(Duration::from_millis(300)).await;
    assert!(
        scene.pane_captured().await.is_empty(),
        "x does not reach the pane while the binding runs"
    );

    std::fs::write(&binding_release, b"go").expect("end the binding");
    scene.wait_for_pane(b"x").await;
    assert!(binding_end.exists());
    assert_eq!(scene.pane_captured().await, b"x");
    completes(menu).await;
    remove(&[&binding_release, &binding_end]);
}

pub(super) const F5: &[u8] = b"\x1b[15~";
pub(super) const C_G: u8 = 0x07;

/// Binds `key` to a popup it waits for, then to a command that marks its end; returns the file
/// that closes the popup and the end marker.
pub(super) async fn bind_popup_then_end(
    scene: &Scene,
    key: &str,
    label: &str,
) -> (std::path::PathBuf, std::path::PathBuf) {
    let job = release_path(&format!("{label}-job"));
    let end = release_path(&format!("{label}-end"));
    scene
        .bind(
            key,
            &format!(
                "display-popup -w 20 -h 5 -E \"{}\" ; run-shell \"touch {}\"",
                blocking_job(&job, 0),
                end.display()
            ),
        )
        .await;
    (job, end)
}

/// Types `frame` - ordinary keys around `key` - checks that only `before`, the keys ahead of it,
/// reaches the pane while its popup is up, closes the popup and expects `after` once its binding
/// has ended.
async fn frame_behind_its_own_popup(
    scene: &Scene,
    key: &str,
    label: &str,
    frame: &[u8],
    before: &[u8],
    after: &[u8],
) {
    let (job, end) = bind_popup_then_end(scene, key, label).await;
    scene.capture_pane().await;
    scene.type_live(frame).await;
    scene.wait_for_overlay(true).await;
    assert_eq!(
        scene.released_work().await,
        1,
        "the binding runs as released work"
    );
    sleep(Duration::from_millis(300)).await;
    assert_eq!(
        scene.pane_captured().await,
        before,
        "the keys after it do not reach the pane while its binding runs"
    );
    assert!(
        scene.queued_frames().await.is_some(),
        "the keys after it wait behind its binding, not in its popup"
    );

    std::fs::write(&job, b"go").expect("close the popup");
    scene.wait_for_pane(after).await;
    assert!(end.exists(), "the binding ended before the keys after it");
    assert_eq!(scene.pane_captured().await, [before, after].concat());
    assert_eq!(scene.queued_frames().await, None);
    remove(&[&job, &end]);
}

#[tokio::test]
async fn a_key_after_a_decoded_key_whose_binding_opens_a_popup_waits_behind_that_binding() {
    let scene = scene("d1-r6-mid-frame-event", 974_641).await;
    let frame = [F5, b"x"].concat();
    frame_behind_its_own_popup(&scene, "F5", "r6-mid-frame-event", &frame, b"", b"x").await;
}

#[tokio::test]
async fn a_key_after_one_whose_binding_opens_a_popup_waits_behind_that_binding() {
    let scene = scene("d1-r6-mid-frame-popup", 974_644).await;
    frame_behind_its_own_popup(&scene, "C-g", "r6-mid-frame-popup", &[C_G, b'x'], b"", b"x").await;
}

#[tokio::test]
async fn every_piece_of_a_large_frame_after_a_popup_binding_waits_behind_it() {
    let scene = scene("d1-r6-mid-frame-large", 974_642).await;
    let keys: Vec<u8> = (0..1100).map(|index| b'a' + (index % 26) as u8).collect();
    let frame = [&[C_G][..], &keys].concat();
    frame_behind_its_own_popup(&scene, "C-g", "r6-mid-frame-large", &frame, b"", &keys).await;
}

#[tokio::test]
async fn the_piece_of_a_large_frame_after_a_popup_binding_ending_the_first_waits_behind_it() {
    let scene = scene("d1-r6-mid-frame-piece", 974_643).await;
    // C-g ends the first 1024-byte piece of the frame; the next piece follows it.
    let ahead = vec![b'a'; 1023];
    let behind = vec![b'b'; 100];
    let frame = [ahead.as_slice(), &[C_G], &behind].concat();
    frame_behind_its_own_popup(&scene, "C-g", "r6-mid-frame-piece", &frame, &ahead, &behind).await;
}
