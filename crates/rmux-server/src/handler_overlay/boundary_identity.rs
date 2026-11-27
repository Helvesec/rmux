//! tmux 3.7b forwards retained bytes to a replacement pane in this scenario.
//! L05 deliberately rejects that stale origin while retaining legitimate selection changes.
use super::*;
use rmux_proto::{KillPaneRequest, PaneTarget, Request, SessionName};

#[tokio::test]
async fn l05_1_binding_prompt_tail_identity_and_multiplicity() {
    for (case, surface, mutation) in [
        (0, "popup", "keep"),
        (1, "popup", "remove"),
        (2, "prompt", "keep"),
        (3, "prompt", "remove"),
        (4, "popup", "select"),
        (5, "prompt", "select"),
    ] {
        let name = format!("l05-boundary-{case}");
        let scene = scene(&name, 978_500 + case).await;
        let response = scene
            .handler
            .handle(cli_command(format!("split-window -d -t {name}")))
            .await;
        assert!(matches!(response, Response::SourceFile(_)), "{response:?}");
        scene.handler.wait_for_initial_panes_for_test().await;
        let survivor = PaneTarget::with_window(SessionName::new(&name).unwrap(), 0, 1);
        for pane in [&scene.pane, &survivor] {
            scene
                .handler
                .disable_pane_focus_reports_for_test(pane)
                .await;
        }
        let old_id = scene
            .handler
            .state
            .lock()
            .await
            .sessions
            .session(scene.pane.session_name())
            .unwrap()
            .window()
            .pane(0)
            .unwrap()
            .id();
        let replacement_id = scene
            .handler
            .state
            .lock()
            .await
            .sessions
            .session(scene.pane.session_name())
            .unwrap()
            .window()
            .pane(1)
            .unwrap()
            .id();
        let (release, end) = scene.released_binding(&name).await;
        scene.capture_pane().await;
        scene
            .handler
            .state
            .lock()
            .await
            .start_pane_input_capture_for_test(&survivor);
        scene.type_live(b"q").await;
        scene.type_live(b"\x01").await;
        assert_eq!(
            scene.queued_frames().await,
            Some(2),
            "two distinct admitted frames"
        );
        let job = release_path(&format!("{name}-surface"));
        let command = if surface == "popup" {
            format!(
                "display-popup -c {} -w 20 -h 5 -E \"{}\"",
                scene.pid,
                blocking_job(&job, 0)
            )
        } else {
            format!(
                "command-prompt -t {} -p L05 {{ display-message done }}",
                scene.pid
            )
        };
        let opening = open_through_cli(&scene, command);
        if surface == "popup" {
            scene.wait_for_overlay(true).await;
        } else {
            wait_for_surfaces(&scene, (true, false)).await;
        }
        if mutation == "remove" {
            let killed = scene
                .handler
                .handle(Request::KillPane(KillPaneRequest {
                    target: scene.pane.clone(),
                    kill_all_except: false,
                }))
                .await;
            assert!(matches!(killed, Response::KillPane(_)), "{killed:?}");
            let state = scene.handler.state.lock().await;
            let now = state
                .sessions
                .session(scene.pane.session_name())
                .unwrap()
                .window()
                .pane(0)
                .unwrap()
                .id();
            assert_eq!(now, replacement_id);
            assert_ne!(now, old_id);
        }
        if mutation == "select" {
            let response = scene
                .handler
                .handle(cli_command(format!("select-pane -t {name}:0.1")))
                .await;
            assert!(matches!(response, Response::SourceFile(_)), "{response:?}");
        }
        std::fs::write(&release, b"go").unwrap();
        scene.wait_for_released_work(0).await;
        timeout(Duration::from_secs(5), async {
            while scene.queued_frames().await.is_some() {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("both accepted frames finish or are discarded");
        assert!(end.exists());
        let state = scene.handler.state.lock().await;
        if mutation == "remove" {
            assert_eq!(
                state.pane_input_capture_for_test(&scene.pane),
                Some(Vec::new()),
                "old input must not hit the reindexed replacement at the {surface} boundary"
            );
        } else if mutation == "select" {
            assert_eq!(
                state.pane_input_capture_for_test(&scene.pane),
                Some(Vec::new())
            );
            assert_eq!(
                state.pane_input_capture_for_test(&survivor),
                Some(b"q\x01".to_vec()),
                "a live binding's selection still owns replay"
            );
        } else {
            assert_eq!(
                state.pane_input_capture_for_test(&scene.pane),
                Some(b"q\x01".to_vec()),
                "each frame once, in order, past the {surface}"
            );
            assert_eq!(
                state.pane_input_capture_for_test(&survivor),
                Some(Vec::new())
            );
        }
        drop(state);
        if surface == "popup" {
            std::fs::write(&job, b"go").unwrap();
        } else {
            queue_escape(&scene).await;
            wait_for_surfaces(&scene, (false, false)).await;
        }
        completes(opening).await;
        remove(&[&release, &end, &job]);
    }
}

#[tokio::test]
async fn l05_1_removed_queued_origin_keeps_later_live_frames() {
    let scene = scene("l05-later-frame", 978_520).await;
    let split = scene
        .handler
        .handle(cli_command("split-window -d -t l05-later-frame".to_owned()))
        .await;
    assert!(matches!(split, Response::SourceFile(_)));
    scene.handler.wait_for_initial_panes_for_test().await;
    for index in [0, 1] {
        let pane = PaneTarget::with_window(scene.pane.session_name().clone(), 0, index);
        scene
            .handler
            .disable_pane_focus_reports_for_test(&pane)
            .await;
        scene
            .handler
            .state
            .lock()
            .await
            .start_pane_input_capture_for_test(&pane);
    }
    let (release, end) = scene.released_binding("l05-later-frame").await;
    scene.type_live(b"q").await;
    let killed = scene
        .handler
        .handle(Request::KillPane(KillPaneRequest {
            target: scene.pane.clone(),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillPane(_)));
    scene.type_live(b"r").await;
    assert_eq!(scene.queued_frames().await, Some(2));
    std::fs::write(&release, b"go").unwrap();
    scene.wait_for_pane(b"r").await;
    scene.wait_for_released_work(0).await;
    assert_eq!(
        scene.pane_captured().await,
        b"r",
        "retired q is discarded, later live r remains once"
    );
    remove(&[&release, &end]);
}

#[tokio::test]
async fn l05_1_queued_live_origin_survives_pane_reindex() {
    let scene = scene("l05-live-reindex", 978_521).await;
    let split = scene
        .handler
        .handle(cli_command("split-window -t l05-live-reindex".to_owned()))
        .await;
    assert!(matches!(split, Response::SourceFile(_)));
    scene.handler.wait_for_initial_panes_for_test().await;
    let original = PaneTarget::with_window(scene.pane.session_name().clone(), 0, 1);
    scene
        .handler
        .disable_pane_focus_reports_for_test(&original)
        .await;
    scene
        .handler
        .state
        .lock()
        .await
        .start_pane_input_capture_for_test(&original);
    let (release, end) = scene.released_binding("l05-live-reindex").await;
    scene.type_live(b"q").await;
    let killed = scene
        .handler
        .handle(Request::KillPane(KillPaneRequest {
            target: scene.pane.clone(),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillPane(_)));
    // Input capture is slot-addressed, so observe the pane at its new slot.
    scene.capture_pane().await;
    std::fs::write(&release, b"go").unwrap();
    scene.wait_for_pane(b"q").await;
    scene.wait_for_released_work(0).await;
    assert_eq!(
        scene.pane_captured().await,
        b"q",
        "the same live pane still receives its retained input once"
    );
    remove(&[&release, &end]);
}

#[tokio::test]
async fn l05_1_reindexed_origin_keeps_fragmented_queued_character() {
    let scene = scene("l05-fragment-reindex", 978_522).await;
    let split = scene
        .handler
        .handle(cli_command(
            "split-window -t l05-fragment-reindex".to_owned(),
        ))
        .await;
    assert!(matches!(split, Response::SourceFile(_)));
    scene.handler.wait_for_initial_panes_for_test().await;
    let original = PaneTarget::with_window(scene.pane.session_name().clone(), 0, 1);
    scene
        .handler
        .disable_pane_focus_reports_for_test(&original)
        .await;
    scene.bind("F9", "display-message -p queued-marker").await;
    let (release, end) = scene.released_binding("l05-fragment-reindex").await;
    // The complete F9 sequence keeps the UTF-8 prefix inside the admitted
    // frame; replay must retain that prefix for the following frame.
    scene.type_live(b"\xc3\x1b[20~").await;
    let killed = scene
        .handler
        .handle(Request::KillPane(KillPaneRequest {
            target: scene.pane.clone(),
            kill_all_except: false,
        }))
        .await;
    assert!(matches!(killed, Response::KillPane(_)));
    scene.capture_pane().await;
    scene.type_live(b"\xa9").await;
    assert_eq!(scene.queued_frames().await, Some(2));
    std::fs::write(&release, b"go").unwrap();
    scene.wait_for_released_work(0).await;
    timeout(Duration::from_secs(5), async {
        while scene.queued_frames().await.is_some() {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("both frames drain");
    assert_eq!(
        scene.pane_captured().await,
        "é".as_bytes(),
        "reindex changes a slot, not the origin lifetime of retained bytes"
    );
    remove(&[&release, &end]);
}
