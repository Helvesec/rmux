//! D1 R3: who waits for a popup's binding, in which order the client's input goes on, and how a
//! menu action completes around its popup.
//!
//! Every test drives the real entry of its caller - a CLI request, the client's live input - so
//! the client's input path is exactly where production puts it.

use std::future::{poll_fn, Future};
use std::sync::Arc;
use std::task::Poll;

use tokio::sync::Notify;
use tokio::time::{sleep, timeout, timeout_at, Duration, Instant};

use super::completion_tests::{
    bind_root_key, blocking_job, cli_command, exit_status, release_path, scene, wait_for_file,
    wait_for_popup, ATTACH_PID, F10,
};
use rmux_proto::{PaneTarget, Response, SessionName};

use super::super::RequestHandler;

async fn pane_received(handler: &RequestHandler, pane: &PaneTarget, bytes: &[u8]) -> bool {
    handler
        .state
        .lock()
        .await
        .pane_input_capture_for_test(pane)
        .is_some_and(|captured| captured.windows(bytes.len()).any(|window| window == bytes))
}

async fn wait_for_pane_input(handler: &RequestHandler, pane: &PaneTarget, bytes: &[u8]) {
    timeout(Duration::from_secs(10), async {
        while !pane_received(handler, pane, bytes).await {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the pane receives the input");
}

#[tokio::test]
async fn a_send_keys_caller_keeps_waiting_for_its_binding_and_receives_its_error() {
    let (handler, _control_rx) = scene("d1-send-keys-owner").await;
    let release = release_path("send-keys-job");
    bind_root_key(
        &handler,
        "F9",
        &format!(
            "display-popup -w 20 -h 5 -E \"{}\" ; select-window -t no-such-session-r3:9",
            blocking_job(&release, 0)
        ),
    )
    .await;

    // send-keys -K dispatches the binding itself; it does not carry the client's input.
    let sending = tokio::spawn({
        let handler = handler.clone();
        async move {
            handler
                .handle(cli_command(format!("send-keys -K -c {ATTACH_PID} F9")))
                .await
        }
    });
    wait_for_popup(&handler).await;
    sleep(Duration::from_millis(300)).await;
    assert!(
        !sending.is_finished(),
        "the popup must not release a caller that awaits the binding for itself"
    );

    std::fs::write(&release, b"go").expect("release the popup job");
    let response = timeout(Duration::from_secs(10), sending)
        .await
        .expect("send-keys completes once its binding has run")
        .expect("the send-keys task");
    let _ = std::fs::remove_file(&release);
    let Response::SourceFile(source) = &response else {
        panic!("expected the queued command response, got {response:?}");
    };
    assert_eq!(
        exit_status(&response),
        Some(1),
        "the binding's failing command fails the caller: {response:?}"
    );
    assert!(
        String::from_utf8_lossy(source.stderr()).contains("no-such-session-r3"),
        "the caller receives the error of the command after the popup: {response:?}"
    );
}

#[tokio::test]
async fn ordinary_keys_stay_behind_a_released_binding_while_popup_keys_go_on() {
    let session = "d1-ordinary-order";
    let (handler, _control_rx) = scene(session).await;
    let binding_release = release_path("order-binding");
    let binding_end = release_path("order-binding-end");
    bind_root_key(
        &handler,
        "F10",
        &format!(
            "run-shell \"while [ ! -e {} ]; do sleep 0.02; done; touch {}\"",
            binding_release.display(),
            binding_end.display()
        ),
    )
    .await;
    let binding_input = tokio::spawn({
        let handler = handler.clone();
        async move {
            handler
                .handle_attached_live_input_for_test(ATTACH_PID, F10)
                .await
        }
    });
    sleep(Duration::from_millis(200)).await;

    let job_release = release_path("order-job");
    let waiting = tokio::spawn({
        let handler = handler.clone();
        let command = format!(
            "display-popup -c {ATTACH_PID} -w 20 -h 5 -E \"{}\"",
            blocking_job(&job_release, 5)
        );
        async move { handler.handle(cli_command(command)).await }
    });
    wait_for_popup(&handler).await;
    timeout(Duration::from_secs(5), binding_input)
        .await
        .expect("the popup releases the input path from the binding")
        .expect("the input task")
        .expect("the key is handled");
    timeout(
        Duration::from_secs(2),
        handler.handle_attached_live_input_for_test(ATTACH_PID, b"a"),
    )
    .await
    .expect("keys for the popup go on while the binding still runs")
    .expect("the popup key is handled");

    std::fs::write(&job_release, b"go").expect("release the popup job");
    let response = timeout(Duration::from_secs(10), waiting)
        .await
        .expect("the CLI completes once its job exits")
        .expect("the CLI task");
    assert_eq!(exit_status(&response), Some(5));

    let pane = PaneTarget::new(SessionName::new(session).expect("valid session"), 0);
    handler
        .state
        .lock()
        .await
        .start_pane_input_capture_for_test(&pane);
    // The key is queued as it arrives; the client's input path does not block on it.
    timeout(
        Duration::from_secs(2),
        handler.handle_attached_live_input_for_test(ATTACH_PID, b"q"),
    )
    .await
    .expect("an ordinary key is queued without holding the input path")
    .expect("the key is handled");
    sleep(Duration::from_millis(300)).await;
    assert!(
        !pane_received(&handler, &pane, b"q").await,
        "the pane must not receive the key before the binding ends"
    );

    std::fs::write(&binding_release, b"go").expect("end the binding");
    wait_for_pane_input(&handler, &pane, b"q").await;
    assert!(
        binding_end.exists(),
        "the binding finished before the key went on"
    );
    for path in [&binding_release, &binding_end, &job_release] {
        let _ = std::fs::remove_file(path);
    }
}

#[tokio::test]
async fn a_menu_item_holds_its_next_command_until_its_popup_closes_and_keeps_input_live() {
    let (handler, _control_rx) = scene("d1-menu-chain").await;
    let release = release_path("menu-job");
    let consumer = release_path("menu-consumer");
    let menu = format!(
        "display-menu -c {ATTACH_PID} -x 0 -y 0 -T Menu Chain k {{ display-popup -w 20 -h 5 -E \"{}\" ; run-shell \"touch {}\" }}",
        blocking_job(&release, 0),
        consumer.display()
    );
    let opening = tokio::spawn({
        let handler = handler.clone();
        async move { handler.handle(cli_command(menu)).await }
    });
    wait_for_popup(&handler).await;

    // The item is chosen through the client's live input, where menu choices run.
    timeout(
        Duration::from_secs(5),
        handler.handle_attached_live_input_for_test(ATTACH_PID, b"k"),
    )
    .await
    .expect("the input path is free while the item's popup is open")
    .expect("the menu key is handled");
    wait_for_popup(&handler).await;
    sleep(Duration::from_millis(300)).await;
    assert!(
        !consumer.exists(),
        "the item's next command waits until its popup closes"
    );
    timeout(
        Duration::from_secs(2),
        handler.handle_attached_live_input_for_test(ATTACH_PID, b"a"),
    )
    .await
    .expect("keys for the item's popup go on")
    .expect("the popup key is handled");

    std::fs::write(&release, b"go").expect("release the popup job");
    wait_for_file(&consumer).await;
    assert!(
        !handler.overlay_active(ATTACH_PID).await,
        "the popup closed before the item's next command ran"
    );
    let _ = timeout(Duration::from_secs(5), opening).await;
    let _ = std::fs::remove_file(&release);
    let _ = std::fs::remove_file(&consumer);
}

struct PopupInstallPause {
    ready: std::path::PathBuf,
    reached: Arc<Notify>,
    resume: Arc<Notify>,
}

// Each current-thread test runtime has its own pause; another client's test cannot take it.
thread_local! {
    static INSTALL_PAUSE: std::cell::RefCell<Option<PopupInstallPause>> = const { std::cell::RefCell::new(None) };
}

pub(super) async fn pause_popup_install_for_test() {
    let pause = INSTALL_PAUSE.with(|pause| pause.borrow_mut().take());
    if let Some(pause) = pause {
        wait_for_file(&pause.ready).await;
        pause.reached.notify_one();
        pause.resume.notified().await;
    }
}

async fn trace_gap_transition(handler: &RequestHandler, transition: &str) {
    let active = handler.active_attach.lock().await;
    eprintln!(
        "gap transition={transition} overlay={} released={} queued={:?}",
        active
            .by_pid
            .get(&ATTACH_PID)
            .is_some_and(|client| client.overlay.is_some()),
        active.attached_input_waits.released_work_count(ATTACH_PID),
        active.attached_input_waits.queued_frame_count(ATTACH_PID),
    );
}

#[tokio::test]
async fn a_key_typed_between_two_popups_of_a_binding_reaches_the_pane_after_the_binding() {
    let session = "d1-gap-key";
    let (handler, _control_rx) = scene(session).await;
    let first_release = release_path("gap-first");
    let gap_release = release_path("gap-between");
    let second_ready = release_path("gap-second-ready");
    let second_first_byte = release_path("gap-second-first-byte");
    let second_release = release_path("gap-second");
    let binding_end = release_path("gap-binding-end");
    // The second popup's job records the first byte it ever receives.
    let second_job = format!(
        "sh -c 'stty raw -echo; touch {}; dd bs=1 count=1 of={} 2>/dev/null; \
         while [ ! -e {} ]; do sleep 0.02; done'",
        second_ready.display(),
        second_first_byte.display(),
        second_release.display()
    );
    bind_root_key(
        &handler,
        "F7",
        &format!(
            "display-popup -w 20 -h 5 -E \"{}\" ; \
             run-shell \"while [ ! -e {} ]; do sleep 0.02; done\" ; \
             display-popup -w 20 -h 5 -E \"{second_job}\" ; run-shell \"touch {}\"",
            blocking_job(&first_release, 0),
            gap_release.display(),
            binding_end.display()
        ),
    )
    .await;
    timeout(
        Duration::from_secs(5),
        handler.handle_attached_live_input_for_test(ATTACH_PID, b"\x1b[18~"),
    )
    .await
    .expect("the first popup releases the input path")
    .expect("the key is handled");
    wait_for_popup(&handler).await;
    let pane = PaneTarget::new(SessionName::new(session).expect("valid session"), 0);
    trace_gap_transition(&handler, "first popup open").await;
    std::fs::write(&first_release, b"go").expect("close the first popup");
    timeout(Duration::from_secs(5), async {
        while handler.overlay_active(ATTACH_PID).await {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the first popup closes while the binding goes on");
    handler
        .state
        .lock()
        .await
        .start_pane_input_capture_for_test(&pane);

    trace_gap_transition(&handler, "first popup closed").await;
    // Typed in the gap: no popup is up, so the key is ordinary and waits behind the binding.
    timeout(
        Duration::from_secs(2),
        handler.handle_attached_live_input_for_test(ATTACH_PID, b"q"),
    )
    .await
    .expect("the gap key is queued without holding the input path")
    .expect("the key is handled");
    sleep(Duration::from_millis(200)).await;
    assert!(!pane_received(&handler, &pane, b"q").await);

    trace_gap_transition(&handler, "gap key queued").await;
    let install_reached = Arc::new(Notify::new());
    let install_resume = Arc::new(Notify::new());
    INSTALL_PAUSE.with(|pause| {
        *pause.borrow_mut() = Some(PopupInstallPause {
            ready: second_ready.clone(),
            reached: install_reached.clone(),
            resume: install_resume.clone(),
        });
    });
    let ready_deadline = Instant::now() + Duration::from_secs(10);
    std::fs::write(&gap_release, b"go").expect("let the binding show its second popup");
    timeout_at(ready_deadline, install_reached.notified())
        .await
        .expect("second job started before installation");
    let mut second_input = Box::pin(async {
        // The job can touch its marker before the server installs its popup. Only an installed
        // overlay takes new keys; a key sent earlier correctly waits behind the binding.
        timeout_at(ready_deadline, async {
            while !second_ready.exists() || !handler.overlay_active(ATTACH_PID).await {
                sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the second popup and its job are ready for input");
        trace_gap_transition(&handler, "second job ready before input").await;
        timeout(
            Duration::from_secs(2),
            handler.handle_attached_live_input_for_test(ATTACH_PID, b"a"),
        )
        .await
        .expect("the second popup takes the keys typed while it is up")
        .expect("the popup key is handled");
        trace_gap_transition(&handler, "second input handled").await;
    });
    // Poll the input attempt while installation is paused: job readiness alone would enqueue
    // `a` behind `q`. Waiting for the overlay must instead leave this attempt pending.
    let first_poll = poll_fn(|cx| Poll::Ready(second_input.as_mut().poll(cx))).await;
    install_resume.notify_one();
    if first_poll.is_pending() {
        second_input.await;
    }
    wait_for_file(&second_first_byte).await;
    timeout(Duration::from_secs(5), async {
        while std::fs::read(&second_first_byte)
            .unwrap_or_default()
            .is_empty()
        {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the second popup job records its first byte");
    assert_eq!(
        std::fs::read(&second_first_byte).expect("first byte"),
        b"a",
        "the key typed in the gap must never reach the popup that opened after it"
    );

    std::fs::write(&second_release, b"go").expect("close the second popup");
    wait_for_pane_input(&handler, &pane, b"q").await;
    assert!(
        binding_end.exists(),
        "the gap key reached the pane after the binding ended"
    );
    for path in [
        &first_release,
        &gap_release,
        &second_ready,
        &second_first_byte,
        &second_release,
        &binding_end,
    ] {
        let _ = std::fs::remove_file(path);
    }
}

#[tokio::test]
async fn a_focus_report_goes_on_while_ordinary_keys_wait_behind_a_released_binding() {
    let session = "d1-focus-order";
    let (handler, _control_rx) = scene(session).await;
    let focus = handler
        .handle(cli_command("set-option -g focus-events on".to_owned()))
        .await;
    assert_eq!(exit_status(&focus), None, "{focus:?}");
    let pane = PaneTarget::new(SessionName::new(session).expect("valid session"), 0);
    handler
        .state
        .lock()
        .await
        .append_bytes_to_pane_transcript_for_test(
            &SessionName::new(session).expect("valid session"),
            0,
            0,
            b"\x1b[?1004h",
        )
        .expect("the pane asks for focus reports");
    let binding_release = release_path("focus-binding");
    let binding_end = release_path("focus-binding-end");
    bind_root_key(
        &handler,
        "F10",
        &format!(
            "run-shell \"while [ ! -e {} ]; do sleep 0.02; done; touch {}\"",
            binding_release.display(),
            binding_end.display()
        ),
    )
    .await;
    let binding_input = tokio::spawn({
        let handler = handler.clone();
        async move {
            handler
                .handle_attached_live_input_for_test(ATTACH_PID, F10)
                .await
        }
    });
    sleep(Duration::from_millis(200)).await;
    let job_release = release_path("focus-job");
    let waiting = tokio::spawn({
        let handler = handler.clone();
        let command = format!(
            "display-popup -c {ATTACH_PID} -w 20 -h 5 -E \"{}\"",
            blocking_job(&job_release, 0)
        );
        async move { handler.handle(cli_command(command)).await }
    });
    wait_for_popup(&handler).await;
    timeout(Duration::from_secs(5), binding_input)
        .await
        .expect("the popup releases the input path")
        .expect("the input task")
        .expect("the key is handled");
    std::fs::write(&job_release, b"go").expect("close the popup");
    timeout(Duration::from_secs(10), waiting)
        .await
        .expect("the CLI completes")
        .expect("the CLI task");
    handler
        .state
        .lock()
        .await
        .start_pane_input_capture_for_test(&pane);

    handler
        .handle_attached_live_input_for_test(ATTACH_PID, b"\x1b[O")
        .await
        .expect("the focus report is handled");
    timeout(Duration::from_secs(2), async {
        while !pane_received(&handler, &pane, b"\x1b[O").await {
            sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("the focus report reaches the pane while the binding still runs, as in tmux");
    assert!(!binding_end.exists(), "the binding is still running");
    handler
        .handle_attached_live_input_for_test(ATTACH_PID, b"q")
        .await
        .expect("the key is handled");
    sleep(Duration::from_millis(200)).await;
    assert!(
        !pane_received(&handler, &pane, b"q").await,
        "the ordinary key still waits behind the binding"
    );

    std::fs::write(&binding_release, b"go").expect("end the binding");
    wait_for_pane_input(&handler, &pane, b"q").await;
    assert!(binding_end.exists());
    for path in [&binding_release, &binding_end, &job_release] {
        let _ = std::fs::remove_file(path);
    }
}
