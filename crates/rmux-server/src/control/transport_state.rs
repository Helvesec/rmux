#[cfg(windows)]
use super::consume_control_eof_marker;
#[cfg(windows)]
use super::{acquire_control_eof_queue_lease, arm_control_eof_transition};
use super::{
    append_control_input_with_rejection, format_guard_line, io, mpsc, unix_epoch_seconds,
    validate_initial_control_command_count, watch, ActiveControlCommand,
    ActiveControlResponseFrame, Arc, ControlClientFlags, ControlClientIdentity,
    ControlCommandNumbering, ControlCommandResponseEvent, ControlEofCompletion,
    ControlEofTransition, ControlFormatSubscriptionTick, ControlFormatSubscriptions,
    ControlGuardKind, ControlLifecycle, ControlMode, ControlOutputQueue, ControlPaneOutputStates,
    ControlQueueDrainLease, ControlServerEvent, ControlSessionAttachment, ControlTransportDrain,
    ControlUpgradeInput, ControlWriter, DeferredServerEvents, HashMap, HashSet, PaneEvent,
    PaneOutputAttachedClientReadDemandGuard, PaneOutputReadDemandGuard, PaneSubscription,
    RequestHandler, ServerEventContext, VecDeque, CONTROL_PANE_EVENT_CAPACITY,
};
use rmux_ipc::LocalStream;
use tokio::io::AsyncReadExt;

pub(super) struct ControlTransport {
    // Abort the active task before dropping subscriptions and transport halves.
    pub(super) current_response_notifications: Option<mpsc::Receiver<ControlCommandResponseEvent>>,
    pub(super) current_response_frame: Option<ActiveControlResponseFrame>,
    pub(super) current_command: Option<ActiveControlCommand>,
    pub(super) events: ControlTransportEvents,
    pub(super) write_half: ControlWriter,
    pub(super) read_half: tokio::io::ReadHalf<LocalStream>,
    pub(super) handler: Arc<RequestHandler>,
    pub(super) control_identity: ControlClientIdentity,
    pub(super) lifecycle: ControlLifecycle,
    pub(super) mode: ControlMode,
    pub(super) exit_status: i32,
    pub(super) normal_exit: bool,
    pub(super) shutdown: watch::Receiver<()>,
    pub(super) server_events: mpsc::Receiver<ControlServerEvent>,
    pub(super) input_buffer: Vec<u8>,
    pub(super) queued_lines: VecDeque<String>,
    pub(super) queued_input_bytes: usize,
    pub(super) read_buffer: [u8; 8192],
    pub(super) shutdown_draining: bool,
    pub(super) input_closed: bool,
    pub(super) eof_queue_lease: Option<ControlQueueDrainLease>,
    pub(super) eof_transition: Option<ControlEofTransition>,
    pub(super) eof_command_ready: bool,
    pub(super) initial_command_completion_pending: bool,
    pub(super) shutdown_write_half_after_drain: bool,
    pub(super) eof_completion: ControlEofCompletion,
    pub(super) command_numbering: ControlCommandNumbering,
    pub(super) transport_drain: ControlTransportDrain,
    pub(super) pane_event_rx: mpsc::Receiver<PaneEvent>,
    pub(super) pane_event_tx: super::PaneEventSender,
}

pub(super) struct ControlTransportEvents {
    // Release read demand before stopping subscriptions, as in the local transport.
    pub(super) attached_client_read_presence: Option<PaneOutputAttachedClientReadDemandGuard>,
    pub(super) unrestricted_read_demand: Option<PaneOutputReadDemandGuard>,
    pub(super) flags: ControlClientFlags,
    pub(super) attachment: ControlSessionAttachment,
    pub(super) deferred_server_events: DeferredServerEvents,
    pub(super) paused_panes: HashSet<u32>,
    pub(super) format_subscription_tick: Option<ControlFormatSubscriptionTick>,
    pub(super) format_subscriptions: ControlFormatSubscriptions,
    pub(super) pane_output_states: ControlPaneOutputStates,
    pub(super) reader_demands: HashMap<u32, PaneOutputReadDemandGuard>,
    pub(super) subscriptions: HashMap<u32, PaneSubscription>,
    pub(super) output_queue: ControlOutputQueue,
}

impl ControlTransportEvents {
    pub(super) fn context<'a>(
        &'a mut self,
        handler: &'a RequestHandler,
        control_identity: ControlClientIdentity,
        write_half: &'a mut ControlWriter,
        pane_events: (&super::PaneEventSender, &'a mut mpsc::Receiver<PaneEvent>),
    ) -> ServerEventContext<'a> {
        ServerEventContext {
            handler,
            control_identity,
            requester_pid: control_identity.requester_pid(),
            session_name: self.attachment.session_name_mut(),
            subscriptions: &mut self.subscriptions,
            reader_demands: &mut self.reader_demands,
            pane_output_states: &mut self.pane_output_states,
            format_subscriptions: &mut self.format_subscriptions,
            format_subscription_tick: &mut self.format_subscription_tick,
            pane_event_tx: pane_events.0.clone(),
            pane_event_rx: pane_events.1,
            output_queue: &mut self.output_queue,
            write_half,
            paused_panes: &mut self.paused_panes,
            flags: &mut self.flags,
            unrestricted_read_demand: &mut self.unrestricted_read_demand,
            attached_client_read_presence: &mut self.attached_client_read_presence,
            deferred: &mut self.deferred_server_events,
        }
    }
}
pub(super) async fn forward_control_inner(
    stream: LocalStream,
    handler: Arc<RequestHandler>,
    control_identity: ControlClientIdentity,
    upgrade_input: ControlUpgradeInput,
    mut shutdown: watch::Receiver<()>,
    server_events: mpsc::Receiver<ControlServerEvent>,
    lifecycle: ControlLifecycle,
) -> io::Result<()> {
    let requester_pid = control_identity.requester_pid();
    let ControlUpgradeInput {
        buffered_bytes: initial_socket_bytes,
        initial_command_count,
        mode,
        records,
    } = upgrade_input;
    validate_initial_control_command_count(initial_command_count)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error.to_string()))?;
    let (pane_event_tx, pane_event_rx) = mpsc::channel(CONTROL_PANE_EVENT_CAPACITY);
    let pane_event_tx = super::PaneEventSender::from(pane_event_tx);
    let transport_drain = ControlTransportDrain::capture(&stream)?;
    let (mut read_half, write_half) = tokio::io::split(stream);
    let mut write_half = ControlWriter::new(write_half, records);
    let mut input_buffer = Vec::new();
    let mut queued_lines = VecDeque::new();
    let mut queued_input_bytes = 0_usize;
    append_control_input_with_rejection(
        &mut input_buffer,
        &mut queued_lines,
        &mut queued_input_bytes,
        &initial_socket_bytes,
        &mut write_half,
    )
    .await?;
    let mut read_buffer = [0_u8; 8192];
    let mut shutdown_draining = shutdown.has_changed().unwrap_or(true);
    while queued_lines.len() < initial_command_count && !shutdown_draining {
        let bytes_read = tokio::select! {
            biased;
            result = shutdown.changed() => {
                let _ = result;
                shutdown_draining = true;
                continue;
            }
            result = read_half.read(&mut read_buffer) => result?,
        };
        if bytes_read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                format!(
                    "control-mode stream closed after {} of {initial_command_count} initial commands",
                    queued_lines.len()
                ),
            ));
        }
        append_control_input_with_rejection(
            &mut input_buffer,
            &mut queued_lines,
            &mut queued_input_bytes,
            &read_buffer[..bytes_read],
            &mut write_half,
        )
        .await?;
    }
    if shutdown_draining {
        input_buffer.clear();
        queued_lines.clear();
        queued_input_bytes = 0;
    }
    let mut output_queue = ControlOutputQueue::default();
    let subscriptions = HashMap::new();
    let reader_demands = HashMap::new();
    let pane_output_states = ControlPaneOutputStates::default();
    let format_subscriptions = ControlFormatSubscriptions::default();
    let format_subscription_tick: Option<ControlFormatSubscriptionTick> = None;
    let paused_panes = HashSet::new();
    let deferred_server_events = DeferredServerEvents::default();
    #[cfg(unix)]
    let (input_closed, eof_queue_lease, eof_transition) = (shutdown_draining, None, None);
    #[cfg(windows)]
    let initial_stdin_eof_marker = consume_control_eof_marker(
        &mut input_buffer,
        &mut queued_lines,
        &mut queued_input_bytes,
    );
    #[cfg(windows)]
    let (input_closed, eof_queue_lease, eof_transition) = {
        let mut lease = None;
        let mut transition = None;
        if initial_stdin_eof_marker {
            acquire_control_eof_queue_lease(&mut lease, &handler, control_identity).await;
            arm_control_eof_transition(&mut transition);
        }
        (
            shutdown_draining || initial_stdin_eof_marker,
            lease,
            transition,
        )
    };
    let attachment =
        ControlSessionAttachment::new(handler.control_session_name(requester_pid).await);
    let flags: ControlClientFlags = handler
        .control_client_flags(requester_pid)
        .await
        .unwrap_or_default();
    let unrestricted_read_demand: Option<PaneOutputReadDemandGuard> = None;
    let attached_client_read_presence: Option<PaneOutputAttachedClientReadDemandGuard> = None;
    let current_command: Option<ActiveControlCommand> = None;
    let current_response_frame: Option<ActiveControlResponseFrame> = None;
    let current_response_notifications: Option<mpsc::Receiver<ControlCommandResponseEvent>> = None;
    let initial_command_completion_pending = false;
    let shutdown_write_half_after_drain = false;
    let eof_completion = ControlEofCompletion::default();
    #[cfg(windows)]
    let eof_completion = {
        let mut completion = eof_completion;
        if initial_stdin_eof_marker {
            completion.observe_stdin_eof_marker(attachment.is_attached(), !queued_lines.is_empty());
        }
        completion
    };
    let command_numbering = if initial_command_count == 0 {
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

    ControlTransport {
        handler,
        control_identity,
        lifecycle,
        exit_status: 0,
        normal_exit: false,
        mode,
        shutdown,
        server_events,
        read_half,
        write_half,
        input_buffer,
        queued_lines,
        queued_input_bytes,
        read_buffer,
        shutdown_draining,
        input_closed,
        eof_queue_lease,
        eof_transition,
        eof_command_ready: false,
        current_command,
        current_response_frame,
        current_response_notifications,
        initial_command_completion_pending,
        shutdown_write_half_after_drain,
        eof_completion,
        command_numbering,
        transport_drain,
        pane_event_rx,
        pane_event_tx,
        events: ControlTransportEvents {
            subscriptions,
            reader_demands,
            pane_output_states,
            format_subscriptions,
            format_subscription_tick,
            output_queue,
            paused_panes,
            flags,
            unrestricted_read_demand,
            attached_client_read_presence,
            deferred_server_events,
            attachment,
        },
    }
    .run()
    .await
}
