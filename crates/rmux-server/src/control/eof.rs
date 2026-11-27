#[cfg(all(test, unix))]
use super::Notify;
#[cfg(all(test, unix))]
use super::OnceLock;
#[cfg(all(test, unix))]
use super::StdMutex;
#[cfg(windows)]
use super::CONTROL_STDIN_EOF_MARKER;
use super::{
    control_commands_require_drain, flush_output_queue, format_guard_line, io, mpsc,
    validate_control_command_arguments, watch, with_control_queue_eof_cancellation,
    ActiveControlCommand, ActiveControlResponseFrame, Arc, ControlClientFlags,
    ControlClientIdentity, ControlCommandResult, ControlGuardKind, ControlOutputQueue,
    ControlQueueDrainLease, ControlQueueEofCancellation, ControlServerEvent, ControlWriter,
    HashSet, JoinHandle, RequestHandler, ShutdownHandle, VecDeque,
};
use tokio::io::AsyncWriteExt;

#[cfg(any(unix, windows))]
pub(super) async fn acquire_control_eof_queue_lease(
    eof_queue_lease: &mut Option<ControlQueueDrainLease>,
    handler: &RequestHandler,
    control_identity: ControlClientIdentity,
) {
    if eof_queue_lease.is_some() {
        return;
    }
    *eof_queue_lease = Some(handler.begin_control_queue_drain(control_identity).await);
    #[cfg(all(test, unix))]
    pause_after_control_eof_queue_lease(handler, control_identity).await;
}

#[cfg(all(test, unix))]
pub(super) struct ControlEofQueueLeasePause {
    pub(super) handler: Arc<RequestHandler>,
    pub(super) identity: ControlClientIdentity,
    pub(super) reached: Notify,
    pub(super) release: Notify,
}

#[cfg(all(test, unix))]
impl ControlEofQueueLeasePause {
    pub(super) fn matches(
        &self,
        handler: &RequestHandler,
        identity: ControlClientIdentity,
    ) -> bool {
        std::ptr::eq(self.handler.as_ref(), handler) && self.identity == identity
    }
}

#[cfg(all(test, unix))]
pub(super) struct ControlEofQueueLeasePauseGuard {
    pub(super) pause: Arc<ControlEofQueueLeasePause>,
}

#[cfg(all(test, unix))]
impl std::ops::Deref for ControlEofQueueLeasePauseGuard {
    type Target = ControlEofQueueLeasePause;

    fn deref(&self) -> &Self::Target {
        &self.pause
    }
}

#[cfg(all(test, unix))]
impl Drop for ControlEofQueueLeasePauseGuard {
    fn drop(&mut self) {
        let Some(pauses) = CONTROL_EOF_QUEUE_LEASE_PAUSES.get() else {
            return;
        };
        let mut installed = pauses.lock().expect("control EOF queue lease pause");
        if let Some(index) = installed
            .iter()
            .position(|pause| Arc::ptr_eq(pause, &self.pause))
        {
            installed.swap_remove(index);
        }
    }
}

#[cfg(all(test, unix))]
static CONTROL_EOF_QUEUE_LEASE_PAUSES: OnceLock<StdMutex<Vec<Arc<ControlEofQueueLeasePause>>>> =
    OnceLock::new();

#[cfg(all(test, unix))]
pub(super) fn install_control_eof_queue_lease_pause(
    handler: &Arc<RequestHandler>,
    identity: ControlClientIdentity,
) -> ControlEofQueueLeasePauseGuard {
    let pause = Arc::new(ControlEofQueueLeasePause {
        handler: Arc::clone(handler),
        identity,
        reached: Notify::new(),
        release: Notify::new(),
    });
    let mut installed = CONTROL_EOF_QUEUE_LEASE_PAUSES
        .get_or_init(|| StdMutex::new(Vec::new()))
        .lock()
        .expect("control EOF queue lease pause");
    let duplicate = installed
        .iter()
        .any(|installed| installed.matches(handler, identity));
    if !duplicate {
        installed.push(Arc::clone(&pause));
    }
    drop(installed);
    assert!(
        !duplicate,
        "only one control EOF queue lease pause may be installed per handler and identity"
    );
    ControlEofQueueLeasePauseGuard { pause }
}

#[cfg(all(test, unix))]
pub(super) async fn pause_after_control_eof_queue_lease(
    handler: &RequestHandler,
    identity: ControlClientIdentity,
) {
    let pause = {
        let mut installed = CONTROL_EOF_QUEUE_LEASE_PAUSES
            .get_or_init(|| StdMutex::new(Vec::new()))
            .lock()
            .expect("control EOF queue lease pause");
        installed
            .iter()
            .position(|pause| pause.matches(handler, identity))
            .map(|index| installed.swap_remove(index))
    };
    if let Some(pause) = pause {
        pause.reached.notify_one();
        pause.release.notified().await;
    }
}

#[cfg(any(unix, windows))]
pub(super) struct ClosedControlCommand {
    pub(super) task: Option<JoinHandle<ControlCommandResult>>,
    pub(super) transport_result: io::Result<()>,
}

#[cfg(any(unix, windows))]
pub(super) async fn close_active_control_command_on_eof(
    current_command: &mut Option<ActiveControlCommand>,
    current_response_frame: &mut Option<ActiveControlResponseFrame>,
    output_queue: &mut ControlOutputQueue,
    write_half: &mut ControlWriter,
    flags: ControlClientFlags,
    paused_panes: &mut HashSet<u32>,
    exit_reason: Option<&str>,
) -> ClosedControlCommand {
    let Some(mut command) = current_command.take() else {
        *current_response_frame = None;
        return ClosedControlCommand {
            task: None,
            transport_result: Ok(()),
        };
    };
    let response_frame = current_response_frame
        .take()
        .expect("active commands own response frame state");
    if response_frame.open {
        output_queue.response_open = false;
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
    output_queue.enqueue_exit(exit_reason);
    command.eof_cancellation.cancel_for_eof();
    let task = command.task.take();
    drop(command);
    let transport_result = async {
        flush_output_queue(output_queue, write_half, flags, paused_panes, None).await?;
        write_half.finish(0).await?;
        write_half.shutdown().await
    }
    .await;
    ClosedControlCommand {
        task,
        transport_result,
    }
}

#[cfg(any(unix, windows))]
pub(super) struct EofDrainContext<'a> {
    pub(super) server_events: &'a mut mpsc::Receiver<ControlServerEvent>,
    pub(super) events_open: bool,
    pub(super) handler: &'a Arc<RequestHandler>,
    pub(super) control_identity: ControlClientIdentity,
    pub(super) shutdown: &'a mut watch::Receiver<()>,
    pub(super) shutdown_handle: &'a ShutdownHandle,
}

#[cfg(any(unix, windows))]
pub(super) async fn drain_control_queue_after_eof(
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
        let Some(normal_request_guard) = context
            .handler
            .try_begin_normal_request(control_commands_require_drain(&commands))
        else {
            break;
        };
        let handler_for_command = Arc::clone(context.handler);
        let control_identity = context.control_identity;
        let eof_cancellation = ControlQueueEofCancellation::new(control_identity);
        eof_cancellation.cancel_for_eof();
        let task = tokio::spawn(async move {
            let _normal_request_guard = normal_request_guard;
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
pub(super) async fn drain_control_command_after_eof(
    mut task: JoinHandle<ControlCommandResult>,
    context: &mut EofDrainContext<'_>,
) -> io::Result<bool> {
    let mut exit_received = false;
    let result = loop {
        tokio::select! {
            biased;

            result = &mut task => break result,
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
    // A terminal action in the command can mark the client closing while the
    // event channel is full. In that case no Exit reaches this receiver, so
    // revalidate after the task joins rather than relying on the closing
    // snapshot taken before the active frame.
    let registration_closed = !context
        .handler
        .control_queue_identity_is_open(context.control_identity)
        .await;
    Ok(exit_received || shutdown_requested || registration_closed)
}
