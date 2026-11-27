//! D1 R10: a bracketed paste in the rest of a frame whose own key released a binding.
//!
//! The rest of a live frame is split into parts only at the frame's decoded keyboard events, and
//! the decoder leaves paste bodies opaque: a paste the rest opens and leaves open is always its last
//! part, so it stays with the client from its opener and goes on as pasted text after the binding
//! ends, exactly as live. A paste closed before an event is a part of its own, in order.

use rmux_proto::{SessionName, DEFAULT_MAX_FRAME_LENGTH};
use tokio::time::{sleep, timeout, Duration};

use super::super::pane_support::take_delimiter_search_bytes_for_test;
use super::completion_tests::release_path;
use super::queued_continuity_tests::{bind_popup_then_end, C_G, F5};
use super::queued_destination_tests::remove;
use super::queued_input_tests::{scene, Scene};

const START: &[u8] = b"\x1b[200~";
const END: &[u8] = b"\x1b[201~";
const REST: &[u8] = b"ef\x1b[201~";
/// The live input path hands a large frame to its steps one reroute chunk (1 KiB) at a time.
const LIVE_INPUT_REROUTE_CHUNK_BYTES: usize = 1024;
/// The most keyboard events a frame typed here holds outside a paste (the binding's key, a CSI u):
/// each may take a live input step of its own.
const EVENTS_OUTSIDE_A_PASTE: usize = 2;
/// A held input path never returns. This only turns that hang into a failure: the work a typing
/// may cost is bounded by counting it, never by how long a loaded host takes to do it.
const HELD_INPUT_PATH_GUARD: Duration = Duration::from_secs(120);

/// Types `bytes` live, as the client's input path does, and bounds the work this causes: at most a
/// live input step per reroute chunk of `bytes` and per event outside a paste, and a search for
/// the paste's end that visits each typed byte once (plus a delimiter's length per step, for a
/// delimiter split across two steps), never again the open paste the client retains.
async fn type_live_retaining(scene: &Scene, pending: &mut Vec<u8>, bytes: &[u8]) {
    scene.handler.live_input_work.take();
    take_delimiter_search_bytes_for_test();
    timeout(
        HELD_INPUT_PATH_GUARD,
        scene
            .handler
            .handle_attached_live_input(scene.pid, pending, bytes),
    )
    .await
    .expect("the client's input path is not held")
    .expect("the input is handled");
    let steps = scene.handler.live_input_work.take().steps;
    let searched = take_delimiter_search_bytes_for_test();
    assert!(
        steps <= bytes.len().div_ceil(LIVE_INPUT_REROUTE_CHUNK_BYTES) + EVENTS_OUTSIDE_A_PASTE,
        "typing {} bytes must take at most a live input step per reroute chunk and event: \
         {steps} steps",
        bytes.len()
    );
    assert!(
        searched <= bytes.len() + steps * END.len(),
        "typing {} bytes must search each typed byte for the paste's end once: {searched} bytes \
         searched in {steps} steps, {} retained",
        bytes.len(),
        pending.len()
    );
}

/// A scene whose pane asks for bracketed paste (no focus reports, so that the binding's popup adds
/// nothing to what the pane receives), with M-y bound to record its presses.
async fn paste_only_scene(name: &str, pid: u32) -> (Scene, std::path::PathBuf) {
    let scene = scene(name, pid).await;
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
    let presses = release_path(&format!("{name}-m-y"));
    scene
        .bind(
            "M-y",
            &format!("run-shell \"echo M-y >> {}\"", presses.display()),
        )
        .await;
    (scene, presses)
}

/// What the pane received once nothing more arrives for 150 ms.
async fn settled_pane(scene: &Scene) -> Vec<u8> {
    let mut seen = Vec::new();
    timeout(Duration::from_secs(20), async {
        loop {
            sleep(Duration::from_millis(150)).await;
            let now = scene.pane_captured().await;
            if !now.is_empty() && now == seen {
                break;
            }
            seen = now;
        }
    })
    .await
    .expect("the pane settles");
    seen
}

/// Ends the binding the frame's key released (its popup's job exits) and waits for its queue.
async fn end_binding(scene: &Scene, job: &std::path::Path, end: &std::path::Path) {
    std::fs::write(job, b"go").expect("close the binding's popup");
    timeout(Duration::from_secs(20), async {
        while !end.exists()
            || scene.released_work().await != 0
            || scene.queued_frames().await.is_some()
            || scene.overlay_active().await
        {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the binding ends and nothing it held remains queued");
}

/// Types `paste` (then REST if the paste stays open) live as a control, which must give the pane
/// `expected`; then `key` + `paste` as one frame whose key's binding shows a popup and waits, and the
/// rest after that binding ended. `retained` is what must stay with the client after the key's
/// frame, `queued` the units queued behind the binding; the pane must get `expected` again.
async fn rest_of_frame_behind_its_key(
    name: &str,
    pid: u32,
    key: (&str, &[u8]),
    paste: &[u8],
    retained: Option<&[u8]>,
    queued: Option<usize>,
    expected: &[u8],
) {
    let (scene, presses) = paste_only_scene(name, pid).await;
    let (job, end) = bind_popup_then_end(&scene, key.0, name).await;

    scene.capture_pane().await;
    let mut pending = Vec::new();
    type_live_retaining(&scene, &mut pending, paste).await;
    if retained.is_some() {
        type_live_retaining(&scene, &mut pending, REST).await;
    }
    assert!(pending.is_empty());
    assert!(settled_pane(&scene).await == expected, "live control");

    scene.capture_pane().await;
    type_live_retaining(&scene, &mut pending, &[key.1, paste].concat()).await;
    scene.wait_for_overlay(true).await;
    match retained {
        Some(bytes) => assert!(
            pending == bytes,
            "the open paste stays with the client from its opener: {} bytes retained",
            pending.len()
        ),
        None => assert!(pending.is_empty()),
    }
    assert_eq!(
        scene.queued_frames().await,
        queued,
        "units queued behind the binding"
    );
    sleep(Duration::from_millis(100)).await;
    assert!(
        scene.pane_captured().await.is_empty(),
        "nothing of the rest reaches the pane while the binding runs"
    );

    end_binding(&scene, &job, &end).await;
    if retained.is_some() {
        type_live_retaining(&scene, &mut pending, REST).await;
    }
    assert!(pending.is_empty(), "the end delimiter closes the paste");
    assert!(
        settled_pane(&scene).await == expected,
        "the pane receives exactly the live bytes, once, in order"
    );
    assert!(!presses.exists(), "M-y inside the paste stays text");
    remove(&[&job, &end, &presses]);
}

#[tokio::test]
async fn an_open_paste_after_a_decoded_key_that_releases_a_binding_stays_with_the_client() {
    // The CSI u and M-y inside the paste stay pasted text: the rest is one part.
    let paste = b"\x1b[200~ab\x1b[97u\x1bycd";
    let expected = [paste.as_slice(), REST].concat();
    rest_of_frame_behind_its_key(
        "d1-r10-rest-open",
        974_901,
        ("F5", F5),
        paste,
        Some(paste),
        None,
        &expected,
    )
    .await;
}

#[tokio::test]
async fn a_long_open_paste_after_a_raw_key_that_releases_a_binding_stays_with_the_client() {
    let mut paste = START.to_vec();
    paste.extend((0..375).flat_map(|index| format!("{index:07}\n").into_bytes()));
    paste[100..103].copy_from_slice(b"\x1b[I");
    paste[2000..2002].copy_from_slice(b"\x1by");
    let expected = [paste.as_slice(), REST].concat();
    rest_of_frame_behind_its_key(
        "d1-r10-rest-open-long",
        974_902,
        ("C-g", &[C_G]),
        &paste,
        Some(&paste),
        None,
        &expected,
    )
    .await;
}

#[tokio::test]
async fn an_open_paste_filling_the_largest_frame_after_its_key_stays_with_the_client() {
    // The whole frame, key included, is as long as a frame may be.
    let mut paste = START.to_vec();
    let text_len = DEFAULT_MAX_FRAME_LENGTH - F5.len() - START.len();
    paste.extend((0..text_len).map(|index| b'a' + (index % 26) as u8));
    assert_eq!(F5.len() + paste.len(), DEFAULT_MAX_FRAME_LENGTH);
    let expected = [paste.as_slice(), REST].concat();
    rest_of_frame_behind_its_key(
        "d1-r10-rest-open-limit",
        974_903,
        ("F5", F5),
        &paste,
        Some(&paste),
        None,
        &expected,
    )
    .await;
}

#[tokio::test]
async fn a_closed_paste_an_event_and_bytes_after_a_key_that_releases_a_binding_keep_their_order() {
    // The rest is three parts: the closed paste, the decoded a, then cd.
    let frame = [START, b"ab", END, b"\x1b[97u", b"cd"].concat();
    let expected = [START, b"ab", END, b"a", b"cd"].concat();
    rest_of_frame_behind_its_key(
        "d1-r10-rest-parts",
        974_904,
        ("F5", F5),
        &frame,
        None,
        Some(3),
        &expected,
    )
    .await;
}

#[tokio::test]
async fn meta_o_then_a_key_after_a_key_that_releases_a_binding_keep_their_order() {
    // The rest is M-O, which the replay retains for escape-time, then the decoded Delete; measured
    // against tmux 3.7b, M-O reaches the pane first, as live. Delete is encoded the same whatever
    // cursor key mode the pane's shell has set, so the bytes do not depend on the host shell.
    let frame = b"\x1bO\x1b[3~";
    rest_of_frame_behind_its_key(
        "d1-r10-rest-meta-o",
        974_905,
        ("F5", F5),
        frame,
        None,
        Some(2),
        frame,
    )
    .await;
}
