//! Client-side attach actions: lock, suspend and detach requests relayed from
//! the output thread and executed against the managed terminal.

use std::io;
use std::os::unix::net::UnixStream;
use std::sync::{mpsc, Arc};
use std::thread;
use std::time::{Duration, Instant};

use rmux_proto::{AttachMessage, AttachShellCommand, RmuxError};
use rustix::process::{kill_process, Signal};

use crate::attach_lock_state::AttachLockState;
use crate::ClientError;

use super::screen::{AttachScreenTracker, AttachStopGeneration};
use super::terminal::{current_process_pid, RawTerminal};
use super::{join_attach_thread, termination, write_attach_message};

const TERMINATION_POLL_INTERVAL: Duration = Duration::from_millis(100);
const TERMINATION_OUTPUT_SHUTDOWN_TIMEOUT: Duration = Duration::from_millis(500);

pub(super) fn wait_for_output_thread(
    output_thread: thread::JoinHandle<std::result::Result<(), ClientError>>,
    raw_terminal: Option<&RawTerminal>,
    lock_stream: &mut UnixStream,
    locked: &Arc<AttachLockState>,
    screen_tracker: &AttachScreenTracker,
    event_rx: mpsc::Receiver<ClientAttachEvent>,
    termination_signals_enabled: bool,
) -> std::result::Result<std::result::Result<(), ClientError>, ClientError> {
    if termination_signals_enabled {
        loop {
            if termination::was_requested() {
                wait_for_backpressured_output_shutdown(&output_thread, raw_terminal, &event_rx);
                return Err(ClientError::Io(termination::interruption_error()));
            }
            match event_rx.recv_timeout(TERMINATION_POLL_INTERVAL) {
                Ok(ClientAttachEvent::Action(action)) => {
                    handle_attach_action(
                        raw_terminal,
                        lock_stream,
                        locked,
                        screen_tracker,
                        action,
                    )?;
                }
                Ok(ClientAttachEvent::OutputDone) | Err(mpsc::RecvTimeoutError::Disconnected) => {
                    break;
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
        }
    } else {
        while let Ok(ClientAttachEvent::Action(action)) = event_rx.recv() {
            handle_attach_action(raw_terminal, lock_stream, locked, screen_tracker, action)?;
        }
    }

    while let Ok(event) = event_rx.try_recv() {
        match event {
            ClientAttachEvent::Action(action) => {
                handle_attach_action(raw_terminal, lock_stream, locked, screen_tracker, action)?;
            }
            ClientAttachEvent::OutputDone => {}
        }
    }

    join_attach_thread(output_thread)
}

fn wait_for_backpressured_output_shutdown(
    output_thread: &thread::JoinHandle<std::result::Result<(), ClientError>>,
    raw_terminal: Option<&RawTerminal>,
    event_rx: &mpsc::Receiver<ClientAttachEvent>,
) {
    termination::interrupt_thread(output_thread);
    let _nonblocking_output =
        raw_terminal.and_then(|raw_terminal| raw_terminal.interrupt_output_writer().ok());
    let deadline = Instant::now() + TERMINATION_OUTPUT_SHUTDOWN_TIMEOUT;
    loop {
        let remaining = deadline.saturating_duration_since(Instant::now());
        if remaining.is_zero() {
            break;
        }
        match event_rx.recv_timeout(remaining) {
            Ok(ClientAttachEvent::OutputDone) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Ok(ClientAttachEvent::Action(_)) => {}
            Err(mpsc::RecvTimeoutError::Timeout) => break,
        }
    }
}

pub(super) fn send_attach_action(
    event_tx: &mpsc::Sender<ClientAttachEvent>,
    action: ClientAttachAction,
) -> std::result::Result<(), ClientError> {
    event_tx
        .send(ClientAttachEvent::Action(action))
        .map_err(|_| ClientError::Io(io::Error::other("attach event receiver closed")))
}

pub(super) fn handle_attach_action(
    raw_terminal: Option<&RawTerminal>,
    lock_stream: &mut UnixStream,
    locked: &Arc<AttachLockState>,
    screen_tracker: &AttachScreenTracker,
    action: ClientAttachAction,
) -> std::result::Result<(), ClientError> {
    match action {
        ClientAttachAction::Lock {
            command,
            stop_generation,
        } => {
            locked.wait_until_input_idle();
            let Some(raw_terminal) = raw_terminal else {
                locked.unlock();
                return Err(ClientError::Protocol(RmuxError::Decode(
                    "received unexpected lock request without a managed terminal".to_owned(),
                )));
            };
            let result = raw_terminal
                .run_lock_command(&command)
                .map_err(ClientError::from)
                .and_then(|()| write_attach_unlock(lock_stream, screen_tracker, stop_generation));
            locked.unlock();
            result
        }
        ClientAttachAction::LockShell {
            command,
            stop_generation,
        } => {
            locked.wait_until_input_idle();
            let Some(raw_terminal) = raw_terminal else {
                locked.unlock();
                return Err(ClientError::Protocol(RmuxError::Decode(
                    "received unexpected lock request without a managed terminal".to_owned(),
                )));
            };
            let result = raw_terminal
                .run_lock_shell_command(&command)
                .map_err(ClientError::from)
                .and_then(|()| write_attach_unlock(lock_stream, screen_tracker, stop_generation));
            locked.unlock();
            result
        }
        ClientAttachAction::Suspend { stop_generation } => {
            locked.wait_until_input_idle();
            let Some(raw_terminal) = raw_terminal else {
                locked.unlock();
                return Err(ClientError::Protocol(RmuxError::Decode(
                    "received unexpected suspend request without a managed terminal".to_owned(),
                )));
            };
            let result = raw_terminal
                .suspend_self()
                .map_err(ClientError::from)
                .and_then(|()| write_attach_unlock(lock_stream, screen_tracker, stop_generation));
            locked.unlock();
            result
        }
        ClientAttachAction::DetachKill => {
            if let Some(raw_terminal) = raw_terminal {
                raw_terminal.restore().map_err(ClientError::from)?;
            }
            kill_process(current_process_pid().map_err(ClientError::Io)?, Signal::HUP)
                .map_err(|error| ClientError::Io(error.into()))?;
            Ok(())
        }
        ClientAttachAction::DetachExec(command) => {
            let Some(raw_terminal) = raw_terminal else {
                return Err(ClientError::Protocol(RmuxError::Decode(
                    "received unexpected detach exec request without a managed terminal".to_owned(),
                )));
            };
            raw_terminal
                .run_detach_exec_command(&command)
                .map_err(ClientError::from)
        }
        ClientAttachAction::DetachExecShell(command) => {
            let Some(raw_terminal) = raw_terminal else {
                return Err(ClientError::Protocol(RmuxError::Decode(
                    "received unexpected detach exec request without a managed terminal".to_owned(),
                )));
            };
            raw_terminal
                .run_detach_exec_shell_command(&command)
                .map_err(ClientError::from)
        }
    }
}

pub(super) fn write_attach_unlock(
    stream: &mut UnixStream,
    screen_tracker: &AttachScreenTracker,
    stop_generation: Option<AttachStopGeneration>,
) -> std::result::Result<(), ClientError> {
    // Rearm only the stop published by this lock/suspend prelude. A newer
    // generation belongs to a concurrent detach or session exit and must stay
    // authoritative even when the local action completes later.
    let rearmed =
        stop_generation.is_some_and(|generation| screen_tracker.rearm_if_current(generation));
    if !rearmed && screen_tracker.was_stopped() {
        return Ok(());
    }

    match write_attach_message(stream, AttachMessage::Unlock) {
        Err(ClientError::Io(error))
            if screen_tracker.was_stopped()
                && matches!(
                    error.kind(),
                    io::ErrorKind::ConnectionReset | io::ErrorKind::BrokenPipe
                ) =>
        {
            Ok(())
        }
        result => result,
    }
}

#[derive(Debug)]
pub(super) enum ClientAttachAction {
    Lock {
        command: String,
        stop_generation: Option<AttachStopGeneration>,
    },
    LockShell {
        command: AttachShellCommand,
        stop_generation: Option<AttachStopGeneration>,
    },
    Suspend {
        stop_generation: Option<AttachStopGeneration>,
    },
    DetachKill,
    DetachExec(String),
    DetachExecShell(AttachShellCommand),
}

#[derive(Debug)]
pub(super) enum ClientAttachEvent {
    Action(ClientAttachAction),
    OutputDone,
}
