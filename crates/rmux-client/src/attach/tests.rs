use std::fs::File;
use std::io::{Read, Write};
use std::os::fd::AsFd;
use std::os::unix::net::UnixStream;
use std::sync::atomic::AtomicBool;
use std::sync::mpsc;
use std::sync::Arc;
use std::time::Duration;

use rmux_core::alternate_screen_exit_sequence;
use rmux_proto::{encode_attach_message, AttachFrameDecoder, AttachMessage, AttachedKeystroke};
use rmux_pty::PtyPair;
use rustix::event::{poll, PollFd, PollFlags, Timespec};
use rustix::termios::{tcsetwinsize, Winsize};

use super::{
    attach_with_terminal, fallback_attach_stop_sequence, input_loop, output_loop,
    terminal_size_from_fd, AttachScreenTracker, RawTerminal, ResizeWatcher, SignalMaskGuard,
    TerminalSize,
};

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
        TerminalSize {
            cols: 120,
            rows: 40,
        }
    );

    drop(watcher);
    Ok(())
}

#[test]
fn input_loop_emits_typed_keystroke_frame() -> Result<(), Box<dyn std::error::Error>> {
    let (client_stream, mut server_stream) = UnixStream::pair()?;
    let (mut input_writer, input_reader) = UnixStream::pair()?;
    let (_resize_tx, resize_rx) = mpsc::channel();
    let closed = Arc::new(AtomicBool::new(false));
    let locked = Arc::new(AtomicBool::new(false));

    let input_thread = std::thread::spawn(move || {
        input_loop(client_stream, input_reader, resize_rx, closed, locked)
    });

    input_writer.write_all(b"a")?;
    input_writer.shutdown(std::net::Shutdown::Write)?;

    let mut frame = [0_u8; 64];
    let bytes_read = server_stream.read(&mut frame)?;
    let mut decoder = AttachFrameDecoder::new();
    decoder.push_bytes(&frame[..bytes_read]);

    assert_eq!(
        decoder.next_message()?,
        Some(AttachMessage::Keystroke(AttachedKeystroke::new(
            b"a".to_vec()
        )))
    );
    assert_ne!(
        decoder.next_message()?,
        Some(AttachMessage::Data(b"a".to_vec()))
    );

    input_thread
        .join()
        .map_err(|_| "input loop thread panicked")??;
    Ok(())
}

#[test]
fn output_loop_errors_when_server_hangs_up_before_attach_stop(
) -> Result<(), Box<dyn std::error::Error>> {
    let (client_stream, server_stream) = UnixStream::pair()?;
    let mut output = Vec::new();
    let closed = Arc::new(AtomicBool::new(false));
    let locked = Arc::new(AtomicBool::new(false));
    let (action_tx, _action_rx) = mpsc::channel();
    let screen_tracker = AttachScreenTracker::default();

    drop(server_stream);
    let error = output_loop(
        client_stream,
        &mut output,
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
fn raw_terminal_flush_pending_input_discards_mouse_sequence_tails(
) -> Result<(), Box<dyn std::error::Error>> {
    let pair = PtyPair::open_with_size(rmux_pty::TerminalSize::new(80, 24))?;
    let (master, slave) = pair.into_split();
    let mut master = File::from(master.into_owned_fd());
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
    let mut master = File::from(master.into_owned_fd());
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
    let mut saw_keystroke = false;
    let mut frame = [0_u8; 128];
    while !saw_keystroke {
        let bytes_read = server_stream.read(&mut frame)?;
        decoder.push_bytes(&frame[..bytes_read]);
        while let Some(message) = decoder.next_message()? {
            match message {
                AttachMessage::Resize(TerminalSize { cols: 80, rows: 24 }) => saw_resize = true,
                AttachMessage::Keystroke(keystroke) => {
                    assert_eq!(keystroke.bytes(), b"a");
                    saw_keystroke = true;
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
    assert!(stop
        .windows(b"\x1b[?1000l\x1b[?1002l\x1b[?1006l".len())
        .any(|window| window == b"\x1b[?1000l\x1b[?1002l\x1b[?1006l"));
    assert!(
        stop.windows(expected_alt_exit.len())
            .any(|window| window == expected_alt_exit),
        "fallback stop should leave the alternate screen"
    );
}

#[test]
fn attach_with_terminal_restores_mouse_off_after_protocol_error(
) -> Result<(), Box<dyn std::error::Error>> {
    let pair = PtyPair::open_with_size(rmux_pty::TerminalSize::new(80, 24))?;
    let (master, slave) = pair.into_split();
    let mut master = File::from(master.into_owned_fd());
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

    assert!(
        collected
            .windows(b"\x1b[?1000l\x1b[?1002l\x1b[?1006l".len())
            .any(|window| window == b"\x1b[?1000l\x1b[?1002l\x1b[?1006l"),
        "client fallback should disable mouse on protocol errors, collected={collected:?}"
    );
    assert!(
        collected
            .windows(b"\x1b[0m\x1b[H\x1b[2J".len())
            .any(|window| window == b"\x1b[0m\x1b[H\x1b[2J"),
        "client fallback should redraw a clean terminal frame on protocol errors"
    );
    Ok(())
}
