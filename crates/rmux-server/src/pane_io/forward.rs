use super::*;
mod control;
mod input;
mod output;
mod prepare;
mod refresh;
mod status;

struct AttachControlBacklogCleanup(Arc<AtomicUsize>);

impl Drop for AttachControlBacklogCleanup {
    fn drop(&mut self) {
        // The receiver and all deferred controls are dropped before this
        // guard, so no retained queue allocation remains to be accounted.
        self.0.store(0, Ordering::Release);
    }
}

/// Attach-stream messages a client opted into beyond the raw output stream.
#[derive(Clone, Copy, Debug)]
pub(crate) struct AttachStreamOptIns {
    /// Coalescible `Render` frames (`stream.attach.render`).
    pub(crate) render: bool,
    /// The typed `ServerExited` notice (`stream.attach.server_exited`).
    pub(crate) server_exited: bool,
}

/// Forwards an attach stream for a client without the server-exit notice.
#[allow(clippy::too_many_arguments)]
pub(crate) async fn forward_attach(
    stream: impl Into<AttachTransport>,
    target: AttachTarget,
    initial_socket_bytes: Vec<u8>,
    shutdown: watch::Receiver<()>,
    control_rx: mpsc::UnboundedReceiver<AttachControl>,
    control_backlog: Arc<AtomicUsize>,
    closing: Arc<AtomicBool>,
    persistent_overlay_epoch: Arc<AtomicU64>,
    live_input: LiveAttachInputContext,
    render_stream: bool,
) -> io::Result<()> {
    forward_attach_with_opt_ins(
        stream,
        target,
        initial_socket_bytes,
        shutdown,
        control_rx,
        control_backlog,
        closing,
        persistent_overlay_epoch,
        live_input,
        AttachStreamOptIns {
            render: render_stream,
            server_exited: false,
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
pub(crate) async fn forward_attach_with_opt_ins(
    stream: impl Into<AttachTransport>,
    target: AttachTarget,
    initial_socket_bytes: Vec<u8>,
    shutdown: watch::Receiver<()>,
    control_rx: mpsc::UnboundedReceiver<AttachControl>,
    control_backlog: Arc<AtomicUsize>,
    closing: Arc<AtomicBool>,
    persistent_overlay_epoch: Arc<AtomicU64>,
    live_input: LiveAttachInputContext,
    opt_ins: AttachStreamOptIns,
) -> io::Result<()> {
    // Declare the guard before receiver/deferred-control locals so it runs
    // after their destructors on every normal, error, or cancellation exit.
    let _control_backlog_cleanup = AttachControlBacklogCleanup(Arc::clone(&control_backlog));
    let stream = stream.into();
    let mut decoder = AttachFrameDecoder::new();
    let attach_controls = Some(control_rx);
    let current_target = open_attach_target(target, opt_ins.render)?;
    let status_job_updates = live_input.handler.status_job_updates().await;
    let status_refresh = AttachStatusRefreshScheduler::new(
        live_input
            .handler
            .attached_status_interval(&current_target.session_name)
            .await,
    );
    decoder.push_bytes(&initial_socket_bytes);
    let mut forwarder = AttachForwarder {
        _control_backlog_cleanup,
        stream,
        shutdown,
        control_backlog,
        closing,
        persistent_overlay_epoch,
        live_input,
        decoder,
        pending_input: Vec::new(),
        active_emit_cache: None,
        attach_controls,
        deferred_controls: VecDeque::new(),
        pending_escape_flush: PendingEscapeFlush::default(),
        render_generation: 0_u64,
        overlay_generation: 0_u64,
        persistent_overlay: None::<CachedPersistentOverlay>,
        persistent_overlay_visible: false,
        persistent_overlay_state_id: current_target.persistent_overlay_state_id,
        current_target,
        pane_refresh: AttachRefreshScheduler::default(),
        pane_refresh_requires_full: false,
        close_pane_output_after_refresh: false,
        deferred_passthroughs: DeferredPassthroughQueue::default(),
        last_client_input_at: None::<Instant>,
        status_job_updates,
        status_refresh,
        locked: false,
        pending_shutdown_requested: false,
        server_exit_notice: opt_ins.server_exited,
        shutdown_draining: false,
        shutdown_pending_output_batch: None,
    };
    forwarder.start().await?;
    let result = forwarder.run().await;
    if let Err(error) = &result {
        record_attach_error(
            forwarder.live_input.attach_pid(),
            &forwarder.current_target.session_name,
            error,
        );
        // A timed-out transport cannot drain the stop sequence either. Retrying
        // would postpone listener cleanup by a second full write deadline.
        if error.kind() != io::ErrorKind::TimedOut {
            let _ = emit_attach_stop(&forwarder.stream, &forwarder.current_target).await;
        }
    }
    result
}

// Fields drop in declaration order, unlike the original locals. Preserve their
// reverse declaration order, then the backlog guard, then the owned parameters:
// pane output and controls close before the transport; shutdown stays alive.
struct AttachForwarder {
    shutdown_pending_output_batch: Option<AttachOutputBatch>,
    shutdown_draining: bool,
    server_exit_notice: bool,
    pending_shutdown_requested: bool,
    locked: bool,
    status_refresh: AttachStatusRefreshScheduler,
    status_job_updates: watch::Receiver<u64>,
    last_client_input_at: Option<Instant>,
    deferred_passthroughs: DeferredPassthroughQueue,
    close_pane_output_after_refresh: bool,
    pane_refresh_requires_full: bool,
    pane_refresh: AttachRefreshScheduler,
    persistent_overlay_state_id: Option<u64>,
    persistent_overlay_visible: bool,
    persistent_overlay: Option<CachedPersistentOverlay>,
    overlay_generation: u64,
    render_generation: u64,
    current_target: types::OpenAttachTarget,
    pending_escape_flush: PendingEscapeFlush,
    deferred_controls: VecDeque<AttachControl>,
    attach_controls: Option<mpsc::UnboundedReceiver<AttachControl>>,
    active_emit_cache: Option<(u64, rmux_proto::WindowTarget)>,
    pending_input: Vec<u8>,
    decoder: AttachFrameDecoder,
    stream: AttachTransport,
    _control_backlog_cleanup: AttachControlBacklogCleanup,
    live_input: LiveAttachInputContext,
    persistent_overlay_epoch: Arc<AtomicU64>,
    closing: Arc<AtomicBool>,
    control_backlog: Arc<AtomicUsize>,
    shutdown: watch::Receiver<()>,
}

enum AttachStep {
    Continue,
    Wait,
    Exit,
}

impl AttachForwarder {
    async fn start(&mut self) -> io::Result<()> {
        emit_attach_bytes(
            &self.stream,
            &self.current_target.outer_terminal.attach_start_sequence(),
        )
        .await?;
        if let Some(sequence) = self
            .current_target
            .outer_terminal
            .render_cursor_state_transition(None, self.current_target.cursor_style)
        {
            emit_attach_bytes(&self.stream, sequence.as_bytes()).await?;
        }
        emit_coalescible_render_frame(
            &self.stream,
            &self.current_target.outer_terminal,
            &self.current_target.render_frame,
            self.current_target.render_stream,
        )
        .await?;
        Ok(())
    }
    async fn run(&mut self) -> io::Result<()> {
        loop {
            match self.prepare().await? {
                AttachStep::Continue => continue,
                AttachStep::Exit => return Ok(()),
                AttachStep::Wait => {}
            }
            let step = tokio::select! {
                biased;
                result = self.shutdown.changed() => {
                    let _ = result;
                    self.shutdown_draining = true;
                    AttachStep::Continue
                }
                result = read_socket_bytes(&self.stream, &mut self.decoder) => {
                    self.socket(result).await?
                }
                _ = wait_for_refresh_deadline(self.pane_refresh.deadline()) => {
                    self.refresh().await?
                }
                _ = async {
                    tokio::select! {
                        _ = wait_for_refresh_deadline(self.status_refresh.deadline()) => {},
                        _ = async {
                            if self.status_job_updates.changed().await.is_err() {
                                std::future::pending::<()>().await;
                            }
                        } => {},
                    }
                } => {
                    self.status().await?
                }
                _ = wait_for_refresh_deadline(self.pending_escape_flush.deadline()) => {
                    self.escape().await?
                }
                control = recv_attach_control(&mut self.deferred_controls, self.attach_controls.as_mut(), &self.control_backlog) => {
                    self.control(control).await?
                }
                result = recv_pane_output_optional(self.current_target.pane_output.as_mut()), if !self.pane_refresh.is_pending() => {
                    self.output(result).await?
                }
            };
            if let AttachStep::Exit = step {
                return Ok(());
            }
        }
    }
}
