use super::*;
use std::sync::atomic::Ordering;

// Derived before execution: fill + surface, <=cols+64 ASCII bytes per row,
// plus cursor. Vec growth is bounded by 65536 for this fixed representation.
const FRAME_BYTES: usize = 2 * 54 * (225 + 64) + 32;
const FRAME_CAPACITY: usize = 65536;

#[tokio::test]
async fn popup_500k_burst_keeps_one_pending_and_one_in_flight_at_225x54() {
    let handler = RequestHandler::new();
    let name = session_name("popup-500k-bounded");
    let pid = 920_233;
    let mut rx = create_in_process_attached_session(&handler, &name, pid).await;
    run_overlay_command(&handler, pid, "display-popup -N -B -w 100% -h 100%").await;
    let (identity, id, surface, backlog, closing) = {
        let active = handler.active_attach.lock().await;
        let client = &active.by_pid[&pid];
        let Some(ClientOverlayState::Popup(popup)) = client.overlay.as_ref() else {
            panic!("popup")
        };
        assert_eq!((popup.rect.width, popup.rect.height), (225, 54));
        (
            client.identity(pid),
            popup.id,
            Arc::clone(&popup.surface),
            Arc::clone(&client.control_backlog),
            Arc::clone(&client.closing),
        )
    };
    while let Ok(control) = rx.try_recv() {
        crate::pane_io::release_attach_control_backlog(&backlog, control.received_backlog_units());
    }
    let rows: Vec<String> = (0..54)
        .map(|row| format!("row-{row:02}|{}", "X".repeat(218)))
        .collect();
    assert!(rows.iter().all(|row| row.len() == 225));
    let mut final_bytes = b"\x1b[0m\x1b[2J".to_vec();
    for (row, text) in rows.iter().enumerate() {
        final_bytes.extend_from_slice(format!("\x1b[{};1H{}", row + 1, text).as_bytes());
    }
    final_bytes.extend_from_slice(b"\x1b[8;12H");
    let mut data = vec![b'Q'; 512_000 - final_bytes.len()];
    data.extend_from_slice(&final_bytes);
    let mut in_flight = None;
    let mut chunks = 0;
    for chunk in data.chunks(8192) {
        surface.lock().unwrap().append_for_test(chunk);
        let done = Arc::clone(&surface.lock().unwrap().flush_done_for_test);
        handler.popup_reader_tick(identity, id).await.unwrap();
        timeout(Duration::from_secs(2), done.notified())
            .await
            .unwrap();
        chunks += 1;
        assert_eq!(
            rx.len(),
            1,
            "one replaceable marker, irrespective of burst length"
        );
        if in_flight.is_none() {
            let control = rx.try_recv().unwrap().materialize_received();
            let AttachControl::Overlay(frame) = control else {
                panic!("overlay")
            };
            assert!(frame.frame.len() <= FRAME_BYTES && frame.frame.capacity() <= FRAME_CAPACITY);
            in_flight = Some(frame);
        }
        assert_eq!(
            backlog.load(Ordering::Acquire),
            if chunks == 1 { 1 } else { 2 }
        );
        assert!(!closing.load(Ordering::SeqCst));
    }
    let AttachControl::Overlay(frame) = rx.try_recv().unwrap().materialize_received() else {
        panic!("overlay")
    };
    assert!(frame.frame.len() <= FRAME_BYTES && frame.frame.capacity() <= FRAME_CAPACITY);
    assert!(rx.is_empty());
    let mut displayed = rmux_core::Screen::new(
        TerminalSize {
            cols: 225,
            rows: 54,
        },
        0,
    );
    rmux_core::input::InputParser::new().parse(&frame.frame, &mut displayed);
    let options = rmux_core::GridRenderOptions {
        include_empty_cells: true,
        trim_spaces: false,
        ..Default::default()
    };
    for (row, expected) in rows.iter().enumerate() {
        assert_eq!(
            displayed
                .render_visible_line_independent(row, options)
                .unwrap(),
            expected.as_bytes()
        );
    }
    eprintln!("popup burst: bytes=512000 chunks={chunks} geometry=225x54 pending=1 in_flight=1 backlog_units=2 frame_bytes={} frame_capacity={} pty_buffer=8192", frame.frame.len(), frame.frame.capacity());
    drop((in_flight, frame));
    assert_eq!(backlog.load(Ordering::Acquire), 0);
    run_overlay_command(&handler, pid, "display-popup -C").await;
}

// Core state only: unlike create_quiet_attached_session this creates no pane
// process, terminal, shell, daemon or native reader. -N below also forbids a job.
async fn create_in_process_attached_session(
    handler: &RequestHandler,
    name: &SessionName,
    pid: u32,
) -> mpsc::UnboundedReceiver<AttachControl> {
    {
        let mut state = handler.state.lock().await;
        state
            .sessions
            .create_session(
                name.clone(),
                TerminalSize {
                    cols: 225,
                    rows: 54,
                },
            )
            .unwrap();
        // There is no native pane to resize. The real manual window policy
        // leaves its core geometry alone while the attached popup still resizes.
        state
            .options
            .set(
                ScopeSelector::Window(WindowTarget::with_window(name.clone(), 0)),
                OptionName::WindowSize,
                "manual".to_owned(),
                SetOptionMode::Replace,
            )
            .unwrap();
    }
    let (tx, rx) = mpsc::unbounded_channel();
    handler.register_attach(pid, name.clone(), tx).await;
    rx
}

#[tokio::test]
async fn popup_reader_ticks_coalesce_actual_production_before_flush() {
    timeout(Duration::from_secs(5), process_free_coalescence_burst())
        .await
        .expect("reader ticks and final flush must complete without a gate deadlock");
}

async fn process_free_coalescence_burst() {
    let handler = RequestHandler::new();
    let pid = 920_240;
    let mut rx =
        create_in_process_attached_session(&handler, &session_name("popup-coalescence"), pid).await;
    run_overlay_command(&handler, pid, "display-popup -N -B -w 100% -h 100%").await;
    let (identity, id, surface, backlog, closing) = {
        let mut active = handler.active_attach.lock().await;
        let client = active.by_pid.get_mut(&pid).unwrap();
        let identity = client.identity(pid);
        let Some(ClientOverlayState::Popup(popup)) = client.overlay.as_mut() else {
            panic!("popup")
        };
        assert!(popup.job.is_none());
        // -N safely installed the fixture without spawning a process. Model
        // a terminal-backed surface for the real outer cursor projection.
        popup.no_job = false;
        assert_eq!((popup.rect.width, popup.rect.height), (225, 54));
        (
            identity,
            popup.id,
            Arc::clone(&popup.surface),
            Arc::clone(&client.control_backlog),
            Arc::clone(&client.closing),
        )
    };
    while let Ok(control) = rx.try_recv() {
        crate::pane_io::release_attach_control_backlog(&backlog, control.received_backlog_units());
    }
    assert_eq!(backlog.load(Ordering::Acquire), 0);
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    surface.lock().unwrap().flush_gate_for_test = Some(Arc::clone(&gate));
    let baseline = surface.lock().unwrap().production_counts_for_test();
    let rows: Vec<String> = (0..54)
        .map(|row| format!("row-{row:02}|{}", "X".repeat(218)))
        .collect();
    let mut final_bytes = b"\x1b[0m\x1b[2J".to_vec();
    for (row, text) in rows.iter().enumerate() {
        final_bytes.extend_from_slice(format!("\x1b[{};1H{}", row + 1, text).as_bytes());
    }
    final_bytes.extend_from_slice(b"\x1b[?25h\x1b[8;12H");
    let mut data = vec![b'Q'; 512_000 - final_bytes.len()];
    data.extend_from_slice(&final_bytes);
    let mut invalidations = 0;
    for chunk in data.chunks(8192) {
        surface.lock().unwrap().append_for_test(chunk);
        handler.popup_reader_tick(identity, id).await.unwrap();
        invalidations += 1;
    }
    assert_eq!(invalidations, 63);
    let pending_counts = surface.lock().unwrap().production_counts_for_test();
    // All ticks returned while the actual production worker was held.
    assert_eq!(pending_counts, baseline);
    let done = Arc::clone(&surface.lock().unwrap().flush_done_for_test);
    gate.add_permits(1);
    timeout(Duration::from_secs(2), done.notified())
        .await
        .unwrap();
    let control = timeout(Duration::from_secs(2), rx.recv())
        .await
        .unwrap()
        .unwrap();
    crate::pane_io::release_attach_control_backlog(&backlog, control.received_backlog_units());
    let AttachControl::Overlay(frame) = control.materialize_received() else {
        panic!("overlay")
    };
    let actual = surface.lock().unwrap().production_counts_for_test();
    let produced = actual.0 - baseline.0;
    let published = actual.1 - baseline.1;
    let mut displayed = rmux_core::Screen::new(
        TerminalSize {
            cols: 225,
            rows: 54,
        },
        0,
    );
    rmux_core::input::InputParser::new().parse(&frame.frame, &mut displayed);
    let options = rmux_core::GridRenderOptions {
        include_empty_cells: true,
        trim_spaces: false,
        ..Default::default()
    };
    for (row, expected) in rows.iter().enumerate() {
        assert_eq!(
            displayed
                .render_visible_line_independent(row, options)
                .unwrap(),
            expected.as_bytes()
        );
    }
    assert_eq!(displayed.cursor_position(), (11, 7));
    assert_ne!(displayed.mode() & rmux_core::input::mode::MODE_CURSOR, 0);
    assert!(frame.frame.len() <= FRAME_BYTES && frame.frame.capacity() <= FRAME_CAPACITY);
    assert!(rx.is_empty());
    assert!(!closing.load(Ordering::SeqCst));
    drop(frame);
    assert_eq!(backlog.load(Ordering::Acquire), 0);
    eprintln!("popup actual production: bytes={} invalidations={invalidations} pending_renders={} pending_publications={} renders={produced} publications={published} final=225x54 cursor=11,7 backlog=0", data.len(), pending_counts.0 - baseline.0, pending_counts.1 - baseline.1);
    assert!(produced > 0 && produced < invalidations, "queue replacement cannot stand in for reduced full surface production: {produced}/{invalidations}");
    assert!(published > 0 && published < invalidations);
}

use crate::handler::attach_support::ActiveAttachIdentity;
use crate::handler::overlay_support::PopupOverlayState;
use crate::pane_io::OverlayFrame;
use std::sync::atomic::{AtomicBool, AtomicUsize};

async fn open_terminal_surface(handler: &RequestHandler, pid: u32) {
    run_overlay_command(handler, pid, "display-popup -N -B -w 100% -h 100%").await;
    let mut active = handler.active_attach.lock().await;
    let Some(ClientOverlayState::Popup(popup)) =
        active.by_pid.get_mut(&pid).unwrap().overlay.as_mut()
    else {
        panic!("popup")
    };
    assert!(popup.job.is_none());
    popup.no_job = false;
}

async fn popup_snapshot(
    handler: &RequestHandler,
    pid: u32,
) -> (
    ActiveAttachIdentity,
    PopupOverlayState,
    Arc<AtomicUsize>,
    Arc<AtomicBool>,
) {
    let active = handler.active_attach.lock().await;
    let client = &active.by_pid[&pid];
    let Some(ClientOverlayState::Popup(popup)) = client.overlay.as_ref() else {
        panic!("popup")
    };
    (
        client.identity(pid),
        (**popup).clone(),
        Arc::clone(&client.control_backlog),
        Arc::clone(&client.closing),
    )
}

fn hold_reader_flush(
    popup: &PopupOverlayState,
) -> (Arc<tokio::sync::Semaphore>, Arc<tokio::sync::Notify>) {
    let gate = Arc::new(tokio::sync::Semaphore::new(0));
    let mut surface = popup.surface.lock().unwrap();
    surface.flush_gate_for_test = Some(Arc::clone(&gate));
    (gate, Arc::clone(&surface.flush_done_for_test))
}

async fn release_reader_flush(gate: &tokio::sync::Semaphore, done: &tokio::sync::Notify) {
    gate.add_permits(1);
    timeout(Duration::from_secs(2), done.notified())
        .await
        .expect("actual flush completion");
}

fn drain_frames(
    rx: &mut mpsc::UnboundedReceiver<AttachControl>,
    backlog: &AtomicUsize,
) -> Vec<OverlayFrame> {
    let mut frames = Vec::new();
    while let Ok(control) = rx.try_recv() {
        crate::pane_io::release_attach_control_backlog(backlog, control.received_backlog_units());
        if let AttachControl::Overlay(frame) = control.materialize_received() {
            frames.push(frame);
        }
    }
    frames
}

#[tokio::test]
async fn popup_pending_flush_rejects_stale_attach_and_replacement() {
    timeout(Duration::from_secs(5), async {
        for replace_attach in [false, true] {
            let handler = RequestHandler::new();
            let name = session_name("pending-replacement");
            let pid = 920_241;
            let mut rx = create_in_process_attached_session(&handler, &name, pid).await;
            open_terminal_surface(&handler, pid).await;
            let (old_identity, old_popup, old_backlog, _) = popup_snapshot(&handler, pid).await;
            drop(drain_frames(&mut rx, &old_backlog));
            let (gate, done) = hold_reader_flush(&old_popup);
            let old_counts = old_popup
                .surface
                .lock()
                .unwrap()
                .production_counts_for_test();
            old_popup
                .surface
                .lock()
                .unwrap()
                .append_for_test(b"old reader");
            handler
                .popup_reader_tick(old_identity, old_popup.id)
                .await
                .unwrap();
            handler.clear_interactive_overlay(pid, true).await.unwrap();
            if replace_attach {
                let (tx, replacement_rx) = mpsc::unbounded_channel();
                handler.register_attach(pid, name.clone(), tx).await;
                drop(drain_frames(&mut rx, &old_backlog));
                assert_eq!(old_backlog.load(Ordering::Acquire), 0);
                rx = replacement_rx;
            }
            open_terminal_surface(&handler, pid).await;
            {
                let mut active = handler.active_attach.lock().await;
                let Some(ClientOverlayState::Popup(popup)) =
                    active.by_pid.get_mut(&pid).unwrap().overlay.as_mut()
                else {
                    panic!("popup")
                };
                popup.close_on_exit = true;
            }
            let (new_identity, new_popup, backlog, closing) = popup_snapshot(&handler, pid).await;
            if replace_attach {
                assert_ne!(old_identity, new_identity);
                assert_eq!(
                    old_popup.id, new_popup.id,
                    "same PID with colliding popup id"
                );
            } else {
                assert_eq!(old_identity, new_identity);
                assert_ne!(old_popup.id, new_popup.id);
            }
            drop(drain_frames(&mut rx, &backlog));
            new_popup
                .surface
                .lock()
                .unwrap()
                .append_for_test(b"replacement reader");
            let new_done = Arc::clone(&new_popup.surface.lock().unwrap().flush_done_for_test);
            handler
                .popup_reader_tick(new_identity, new_popup.id)
                .await
                .unwrap();
            timeout(Duration::from_secs(2), new_done.notified())
                .await
                .unwrap();
            let frames = drain_frames(&mut rx, &backlog);
            assert!(String::from_utf8_lossy(&frames.last().unwrap().frame)
                .contains("replacement reader"));
            drop(frames);
            let counts = new_popup
                .surface
                .lock()
                .unwrap()
                .production_counts_for_test();
            release_reader_flush(&gate, &done).await;
            handler
                .popup_reader_tick(old_identity, old_popup.id)
                .await
                .unwrap();
            handler
                .popup_job_finished(old_identity, old_popup.id, 0)
                .await
                .unwrap();
            let (identity, popup, _, _) = popup_snapshot(&handler, pid).await;
            assert_eq!((identity, popup.id), (new_identity, new_popup.id));
            assert_eq!(
                old_popup
                    .surface
                    .lock()
                    .unwrap()
                    .production_counts_for_test(),
                old_counts
            );
            assert_eq!(
                new_popup
                    .surface
                    .lock()
                    .unwrap()
                    .production_counts_for_test(),
                counts
            );
            assert!(rx.is_empty());
            assert!(!closing.load(Ordering::SeqCst));
            assert_eq!(backlog.load(Ordering::Acquire), 0);
        }
    })
    .await
    .expect("stale pending flush is bounded");
}

#[tokio::test]
async fn popup_pending_close_prevents_late_repaint() {
    timeout(Duration::from_secs(5), async {
        for close_on_exit in [false, true] {
            let handler = RequestHandler::new();
            let pid = 920_242;
            let mut rx =
                create_in_process_attached_session(&handler, &session_name("pending-close"), pid)
                    .await;
            open_terminal_surface(&handler, pid).await;
            {
                let mut active = handler.active_attach.lock().await;
                let Some(ClientOverlayState::Popup(popup)) =
                    active.by_pid.get_mut(&pid).unwrap().overlay.as_mut()
                else {
                    panic!("popup")
                };
                popup.close_on_exit = close_on_exit;
            }
            let (identity, popup, backlog, closing) = popup_snapshot(&handler, pid).await;
            drop(drain_frames(&mut rx, &backlog));
            let (gate, done) = hold_reader_flush(&popup);
            let before = popup.surface.lock().unwrap().production_counts_for_test();
            popup
                .surface
                .lock()
                .unwrap()
                .append_for_test(b"must never repaint after clear");
            handler.popup_reader_tick(identity, popup.id).await.unwrap();
            if close_on_exit {
                handler
                    .popup_job_finished(identity, popup.id, 0)
                    .await
                    .unwrap();
            } else {
                handler.clear_interactive_overlay(pid, true).await.unwrap();
            }
            let frames = drain_frames(&mut rx, &backlog);
            assert_eq!(frames.len(), 1);
            assert!(frames[0].frame.is_empty(), "close remains a reliable clear");
            drop(frames);
            release_reader_flush(&gate, &done).await;
            assert!(handler.active_attach.lock().await.by_pid[&pid]
                .overlay
                .is_none());
            assert_eq!(
                popup.surface.lock().unwrap().production_counts_for_test(),
                before
            );
            assert!(rx.is_empty());
            assert!(!closing.load(Ordering::SeqCst));
            assert_eq!(backlog.load(Ordering::Acquire), 0);
        }
    })
    .await
    .expect("close with pending reader is bounded");
}

#[tokio::test]
async fn popup_pending_flush_preserves_resize_and_final_exit_surface() {
    timeout(Duration::from_secs(5), async {
        let handler = RequestHandler::new();
        let pid = 920_243;
        let mut rx =
            create_in_process_attached_session(&handler, &session_name("pending-resize-eof"), pid)
                .await;
        open_terminal_surface(&handler, pid).await;
        let (identity, popup, backlog, closing) = popup_snapshot(&handler, pid).await;
        drop(drain_frames(&mut rx, &backlog));
        let (gate, done) = hold_reader_flush(&popup);
        popup
            .surface
            .lock()
            .unwrap()
            .append_for_test(b"before resize");
        handler.popup_reader_tick(identity, popup.id).await.unwrap();
        for size in [
            TerminalSize {
                cols: 200,
                rows: 40,
            },
            TerminalSize {
                cols: 225,
                rows: 54,
            },
        ] {
            handler
                .handle_attached_resize_for_identity(identity, size)
                .await
                .unwrap();
            let (_, current, _, _) = popup_snapshot(&handler, pid).await;
            assert_eq!(
                (current.rect.width, current.rect.height),
                (size.cols, size.rows)
            );
        }
        let rows: Vec<String> = (0..54)
            .map(|row| format!("row-{row:02}|{}", "Z".repeat(218)))
            .collect();
        let mut bytes = b"\x1b[0m\x1b[2J".to_vec();
        for (row, text) in rows.iter().enumerate() {
            bytes.extend_from_slice(format!("\x1b[{};1H{}", row + 1, text).as_bytes());
        }
        bytes.extend_from_slice(b"\x1b[?25h\x1b[8;12H");
        popup.surface.lock().unwrap().append_for_test(&bytes);
        // Exercise the real completion callback while the reader flush is held.
        handler
            .popup_job_finished(identity, popup.id, 0)
            .await
            .unwrap();
        let frames = drain_frames(&mut rx, &backlog);
        assert!(
            frames.len() >= 3,
            "resize/reliable controls precede final EOF repaint"
        );
        assert!(frames
            .windows(2)
            .all(|pair| pair[0].overlay_generation < pair[1].overlay_generation));
        let eof_bytes = frames.last().unwrap().frame.clone();
        drop(frames);
        assert_eq!(backlog.load(Ordering::Acquire), 0);
        release_reader_flush(&gate, &done).await;
        let frames = drain_frames(&mut rx, &backlog);
        assert_eq!(frames.len(), 1);
        assert_eq!(
            frames[0].frame, eof_bytes,
            "pending flush keeps the exact EOF surface"
        );
        let mut displayed = rmux_core::Screen::new(
            TerminalSize {
                cols: 225,
                rows: 54,
            },
            0,
        );
        rmux_core::input::InputParser::new().parse(&frames[0].frame, &mut displayed);
        let options = rmux_core::GridRenderOptions {
            include_empty_cells: true,
            trim_spaces: false,
            ..Default::default()
        };
        for (row, expected) in rows.iter().enumerate() {
            assert_eq!(
                displayed
                    .render_visible_line_independent(row, options)
                    .unwrap(),
                expected.as_bytes()
            );
        }
        assert_eq!(displayed.cursor_position(), (11, 7));
        assert_ne!(displayed.mode() & rmux_core::input::mode::MODE_CURSOR, 0);
        drop(frames);
        assert!(!closing.load(Ordering::SeqCst));
        assert_eq!(backlog.load(Ordering::Acquire), 0);
    })
    .await
    .expect("resize/EOF/pending flush is bounded");
}

#[tokio::test]
async fn popup_mouse_resize_survives_a_client_smaller_than_its_border() {
    let handler = RequestHandler::new();
    let pid = 920_244;
    let mut rx =
        create_in_process_attached_session(&handler, &session_name("popup-small-drag"), pid).await;
    run_overlay_command(&handler, pid, "display-popup -N -w 20 -h 6 -x 0 -y 0").await;
    let (identity, popup, backlog, closing) = popup_snapshot(&handler, pid).await;
    let x = popup.rect.x + popup.rect.width - 1;
    let y = popup.rect.y + popup.rect.height - 1;
    for report in [
        sgr_mouse(2, x, y),
        sgr_mouse(34, x, y - 1),
        sgr_mouse(34, x + 4, y + 2),
    ] {
        handler
            .handle_attached_live_input_for_test(pid, &report)
            .await
            .unwrap();
    }
    // The existing popup_drag_resize_releases_attach_lock_while_pty_resize_blocks
    // contract includes the pointer cell in the rectangle. Preserve that rule:
    // 24x8 here (tmux 3.7b measures 23x7), without turning a resize into a move.
    let (_, resized, _, _) = popup_snapshot(&handler, pid).await;
    assert_eq!(
        (
            resized.rect.x,
            resized.rect.y,
            resized.rect.width,
            resized.rect.height
        ),
        (popup.rect.x, popup.rect.y, 24, 8)
    );
    drop(drain_frames(&mut rx, &backlog));
    handler
        .handle_attached_resize_for_identity(identity, TerminalSize { cols: 2, rows: 2 })
        .await
        .unwrap();
    handler
        .handle_attached_live_input_for_test(pid, &sgr_mouse(34, 1, 1))
        .await
        .expect("a continued drag in a tiny client must not panic");
    let (_, small, _, _) = popup_snapshot(&handler, pid).await;
    assert_eq!((small.rect.width, small.rect.height), (2, 2));
    assert!(!closing.load(Ordering::SeqCst));
    handler.clear_interactive_overlay(pid, true).await.unwrap();
    drop(drain_frames(&mut rx, &backlog));
    assert_eq!(backlog.load(Ordering::Acquire), 0);
}

#[cfg(unix)]
mod menu_execute_blocked;
mod menu_repaint;
mod popup_status;
