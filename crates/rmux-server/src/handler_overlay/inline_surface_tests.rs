//! D1 R8: a display-panes that a key of a live frame shows does not take the rest of that frame.
//!
//! Measured on tmux 3.7b with a real client (F3 bound to `display-panes -b`, F3 a b in one write):
//! a and b reach the pane at once while display-panes stays up, and a key typed later closes it.
//! tmux classifies every key of a read before the commands they run; a display-panes up when a read
//! arrives takes its first key.

use tokio::time::{sleep, timeout, Duration};

use super::queued_input_tests::{scene, Scene};

const F3: &[u8] = b"\x1bOR";

async fn display_panes_scene(name: &str, pid: u32) -> Scene {
    let scene = scene(name, pid).await;
    scene.bind("F3", "display-panes -b -d 10000").await;
    scene.capture_pane().await;
    scene
}

async fn display_panes_up(scene: &Scene) -> bool {
    scene
        .handler
        .active_attach
        .lock()
        .await
        .by_pid
        .get(&scene.pid)
        .is_some_and(|active| active.display_panes.is_some())
}

async fn wait_for_display_panes(scene: &Scene, up: bool) {
    timeout(Duration::from_secs(5), async {
        while display_panes_up(scene).await != up {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("display-panes reaches the expected state");
}

/// The rest of `frame` after F3 reaches the pane while display-panes stays up; a key that arrives
/// afterwards closes it and does not reach the pane.
async fn rest_of_frame_passes_display_panes(name: &str, pid: u32, frame: &[u8], rest: &[u8]) {
    let scene = display_panes_scene(name, pid).await;
    scene.type_live(frame).await;
    scene.wait_for_pane(rest).await;
    sleep(Duration::from_millis(100)).await;
    assert_eq!(
        scene.pane_captured().await,
        rest,
        "the rest of the frame, exactly"
    );
    assert!(
        display_panes_up(&scene).await,
        "display-panes stays up: nothing of its own frame took it"
    );

    scene.type_live(b"z").await;
    wait_for_display_panes(&scene, false).await;
    sleep(Duration::from_millis(100)).await;
    assert_eq!(
        scene.pane_captured().await,
        rest,
        "a key that arrives while display-panes is up closes it"
    );
}

#[tokio::test]
async fn the_rest_of_a_frame_whose_key_shows_display_panes_reaches_the_pane() {
    rest_of_frame_passes_display_panes("d1-r8-dp-rest", 974_811, &[F3, b"ab"].concat(), b"ab")
        .await;
}

#[tokio::test]
async fn a_decoded_key_after_one_that_shows_display_panes_reaches_the_pane() {
    rest_of_frame_passes_display_panes(
        "d1-r8-dp-event",
        974_812,
        &[F3, b"\x1b[97u"].concat(),
        b"a",
    )
    .await;
}

#[tokio::test]
async fn display_panes_up_when_a_frame_arrives_takes_its_first_key() {
    let scene = display_panes_scene("d1-r8-dp-present", 974_813).await;
    scene.type_live(F3).await;
    wait_for_display_panes(&scene, true).await;
    scene.type_live(b"ab").await;
    scene.wait_for_pane(b"b").await;
    sleep(Duration::from_millis(100)).await;
    assert_eq!(scene.pane_captured().await, b"b", "a closed display-panes");
    assert!(!display_panes_up(&scene).await);
}
