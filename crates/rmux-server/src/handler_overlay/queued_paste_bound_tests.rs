//! D1 R8: a bracketed paste longer than the input a client retains, cut across frames that are each
//! admissible, behind a released binding that ends and drains before the rest of the paste arrives.
//! The live path forwards such a paste in bounded pieces and keeps only its opener, so the rest stays
//! pasted text until its end delimiter or the escape time; the queued paste goes on the same way.

use rmux_proto::DEFAULT_MAX_FRAME_LENGTH;
use tokio::time::{sleep, timeout, Duration};

use super::queued_destination_tests::remove;
use super::queued_drain_tests::{escape_time_flushes, paste_scene};
use super::queued_focus_tests::type_live_retaining;
use super::queued_input_tests::Scene;

const START: &[u8] = b"\x1b[200~";
const END: &[u8] = b"\x1b[201~";
const FOCUS_IN: &[u8] = b"\x1b[I";
const FRAME: usize = 256 * 1024;
/// What follows the long part of the paste: text, a literal focus-in and M-y, and the end delimiter
/// cut across two frames.
const LATER: [&[u8]; 2] = [b"TAIL\x1b[I\x1bymore\x1b[20", b"1~"];
const LATER_TEXT: &[u8] = b"TAIL\x1b[I\x1bymore";

/// Numbered records, so that a lost or repeated byte changes the text, with a literal M-y and
/// focus-in inside and an e-acute cut across the second and third frames.
fn long_text() -> Vec<u8> {
    let mut text: Vec<u8> = (0..5 * FRAME / 8)
        .flat_map(|index| format!("{index:07}\n").into_bytes())
        .collect();
    text[40..42].copy_from_slice(b"\x1by");
    text[300..303].copy_from_slice(FOCUS_IN);
    text[700_000..700_003].copy_from_slice(FOCUS_IN);
    text[900_000..900_002].copy_from_slice(b"\x1by");
    text[2 * FRAME - 1..2 * FRAME + 1].copy_from_slice("é".as_bytes());
    text
}

/// The client's frames for the long part: the opener with the first piece, then piece by piece.
fn long_frames(text: &[u8]) -> Vec<Vec<u8>> {
    assert!(
        START.len() + FRAME <= DEFAULT_MAX_FRAME_LENGTH,
        "each frame is admissible"
    );
    assert!(
        START.len() + text.len() > DEFAULT_MAX_FRAME_LENGTH,
        "the paste goes past the bound"
    );
    text.chunks(FRAME)
        .enumerate()
        .map(|(index, piece)| {
            if index == 0 {
                [START, piece].concat()
            } else {
                piece.to_vec()
            }
        })
        .collect()
}

/// What the pane received, as the texts of its bracketed pastes and the bytes outside them.
fn pastes(received: &[u8]) -> (Vec<Vec<u8>>, Vec<u8>) {
    let mut texts = Vec::new();
    let mut outside = Vec::new();
    let mut rest = received;
    while !rest.is_empty() {
        let Some(start) = rest.windows(START.len()).position(|window| window == START) else {
            outside.extend_from_slice(rest);
            break;
        };
        outside.extend_from_slice(&rest[..start]);
        let body = &rest[start + START.len()..];
        let Some(end) = body.windows(END.len()).position(|window| window == END) else {
            outside.extend_from_slice(&rest[start..]);
            break;
        };
        texts.push(body[..end].to_vec());
        rest = &body[end + END.len()..];
    }
    (texts, outside)
}

async fn wait_for_pane_len(scene: &Scene, len: usize) {
    timeout(Duration::from_secs(20), async {
        while scene.pane_captured().await.len() < len {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the pane receives the paste");
    sleep(Duration::from_millis(200)).await;
}

/// Ends the released binding and waits until everything it held is replayed.
async fn end_and_drain(
    scene: &Scene,
    binding_release: &std::path::Path,
    binding_end: &std::path::Path,
) {
    std::fs::write(binding_release, b"go").expect("end the binding");
    scene.wait_for_released_work(0).await;
    timeout(Duration::from_secs(20), async {
        while !binding_end.exists() || scene.queued_frames().await.is_some() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the binding ends and its queue drains");
}

/// Asserts that `received` holds exactly the paste `text` as bracketed pastes, nothing outside them
/// and no empty paste; returns how many pastes carried it.
fn assert_one_paste_context(received: &[u8], text: &[u8], what: &str) -> usize {
    let (texts, outside) = pastes(received);
    assert!(
        outside.is_empty(),
        "{what}: nothing outside the paste, got {:?}",
        String::from_utf8_lossy(&outside[..outside.len().min(80)])
    );
    assert!(
        texts.iter().all(|piece| !piece.is_empty()),
        "{what}: no empty paste"
    );
    assert!(
        texts.concat() == text,
        "{what}: the pasted text arrives whole, once, in order"
    );
    texts.len()
}

#[tokio::test]
async fn a_paste_beyond_the_retained_bound_stays_pasted_text_after_its_binding_ends_and_drains() {
    let name = "d1-r8-paste-bound";
    let (scene, presses) = paste_scene(name, 974_801, name).await;
    let text = long_text();
    let whole = [text.as_slice(), LATER_TEXT].concat();

    // Control: the same frames while nothing waits.
    scene.capture_pane().await;
    let mut pending = Vec::new();
    for frame in long_frames(&text) {
        type_live_retaining(&scene, &mut pending, &frame).await;
    }
    for frame in LATER {
        type_live_retaining(&scene, &mut pending, frame).await;
    }
    assert!(pending.is_empty());
    wait_for_pane_len(&scene, whole.len() + START.len() + END.len()).await;
    let live = scene.pane_captured().await;
    assert_one_paste_context(&live, &whole, "live");
    assert!(!presses.exists(), "M-y inside the paste stays text, live");

    let (binding_release, binding_end) = scene.released_binding(name).await;
    scene.capture_pane().await;
    for frame in long_frames(&text) {
        type_live_retaining(&scene, &mut pending, &frame).await;
    }
    assert!(
        pending.starts_with(START) && pending.len() <= DEFAULT_MAX_FRAME_LENGTH,
        "the open paste stays with the client from its opener, within the live bound: {} bytes",
        pending.len()
    );
    sleep(Duration::from_millis(100)).await;
    assert!(
        scene.pane_captured().await.is_empty(),
        "nothing of the paste reaches the pane while the binding runs"
    );

    // The binding ends and its queue drains before the rest of the paste arrives.
    end_and_drain(&scene, &binding_release, &binding_end).await;
    for frame in LATER {
        type_live_retaining(&scene, &mut pending, frame).await;
    }
    assert!(pending.is_empty(), "the end delimiter closes the paste");
    wait_for_pane_len(&scene, whole.len() + START.len() + END.len()).await;
    let queued = scene.pane_captured().await;
    assert_one_paste_context(&queued, &whole, "behind the binding");
    assert!(
        !presses.exists(),
        "M-y inside the paste stays text behind the binding"
    );
    remove(&[&binding_release, &binding_end, &presses]);
}

#[tokio::test]
async fn a_paste_beyond_the_bound_completed_while_its_binding_runs_arrives_whole() {
    let name = "d1-r8-paste-bound-closed";
    let (scene, presses) = paste_scene(name, 974_802, name).await;
    let text = long_text();
    let whole = [text.as_slice(), LATER_TEXT].concat();
    let (binding_release, binding_end) = scene.released_binding(name).await;
    scene.capture_pane().await;
    let mut pending = Vec::new();
    for frame in long_frames(&text) {
        type_live_retaining(&scene, &mut pending, &frame).await;
    }
    for frame in LATER {
        type_live_retaining(&scene, &mut pending, frame).await;
    }
    assert!(pending.is_empty(), "the end delimiter closes the paste");
    sleep(Duration::from_millis(100)).await;
    assert!(scene.pane_captured().await.is_empty());

    end_and_drain(&scene, &binding_release, &binding_end).await;
    wait_for_pane_len(&scene, whole.len() + START.len() + END.len()).await;
    assert_one_paste_context(
        &scene.pane_captured().await,
        &whole,
        "completed behind the binding",
    );
    assert!(!presses.exists(), "M-y inside the paste stays text");
    remove(&[&binding_release, &binding_end, &presses]);
}

#[tokio::test]
async fn a_paste_beyond_the_bound_the_escape_time_finalises_behind_a_binding_ends_as_it_does_live()
{
    let name = "d1-r8-paste-bound-expired";
    let (scene, presses) = paste_scene(name, 974_803, name).await;
    let text = long_text();

    // Control: the escape time finalises the paste still open after the long part.
    scene.capture_pane().await;
    let mut pending = Vec::new();
    for frame in long_frames(&text) {
        type_live_retaining(&scene, &mut pending, &frame).await;
    }
    escape_time_flushes(&scene, &mut pending).await;
    wait_for_pane_len(&scene, text.len()).await;
    let live = scene.pane_captured().await;
    assert!(
        pastes(&live).0.concat() == text,
        "live: the pasted text arrives whole"
    );

    let (binding_release, binding_end) = scene.released_binding(name).await;
    scene.capture_pane().await;
    for frame in long_frames(&text) {
        type_live_retaining(&scene, &mut pending, &frame).await;
    }
    escape_time_flushes(&scene, &mut pending).await;
    sleep(Duration::from_millis(100)).await;
    assert!(scene.pane_captured().await.is_empty());

    end_and_drain(&scene, &binding_release, &binding_end).await;
    wait_for_pane_len(&scene, text.len()).await;
    let queued = scene.pane_captured().await;
    let (live_texts, live_outside) = pastes(&live);
    let (queued_texts, queued_outside) = pastes(&queued);
    assert!(
        queued_texts.concat() == live_texts.concat(),
        "the finalised paste's text, as live"
    );
    assert_eq!(
        queued_outside, live_outside,
        "what the expiry leaves outside a paste, as live"
    );
    assert!(!presses.exists(), "M-y inside the paste stays text");
    remove(&[&binding_release, &binding_end, &presses]);
}
