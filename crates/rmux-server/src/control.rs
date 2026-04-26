use std::collections::{HashMap, HashSet, VecDeque};
use std::io;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use rmux_ipc::LocalStream;
use rmux_proto::{
    format_exit_line, format_extended_output_line, format_guard_line, format_output_line,
    format_pause_line, ControlGuardKind, SessionName, CONTROL_BUFFER_HIGH,
};
use tokio::io::{AsyncReadExt, WriteHalf};
use tokio::sync::{broadcast, mpsc, watch};
use tokio::task::JoinHandle;

pub(crate) use crate::control_mode::ControlModeUpgrade;
use crate::daemon::ShutdownHandle;
use crate::handler::RequestHandler;

#[path = "control/output_queue.rs"]
mod output_queue;
use output_queue::{ensure_control_newline, flush_output_queue, ControlOutputQueue};

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
    Refresh,
    Notification(String),
    Exit(Option<String>),
}

#[derive(Debug, Clone)]
pub(crate) struct ControlCommandResult {
    pub(crate) stdout: Vec<u8>,
    pub(crate) error: Option<rmux_proto::RmuxError>,
}

#[derive(Debug)]
pub(crate) struct ControlLifecycle {
    pub(crate) closing: Arc<AtomicBool>,
    pub(crate) shutdown_handle: ShutdownHandle,
}

pub(crate) async fn forward_control(
    stream: LocalStream,
    handler: Arc<RequestHandler>,
    requester_pid: u32,
    initial_socket_bytes: Vec<u8>,
    mut shutdown: watch::Receiver<()>,
    mut server_events: mpsc::UnboundedReceiver<ControlServerEvent>,
    lifecycle: ControlLifecycle,
) -> io::Result<()> {
    let (pane_event_tx, mut pane_event_rx) = mpsc::unbounded_channel();
    let (mut read_half, mut write_half) = tokio::io::split(stream);
    let mut input_buffer = initial_socket_bytes;
    let mut queued_lines: VecDeque<String> =
        extract_complete_control_lines(&mut input_buffer).into();
    let mut output_queue = ControlOutputQueue::default();
    let mut subscriptions = HashMap::new();
    let mut paused_panes = HashSet::new();
    let mut deferred_server_events = DeferredServerEvents::default();
    let mut input_closed = false;
    let mut session_name: Option<SessionName> = handler.control_session_name(requester_pid).await;
    let mut flags: ControlClientFlags = handler
        .control_client_flags(requester_pid)
        .await
        .unwrap_or_default();
    let mut current_command: Option<ActiveControlCommand> = None;
    let mut next_command_number = 1_u64;

    refresh_subscriptions(
        &handler,
        session_name.as_ref(),
        &mut subscriptions,
        pane_event_tx.clone(),
    )
    .await;
    while let Ok(event) = server_events.try_recv() {
        let mut event_context = ServerEventContext {
            handler: &handler,
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
            let mut event_context = ServerEventContext {
                handler: &handler,
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
        }
        if lifecycle.closing.load(Ordering::SeqCst) && current_command.is_none() {
            output_queue.enqueue_line(format_exit_line(None).into_bytes(), false);
            flush_output_queue(&mut output_queue, &mut write_half, flags, &mut paused_panes)
                .await?;
            return Ok(());
        }
        if input_closed && current_command.is_none() && queued_lines.is_empty() {
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
            if line.is_empty() {
                output_queue.enqueue_line(format_exit_line(None).into_bytes(), false);
                flush_output_queue(&mut output_queue, &mut write_half, flags, &mut paused_panes)
                    .await?;
                return Ok(());
            }

            let timestamp = unix_epoch_seconds();
            let command_number = next_command_number;
            next_command_number = next_command_number.saturating_add(1);
            output_queue.enqueue_line(
                format_guard_line(ControlGuardKind::Begin, timestamp, command_number, 1)
                    .into_bytes(),
                false,
            );
            flush_output_queue(&mut output_queue, &mut write_half, flags, &mut paused_panes)
                .await?;

            match handler.parse_control_commands(&line).await {
                Ok(commands) => {
                    let handler = Arc::clone(&handler);
                    current_command = Some(ActiveControlCommand {
                        timestamp,
                        command_number,
                        task: tokio::spawn(async move {
                            handler
                                .execute_control_commands(requester_pid, commands)
                                .await
                        }),
                    });
                }
                Err(error) => {
                    output_queue.enqueue_stdout(format!("parse error: {error}").into_bytes());
                    drain_ready_pane_events(
                        &mut pane_event_rx,
                        &mut output_queue,
                        &mut paused_panes,
                        flags,
                    )?;
                    output_queue.enqueue_line(
                        format_guard_line(ControlGuardKind::Error, timestamp, command_number, 1)
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

        tokio::select! {
            result = shutdown.changed() => {
                let _ = result;
                output_queue.enqueue_line(
                    format_exit_line(Some("server shutting down")).into_bytes(),
                    false,
                );
                flush_output_queue(&mut output_queue, &mut write_half, flags, &mut paused_panes).await?;
                return Ok(());
            }
            result = read_half.read_buf(&mut input_buffer), if !input_closed => {
                let bytes_read = result?;
                if bytes_read == 0 {
                    input_closed = true;
                } else {
                    queued_lines.extend(extract_complete_control_lines(&mut input_buffer));
                }
            }
            Some(event) = server_events.recv() => {
                let mut event_context = ServerEventContext {
                    handler: &handler,
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
                handle_pane_event(event, &mut output_queue, &mut paused_panes, flags)?;
                flush_output_queue(&mut output_queue, &mut write_half, flags, &mut paused_panes).await?;
            }
            result = async {
                match current_command.as_mut() {
                    Some(command) => Some((&mut command.task).await),
                    None => std::future::pending().await,
                }
            } => {
                let Some(result) = result else {
                    continue;
                };
                let Some(command) = current_command.take() else {
                    continue;
                };
                let result = result
                    .map_err(|error| io::Error::other(format!("control command task failed: {error}")))?;
                if !result.stdout.is_empty() {
                    output_queue.enqueue_stdout(result.stdout);
                }
                drain_ready_pane_events(
                    &mut pane_event_rx,
                    &mut output_queue,
                    &mut paused_panes,
                    flags,
                )?;
                match result.error {
                    Some(error) => {
                        output_queue.enqueue_stdout(error.to_string().into_bytes());
                        output_queue.enqueue_line(
                            format_guard_line(
                                ControlGuardKind::Error,
                                command.timestamp,
                                command.command_number,
                                1,
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
                                1,
                            )
                            .into_bytes(),
                            false,
                        );
                    }
                }
                flush_output_queue(&mut output_queue, &mut write_half, flags, &mut paused_panes).await?;
                if handler.request_shutdown_if_pending() {
                    lifecycle.shutdown_handle.request_shutdown();
                }
            }
        }
    }
}

async fn handle_server_event(
    event: ControlServerEvent,
    context: &mut ServerEventContext<'_>,
    command_active: bool,
) -> io::Result<bool> {
    match event {
        ControlServerEvent::SessionChanged(next_session) => {
            *context.session_name = next_session;
            refresh_subscriptions(
                context.handler,
                context.session_name.as_ref(),
                context.subscriptions,
                context.pane_event_tx.clone(),
            )
            .await;
        }
        ControlServerEvent::Refresh => {
            refresh_subscriptions(
                context.handler,
                context.session_name.as_ref(),
                context.subscriptions,
                context.pane_event_tx.clone(),
            )
            .await;
        }
        ControlServerEvent::Notification(line) => {
            if command_active || context.deferred.exit_reason.is_some() {
                context.deferred.notifications.push_back(line);
                return Ok(false);
            }
            drain_ready_pane_events(
                context.pane_event_rx,
                context.output_queue,
                context.paused_panes,
                *context.flags,
            )?;
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

async fn flush_deferred_server_events(context: &mut ServerEventContext<'_>) -> io::Result<bool> {
    while let Some(line) = context.deferred.notifications.pop_front() {
        if handle_server_event(ControlServerEvent::Notification(line), context, false).await? {
            return Ok(true);
        }
    }

    if let Some(reason) = context.deferred.exit_reason.take() {
        return handle_server_event(ControlServerEvent::Exit(reason), context, false).await;
    }

    Ok(false)
}

struct ServerEventContext<'a> {
    handler: &'a RequestHandler,
    requester_pid: u32,
    session_name: &'a mut Option<SessionName>,
    subscriptions: &'a mut HashMap<u32, PaneSubscription>,
    pane_event_tx: mpsc::UnboundedSender<PaneEvent>,
    pane_event_rx: &'a mut mpsc::UnboundedReceiver<PaneEvent>,
    output_queue: &'a mut ControlOutputQueue,
    write_half: &'a mut WriteHalf<LocalStream>,
    paused_panes: &'a mut HashSet<u32>,
    flags: &'a mut ControlClientFlags,
    deferred: &'a mut DeferredServerEvents,
}

#[derive(Debug, Default)]
struct DeferredServerEvents {
    notifications: VecDeque<String>,
    exit_reason: Option<Option<String>>,
}

#[derive(Debug)]
struct ActiveControlCommand {
    timestamp: i64,
    command_number: u64,
    task: JoinHandle<ControlCommandResult>,
}

#[derive(Debug)]
enum PaneEvent {
    Data {
        pane_id: u32,
        bytes: Vec<u8>,
        received_at: Instant,
    },
    Lagged,
}

#[derive(Debug)]
struct PaneSubscription {
    stop_tx: tokio::sync::oneshot::Sender<()>,
}

async fn refresh_subscriptions(
    handler: &RequestHandler,
    session_name: Option<&SessionName>,
    subscriptions: &mut HashMap<u32, PaneSubscription>,
    pane_event_tx: mpsc::UnboundedSender<PaneEvent>,
) {
    let Some(session_name) = session_name else {
        subscriptions.clear();
        return;
    };
    let panes = handler
        .control_session_panes(session_name)
        .await
        .unwrap_or_default();
    let desired = panes
        .iter()
        .map(|(pane_id, _)| *pane_id)
        .collect::<HashSet<_>>();
    let existing = subscriptions.keys().copied().collect::<Vec<_>>();
    for pane_id in existing {
        if desired.contains(&pane_id) {
            continue;
        }
        if let Some(subscription) = subscriptions.remove(&pane_id) {
            let _ = subscription.stop_tx.send(());
        }
    }

    for (pane_id, sender) in panes {
        if subscriptions.contains_key(&pane_id) {
            continue;
        }
        let mut receiver = sender.subscribe();
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel();
        let pane_event_tx = pane_event_tx.clone();
        tokio::spawn(async move {
            loop {
                tokio::select! {
                    _ = &mut stop_rx => return,
                    result = receiver.recv() => {
                        match result {
                            Ok(bytes) => {
                                let _ = pane_event_tx.send(PaneEvent::Data {
                                    pane_id,
                                    bytes,
                                    received_at: Instant::now(),
                                });
                            }
                            Err(broadcast::error::RecvError::Lagged(_)) => {
                                let _ = pane_event_tx.send(PaneEvent::Lagged);
                                return;
                            }
                            Err(broadcast::error::RecvError::Closed) => return,
                        }
                    }
                }
            }
        });
        subscriptions.insert(pane_id, PaneSubscription { stop_tx });
    }
}

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

fn handle_pane_event(
    event: PaneEvent,
    output_queue: &mut ControlOutputQueue,
    paused_panes: &mut HashSet<u32>,
    flags: ControlClientFlags,
) -> io::Result<()> {
    if flags.no_output {
        return Ok(());
    }

    match event {
        PaneEvent::Data {
            pane_id,
            bytes,
            received_at,
        } => {
            if flags.uses_extended_output()
                && output_queue.buffered_bytes >= CONTROL_BUFFER_HIGH
                && paused_panes.insert(pane_id)
            {
                output_queue.enqueue_line(format_pause_line(pane_id).into_bytes(), false);
            }
            let age_ms = received_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
            let line = if flags.uses_extended_output() {
                format_extended_output_line(pane_id, age_ms, &bytes)
            } else {
                format_output_line(pane_id, &bytes)
            };
            output_queue.enqueue_line(line.into_bytes(), true);
        }
        PaneEvent::Lagged => {
            return Err(io::Error::other("too far behind"));
        }
    }

    Ok(())
}

fn drain_ready_pane_events(
    pane_event_rx: &mut mpsc::UnboundedReceiver<PaneEvent>,
    output_queue: &mut ControlOutputQueue,
    paused_panes: &mut HashSet<u32>,
    flags: ControlClientFlags,
) -> io::Result<()> {
    loop {
        match pane_event_rx.try_recv() {
            Ok(event) => handle_pane_event(event, output_queue, paused_panes, flags)?,
            Err(mpsc::error::TryRecvError::Empty) => return Ok(()),
            Err(mpsc::error::TryRecvError::Disconnected) => return Ok(()),
        }
    }
}

fn unix_epoch_seconds() -> i64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs() as i64
}

#[cfg(test)]
#[path = "control/tests.rs"]
mod tests;
