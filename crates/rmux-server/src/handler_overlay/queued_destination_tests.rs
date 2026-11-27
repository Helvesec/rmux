//! D1 R5: input that waits behind a released binding keeps the destination it had when it
//! arrived. A popup, menu, prompt or display-panes opened since does not take it, and what a
//! closing surface hands back of a frame waits behind the binding, as in tmux.

use rmux_proto::Response;
use tokio::task::JoinHandle;
use tokio::time::{sleep, timeout, Duration};

use super::completion_tests::{blocking_job, cli_command, release_path};
use super::queued_input_tests::{scene, Scene};

/// Runs a command that opens a surface for the client through the CLI, in the background.
pub(super) fn open_through_cli(scene: &Scene, command: String) -> JoinHandle<Response> {
    let handler = scene.handler.clone();
    tokio::spawn(async move { handler.handle(cli_command(command)).await })
}

/// Whether the client shows a prompt, and whether it shows display-panes.
async fn prompt_and_display_panes(scene: &Scene) -> (bool, bool) {
    let active_attach = scene.handler.active_attach.lock().await;
    let active = active_attach.by_pid.get(&scene.pid);
    (
        active.is_some_and(|active| active.prompt.is_some()),
        active.is_some_and(|active| active.display_panes.is_some()),
    )
}

async fn wait_for_surfaces(scene: &Scene, expected: (bool, bool)) {
    timeout(Duration::from_secs(5), async {
        while prompt_and_display_panes(scene).await != expected {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the prompt and display-panes reach the expected state");
}

/// A key typed while a surface is up is that surface's: it does not reach the pane.
async fn taken_by_the_surface(scene: &Scene, key: &[u8]) {
    scene.type_live(key).await;
    sleep(Duration::from_millis(200)).await;
    assert!(
        !scene.pane_received(key).await,
        "the surface up when the key arrived takes it"
    );
}

/// Queues a lone Escape: retained by the live entry, then flushed by the escape time.
pub(super) async fn queue_escape(scene: &Scene) {
    let mut pending = Vec::new();
    timeout(
        Duration::from_secs(5),
        scene
            .handler
            .handle_attached_live_input(scene.pid, &mut pending, b"\x1b"),
    )
    .await
    .expect("the client's input path is not held")
    .expect("the input is handled");
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
}

pub(super) async fn completes(task: JoinHandle<Response>) {
    timeout(Duration::from_secs(10), task)
        .await
        .expect("the surface's CLI completes")
        .expect("the CLI task");
}

pub(super) fn remove(paths: &[&std::path::PathBuf]) {
    for path in paths {
        let _ = std::fs::remove_file(path);
    }
}

#[tokio::test]
async fn keys_typed_before_an_independent_popup_reach_the_pane_when_the_binding_ends() {
    let scene = scene("d1-r5-dest-popup", 974_421).await;
    let (binding_release, binding_end) = scene.released_binding("r5-dest-popup").await;
    scene.capture_pane().await;
    scene.type_live(b"q").await;
    queue_escape(&scene).await;
    assert_eq!(scene.queued_frames().await, Some(2), "q and Escape wait");

    let job = release_path("r5-dest-popup-job");
    let popup = open_through_cli(
        &scene,
        format!(
            "display-popup -c {} -w 20 -h 5 -E \"{}\"",
            scene.pid,
            blocking_job(&job, 0)
        ),
    );
    scene.wait_for_overlay(true).await;
    std::fs::write(&binding_release, b"go").expect("end the binding");
    scene.wait_for_pane(b"q\x1b").await;
    assert!(binding_end.exists());
    assert!(
        scene.overlay_active().await,
        "q and Escape went past the popup, which stays up"
    );
    taken_by_the_surface(&scene, b"o").await;

    std::fs::write(&job, b"go").expect("close the popup");
    completes(popup).await;
    remove(&[&binding_release, &binding_end, &job]);
}

#[tokio::test]
async fn a_key_after_one_that_closes_a_menu_waits_behind_the_binding() {
    let scene = scene("d1-r5-dest-menu", 974_422).await;
    let (binding_release, binding_end) = scene.released_binding("r5-dest-menu").await;
    scene.capture_pane().await;
    let menu = open_through_cli(
        &scene,
        format!(
            "display-menu -c {} -x 0 -y 0 -T Menu Nothing n {{ display-message nothing }}",
            scene.pid
        ),
    );
    scene.wait_for_overlay(true).await;

    // q closes the menu it arrived in; x, in the same frame, meets no surface any more.
    scene.type_live(b"qx").await;
    scene.wait_for_overlay(false).await;
    assert_eq!(
        scene.queued_frames().await,
        Some(1),
        "x waits behind the binding"
    );
    sleep(Duration::from_millis(200)).await;
    assert!(!scene.pane_received(b"x").await);
    assert!(!scene.pane_received(b"q").await, "q was the menu's");

    std::fs::write(&binding_release, b"go").expect("end the binding");
    scene.wait_for_pane(b"x").await;
    assert!(binding_end.exists());
    assert_eq!(scene.queued_frames().await, None);
    completes(menu).await;
    remove(&[&binding_release, &binding_end]);
}

#[tokio::test]
async fn a_frame_whose_keyboard_events_follow_a_closing_menu_keeps_its_order() {
    let scene = scene("d1-r5-dest-menu-events", 974_425).await;
    let (binding_release, binding_end) = scene.released_binding("r5-dest-menu-events").await;
    scene.capture_pane().await;
    let menu = open_through_cli(
        &scene,
        format!(
            "display-menu -c {} -x 0 -y 0 -T Menu Nothing n {{ display-message nothing }}",
            scene.pid
        ),
    );
    scene.wait_for_overlay(true).await;

    // q closes the menu; x and the decoded C-a after it go on in the order they came.
    scene.type_live(b"qx\x1b[97;5u").await;
    scene.wait_for_overlay(false).await;
    std::fs::write(&binding_release, b"go").expect("end the binding");
    scene.wait_for_pane(b"x").await;
    scene.wait_for_pane(b"\x01").await;
    let captured = scene.pane_captured().await;
    let at = |byte: u8| captured.iter().position(|seen| *seen == byte);
    assert!(
        at(b'x') < at(0x01),
        "the frame keeps its order: {captured:?}"
    );
    completes(menu).await;
    remove(&[&binding_release, &binding_end]);
}

#[tokio::test]
async fn keys_typed_before_an_independent_prompt_reach_the_pane_when_the_binding_ends() {
    let scene = scene("d1-r5-dest-prompt", 974_423).await;
    let (binding_release, binding_end) = scene.released_binding("r5-dest-prompt").await;
    scene.capture_pane().await;
    scene.type_live(b"q").await;
    queue_escape(&scene).await;
    assert_eq!(scene.queued_frames().await, Some(2), "q and Escape wait");

    let prompt = open_through_cli(
        &scene,
        format!(
            "command-prompt -t {} -p R5 {{ display-message done }}",
            scene.pid
        ),
    );
    wait_for_surfaces(&scene, (true, false)).await;
    std::fs::write(&binding_release, b"go").expect("end the binding");
    scene.wait_for_pane(b"q\x1b").await;
    assert!(binding_end.exists());
    assert_eq!(
        prompt_and_display_panes(&scene).await,
        (true, false),
        "q and Escape went past the prompt, which stays up"
    );
    taken_by_the_surface(&scene, b"z").await;

    queue_escape(&scene).await;
    wait_for_surfaces(&scene, (false, false)).await;
    completes(prompt).await;
    remove(&[&binding_release, &binding_end]);
}

#[tokio::test]
async fn keys_waiting_behind_the_binding_keep_their_bindings_while_a_prompt_is_up() {
    let scene = scene("d1-r5-dest-prompt-keys", 974_426).await;
    let runs = release_path("r5-dest-prompt-keys-m-y");
    scene
        .bind(
            "M-y",
            &format!("run-shell \"echo m-y >> {}\"", runs.display()),
        )
        .await;
    let (binding_release, binding_end) = scene.released_binding("r5-dest-prompt-keys").await;
    scene.capture_pane().await;
    // An unbound key goes on to the pane; the bound M-y after it in the frame runs its binding.
    scene.type_live(b"\x1bx\x1by").await;
    scene.type_live(b"\x1b[27;5;97~\x1by").await;
    assert_eq!(scene.queued_frames().await, Some(2), "both frames wait");

    let prompt = open_through_cli(
        &scene,
        format!(
            "command-prompt -t {} -p R5 {{ display-message done }}",
            scene.pid
        ),
    );
    wait_for_surfaces(&scene, (true, false)).await;
    std::fs::write(&binding_release, b"go").expect("end the binding");
    timeout(Duration::from_secs(10), async {
        while std::fs::read_to_string(&runs).map_or(0, |text| text.lines().count()) < 2 {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("M-y runs its binding from both frames");
    scene.wait_for_pane(b"\x1bx").await;
    assert!(
        !scene.pane_received(b"\x1by").await,
        "M-y is not written to the pane raw"
    );
    assert_eq!(prompt_and_display_panes(&scene).await, (true, false));

    queue_escape(&scene).await;
    wait_for_surfaces(&scene, (false, false)).await;
    completes(prompt).await;
    remove(&[&binding_release, &binding_end, &runs]);
}

#[test]
fn a_key_typed_before_display_panes_reaches_the_pane_when_the_binding_ends() {
    // display-panes run through the CLI needs more stack than a test thread has in a debug build.
    std::thread::Builder::new()
        .stack_size(16 * 1024 * 1024)
        .spawn(|| {
            tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .expect("a test runtime")
                .block_on(a_key_typed_before_display_panes())
        })
        .expect("a test thread")
        .join()
        .expect("the test passes");
}

async fn a_key_typed_before_display_panes() {
    let scene = scene("d1-r5-dest-display-panes", 974_424).await;
    let (binding_release, binding_end) = scene.released_binding("r5-dest-display-panes").await;
    scene.capture_pane().await;
    scene.type_live(b"q").await;
    assert_eq!(scene.queued_frames().await, Some(1), "q waits");

    let shown = open_through_cli(
        &scene,
        format!("display-panes -b -t {} -d 10000", scene.pid),
    );
    wait_for_surfaces(&scene, (false, true)).await;
    std::fs::write(&binding_release, b"go").expect("end the binding");
    scene.wait_for_pane(b"q").await;
    assert!(binding_end.exists());
    assert_eq!(
        prompt_and_display_panes(&scene).await,
        (false, true),
        "q went past display-panes, which stays up"
    );
    taken_by_the_surface(&scene, b"z").await;
    wait_for_surfaces(&scene, (false, false)).await;

    completes(shown).await;
    remove(&[&binding_release, &binding_end]);
}

mod boundary_identity;
