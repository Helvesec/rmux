use rmux_core::PaneId;
use rmux_proto::TerminalSize;
use rmux_pty::PtyMaster;
use std::sync::atomic::{AtomicBool, AtomicU64};
use std::sync::Arc;
use tokio::sync::{broadcast, mpsc};

use crate::control_mode::ControlModeUpgrade;
use crate::handler::attach_support::ClientFlags;
use crate::handler::RequestHandler;
use crate::outer_terminal::OuterTerminal;

const PANE_OUTPUT_BUFFER: usize = 256;

#[derive(Debug)]
pub(crate) enum AttachControl {
    Detach,
    Exited,
    DetachKill,
    DetachExec(String),
    Switch(Box<AttachTarget>),
    AdvancePersistentOverlayState(u64),
    Overlay(OverlayFrame),
    Write(Vec<u8>),
    Lock(String),
    Suspend,
}

impl AttachControl {
    pub(crate) fn switch(target: AttachTarget) -> Self {
        Self::Switch(Box::new(target))
    }
}

#[derive(Debug)]
pub(crate) struct OverlayFrame {
    pub(crate) frame: Vec<u8>,
    pub(crate) render_generation: u64,
    pub(crate) overlay_generation: u64,
    pub(crate) persistent: bool,
    pub(crate) persistent_state_id: Option<u64>,
}

impl OverlayFrame {
    pub(crate) fn new(frame: Vec<u8>, render_generation: u64, overlay_generation: u64) -> Self {
        Self {
            frame,
            render_generation,
            overlay_generation,
            persistent: false,
            persistent_state_id: None,
        }
    }

    pub(crate) fn persistent(
        frame: Vec<u8>,
        render_generation: u64,
        overlay_generation: u64,
    ) -> Self {
        Self {
            frame,
            render_generation,
            overlay_generation,
            persistent: true,
            persistent_state_id: None,
        }
    }

    pub(crate) fn persistent_with_state(
        frame: Vec<u8>,
        render_generation: u64,
        overlay_generation: u64,
        persistent_state_id: u64,
    ) -> Self {
        Self {
            frame,
            render_generation,
            overlay_generation,
            persistent: true,
            persistent_state_id: Some(persistent_state_id),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneAlertEvent {
    pub(crate) session_name: rmux_proto::SessionName,
    pub(crate) pane_id: PaneId,
    pub(crate) bell_count: u64,
    pub(crate) generation: Option<u64>,
}

pub(crate) type PaneAlertCallback = Arc<dyn Fn(PaneAlertEvent) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneExitEvent {
    pub(crate) session_name: rmux_proto::SessionName,
    pub(crate) pane_id: PaneId,
    pub(crate) generation: Option<u64>,
}

pub(crate) type PaneExitCallback = Arc<dyn Fn(PaneExitEvent) + Send + Sync>;

#[derive(Debug)]
pub(crate) struct AttachTarget {
    pub(crate) session_name: rmux_proto::SessionName,
    pub(crate) pane_master: PtyMaster,
    pub(crate) pane_output: PaneOutputSender,
    pub(crate) render_frame: Vec<u8>,
    pub(crate) outer_terminal: OuterTerminal,
    pub(crate) cursor_style: u32,
    pub(crate) persistent_overlay_state_id: Option<u64>,
}

pub(crate) struct LiveAttachInputContext {
    pub(crate) handler: Arc<RequestHandler>,
    pub(crate) attach_pid: u32,
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

    pub(crate) fn attach(
        response: rmux_proto::Response,
        target: AttachTarget,
        control_tx: mpsc::UnboundedSender<AttachControl>,
        control_rx: mpsc::UnboundedReceiver<AttachControl>,
        flags: ClientFlags,
        client_size: Option<TerminalSize>,
    ) -> Self {
        Self {
            response,
            attach: Some(AttachSessionUpgrade {
                target,
                control_tx,
                control_rx,
                closing: Arc::new(AtomicBool::new(false)),
                persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
                flags,
                client_size,
            }),
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
    pub(crate) target: AttachTarget,
    pub(crate) control_tx: mpsc::UnboundedSender<AttachControl>,
    pub(crate) control_rx: mpsc::UnboundedReceiver<AttachControl>,
    pub(crate) closing: Arc<AtomicBool>,
    pub(crate) persistent_overlay_epoch: Arc<AtomicU64>,
    pub(crate) flags: ClientFlags,
    pub(crate) client_size: Option<TerminalSize>,
}

pub(super) struct OpenAttachTarget {
    pub(super) session_name: rmux_proto::SessionName,
    pub(super) _pane_master: PtyMaster,
    pub(super) pane_output: Option<PaneOutputReceiver>,
    pub(super) render_frame: Vec<u8>,
    pub(super) outer_terminal: OuterTerminal,
    pub(super) cursor_style: u32,
    pub(super) persistent_overlay_state_id: Option<u64>,
}

pub(crate) type PaneOutputSender = broadcast::Sender<Vec<u8>>;
pub(super) type PaneOutputReceiver = broadcast::Receiver<Vec<u8>>;

pub(crate) fn pane_output_channel() -> PaneOutputSender {
    let (sender, _receiver) = broadcast::channel(PANE_OUTPUT_BUFFER);
    sender
}
