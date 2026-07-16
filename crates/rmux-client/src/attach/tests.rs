use std::fs::File;
use std::io::{self, Read, Write};
use std::os::fd::{AsFd, BorrowedFd};
use std::os::unix::fs::PermissionsExt;
use std::os::unix::net::UnixStream;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rmux_core::alternate_screen_exit_sequence;
use rmux_proto::{
    encode_attach_message, AttachFrameDecoder, AttachMessage, AttachShellCommand, TerminalGeometry,
    TerminalPixels,
};
use rmux_pty::{ChildCommand, PtyIo, PtyPair};
use rustix::event::{poll, PollFd, PollFlags, Timespec};
use rustix::termios::{tcsetwinsize, Winsize};

use crate::attach_lock_state::AttachLockState;

use super::{
    attach_with_terminal, drain_resize_events, fallback_attach_stop_sequence, handle_attach_action,
    input_loop, output_loop, terminal_size_from_fd, write_attach_unlock, AttachScreenTracker,
    ClientAttachAction, RawTerminal, ResizeWatcher, SignalMaskGuard, TerminalSize,
};

static RESIZE_WATCHER_SIGNAL_TEST_LOCK: Mutex<()> = Mutex::new(());

const CONTROLLING_TTY_CASE_ENV: &str = "RMUX_CLIENT_CONTROLLING_TTY_CASE";
const LOCK_ACTIONS_TTY_CASE: &str = "lock-actions";
const STRUCTURED_SHELL_TTY_CASE: &str = "structured-shell";

fn run_test_with_controlling_tty(
    test_name: &str,
    case: &str,
) -> Result<(), Box<dyn std::error::Error>> {
    let spawned = ChildCommand::new(std::env::current_exe()?)
        .arg(test_name)
        .args(["--exact", "--nocapture", "--test-threads=1"])
        .env(CONTROLLING_TTY_CASE_ENV, case)
        .size(rmux_pty::TerminalSize::new(80, 24))
        .spawn()?;
    let (master, mut child) = spawned.into_parts();
    let output_io = master.into_io();
    output_io.release_startup_slave_guard();
    let output_thread = std::thread::spawn(move || read_test_pty_output(output_io));

    let status = child.wait()?;
    let output = output_thread
        .join()
        .map_err(|_| io::Error::other("controlling-TTY output reader panicked"))??;
    if !status.success() {
        return Err(io::Error::other(format!(
            "controlling-TTY child {test_name} failed with {status}: {}",
            String::from_utf8_lossy(&output)
        ))
        .into());
    }
    Ok(())
}

fn read_test_pty_output(output: PtyIo) -> io::Result<Vec<u8>> {
    let mut collected = Vec::new();
    let mut buffer = [0_u8; 4096];
    loop {
        match output.read(&mut buffer) {
            Ok(0) => return Ok(collected),
            Ok(bytes_read) => collected.extend_from_slice(&buffer[..bytes_read]),
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) if error.raw_os_error() == Some(libc::EIO) => return Ok(collected),
            Err(error) => return Err(error),
        }
    }
}

fn attach_stop_payload() -> Vec<u8> {
    let mut payload = fallback_attach_stop_sequence("xterm-256color");
    payload.extend_from_slice(b"[detached (from session alpha)]\r\n");
    payload
}

#[test]
fn terminal_size_from_fd_ignores_zero_sized_terminals() -> Result<(), Box<dyn std::error::Error>> {
    let pair = PtyPair::open_with_size(rmux_pty::TerminalSize::new(80, 24))?;
    let (_master, slave) = pair.into_split();
    let terminal = File::from(slave.into_owned_fd());

    tcsetwinsize(
        &terminal,
        Winsize {
            ws_row: 0,
            ws_col: 0,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )?;

    assert_eq!(terminal_size_from_fd(&terminal)?, None);
    Ok(())
}

#[test]
fn resize_watcher_reports_sigwinch_updates() -> Result<(), Box<dyn std::error::Error>> {
    let _signal_test_lock = RESIZE_WATCHER_SIGNAL_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let pair = PtyPair::open_with_size(rmux_pty::TerminalSize::new(80, 24))?;
    let (_master, slave) = pair.into_split();
    let terminal = File::from(slave.into_owned_fd());
    let resize_target = terminal.try_clone()?;
    let _signal_mask = SignalMaskGuard::block_winch()?;
    let (resize_tx, resize_rx) = mpsc::channel();
    let watcher = ResizeWatcher::spawn(terminal.as_fd().try_clone_to_owned()?, resize_tx)?;

    tcsetwinsize(
        &resize_target,
        Winsize {
            ws_row: 40,
            ws_col: 120,
            ws_xpixel: 0,
            ws_ypixel: 0,
        },
    )?;

    watcher.notify_for_test()?;
    assert_eq!(
        resize_rx.recv_timeout(Duration::from_secs(1))?,
        TerminalGeometry::new(120, 40)
    );

    drop(watcher);
    Ok(())
}

#[test]
fn resize_watcher_reports_pixel_dimensions_when_available() -> Result<(), Box<dyn std::error::Error>>
{
    let _signal_test_lock = RESIZE_WATCHER_SIGNAL_TEST_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner());
    let pair = PtyPair::open_with_size(rmux_pty::TerminalSize::new(80, 24))?;
    let (_master, slave) = pair.into_split();
    let terminal = File::from(slave.into_owned_fd());
    let resize_target = terminal.try_clone()?;
    let _signal_mask = SignalMaskGuard::block_winch()?;
    let (resize_tx, resize_rx) = mpsc::channel();
    let watcher = ResizeWatcher::spawn(terminal.as_fd().try_clone_to_owned()?, resize_tx)?;

    tcsetwinsize(
        &resize_target,
        Winsize {
            ws_row: 40,
            ws_col: 120,
            ws_xpixel: 1440,
            ws_ypixel: 960,
        },
    )?;

    watcher.notify_for_test()?;
    assert_eq!(
        resize_rx.recv_timeout(Duration::from_secs(1))?,
        TerminalGeometry::new(120, 40).with_pixels(TerminalPixels::new(1440, 960))
    );

    drop(watcher);
    Ok(())
}

#[test]
fn input_loop_emits_raw_data_frame_for_native_typed_bytes() -> Result<(), Box<dyn std::error::Error>>
{
    let (client_stream, mut server_stream) = UnixStream::pair()?;
    let (mut input_writer, input_reader) = UnixStream::pair()?;
    let (_resize_tx, resize_rx) = mpsc::channel();
    let closed = Arc::new(AtomicBool::new(false));
    let locked = Arc::new(AttachLockState::default());
    let (_wakeup, wakeup_reader) = UnixStream::pair()?;

    let input_thread = std::thread::spawn(move || {
        input_loop(
            client_stream,
            input_reader,
            resize_rx,
            false,
            closed,
            locked,
            wakeup_reader,
        )
    });

    input_writer.write_all(b"a")?;
    input_writer.shutdown(std::net::Shutdown::Write)?;

    let mut frame = [0_u8; 64];
    let bytes_read = server_stream.read(&mut frame)?;
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&frame[..bytes_read]);

    assert_eq!(
        decoder.next_message()?,
        Some(AttachMessage::Data(b"a".to_vec()))
    );

    input_thread
        .join()
        .map_err(|_| "input loop thread panicked")??;
    Ok(())
}

#[test]
fn lock_action_waits_for_an_inflight_unix_input_read_and_forward(
) -> Result<(), Box<dyn std::error::Error>> {
    let (client_stream, mut server_stream) = UnixStream::pair()?;
    let (mut readiness_writer, readiness_reader) = UnixStream::pair()?;
    let (entered_tx, entered_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let input = PausedInput {
        readiness: readiness_reader,
        entered: entered_tx,
        release: release_rx,
        pause_next: true,
    };
    let (_resize_tx, resize_rx) = mpsc::channel();
    let closed = Arc::new(AtomicBool::new(false));
    let locked = Arc::new(AttachLockState::default());
    let input_locked = Arc::clone(&locked);
    let (_wakeup, wakeup_reader) = UnixStream::pair()?;
    let input_thread = std::thread::spawn(move || {
        input_loop(
            client_stream,
            input,
            resize_rx,
            false,
            closed,
            input_locked,
            wakeup_reader,
        )
    });

    readiness_writer.write_all(b"a")?;
    entered_rx.recv_timeout(Duration::from_secs(1))?;
    locked.lock();
    let waiter_state = Arc::clone(&locked);
    let (idle_tx, idle_rx) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        waiter_state.wait_until_input_idle();
        idle_tx.send(()).expect("signal idle input");
    });
    assert!(
        idle_rx.recv_timeout(Duration::from_millis(25)).is_err(),
        "exclusive terminal action must wait while the input read is active"
    );

    release_tx.send(())?;
    let mut frame = [0_u8; 64];
    let bytes_read = server_stream.read(&mut frame)?;
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&frame[..bytes_read]);
    assert_eq!(
        decoder.next_message()?,
        Some(AttachMessage::Data(b"a".to_vec()))
    );
    idle_rx.recv_timeout(Duration::from_secs(1))?;
    waiter.join().map_err(|_| "lock waiter panicked")?;

    locked.unlock();
    readiness_writer.shutdown(std::net::Shutdown::Write)?;
    input_thread
        .join()
        .map_err(|_| "input loop thread panicked")??;
    Ok(())
}

#[test]
fn unix_lock_actions_rearm_screen_before_sending_unlock() -> Result<(), Box<dyn std::error::Error>>
{
    if std::env::var(CONTROLLING_TTY_CASE_ENV).as_deref() != Ok(LOCK_ACTIONS_TTY_CASE) {
        return run_test_with_controlling_tty(
            "attach::tests::unix_lock_actions_rearm_screen_before_sending_unlock",
            LOCK_ACTIONS_TTY_CASE,
        );
    }

    let raw_terminal = RawTerminal::from_fd(&io::stdin())?;
    let cwd = std::env::current_dir()?.to_string_lossy().into_owned();

    for structured in [false, true] {
        let (mut client_stream, mut server_stream) = UnixStream::pair()?;
        let locked = Arc::new(AttachLockState::default());
        let screen_tracker = AttachScreenTracker::default();
        locked.lock();
        let stop_generation = screen_tracker.mark_stopped();
        let action = if structured {
            ClientAttachAction::LockShell {
                command: AttachShellCommand::new(
                    "true".to_owned(),
                    "/bin/sh".to_owned(),
                    cwd.clone(),
                ),
                stop_generation: Some(stop_generation),
            }
        } else {
            ClientAttachAction::Lock {
                command: "true".to_owned(),
                stop_generation: Some(stop_generation),
            }
        };

        handle_attach_action(
            Some(&raw_terminal),
            &mut client_stream,
            &locked,
            &screen_tracker,
            action,
        )?;

        assert!(
            !screen_tracker.was_stopped(),
            "successful lock action must rearm attach lifecycle"
        );
        assert!(!locked.is_locked());
        let mut frame = [0_u8; 64];
        let bytes_read = server_stream.read(&mut frame)?;
        let mut decoder = AttachFrameDecoder::new();
        decoder.push_bytes(&frame[..bytes_read]);
        assert_eq!(decoder.next_message()?, Some(AttachMessage::Unlock));
    }

    Ok(())
}

#[test]
fn unix_unlock_write_failure_still_leaves_screen_rearmed() -> Result<(), Box<dyn std::error::Error>>
{
    let (mut client_stream, server_stream) = UnixStream::pair()?;
    let screen_tracker = AttachScreenTracker::default();
    let stop_generation = screen_tracker.mark_stopped();
    drop(server_stream);

    write_attach_unlock(&mut client_stream, &screen_tracker, Some(stop_generation))
        .expect_err("closed peer must reject attach unlock");
    assert!(
        !screen_tracker.was_stopped(),
        "failed unlock transport must trigger outer terminal cleanup"
    );
    Ok(())
}

#[test]
fn unix_lock_completion_preserves_a_later_final_stop() -> Result<(), Box<dyn std::error::Error>> {
    let (mut client_stream, mut server_stream) = UnixStream::pair()?;
    let screen_tracker = AttachScreenTracker::default();
    let lock_prelude = screen_tracker.mark_stopped();
    let final_stop = screen_tracker.mark_stopped();
    server_stream.set_nonblocking(true)?;

    write_attach_unlock(&mut client_stream, &screen_tracker, Some(lock_prelude))?;

    assert_eq!(screen_tracker.current_stop_generation(), Some(final_stop));
    let mut frame = [0_u8; 64];
    let error = server_stream
        .read(&mut frame)
        .expect_err("a stale lock completion must not send unlock");
    assert_eq!(error.kind(), io::ErrorKind::WouldBlock);
    Ok(())
}

#[test]
fn drain_resize_events_uses_legacy_resize_until_geometry_is_enabled(
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut client_stream, mut server_stream) = UnixStream::pair()?;
    let (resize_tx, resize_rx) = mpsc::channel();
    resize_tx.send(TerminalGeometry::new(120, 40).with_pixels(TerminalPixels::new(1440, 960)))?;

    drain_resize_events(&mut client_stream, &resize_rx, false)?;

    let mut frame = [0_u8; 128];
    let bytes_read = server_stream.read(&mut frame)?;
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&frame[..bytes_read]);
    assert_eq!(
        decoder.next_message()?,
        Some(AttachMessage::Resize(TerminalSize {
            cols: 120,
            rows: 40
        }))
    );
    Ok(())
}

#[test]
fn drain_resize_events_sends_geometry_after_capability_gate(
) -> Result<(), Box<dyn std::error::Error>> {
    let (mut client_stream, mut server_stream) = UnixStream::pair()?;
    let (resize_tx, resize_rx) = mpsc::channel();
    let geometry = TerminalGeometry::new(120, 40).with_pixels(TerminalPixels::new(1440, 960));
    resize_tx.send(geometry)?;

    drain_resize_events(&mut client_stream, &resize_rx, true)?;

    let mut frame = [0_u8; 128];
    let bytes_read = server_stream.read(&mut frame)?;
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&frame[..bytes_read]);
    assert_eq!(
        decoder.next_message()?,
        Some(AttachMessage::ResizeGeometry(geometry))
    );
    Ok(())
}

#[test]
fn output_loop_errors_when_server_hangs_up_before_attach_stop(
) -> Result<(), Box<dyn std::error::Error>> {
    let (client_stream, server_stream) = UnixStream::pair()?;
    let output = Vec::new();
    let closed = Arc::new(AtomicBool::new(false));
    let locked = Arc::new(AttachLockState::default());
    let (action_tx, _action_rx) = mpsc::channel();
    let screen_tracker = AttachScreenTracker::default();

    drop(server_stream);
    let error = output_loop(
        client_stream,
        Vec::new(),
        output,
        closed,
        locked,
        screen_tracker,
        action_tx,
    )
    .expect_err("hangup before attach-stop should be treated as an error");

    assert!(
        error
            .to_string()
            .contains("attach stream closed before attach-stop sequence"),
        "unexpected output-loop error: {error}"
    );
    Ok(())
}

#[test]
fn output_loop_treats_literal_exit_banners_as_pane_data() -> Result<(), Box<dyn std::error::Error>>
{
    let (client_stream, mut server_stream) = UnixStream::pair()?;
    let output = SharedOutput::default();
    let captured = output.clone();
    let closed = Arc::new(AtomicBool::new(false));
    let locked = Arc::new(AttachLockState::default());
    let (action_tx, _action_rx) = mpsc::channel();
    let screen_tracker = AttachScreenTracker::default();
    let output_thread = std::thread::spawn(move || {
        output_loop(
            client_stream,
            Vec::new(),
            output,
            closed,
            locked,
            screen_tracker,
            action_tx,
        )
    });

    for bytes in [
        b"[exited]\r\n".as_slice(),
        b"[detached (from session ordinary-pane-output)]\r\n",
        b"still-attached\n",
    ] {
        server_stream.write_all(&encode_attach_message(&AttachMessage::Data(
            bytes.to_vec(),
        ))?)?;
    }
    std::thread::sleep(Duration::from_millis(50));
    assert!(
        !output_thread.is_finished(),
        "banner-shaped pane output must not terminate attach"
    );

    server_stream.write_all(&encode_attach_message(&AttachMessage::Data(
        attach_stop_payload(),
    ))?)?;
    server_stream.shutdown(std::net::Shutdown::Write)?;
    output_thread
        .join()
        .map_err(|_| "output loop thread panicked")??;

    let rendered = captured.into_bytes()?;
    assert!(rendered
        .windows(b"[exited]\r\n".len())
        .any(|part| part == b"[exited]\r\n"));
    assert!(rendered
        .windows(b"still-attached\n".len())
        .any(|part| part == b"still-attached\n"));
    Ok(())
}

#[test]
fn output_loop_flushes_pending_render_before_accepting_next_frame(
) -> Result<(), Box<dyn std::error::Error>> {
    let (client_stream, mut server_stream) = UnixStream::pair()?;
    let output = SharedOutput::default();
    let captured = output.clone();
    let closed = Arc::new(AtomicBool::new(false));
    let locked = Arc::new(AttachLockState::default());
    let (action_tx, _action_rx) = mpsc::channel();
    let screen_tracker = AttachScreenTracker::default();

    server_stream.write_all(&encode_attach_message(&AttachMessage::Render(
        b"render-one\n".to_vec(),
    ))?)?;
    server_stream.write_all(&encode_attach_message(&AttachMessage::Render(
        b"render-two\n".to_vec(),
    ))?)?;
    server_stream.write_all(&encode_attach_message(&AttachMessage::Data(
        attach_stop_payload(),
    ))?)?;
    server_stream.shutdown(std::net::Shutdown::Write)?;

    output_loop(
        client_stream,
        Vec::new(),
        output,
        closed,
        locked,
        screen_tracker,
        action_tx,
    )?;

    let rendered = String::from_utf8(captured.into_bytes()?)?;
    assert!(
        rendered.contains("render-one\n"),
        "render deltas must be flushed before accepting later render frames: {rendered:?}"
    );
    assert!(
        rendered.contains("render-two\n"),
        "latest render frame was not flushed: {rendered:?}"
    );
    Ok(())
}

#[test]
fn output_loop_replaces_pending_render_after_first_paint() -> Result<(), Box<dyn std::error::Error>>
{
    let (client_stream, mut server_stream) = UnixStream::pair()?;
    let output = SharedOutput::default();
    let captured = output.clone();
    let closed = Arc::new(AtomicBool::new(false));
    let locked = Arc::new(AttachLockState::default());
    let (action_tx, _action_rx) = mpsc::channel();
    let screen_tracker = AttachScreenTracker::default();

    server_stream.write_all(&encode_attach_message(&AttachMessage::Render(
        b"initial\n".to_vec(),
    ))?)?;
    server_stream.write_all(&encode_attach_message(&AttachMessage::Render(
        b"stale\n".to_vec(),
    ))?)?;
    server_stream.write_all(&encode_attach_message(&AttachMessage::Render(
        b"latest\n".to_vec(),
    ))?)?;
    server_stream.write_all(&encode_attach_message(&AttachMessage::Data(
        attach_stop_payload(),
    ))?)?;
    server_stream.shutdown(std::net::Shutdown::Write)?;

    output_loop(
        client_stream,
        Vec::new(),
        output,
        closed,
        locked,
        screen_tracker,
        action_tx,
    )?;

    let rendered = String::from_utf8(captured.into_bytes()?)?;
    assert!(rendered.contains("initial\n"), "{rendered:?}");
    assert!(!rendered.contains("stale\n"), "{rendered:?}");
    assert!(rendered.contains("latest\n"), "{rendered:?}");
    Ok(())
}

#[test]
fn output_loop_waits_briefly_to_coalesce_render_frames() -> Result<(), Box<dyn std::error::Error>> {
    let (client_stream, mut server_stream) = UnixStream::pair()?;
    let output = SharedOutput::default();
    let captured = output.clone();
    let closed = Arc::new(AtomicBool::new(false));
    let locked = Arc::new(AttachLockState::default());
    let (action_tx, _action_rx) = mpsc::channel();
    let screen_tracker = AttachScreenTracker::default();

    let output_thread = std::thread::spawn(move || {
        output_loop(
            client_stream,
            Vec::new(),
            output,
            closed,
            locked,
            screen_tracker,
            action_tx,
        )
    });

    server_stream.write_all(&encode_attach_message(&AttachMessage::Render(
        b"initial\n".to_vec(),
    ))?)?;
    let mut replacement_burst = Vec::new();
    replacement_burst.extend(encode_attach_message(&AttachMessage::Render(
        b"stale\n".to_vec(),
    ))?);
    replacement_burst.extend(encode_attach_message(&AttachMessage::Render(
        b"latest\n".to_vec(),
    ))?);
    server_stream.write_all(&replacement_burst)?;
    server_stream.write_all(&encode_attach_message(&AttachMessage::Data(
        attach_stop_payload(),
    ))?)?;
    server_stream.shutdown(std::net::Shutdown::Write)?;
    output_thread
        .join()
        .map_err(|_| "output loop thread panicked")??;

    let rendered = String::from_utf8(captured.into_bytes()?)?;
    assert!(rendered.contains("initial\n"), "{rendered:?}");
    assert!(!rendered.contains("stale\n"), "{rendered:?}");
    assert!(rendered.contains("latest\n"), "{rendered:?}");
    Ok(())
}

#[test]
fn output_loop_flushes_render_before_idle_gap_when_stream_stays_busy(
) -> Result<(), Box<dyn std::error::Error>> {
    let (client_stream, mut server_stream) = UnixStream::pair()?;
    let output = SharedOutput::default();
    let captured = output.clone();
    let closed = Arc::new(AtomicBool::new(false));
    let locked = Arc::new(AttachLockState::default());
    let (action_tx, _action_rx) = mpsc::channel();
    let screen_tracker = AttachScreenTracker::default();

    let output_thread = std::thread::spawn(move || {
        output_loop(
            client_stream,
            Vec::new(),
            output,
            closed,
            locked,
            screen_tracker,
            action_tx,
        )
    });

    for index in 0..8 {
        server_stream.write_all(&encode_attach_message(&AttachMessage::Render(
            format!("render-{index}\n").into_bytes(),
        ))?)?;
        std::thread::sleep(Duration::from_millis(20));
    }

    let deadline = Instant::now() + Duration::from_secs(1);
    while captured.snapshot()?.is_empty() {
        assert!(
            Instant::now() < deadline,
            "render output was not flushed while the stream stayed busy"
        );
        std::thread::sleep(Duration::from_millis(10));
    }

    server_stream.write_all(&encode_attach_message(&AttachMessage::Data(
        attach_stop_payload(),
    ))?)?;
    server_stream.shutdown(std::net::Shutdown::Write)?;
    output_thread
        .join()
        .map_err(|_| "output loop thread panicked")??;

    let rendered = String::from_utf8(captured.into_bytes()?)?;
    assert!(rendered.contains("render-"), "{rendered:?}");
    Ok(())
}

#[test]
fn output_loop_keeps_original_render_deadline_across_replacements(
) -> Result<(), Box<dyn std::error::Error>> {
    let (client_stream, mut server_stream) = UnixStream::pair()?;
    let output = SharedOutput::default();
    let captured = output.clone();
    let closed = Arc::new(AtomicBool::new(false));
    let locked = Arc::new(AttachLockState::default());
    let (action_tx, _action_rx) = mpsc::channel();
    let screen_tracker = AttachScreenTracker::default();
    let writer_done = Arc::new(AtomicBool::new(false));
    let writer_done_for_thread = Arc::clone(&writer_done);

    let output_thread = std::thread::spawn(move || {
        output_loop(
            client_stream,
            Vec::new(),
            output,
            closed,
            locked,
            screen_tracker,
            action_tx,
        )
    });

    let writer_thread = std::thread::spawn(move || -> Result<(), io::Error> {
        let initial = encode_attach_message(&AttachMessage::Render(b"initial\n".to_vec()))
            .expect("initial render frame should encode");
        server_stream.write_all(&initial)?;
        for index in 0..100 {
            let frame = encode_attach_message(&AttachMessage::Render(
                format!("render-{index}\n").into_bytes(),
            ))
            .expect("render frame should encode");
            server_stream.write_all(&frame)?;
            std::thread::sleep(Duration::from_millis(2));
        }
        writer_done_for_thread.store(true, Ordering::SeqCst);
        let detach = encode_attach_message(&AttachMessage::Data(attach_stop_payload()))
            .expect("detach frame should encode");
        server_stream.write_all(&detach)?;
        server_stream.shutdown(std::net::Shutdown::Write)
    });

    let deadline = Instant::now() + Duration::from_secs(1);
    loop {
        let rendered = String::from_utf8(captured.snapshot()?)?;
        if rendered.contains("render-") {
            break;
        }
        assert!(
            !writer_done.load(Ordering::SeqCst),
            "render output was not flushed before the writer went idle"
        );
        assert!(
            Instant::now() < deadline,
            "render output was not flushed while replacements kept arriving"
        );
        std::thread::sleep(Duration::from_millis(5));
    }

    writer_thread
        .join()
        .map_err(|_| "writer thread panicked")??;
    output_thread
        .join()
        .map_err(|_| "output loop thread panicked")??;
    Ok(())
}

#[derive(Clone, Default)]
struct SharedOutput {
    bytes: Arc<Mutex<Vec<u8>>>,
}

impl SharedOutput {
    fn snapshot(&self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        Ok(self
            .bytes
            .lock()
            .map_err(|_| "shared output lock poisoned")?
            .clone())
    }

    fn into_bytes(self) -> Result<Vec<u8>, Box<dyn std::error::Error>> {
        self.snapshot()
    }
}

impl Write for SharedOutput {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.bytes
            .lock()
            .map_err(|_| std::io::Error::other("shared output lock poisoned"))?
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

struct PausedInput {
    readiness: UnixStream,
    entered: mpsc::Sender<()>,
    release: mpsc::Receiver<()>,
    pause_next: bool,
}

impl Read for PausedInput {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.pause_next {
            self.pause_next = false;
            self.entered
                .send(())
                .map_err(|_| io::Error::other("input-read observer closed"))?;
            self.release
                .recv()
                .map_err(|_| io::Error::other("input-read release closed"))?;
        }
        self.readiness.read(buffer)
    }
}

impl AsFd for PausedInput {
    fn as_fd(&self) -> BorrowedFd<'_> {
        self.readiness.as_fd()
    }
}

#[test]
fn raw_terminal_flush_pending_input_discards_mouse_sequence_tails(
) -> Result<(), Box<dyn std::error::Error>> {
    let pair = PtyPair::open_with_size(rmux_pty::TerminalSize::new(80, 24))?;
    let (master, slave) = pair.into_split();
    let mut master = File::from(master.into_owned_fd()?);
    let terminal = File::from(slave.into_owned_fd());
    let raw_terminal = RawTerminal::from_fd(&terminal)?;

    master.write_all(b"\x1b[<0;13;1m")?;
    master.flush()?;

    let timeout = Timespec {
        tv_sec: 0,
        tv_nsec: 0,
    };
    let mut before = [PollFd::new(&terminal, PollFlags::IN)];
    assert_eq!(poll(&mut before, Some(&timeout))?, 1);

    raw_terminal.flush_pending_input()?;

    let mut after = [PollFd::new(&terminal, PollFlags::IN)];
    assert_eq!(poll(&mut after, Some(&timeout))?, 0);
    Ok(())
}

#[test]
fn attach_with_terminal_flushes_stale_mouse_input_before_forwarding_keys(
) -> Result<(), Box<dyn std::error::Error>> {
    let pair = PtyPair::open_with_size(rmux_pty::TerminalSize::new(80, 24))?;
    let (master, slave) = pair.into_split();
    let mut master = File::from(master.into_owned_fd()?);
    let terminal = File::from(slave.into_owned_fd());
    let input = terminal.try_clone()?;
    let output = Vec::new();
    let (client_stream, mut server_stream) = UnixStream::pair()?;
    server_stream.set_read_timeout(Some(Duration::from_secs(2)))?;

    // Simulate stale SGR mouse bytes left behind by a previous attach.
    master.write_all(b"\x1b[<0;13;1m")?;
    master.flush()?;

    let attach_thread =
        std::thread::spawn(move || attach_with_terminal(client_stream, &terminal, input, output));

    std::thread::sleep(Duration::from_millis(100));
    master.write_all(b"a")?;
    master.flush()?;

    let mut decoder = AttachFrameDecoder::new();
    let mut saw_resize = false;
    let mut saw_input = false;
    let mut frame = [0_u8; 128];
    while !saw_input {
        let bytes_read = server_stream.read(&mut frame)?;
        decoder.push_bytes(&frame[..bytes_read]);
        while let Some(message) = decoder.next_message()? {
            match message {
                AttachMessage::Resize(TerminalSize { cols: 80, rows: 24 }) => saw_resize = true,
                AttachMessage::Data(bytes) => {
                    assert_eq!(bytes, b"a");
                    saw_input = true;
                }
                other => panic!("unexpected attach message: {other:?}"),
            }
        }
    }

    assert!(saw_resize, "attach should report the initial terminal size");

    server_stream.write_all(&encode_attach_message(&AttachMessage::Data(
        b"\x1b[?1049l".to_vec(),
    ))?)?;
    server_stream.flush()?;
    drop(server_stream);
    attach_thread
        .join()
        .map_err(|_| "attach thread panicked")??;
    Ok(())
}

#[test]
fn fallback_attach_stop_sequence_disables_mouse_and_exits_alt_screen() {
    let stop = fallback_attach_stop_sequence("xterm-256color");
    let expected_alt_exit = alternate_screen_exit_sequence("xterm-256color");
    assert_contains(&stop, b"\x1b[?1000l");
    assert_contains(&stop, b"\x1b[?1002l");
    assert_contains(&stop, b"\x1b[?1003l");
    assert_contains(&stop, b"\x1b[?1005l");
    assert_contains(&stop, b"\x1b[?1006l");
    assert!(
        stop.windows(expected_alt_exit.len())
            .any(|window| window == expected_alt_exit),
        "fallback stop should leave the alternate screen"
    );
}

#[test]
fn structured_shell_commands_use_server_resolved_shell_and_cwd(
) -> Result<(), Box<dyn std::error::Error>> {
    if std::env::var(CONTROLLING_TTY_CASE_ENV).as_deref() != Ok(STRUCTURED_SHELL_TTY_CASE) {
        return run_test_with_controlling_tty(
            "attach::tests::structured_shell_commands_use_server_resolved_shell_and_cwd",
            STRUCTURED_SHELL_TTY_CASE,
        );
    }

    let nonce = SystemTime::now().duration_since(UNIX_EPOCH)?.as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rmux-client-shell-context-{}-{nonce}",
        std::process::id()
    ));
    let cwd = root.join("cwd");
    std::fs::create_dir_all(&cwd)?;
    let shell = root.join("shell-wrapper");
    std::fs::write(
        &shell,
        b"#!/bin/sh\nprintf wrapper > wrapper.txt\nexec /bin/sh \"$@\"\n",
    )?;
    let mut permissions = std::fs::metadata(&shell)?.permissions();
    permissions.set_mode(0o700);
    std::fs::set_permissions(&shell, permissions)?;

    let raw = RawTerminal::from_fd(&io::stdin())?;
    let command = AttachShellCommand::new(
        "printf command > command.txt".to_owned(),
        shell.to_string_lossy().into_owned(),
        cwd.to_string_lossy().into_owned(),
    );
    raw.run_lock_shell_command(&command)?;
    drop(raw);

    assert_eq!(std::fs::read_to_string(cwd.join("wrapper.txt"))?, "wrapper");
    assert_eq!(std::fs::read_to_string(cwd.join("command.txt"))?, "command");
    std::fs::remove_dir_all(root)?;
    Ok(())
}

#[test]
fn attach_with_terminal_restores_mouse_off_after_protocol_error(
) -> Result<(), Box<dyn std::error::Error>> {
    let pair = PtyPair::open_with_size(rmux_pty::TerminalSize::new(80, 24))?;
    let (master, slave) = pair.into_split();
    let mut master = File::from(master.into_owned_fd()?);
    let terminal = File::from(slave.into_owned_fd());
    let _slave_keepalive = terminal.try_clone()?;
    let input = terminal.try_clone()?;
    let output = terminal.try_clone()?;
    let (client_stream, mut server_stream) = UnixStream::pair()?;

    let attach_thread =
        std::thread::spawn(move || attach_with_terminal(client_stream, &terminal, input, output));

    let start = encode_attach_message(&AttachMessage::Data(
        b"\x1b[?1006h\x1b[?1002h\x1b[?1000h\x1b[?1049h".to_vec(),
    ))?;
    server_stream.write_all(&start)?;
    server_stream.flush()?;
    server_stream.write_all(&[255])?;
    server_stream.flush()?;
    drop(server_stream);

    let attach_result = attach_thread.join().map_err(|_| "attach thread panicked")?;
    assert!(
        attach_result.is_err(),
        "invalid server frame should fail the attach client"
    );

    let timeout = Timespec {
        tv_sec: 0,
        tv_nsec: 50_000_000,
    };
    let mut collected = Vec::new();
    let mut buffer = [0_u8; 512];
    loop {
        let mut fds = [PollFd::new(&master, PollFlags::IN)];
        if poll(&mut fds, Some(&timeout))? == 0 {
            break;
        }
        let bytes_read = match master.read(&mut buffer) {
            Ok(bytes_read) => bytes_read,
            Err(error) if error.raw_os_error() == Some(5) => break,
            Err(error) => return Err(Box::new(error)),
        };
        if bytes_read == 0 {
            break;
        }
        collected.extend_from_slice(&buffer[..bytes_read]);
    }

    assert_contains(&collected, b"\x1b[?1000l");
    assert_contains(&collected, b"\x1b[?1002l");
    assert_contains(&collected, b"\x1b[?1003l");
    assert_contains(&collected, b"\x1b[?1005l");
    assert_contains(&collected, b"\x1b[?1006l");
    assert!(
        collected
            .windows(b"\x1b[0m\x1b[H\x1b[2J".len())
            .any(|window| window == b"\x1b[0m\x1b[H\x1b[2J"),
        "client fallback should redraw a clean terminal frame on protocol errors"
    );
    Ok(())
}

fn assert_contains(haystack: &[u8], needle: &[u8]) {
    assert!(
        haystack
            .windows(needle.len())
            .any(|window| window == needle),
        "expected {needle:?} in {haystack:?}"
    );
}
