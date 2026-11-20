use crate::client_flags::ClientFlags;
use crate::control_mode::ControlModeUpgrade;
#[cfg(any(unix, windows))]
use crate::handler::{attach_support::ActiveAttachIdentity, RequestHandler};
use crate::outer_terminal::{OuterTerminal, RenderedClientTitle};

use super::super::attach_control::AttachControl;
#[cfg(any(unix, windows))]
use super::super::deferred_passthrough::PaneOutputGeneration;
use super::super::live_render::LivePaneRender;
use rmux_core::PaneGeometry;
use rmux_proto::TerminalSize;
use rmux_pty::PtyMaster;
use tokio::time::Instant;

use super::*;

#[derive(Debug)]
pub(crate) struct AttachTarget {
    pub(crate) session_name: rmux_proto::SessionName,
    pub(crate) pane_master: Option<PtyMaster>,
    pub(crate) pane_output: PaneOutputReceiver,
    pub(crate) pane_output_start_sequence: u64,
    pub(crate) render_frame: Vec<u8>,
    pub(crate) outer_terminal: OuterTerminal,
    /// What this render told the outer terminal, or `None` when the session
    /// has `set-titles off` and it was left alone. Carried back so the caller
    /// can remember it and skip an unchanged title on the next render.
    ///
    /// In-process only: `AttachTarget` never crosses the attach wire, and
    /// `open_attach_target` drops this field at the transport boundary.
    pub(crate) client_title: Option<RenderedClientTitle>,
    pub(crate) cursor_style: rmux_core::ResolvedCursorStyle,
    pub(crate) active_pane_geometry: PaneGeometry,
    pub(crate) raw_passthrough: bool,
    pub(crate) kitty_graphics_passthrough: bool,
    pub(crate) sixel_passthrough: bool,
    pub(crate) persistent_overlay_state_id: Option<u64>,
    pub(crate) live_pane: Option<Box<LivePaneRender>>,
}

impl AttachTarget {
    /// A plain re-render of the pane this client already watches: no pane
    /// master handover and no persistent overlay. Live output continuity is
    /// preserved across it, whatever the frame happens to carry.
    pub(crate) fn is_plain_render_refresh(&self) -> bool {
        self.pane_master.is_none() && self.persistent_overlay_state_id.is_none()
    }

    /// A plain render refresh the control queue may drop in favour of a later
    /// one, because only the newest frame is worth drawing.
    ///
    /// A frame carrying OSC 0 / OSC 7 is excluded: the next render
    /// deduplicates the title against it, so dropping this one would leave the
    /// outer terminal showing the previous title with nothing left to correct
    /// it (issue #182).
    pub(crate) fn is_coalescible_render_refresh(&self) -> bool {
        self.is_plain_render_refresh()
            && !self
                .client_title
                .as_ref()
                .is_some_and(RenderedClientTitle::wrote)
    }
}

#[cfg(any(unix, windows))]
pub(crate) struct LiveAttachInputContext {
    pub(crate) handler: Arc<RequestHandler>,
    pub(crate) identity: ActiveAttachIdentity,
    #[cfg(test)]
    pub(crate) validate_identity: bool,
}

#[cfg(any(unix, windows))]
impl LiveAttachInputContext {
    pub(crate) fn new(handler: Arc<RequestHandler>, identity: ActiveAttachIdentity) -> Self {
        Self {
            handler,
            identity,
            #[cfg(test)]
            validate_identity: true,
        }
    }

    pub(crate) const fn attach_pid(&self) -> u32 {
        self.identity.attach_pid()
    }

    #[cfg(test)]
    pub(crate) async fn current_for_test(handler: Arc<RequestHandler>, attach_pid: u32) -> Self {
        let identity = handler.active_attach_identity_for_test(attach_pid).await;
        Self::new(handler, identity)
    }

    #[cfg(test)]
    pub(crate) fn unregistered_for_test(handler: Arc<RequestHandler>, attach_pid: u32) -> Self {
        let mut context = Self::new(
            handler,
            ActiveAttachIdentity::new(attach_pid, u64::MAX, rmux_proto::SessionId::new(u32::MAX)),
        );
        context.validate_identity = false;
        context
    }
}

pub(crate) struct HandleOutcome {
    pub(crate) response: rmux_proto::Response,
    pub(crate) attach: Option<AttachSessionUpgrade>,
    pub(crate) control: Option<ControlModeUpgrade>,
}

impl HandleOutcome {
    pub(crate) fn response(response: rmux_proto::Response) -> Self {
        Self {
            response,
            attach: None,
            control: None,
        }
    }

    pub(crate) fn attach(response: rmux_proto::Response, attach: AttachSessionUpgrade) -> Self {
        Self {
            response,
            attach: Some(attach),
            control: None,
        }
    }

    pub(crate) fn control(response: rmux_proto::Response, upgrade: ControlModeUpgrade) -> Self {
        Self {
            response,
            attach: None,
            control: Some(upgrade),
        }
    }
}

pub(crate) struct AttachSessionUpgrade {
    pub(crate) resize_publication: Option<crate::handler::AttachResizePublication>,
    pub(crate) attach_id: u64,
    pub(crate) session_id: rmux_proto::SessionId,
    pub(crate) target: AttachTarget,
    pub(crate) control_tx: mpsc::UnboundedSender<AttachControl>,
    pub(crate) control_rx: mpsc::UnboundedReceiver<AttachControl>,
    pub(crate) control_backlog: Arc<AtomicUsize>,
    pub(crate) closing: Arc<AtomicBool>,
    pub(crate) persistent_overlay_epoch: Arc<AtomicU64>,
    pub(crate) flags: ClientFlags,
    pub(crate) client_size: Option<TerminalSize>,
    pub(crate) render_stream: bool,
    pub(crate) server_exit_notice: bool,
}

impl AttachSessionUpgrade {
    pub(crate) fn new(
        attach_id: u64,
        session_id: rmux_proto::SessionId,
        target: AttachTarget,
        control_channel: (
            mpsc::UnboundedSender<AttachControl>,
            mpsc::UnboundedReceiver<AttachControl>,
        ),
        flags: ClientFlags,
        client_size: Option<TerminalSize>,
        render_stream: bool,
    ) -> Self {
        let (control_tx, control_rx) = control_channel;
        let control_backlog = Arc::new(AtomicUsize::new(0));
        Self {
            attach_id,
            resize_publication: None,
            session_id,
            target,
            control_tx,
            control_rx,
            control_backlog,
            closing: Arc::new(AtomicBool::new(false)),
            persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
            flags,
            client_size,
            render_stream,
            server_exit_notice: false,
        }
    }
}

pub(in crate::pane_io) struct OpenAttachTarget {
    pub(in crate::pane_io) session_name: rmux_proto::SessionName,
    pub(in crate::pane_io) predicted_echo: VecDeque<u8>,
    pub(in crate::pane_io) predicted_echo_started_at: Option<Instant>,
    pub(in crate::pane_io) pane_output: Option<PaneOutputReceiver>,
    pub(in crate::pane_io) render_frame: Vec<u8>,
    pub(in crate::pane_io) outer_terminal: OuterTerminal,
    pub(in crate::pane_io) cursor_style: rmux_core::ResolvedCursorStyle,
    pub(in crate::pane_io) active_pane_geometry: PaneGeometry,
    pub(in crate::pane_io) raw_passthrough: bool,
    pub(in crate::pane_io) kitty_graphics_passthrough: bool,
    pub(in crate::pane_io) sixel_passthrough: bool,
    pub(in crate::pane_io) persistent_overlay_state_id: Option<u64>,
    pub(in crate::pane_io) live_pane: Option<Box<LivePaneRender>>,
    pub(in crate::pane_io) render_stream: bool,
}

impl OpenAttachTarget {
    /// Reports whether the frame this client is currently showing came from
    /// the frozen DEC 2026 presentation snapshot. The renderer carries the
    /// state of the prebuilt frame it was handed until it publishes one of its
    /// own, so a switch is answered by the frame it emitted rather than by a
    /// later transcript read. Without a live renderer there is no presentation
    /// to converge — those paths already ask the session for a full refresh —
    /// so nothing is held back.
    pub(in crate::pane_io) fn presentation_frozen(&self) -> bool {
        self.live_pane
            .as_ref()
            .is_some_and(|pane| pane.delivered_frame().is_frozen())
    }

    /// The pane-output generation deferred work queued against this target
    /// belongs to. A target with no live subscription names no generation, so
    /// nothing observable can respawn underneath it.
    #[cfg(any(unix, windows))]
    pub(in crate::pane_io) fn pane_output_generation(&self) -> PaneOutputGeneration {
        self.pane_output
            .as_ref()
            .map_or_else(PaneOutputGeneration::unsubscribed, |pane_output| {
                PaneOutputGeneration::subscribed(pane_output.current_generation())
            })
    }
}
