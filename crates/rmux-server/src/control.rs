#[cfg_attr(windows, allow(unused_imports))]
pub(crate) use crate::control_mode::ControlModeUpgrade;
#[cfg(any(unix, windows))]
use crate::daemon::ShutdownHandle;
#[cfg(any(unix, windows))]
use crate::handler::{
    with_control_queue_eof_cancellation, with_control_queue_identity, ControlClientIdentity,
    ControlQueueDrainLease, ControlQueueEofCancellation, RequestHandler,
};
#[cfg(any(unix, windows))]
use rmux_ipc::LocalStream;
#[cfg(windows)]
use rmux_proto::CONTROL_STDIN_EOF_MARKER;
#[cfg(any(unix, windows))]
use rmux_proto::{format_exit_line, format_guard_line, ControlGuardKind};
use rmux_proto::{ControlMode, SessionName};
#[cfg(any(unix, windows))]
use std::collections::{HashMap, HashSet, VecDeque};
#[cfg(any(unix, windows))]
use std::io;
#[cfg(any(unix, windows))]
use std::pin::Pin;
#[cfg(any(unix, windows))]
use std::sync::atomic::{AtomicBool, Ordering};
#[cfg(any(unix, windows))]
use std::sync::Arc;
#[cfg(all(test, unix))]
use std::sync::{Mutex as StdMutex, OnceLock};
#[cfg(any(unix, windows))]
use std::time::{Duration, SystemTime, UNIX_EPOCH};
#[cfg(any(unix, windows))]
use tokio::io::{AsyncReadExt, AsyncWriteExt, WriteHalf};
#[cfg(all(test, unix))]
use tokio::sync::Notify;
#[cfg(any(unix, windows))]
use tokio::sync::{mpsc, watch};
#[cfg(any(unix, windows))]
use tokio::task::JoinHandle;

#[path = "control/output_queue.rs"]
mod output_queue;
#[cfg(any(unix, windows))]
use output_queue::{ensure_control_newline, flush_output_queue, ControlOutputQueue};

#[path = "control/command_numbering.rs"]
mod command_numbering;
#[cfg(any(unix, windows))]
use command_numbering::ControlCommandNumbering;

#[path = "control/command_validation.rs"]
mod command_validation;
#[cfg(any(unix, windows))]
use command_validation::validate_control_command_arguments;

#[path = "control/subscriptions.rs"]
mod subscriptions;
#[cfg(any(unix, windows))]
use subscriptions::{
    drain_ready_pane_events, handle_pane_event, refresh_subscriptions, PaneEvent, PaneSubscription,
    PaneSubscriptionStart,
};

#[cfg(any(unix, windows))]
const MAX_DEFERRED_CONTROL_NOTIFICATIONS: usize = 1024;
#[cfg(any(unix, windows))]
const MAX_DEFERRED_CONTROL_NOTIFICATION_BYTES: usize = 4 * 1024 * 1024;
#[cfg(any(unix, windows))]
const CONTROL_PANE_EVENT_CAPACITY: usize = 256;
#[cfg(any(unix, windows))]
pub(crate) const CONTROL_SERVER_EVENT_CAPACITY: usize = 256;
#[cfg(any(unix, windows))]
const MAX_INITIAL_CONTROL_COMMANDS: usize = 1024;
#[cfg(any(unix, windows))]
const MAX_CONTROL_LINE_BYTES: usize = 1024 * 1024;
#[cfg(any(unix, windows))]
const MAX_QUEUED_CONTROL_LINES: usize = 1024;
#[cfg(any(unix, windows))]
const MAX_QUEUED_CONTROL_BYTES: usize = 4 * 1024 * 1024;

#[cfg(any(unix, windows))]
// Keep one bounded, global post-EOF window for already accepted frames to
// publish fast replies before falling back to the detached finite drain.
const CONTROL_EOF_GRACE: Duration = Duration::from_millis(250);

#[cfg(any(unix, windows))]
type ControlEofTransition = Pin<Box<tokio::time::Sleep>>;

#[cfg(any(unix, windows))]
fn arm_control_eof_transition(transition: &mut Option<ControlEofTransition>) {
    if transition.is_none() {
        *transition = Some(Box::pin(tokio::time::sleep(CONTROL_EOF_GRACE)));
    }
}

#[cfg(any(unix, windows))]
async fn wait_for_control_eof_transition(transition: &mut Option<ControlEofTransition>) {
    match transition.as_mut() {
        Some(transition) => transition.as_mut().await,
        None => std::future::pending().await,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub(crate) struct ControlClientFlags {
    pub(crate) pause_after_millis: Option<u64>,
    pub(crate) no_output: bool,
    pub(crate) wait_exit: bool,
}

impl ControlClientFlags {
    #[must_use]
    pub(crate) const fn uses_extended_output(self) -> bool {
        self.pause_after_millis.is_some()
    }
}

#[derive(Debug, Clone)]
pub(crate) enum ControlServerEvent {
    SessionChanged(Option<SessionName>),
    SessionChangedAt {
        session_name: SessionName,
        pane_sequences: Vec<(u32, u64)>,
    },
    Refresh,
    Notification(String),
    Exit(Option<String>),
}

#[derive(Debug, Clone)]
pub(crate) struct ControlCommandResult {
    pub(crate) stdout: Vec<u8>,
    pub(crate) error: Option<rmux_proto::RmuxError>,
    pub(crate) source_file_error: Option<rmux_proto::RmuxError>,
    pub(crate) execution_error: Option<rmux_proto::RmuxError>,
    pub(crate) exit_status: Option<i32>,
    pub(crate) server_shutdown_started: bool,
}

#[derive(Debug)]
#[cfg(any(unix, windows))]
pub(crate) struct ControlLifecycle {
    pub(crate) closing: Arc<AtomicBool>,
    pub(crate) shutdown_handle: ShutdownHandle,
}

#[derive(Debug)]
#[cfg(any(unix, windows))]
pub(crate) struct ControlUpgradeInput {
    buffered_bytes: Vec<u8>,
    initial_command_count: usize,
    mode: ControlMode,
}

#[cfg(any(unix, windows))]
impl ControlUpgradeInput {
    #[cfg(test)]
    pub(crate) fn new(buffered_bytes: Vec<u8>, initial_command_count: usize) -> Self {
        Self::with_mode(buffered_bytes, initial_command_count, ControlMode::Plain)
    }

    pub(crate) fn with_mode(
        buffered_bytes: Vec<u8>,
        initial_command_count: usize,
        mode: ControlMode,
    ) -> Self {
        Self {
            buffered_bytes,
            initial_command_count,
            mode,
        }
    }
}

#[cfg(any(unix, windows))]
pub(crate) async fn forward_control(
    stream: LocalStream,
    handler: Arc<RequestHandler>,
    control_identity: ControlClientIdentity,
    upgrade_input: ControlUpgradeInput,
    shutdown: watch::Receiver<()>,
    server_events: mpsc::Receiver<ControlServerEvent>,
    lifecycle: ControlLifecycle,
) -> io::Result<()> {
    with_control_queue_identity(
        control_identity,
        forward_control_inner(
            stream,
            handler,
            control_identity,
            upgrade_input,
            shutdown,
            server_events,
            lifecycle,
        ),
    )
    .await
}

#[cfg(any(unix, windows))]
async fn forward_control_inner(
    stream: LocalStream,
    handler: Arc<RequestHandler>,
    control_identity: ControlClientIdentity,
    upgrade_input: ControlUpgradeInput,
    mut shutdown: watch::Receiver<()>,
    mut server_events: mpsc::Receiver<ControlServerEvent>,
    lifecycle: ControlLifecycle,
) -> io::Result<()> {
    let requester_pid = control_identity.requester_pid();
    let control_id = control_identity.control_id();
    let ControlUpgradeInput {
        buffered_bytes: initial_socket_bytes,
        initial_command_count,
        mode,
    } = upgrade_input;
    if initial_command_count > MAX_INITIAL_CONTROL_COMMANDS {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "too many initial control-mode commands: {initial_command_count} (maximum {MAX_INITIAL_CONTROL_COMMANDS})"
            ),
        ));
    }
    let (pane_event_tx, mut pane_event_rx) = mpsc::channel(CONTROL_PANE_EVENT_CAPACITY);
    let (mut read_half, mut write_half) = tokio::io::split(stream);
    let mut input_buffer = Vec::new();
    let mut queued_lines = VecDeque::new();
    let mut queued_input_bytes = 0_usize;
    append_control_input(
        &mut input_buffer,
        &mut queued_lines,
        &mut queued_input_bytes,
        &initial_socket_bytes,
    )?;
    let mut read_buffer = [0_u8; 8192];
    while queued_lines.len() < initial_command_count {
        let bytes_read = read_half.read(&mut read_buffer).await?;
        if bytes_read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "control-mode stream closed after {} of {initial_command_count} initial commands",
                    queued_lines.len()
                ),
            ));
        }
        append_control_input(
            &mut input_buffer,
            &mut queued_lines,
            &mut queued_input_bytes,
            &read_buffer[..bytes_read],
        )?;
    }
    let mut output_queue = ControlOutputQueue::default();
    let mut subscriptions = HashMap::new();
    let mut paused_panes = HashSet::new();
    let mut deferred_server_events = DeferredServerEvents::default();
    let mut input_closed = false;
    let mut eof_queue_lease = None;
    let mut eof_transition = None;
    #[cfg(windows)]
    if consume_control_eof_marker(
        &mut input_buffer,
        &mut queued_lines,
        &mut queued_input_bytes,
    ) {
        input_closed = true;
        acquire_control_eof_queue_lease(&mut eof_queue_lease, &handler, control_identity).await;
        arm_control_eof_transition(&mut eof_transition);
    }
    let mut session_name: Option<SessionName> = handler.control_session_name(requester_pid).await;
    let mut flags: ControlClientFlags = handler
        .control_client_flags(requester_pid)
        .await
        .unwrap_or_default();
    let mut current_command: Option<ActiveControlCommand> = None;
    let mut command_numbering = if initial_command_count == 0 {
        let initial_timestamp = unix_epoch_seconds();
        output_queue.enqueue_line(
            format_guard_line(ControlGuardKind::Begin, initial_timestamp, 1, 0).into_bytes(),
            false,
        );
        output_queue.enqueue_line(
            format_guard_line(ControlGuardKind::End, initial_timestamp, 1, 0).into_bytes(),
            false,
        );
        ControlCommandNumbering::after_initial_ack()
    } else {
        ControlCommandNumbering::with_initial_commands(initial_command_count)
    };

    refresh_subscriptions(
        &handler,
        control_identity,
        session_name.as_ref(),
        &mut subscriptions,
        pane_event_tx.clone(),
        PaneSubscriptionStart::Now,
    )
    .await;
    while let Ok(event) = server_events.try_recv() {
        let mut event_context = ServerEventContext {
            handler: &handler,
            control_identity,
            requester_pid,
            session_name: &mut session_name,
            subscriptions: &mut subscriptions,
            pane_event_tx: pane_event_tx.clone(),
            pane_event_rx: &mut pane_event_rx,
            output_queue: &mut output_queue,
            write_half: &mut write_half,
            paused_panes: &mut paused_panes,
            flags: &mut flags,
            deferred: &mut deferred_server_events,
        };
        if handle_server_event(event, &mut event_context, false).await? {
            return Ok(());
        }
    }

    loop {
        if current_command.is_none() {
            let reconcile_ready_control_attach =
                input_closed && mode.is_control_control() && session_name.is_none();
            let mut event_context = ServerEventContext {
                handler: &handler,
                control_identity,
                requester_pid,
                session_name: &mut session_name,
                subscriptions: &mut subscriptions,
                pane_event_tx: pane_event_tx.clone(),
                pane_event_rx: &mut pane_event_rx,
                output_queue: &mut output_queue,
                write_half: &mut write_half,
                paused_panes: &mut paused_panes,
                flags: &mut flags,
                deferred: &mut deferred_server_events,
            };
            if flush_deferred_server_events(&mut event_context).await? {
                return Ok(());
            }
            if reconcile_ready_control_attach {
                // A successful attach publishes its session-change event before
                // the command task returns. When EOF and both futures are ready,
                // the biased select below deliberately observes command
                // completion first. Reconcile the bounded snapshot of events
                // that were already published before treating a still-unbound
                // -CC client as terminal. New arrivals are left to the select,
                // so a continuous producer cannot starve EOF.
                let ready_server_event_count = server_events.len();
                for _ in 0..ready_server_event_count {
                    let Ok(event) = server_events.try_recv() else {
                        break;
                    };
                    if handle_server_event(event, &mut event_context, false).await? {
                        return Ok(());
                    }
                }
            }
        }
        if lifecycle.closing.load(Ordering::SeqCst) && current_command.is_none() {
            output_queue.enqueue_line(format_exit_line(None).into_bytes(), false);
            flush_output_queue(&mut output_queue, &mut write_half, flags, &mut paused_panes)
                .await?;
            return Ok(());
        }
        if input_closed
            && current_command.is_none()
            && queued_lines.is_empty()
            && !control_control_waits_for_attached_session(mode, session_name.as_ref())
        {
            // Any incomplete line remaining in input_buffer after EOF is
            // discarded, matching tmux's `evbuffer_readln` semantics. EOF
            // itself is promoted to a bare `%exit\n` so the control-mode
            // transcript is terminated by a guard-tuple-free exit line,
            // matching tmux's `server_client_control_mode` close path.
            output_queue.enqueue_line(format_exit_line(None).into_bytes(), false);
            flush_output_queue(&mut output_queue, &mut write_half, flags, &mut paused_panes)
                .await?;
            return Ok(());
        }

        while current_command.is_none() {
            let Some(line) = queued_lines.pop_front() else {
                break;
            };
            queued_input_bytes = queued_input_bytes.saturating_sub(line.len());
            #[cfg(windows)]
            if line == CONTROL_STDIN_EOF_MARKER {
                input_closed = true;
                acquire_control_eof_queue_lease(&mut eof_queue_lease, &handler, control_identity)
                    .await;
                arm_control_eof_transition(&mut eof_transition);
                input_buffer.clear();
                queued_lines.clear();
                queued_input_bytes = 0;
                break;
            }
            if line.is_empty() {
                output_queue.enqueue_line(format_exit_line(None).into_bytes(), false);
                flush_output_queue(&mut output_queue, &mut write_half, flags, &mut paused_panes)
                    .await?;
                return Ok(());
            }

            let timestamp = unix_epoch_seconds();
            let command_frame = command_numbering.next_frame();
            output_queue.enqueue_line(
                format_guard_line(
                    ControlGuardKind::Begin,
                    timestamp,
                    command_frame.number,
                    command_frame.guard_flag,
                )
                .into_bytes(),
                false,
            );
            flush_output_queue(&mut output_queue, &mut write_half, flags, &mut paused_panes)
                .await?;

            match handler
                .parse_control_commands(&line)
                .await
                .and_then(validate_control_command_arguments)
            {
                Ok(commands) => {
                    let handler = Arc::clone(&handler);
                    let eof_cancellation = ControlQueueEofCancellation::new(control_identity);
                    if input_closed {
                        eof_cancellation.cancel_for_eof();
                    }
                    let task_eof_cancellation = eof_cancellation.clone();
                    current_command = Some(ActiveControlCommand {
                        timestamp,
                        command_number: command_frame.number,
                        guard_flag: command_frame.guard_flag,
                        eof_cancellation,
                        task: Some(tokio::spawn(async move {
                            with_control_queue_eof_cancellation(
                                task_eof_cancellation,
                                handler.execute_control_commands_identity(
                                    requester_pid,
                                    control_id,
                                    commands,
                                ),
                            )
                            .await
                        })),
                    });
                    if input_closed {
                        arm_control_eof_transition(&mut eof_transition);
                    }
                }
                Err(error) => {
                    output_queue.enqueue_stdout(format!("parse error: {error}").into_bytes());
                    if drain_ready_pane_events(
                        &mut pane_event_rx,
                        &mut output_queue,
                        &mut paused_panes,
                        flags,
                    )? {
                        flush_output_queue(
                            &mut output_queue,
                            &mut write_half,
                            flags,
                            &mut paused_panes,
                        )
                        .await?;
                        return Ok(());
                    }
                    output_queue.enqueue_line(
                        format_guard_line(
                            ControlGuardKind::Error,
                            timestamp,
                            command_frame.number,
                            command_frame.guard_flag,
                        )
                        .into_bytes(),
                        false,
                    );
                    flush_output_queue(
                        &mut output_queue,
                        &mut write_half,
                        flags,
                        &mut paused_panes,
                    )
                    .await?;
                }
            }
        }
        if input_closed
            && current_command.is_none()
            && queued_lines.is_empty()
            && !control_control_waits_for_attached_session(mode, session_name.as_ref())
        {
            output_queue.enqueue_line(format_exit_line(None).into_bytes(), false);
            flush_output_queue(&mut output_queue, &mut write_half, flags, &mut paused_panes)
                .await?;
            return Ok(());
        }

        tokio::select! {
            biased;

            result = shutdown.changed() => {
                let _ = result;
                output_queue.enqueue_line(
                    format_exit_line(Some("server shutting down")).into_bytes(),
                    false,
                );
                flush_output_queue(&mut output_queue, &mut write_half, flags, &mut paused_panes).await?;
                return Ok(());
            }
            result = read_half.read(&mut read_buffer), if !input_closed => {
                let bytes_read = result?;
                if bytes_read == 0 {
                    input_closed = true;
                    acquire_control_eof_queue_lease(
                        &mut eof_queue_lease,
                        &handler,
                        control_identity,
                    )
                    .await;
                    if let Some(command) = current_command.as_ref() {
                        command.eof_cancellation.cancel_for_eof();
                    }
                    arm_control_eof_transition(&mut eof_transition);
                } else {
                    append_control_input(
                        &mut input_buffer,
                        &mut queued_lines,
                        &mut queued_input_bytes,
                        &read_buffer[..bytes_read],
                    )?;
                    #[cfg(windows)]
                    if consume_control_eof_marker(
                        &mut input_buffer,
                        &mut queued_lines,
                        &mut queued_input_bytes,
                    ) {
                        input_closed = true;
                        acquire_control_eof_queue_lease(
                            &mut eof_queue_lease,
                            &handler,
                            control_identity,
                        )
                        .await;
                        if let Some(command) = current_command.as_ref() {
                            command.eof_cancellation.cancel_for_eof();
                        }
                        arm_control_eof_transition(&mut eof_transition);
                    }
                }
            }
            result = async {
                match current_command.as_mut() {
                    Some(command) => match command.task.as_mut() {
                        Some(task) => Some(task.await),
                        None => std::future::pending().await,
                    },
                    None => std::future::pending().await,
                }
            } => {
                let Some(task_result) = result else {
                    continue;
                };
                let Some(command) = current_command.take() else {
                    continue;
                };
                let result = task_result
                    .map_err(|error| io::Error::other(format!("control command task failed: {error}")))?;
                if !result.stdout.is_empty() {
                    output_queue.enqueue_stdout(result.stdout);
                }
                if drain_ready_pane_events(
                    &mut pane_event_rx,
                    &mut output_queue,
                    &mut paused_panes,
                    flags,
                )? {
                    flush_output_queue(
                        &mut output_queue,
                        &mut write_half,
                        flags,
                        &mut paused_panes,
                    )
                    .await?;
                    return Ok(());
                }
                let server_shutdown_started = result.server_shutdown_started;
                match result.error {
                    Some(error) => {
                        output_queue.enqueue_stdout(error.to_string().into_bytes());
                        output_queue.enqueue_line(
                            format_guard_line(
                                ControlGuardKind::Error,
                                command.timestamp,
                                command.command_number,
                                command.guard_flag,
                            )
                            .into_bytes(),
                            false,
                        );
                    }
                    None => {
                        output_queue.enqueue_line(
                            format_guard_line(
                                ControlGuardKind::End,
                                command.timestamp,
                                command.command_number,
                                command.guard_flag,
                            )
                            .into_bytes(),
                            false,
                        );
                    }
                }
                flush_output_queue(&mut output_queue, &mut write_half, flags, &mut paused_panes).await?;
                if server_shutdown_started || handler.request_shutdown_if_pending() {
                    lifecycle.shutdown_handle.request_shutdown();
                }
                if input_closed {
                    arm_control_eof_transition(&mut eof_transition);
                }
            }
            _ = wait_for_control_eof_transition(&mut eof_transition),
                if input_closed && current_command.is_some() && !mode.is_control_control() =>
            {
                // The transport can close as soon as the active guard has
                // been terminated, but every complete frame accepted before
                // EOF still belongs to this exact control registration. Keep
                // its queue lease until those finite frames have drained.
                let deferred_exit_reason = deferred_server_events.exit_reason.take();
                let lease = match eof_queue_lease {
                    Some(lease) => lease,
                    None => handler.begin_control_queue_drain(control_identity).await,
                };
                // Exit is terminal even when its event was consumed while the
                // active command was still running. A missing exact
                // registration or an already-closing lifecycle is terminal
                // too: the Exit producer may have detached and removed this
                // control client even when its event channel was full.
                // Acquire the lease when possible so a same-PID replacement
                // still waits for the active frame, but never turn its absence
                // into a product error after the transport has reached EOF.
                let stop_after_active = deferred_exit_reason.is_some()
                    || lease == ControlQueueDrainLease::Unavailable
                    || lifecycle.closing.load(Ordering::SeqCst);
                let closed = close_active_control_command_on_eof(
                    &mut current_command,
                    &mut output_queue,
                    &mut write_half,
                    flags,
                    &mut paused_panes,
                    deferred_exit_reason
                        .as_ref()
                        .and_then(Option::as_deref),
                )
                .await;
                #[cfg(windows)]
                {
                    // Tokio's named-pipe `AsyncWrite::shutdown` only flushes;
                    // it does not half-close the duplex handle. Release both
                    // split halves after the terminal `%exit` is flushed so
                    // Windows clients can use transport EOF as the unambiguous
                    // completion signal while the finite queue drains below.
                    drop(read_half);
                    drop(write_half);
                }
                let mut drain_context = EofDrainContext {
                    server_events: &mut server_events,
                    events_open: true,
                    handler: &handler,
                    control_identity,
                    shutdown: &mut shutdown,
                    shutdown_handle: &lifecycle.shutdown_handle,
                };
                let drain_result = drain_control_queue_after_eof(
                    closed.task,
                    &mut queued_lines,
                    &mut queued_input_bytes,
                    stop_after_active,
                    &mut drain_context,
                )
                .await;
                drain_result?;
                return closed.transport_result;
            }
            Some(event) = server_events.recv() => {
                let mut event_context = ServerEventContext {
                    handler: &handler,
                    control_identity,
                    requester_pid,
                    session_name: &mut session_name,
                    subscriptions: &mut subscriptions,
                    pane_event_tx: pane_event_tx.clone(),
                    pane_event_rx: &mut pane_event_rx,
                    output_queue: &mut output_queue,
                    write_half: &mut write_half,
                    paused_panes: &mut paused_panes,
                    flags: &mut flags,
                    deferred: &mut deferred_server_events,
                };
                if handle_server_event(event, &mut event_context, current_command.is_some())
                .await?
                {
                    return Ok(());
                }
            }
            Some(event) = pane_event_rx.recv() => {
                let lagged =
                    handle_pane_event(event, &mut output_queue, &mut paused_panes, flags)?;
                flush_output_queue(&mut output_queue, &mut write_half, flags, &mut paused_panes).await?;
                if lagged {
                    return Ok(());
                }
            }
        }
    }
}

#[cfg(any(unix, windows))]
fn control_control_waits_for_attached_session(
    mode: ControlMode,
    session_name: Option<&SessionName>,
) -> bool {
    mode.is_control_control() && session_name.is_some()
}

#[cfg(any(unix, windows))]
async fn acquire_control_eof_queue_lease(
    eof_queue_lease: &mut Option<ControlQueueDrainLease>,
    handler: &RequestHandler,
    control_identity: ControlClientIdentity,
) {
    if eof_queue_lease.is_some() {
        return;
    }
    *eof_queue_lease = Some(handler.begin_control_queue_drain(control_identity).await);
    #[cfg(all(test, unix))]
    pause_after_control_eof_queue_lease(control_identity).await;
}

#[cfg(all(test, unix))]
struct ControlEofQueueLeasePause {
    identity: ControlClientIdentity,
    reached: Notify,
    release: Notify,
}

#[cfg(all(test, unix))]
static CONTROL_EOF_QUEUE_LEASE_PAUSE: OnceLock<StdMutex<Option<Arc<ControlEofQueueLeasePause>>>> =
    OnceLock::new();

#[cfg(all(test, unix))]
fn install_control_eof_queue_lease_pause(
    identity: ControlClientIdentity,
) -> Arc<ControlEofQueueLeasePause> {
    let pause = Arc::new(ControlEofQueueLeasePause {
        identity,
        reached: Notify::new(),
        release: Notify::new(),
    });
    let mut installed = CONTROL_EOF_QUEUE_LEASE_PAUSE
        .get_or_init(|| StdMutex::new(None))
        .lock()
        .expect("control EOF queue lease pause");
    assert!(
        installed.replace(Arc::clone(&pause)).is_none(),
        "only one control EOF queue lease pause may be installed"
    );
    pause
}

#[cfg(all(test, unix))]
async fn pause_after_control_eof_queue_lease(identity: ControlClientIdentity) {
    let pause = {
        let mut installed = CONTROL_EOF_QUEUE_LEASE_PAUSE
            .get_or_init(|| StdMutex::new(None))
            .lock()
            .expect("control EOF queue lease pause");
        installed
            .as_ref()
            .is_some_and(|pause| pause.identity == identity)
            .then(|| installed.take())
            .flatten()
    };
    if let Some(pause) = pause {
        pause.reached.notify_one();
        pause.release.notified().await;
    }
}

#[cfg(any(unix, windows))]
struct ClosedControlCommand {
    task: Option<JoinHandle<ControlCommandResult>>,
    transport_result: io::Result<()>,
}

#[cfg(any(unix, windows))]
async fn close_active_control_command_on_eof(
    current_command: &mut Option<ActiveControlCommand>,
    output_queue: &mut ControlOutputQueue,
    write_half: &mut WriteHalf<LocalStream>,
    flags: ControlClientFlags,
    paused_panes: &mut HashSet<u32>,
    exit_reason: Option<&str>,
) -> ClosedControlCommand {
    let Some(mut command) = current_command.take() else {
        return ClosedControlCommand {
            task: None,
            transport_result: Ok(()),
        };
    };
    output_queue.enqueue_line(
        format_guard_line(
            ControlGuardKind::End,
            command.timestamp,
            command.command_number,
            command.guard_flag,
        )
        .into_bytes(),
        false,
    );
    output_queue.enqueue_line(format_exit_line(exit_reason).into_bytes(), false);
    command.eof_cancellation.cancel_for_eof();
    let task = command.task.take();
    drop(command);
    let transport_result = async {
        flush_output_queue(output_queue, write_half, flags, paused_panes).await?;
        write_half.shutdown().await
    }
    .await;
    ClosedControlCommand {
        task,
        transport_result,
    }
}

#[cfg(any(unix, windows))]
struct EofDrainContext<'a> {
    server_events: &'a mut mpsc::Receiver<ControlServerEvent>,
    events_open: bool,
    handler: &'a Arc<RequestHandler>,
    control_identity: ControlClientIdentity,
    shutdown: &'a mut watch::Receiver<()>,
    shutdown_handle: &'a ShutdownHandle,
}

#[cfg(any(unix, windows))]
async fn drain_control_queue_after_eof(
    active_task: Option<JoinHandle<ControlCommandResult>>,
    queued_lines: &mut VecDeque<String>,
    queued_input_bytes: &mut usize,
    stop_after_active: bool,
    context: &mut EofDrainContext<'_>,
) -> io::Result<()> {
    let active_requested_stop = match active_task {
        Some(task) => drain_control_command_after_eof(task, context).await?,
        None => context.shutdown.has_changed().unwrap_or(true),
    };
    let mut stop = stop_after_active || active_requested_stop;

    while !stop {
        if context.shutdown.has_changed().unwrap_or(true) {
            break;
        }
        let Some(line) = queued_lines.pop_front() else {
            break;
        };
        *queued_input_bytes = queued_input_bytes.saturating_sub(line.len());
        #[cfg(windows)]
        if line == CONTROL_STDIN_EOF_MARKER {
            break;
        }
        if line.is_empty() {
            break;
        }

        let commands = match context
            .handler
            .parse_control_commands(&line)
            .await
            .and_then(validate_control_command_arguments)
        {
            Ok(commands) => commands,
            // The transport has already received its terminal %exit, so a
            // queued parse error has nowhere to be reported. It behaves like
            // the live queue: this frame fails without suppressing later
            // complete frames.
            Err(_) => continue,
        };
        let handler_for_command = Arc::clone(context.handler);
        let control_identity = context.control_identity;
        let eof_cancellation = ControlQueueEofCancellation::new(control_identity);
        eof_cancellation.cancel_for_eof();
        let task = tokio::spawn(async move {
            with_control_queue_eof_cancellation(
                eof_cancellation,
                handler_for_command.execute_control_commands_identity(
                    control_identity.requester_pid(),
                    control_identity.control_id(),
                    commands,
                ),
            )
            .await
        });
        stop = drain_control_command_after_eof(task, context).await?;
    }

    queued_lines.clear();
    *queued_input_bytes = 0;
    Ok(())
}

#[cfg(any(unix, windows))]
async fn drain_control_command_after_eof(
    mut task: JoinHandle<ControlCommandResult>,
    context: &mut EofDrainContext<'_>,
) -> io::Result<bool> {
    let mut exit_received = false;
    let result = loop {
        tokio::select! {
            biased;

            result = &mut task => break result,
            result = context.shutdown.changed() => {
                let _ = result;
                task.abort();
                let _ = task.await;
                return Ok(true);
            }
            event = context.server_events.recv(), if context.events_open => {
                match event {
                    Some(ControlServerEvent::Exit(_)) => exit_received = true,
                    Some(_) => {}
                    None => context.events_open = false,
                }
            }
        }
    };
    let result = result
        .map_err(|error| io::Error::other(format!("control command task failed: {error}")))?;
    // The command result and its terminal Exit event can become ready in the
    // same scheduler turn. Explicitly drain events emitted before task
    // completion so a later queued frame cannot run after client exit.
    while context.events_open {
        match context.server_events.try_recv() {
            Ok(ControlServerEvent::Exit(_)) => exit_received = true,
            Ok(_) => {}
            Err(mpsc::error::TryRecvError::Empty) => break,
            Err(mpsc::error::TryRecvError::Disconnected) => {
                context.events_open = false;
            }
        }
    }
    if context.shutdown.has_changed().unwrap_or(true) {
        return Ok(true);
    }
    let shutdown_requested =
        result.server_shutdown_started || context.handler.request_shutdown_if_pending();
    if shutdown_requested {
        context.shutdown_handle.request_shutdown();
    }
    // A terminal action in the command can mark the client closing and remove
    // its exact registration when the event channel is full. In that case no
    // Exit reaches this receiver, so revalidate after the task joins rather
    // than relying on the closing snapshot taken before the active frame.
    let registration_closed = !context
        .handler
        .control_queue_identity_is_open(context.control_identity)
        .await;
    Ok(exit_received || shutdown_requested || registration_closed)
}

#[cfg(any(unix, windows))]
async fn handle_server_event(
    event: ControlServerEvent,
    context: &mut ServerEventContext<'_>,
    command_active: bool,
) -> io::Result<bool> {
    match event {
        ControlServerEvent::SessionChanged(next_session) => {
            if command_active {
                context.deferred.defer_session_change(next_session, None);
                return Ok(false);
            }
            *context.session_name = next_session;
            refresh_subscriptions(
                context.handler,
                context.control_identity,
                context.session_name.as_ref(),
                context.subscriptions,
                context.pane_event_tx.clone(),
                PaneSubscriptionStart::Oldest,
            )
            .await;
        }
        ControlServerEvent::SessionChangedAt {
            session_name: next_session,
            pane_sequences,
        } => {
            if command_active {
                context
                    .deferred
                    .defer_session_change(Some(next_session), Some(pane_sequences));
                return Ok(false);
            }
            *context.session_name = Some(next_session);
            refresh_subscriptions(
                context.handler,
                context.control_identity,
                context.session_name.as_ref(),
                context.subscriptions,
                context.pane_event_tx.clone(),
                PaneSubscriptionStart::Sequences(&pane_sequences),
            )
            .await;
        }
        ControlServerEvent::Refresh => {
            refresh_subscriptions(
                context.handler,
                context.control_identity,
                context.session_name.as_ref(),
                context.subscriptions,
                context.pane_event_tx.clone(),
                PaneSubscriptionStart::Oldest,
            )
            .await;
        }
        ControlServerEvent::Notification(line) => {
            if command_active || context.deferred.exit_reason.is_some() {
                context.deferred.defer_notification(line);
                return Ok(false);
            }
            if drain_ready_pane_events(
                context.pane_event_rx,
                context.output_queue,
                context.paused_panes,
                *context.flags,
            )? {
                flush_output_queue(
                    context.output_queue,
                    context.write_half,
                    *context.flags,
                    context.paused_panes,
                )
                .await?;
                return Ok(true);
            }
            context
                .output_queue
                .enqueue_line(ensure_control_newline(line.into_bytes()), false);
            flush_output_queue(
                context.output_queue,
                context.write_half,
                *context.flags,
                context.paused_panes,
            )
            .await?;
        }
        ControlServerEvent::Exit(reason) => {
            if command_active || !context.deferred.notifications.is_empty() {
                context.deferred.exit_reason = Some(reason);
                return Ok(false);
            }
            context
                .output_queue
                .enqueue_line(format_exit_line(reason.as_deref()).into_bytes(), false);
            flush_output_queue(
                context.output_queue,
                context.write_half,
                *context.flags,
                context.paused_panes,
            )
            .await?;
            return Ok(true);
        }
    }

    *context.flags = context
        .handler
        .control_client_flags(context.requester_pid)
        .await
        .unwrap_or(*context.flags);
    Ok(false)
}

#[cfg(any(unix, windows))]
async fn flush_deferred_server_events(context: &mut ServerEventContext<'_>) -> io::Result<bool> {
    while let Some(line) = context.deferred.pop_notification() {
        if handle_server_event(ControlServerEvent::Notification(line), context, false).await? {
            return Ok(true);
        }
    }

    if let Some(DeferredSessionChange {
        session_name,
        pane_sequences,
    }) = context.deferred.session_change.take()
    {
        let event = match (session_name, pane_sequences) {
            (Some(session_name), Some(pane_sequences)) => ControlServerEvent::SessionChangedAt {
                session_name,
                pane_sequences,
            },
            (next_session, None) => ControlServerEvent::SessionChanged(next_session),
            (None, Some(_)) => unreachable!("pane cursors require a control session"),
        };
        if handle_server_event(event, context, false).await? {
            return Ok(true);
        }
    }

    if let Some(reason) = context.deferred.exit_reason.take() {
        return handle_server_event(ControlServerEvent::Exit(reason), context, false).await;
    }

    Ok(false)
}

#[cfg(any(unix, windows))]
struct ServerEventContext<'a> {
    handler: &'a RequestHandler,
    control_identity: ControlClientIdentity,
    requester_pid: u32,
    session_name: &'a mut Option<SessionName>,
    subscriptions: &'a mut HashMap<u32, PaneSubscription>,
    pane_event_tx: mpsc::Sender<PaneEvent>,
    pane_event_rx: &'a mut mpsc::Receiver<PaneEvent>,
    output_queue: &'a mut ControlOutputQueue,
    write_half: &'a mut WriteHalf<LocalStream>,
    paused_panes: &'a mut HashSet<u32>,
    flags: &'a mut ControlClientFlags,
    deferred: &'a mut DeferredServerEvents,
}

#[derive(Debug, Default)]
#[cfg(any(unix, windows))]
struct DeferredServerEvents {
    notifications: VecDeque<String>,
    notification_bytes: usize,
    session_change: Option<DeferredSessionChange>,
    exit_reason: Option<Option<String>>,
}

#[derive(Debug)]
#[cfg(any(unix, windows))]
struct DeferredSessionChange {
    session_name: Option<SessionName>,
    pane_sequences: Option<Vec<(u32, u64)>>,
}

#[cfg(any(unix, windows))]
impl DeferredServerEvents {
    fn defer_notification(&mut self, line: String) {
        if self.exit_reason.is_some() {
            return;
        }
        let next_bytes = self.notification_bytes.saturating_add(line.len());
        if self.notifications.len() >= MAX_DEFERRED_CONTROL_NOTIFICATIONS
            || next_bytes > MAX_DEFERRED_CONTROL_NOTIFICATION_BYTES
        {
            self.notifications.clear();
            self.notification_bytes = 0;
            self.exit_reason = Some(Some("control notification queue exceeded".to_owned()));
            return;
        }
        self.notification_bytes = next_bytes;
        self.notifications.push_back(line);
    }

    fn pop_notification(&mut self) -> Option<String> {
        let line = self.notifications.pop_front()?;
        self.notification_bytes = self.notification_bytes.saturating_sub(line.len());
        Some(line)
    }

    fn defer_session_change(
        &mut self,
        next_session: Option<SessionName>,
        pane_sequences: Option<Vec<(u32, u64)>>,
    ) {
        if self.exit_reason.is_some() {
            return;
        }
        self.session_change = Some(DeferredSessionChange {
            session_name: next_session,
            pane_sequences,
        });
    }
}

#[derive(Debug)]
#[cfg(any(unix, windows))]
struct ActiveControlCommand {
    timestamp: i64,
    command_number: u64,
    guard_flag: u8,
    eof_cancellation: ControlQueueEofCancellation,
    task: Option<JoinHandle<ControlCommandResult>>,
}

#[cfg(any(unix, windows))]
impl Drop for ActiveControlCommand {
    fn drop(&mut self) {
        if let Some(task) = self.task.as_ref() {
            if !task.is_finished() {
                task.abort();
            }
        }
    }
}

#[cfg(any(unix, windows))]
fn extract_complete_control_lines(buffer: &mut Vec<u8>) -> Vec<String> {
    let mut lines = Vec::new();

    while let Some(position) = buffer.iter().position(|byte| *byte == b'\n') {
        let mut line = buffer.drain(..=position).collect::<Vec<_>>();
        if matches!(line.last(), Some(b'\n')) {
            let _ = line.pop();
        }
        if matches!(line.last(), Some(b'\r')) {
            let _ = line.pop();
        }
        lines.push(String::from_utf8_lossy(&line).into_owned());
    }

    lines
}

#[cfg(any(unix, windows))]
fn append_control_input(
    input_buffer: &mut Vec<u8>,
    queued_lines: &mut VecDeque<String>,
    queued_input_bytes: &mut usize,
    bytes: &[u8],
) -> io::Result<()> {
    input_buffer.extend_from_slice(bytes);
    let lines = extract_complete_control_lines(input_buffer);
    if input_buffer.len() > MAX_CONTROL_LINE_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("control input line exceeds {MAX_CONTROL_LINE_BYTES} bytes without a newline"),
        ));
    }
    for line in lines {
        if line.len() > MAX_CONTROL_LINE_BYTES {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!("control input line exceeds {MAX_CONTROL_LINE_BYTES} bytes"),
            ));
        }
        let next_line_count = queued_lines.len().saturating_add(1);
        let next_byte_count = queued_input_bytes.saturating_add(line.len());
        if next_line_count > MAX_QUEUED_CONTROL_LINES || next_byte_count > MAX_QUEUED_CONTROL_BYTES
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                format!(
                    "control input queue exceeds {MAX_QUEUED_CONTROL_LINES} lines or {MAX_QUEUED_CONTROL_BYTES} bytes"
                ),
            ));
        }
        *queued_input_bytes = next_byte_count;
        queued_lines.push_back(line);
    }
    Ok(())
}

#[cfg(windows)]
fn consume_control_eof_marker(
    input_buffer: &mut Vec<u8>,
    queued_lines: &mut VecDeque<String>,
    queued_input_bytes: &mut usize,
) -> bool {
    let Some(marker_index) = queued_lines.iter().position(|line| {
        line == CONTROL_STDIN_EOF_MARKER || line.ends_with(CONTROL_STDIN_EOF_MARKER)
    }) else {
        return false;
    };

    // The Windows client writes this private terminal marker immediately
    // before closing its named-pipe writer. Observe it even while a blocking
    // command is active; waiting for the normal command-dequeue path would
    // deadlock because wait-for cancellation itself depends on input_closed.
    queued_lines.truncate(marker_index);
    *queued_input_bytes = queued_lines.iter().map(String::len).sum();
    input_buffer.clear();
    true
}

#[cfg(any(unix, windows))]
fn unix_epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(all(test, any(unix, windows)))]
mod deferred_tests {
    use super::{
        DeferredServerEvents, MAX_DEFERRED_CONTROL_NOTIFICATIONS,
        MAX_DEFERRED_CONTROL_NOTIFICATION_BYTES,
    };

    #[test]
    fn deferred_control_notifications_are_bounded() {
        let mut deferred = DeferredServerEvents::default();

        for index in 0..MAX_DEFERRED_CONTROL_NOTIFICATIONS {
            deferred.defer_notification(format!("%message {index}"));
        }

        assert_eq!(
            deferred.notifications.len(),
            MAX_DEFERRED_CONTROL_NOTIFICATIONS
        );
        assert!(deferred.exit_reason.is_none());

        deferred.defer_notification("%message overflow".to_owned());

        assert!(deferred.notifications.is_empty());
        assert_eq!(
            deferred.exit_reason,
            Some(Some("control notification queue exceeded".to_owned()))
        );

        deferred.defer_notification("%message after-overflow".to_owned());
        assert!(deferred.notifications.is_empty());
    }

    #[test]
    fn deferred_control_notification_bytes_are_bounded_and_accounted_on_pop() {
        let mut deferred = DeferredServerEvents::default();
        let first = "x".repeat(MAX_DEFERRED_CONTROL_NOTIFICATION_BYTES / 2);
        let second = "y".repeat(MAX_DEFERRED_CONTROL_NOTIFICATION_BYTES / 2);
        deferred.defer_notification(first.clone());
        deferred.defer_notification(second);
        assert_eq!(
            deferred.notification_bytes,
            MAX_DEFERRED_CONTROL_NOTIFICATION_BYTES
        );

        assert_eq!(deferred.pop_notification(), Some(first));
        assert_eq!(
            deferred.notification_bytes,
            MAX_DEFERRED_CONTROL_NOTIFICATION_BYTES / 2
        );

        deferred.defer_notification("z".repeat(MAX_DEFERRED_CONTROL_NOTIFICATION_BYTES / 2 + 1));
        assert!(deferred.notifications.is_empty());
        assert_eq!(deferred.notification_bytes, 0);
        assert!(deferred.exit_reason.is_some());
    }
}

#[cfg(all(test, windows))]
mod windows_eof_marker_tests {
    use std::collections::VecDeque;

    use super::{
        append_control_input, arm_control_eof_transition, consume_control_eof_marker,
        wait_for_control_eof_transition, CONTROL_EOF_GRACE,
    };
    use rmux_proto::CONTROL_STDIN_EOF_MARKER;

    #[test]
    fn windows_eof_marker_discards_an_incomplete_command_prefix() {
        let mut input_buffer = Vec::new();
        let mut queued_lines = VecDeque::new();
        let mut queued_bytes = 0;
        let input = format!("display-message -p must-not-run{CONTROL_STDIN_EOF_MARKER}\n");
        append_control_input(
            &mut input_buffer,
            &mut queued_lines,
            &mut queued_bytes,
            input.as_bytes(),
        )
        .expect("private EOF marker input parses");

        assert!(consume_control_eof_marker(
            &mut input_buffer,
            &mut queued_lines,
            &mut queued_bytes,
        ));
        assert!(queued_lines.is_empty());
        assert_eq!(queued_bytes, 0);
        assert!(input_buffer.is_empty());
    }

    #[tokio::test]
    async fn windows_eof_marker_uses_the_global_persistent_deadline() {
        let mut input_buffer = Vec::new();
        let mut queued_lines = VecDeque::new();
        let mut queued_bytes = 0;
        append_control_input(
            &mut input_buffer,
            &mut queued_lines,
            &mut queued_bytes,
            format!("wait-for marker{CONTROL_STDIN_EOF_MARKER}\n").as_bytes(),
        )
        .expect("private EOF marker input parses");
        assert!(consume_control_eof_marker(
            &mut input_buffer,
            &mut queued_lines,
            &mut queued_bytes,
        ));

        let mut transition = None;
        arm_control_eof_transition(&mut transition);
        let initial_deadline = transition
            .as_ref()
            .expect("marker deadline is armed")
            .deadline();
        arm_control_eof_transition(&mut transition);
        assert_eq!(
            transition
                .as_ref()
                .expect("marker deadline stays armed")
                .deadline(),
            initial_deadline,
            "another post-marker frame must not extend the global budget"
        );
        assert!(
            tokio::time::timeout(
                std::time::Duration::from_millis(50),
                wait_for_control_eof_transition(&mut transition),
            )
            .await
            .is_err(),
            "the marker deadline leaves a bounded grace for fast output"
        );
        assert!(
            tokio::time::timeout(
                CONTROL_EOF_GRACE + std::time::Duration::from_millis(100),
                wait_for_control_eof_transition(&mut transition),
            )
            .await
            .is_ok(),
            "the marker deadline still expires within its global budget"
        );
    }
}

#[cfg(all(test, unix))]
#[path = "control/tests.rs"]
mod tests;
