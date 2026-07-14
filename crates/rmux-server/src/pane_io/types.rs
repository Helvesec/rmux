use rmux_core::events::{
    OutputCursor, OutputCursorItem, OutputEvent, OutputRing, DEFAULT_OUTPUT_RING_CAPACITY,
    DEFAULT_RECENT_LIVE_BUFFER_CAPACITY,
};
use rmux_core::{PaneGeometry, PaneId, TerminalPassthrough};
use rmux_proto::{AttachShellCommand, TerminalSize};
use rmux_pty::PtyMaster;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::mpsc::error::{TryRecvError, TrySendError};
use tokio::sync::{mpsc, Notify};
use tokio::time::Instant;

use crate::client_flags::ClientFlags;
use crate::control_mode::ControlModeUpgrade;
#[cfg(any(unix, windows))]
use crate::handler::{attach_support::ActiveAttachIdentity, RequestHandler};
use crate::outer_terminal::OuterTerminal;

use super::live_render::LivePaneRender;

#[derive(Debug)]
pub(crate) enum AttachControl {
    Detach,
    Exited,
    DetachKill,
    DetachExecShellCommand(AttachShellCommand),
    InteractiveInput,
    Refresh,
    Switch(Box<AttachTarget>),
    AdvancePersistentOverlayState(u64),
    Overlay(OverlayFrame),
    Write(Vec<u8>),
    LockShellCommand(AttachShellCommand),
    Suspend,
}

impl AttachControl {
    pub(crate) fn switch(target: AttachTarget) -> Self {
        Self::Switch(Box::new(target))
    }

    pub(crate) fn is_coalescible_render_switch(&self) -> bool {
        matches!(self, Self::Switch(target) if target.is_coalescible_render_refresh())
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
    pub(crate) title_changed: bool,
    pub(crate) title_change: Option<(String, String)>,
    pub(crate) clipboard_set: bool,
    /// Decoded payloads of the inbound OSC 52 clipboard writes in this batch, in
    /// arrival order. Each becomes a paste buffer under `set-clipboard on`
    /// (tmux's `paste_add` in input_osc_52); empty for a query or for panes that
    /// emitted no clipboard write.
    pub(crate) clipboard_writes: Vec<Vec<u8>>,
    /// True when this batch toggled one of the pane's mouse-tracking modes
    /// (?1000/?1002/?1003). Attached clients must rebuild their outer
    /// terminal so pane-driven tracking reaches the outer terminal without
    /// waiting for an unrelated refresh (issue #93).
    pub(crate) mouse_mode_changed: bool,
    pub(crate) queue_activity_alert: bool,
    pub(crate) generation: Option<u64>,
}

pub(crate) type PaneAlertCallback = Arc<dyn Fn(PaneAlertEvent) + Send + Sync>;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PaneExitEvent {
    pub(crate) session_name: rmux_proto::SessionName,
    pub(crate) pane_id: PaneId,
    pub(crate) generation: Option<u64>,
    output_state: PaneExitOutputState,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum PaneExitOutputState {
    EofPublished,
    #[cfg(any(windows, test))]
    EofPending,
}

impl PaneExitEvent {
    pub(crate) fn eof_published(
        session_name: rmux_proto::SessionName,
        pane_id: PaneId,
        generation: Option<u64>,
    ) -> Self {
        Self {
            session_name,
            pane_id,
            generation,
            output_state: PaneExitOutputState::EofPublished,
        }
    }

    #[cfg(any(windows, test))]
    pub(crate) fn eof_pending(
        session_name: rmux_proto::SessionName,
        pane_id: PaneId,
        generation: Option<u64>,
    ) -> Self {
        Self {
            session_name,
            pane_id,
            generation,
            output_state: PaneExitOutputState::EofPending,
        }
    }

    pub(crate) fn output_eof_published(&self) -> bool {
        matches!(self.output_state, PaneExitOutputState::EofPublished)
    }
}

pub(crate) type PaneExitCallback = Arc<dyn Fn(PaneExitEvent) + Send + Sync>;

#[derive(Debug)]
pub(crate) struct AttachTarget {
    pub(crate) session_name: rmux_proto::SessionName,
    pub(crate) pane_master: Option<PtyMaster>,
    pub(crate) pane_output: PaneOutputReceiver,
    pub(crate) pane_output_start_sequence: u64,
    pub(crate) render_frame: Vec<u8>,
    pub(crate) outer_terminal: OuterTerminal,
    pub(crate) cursor_style: u32,
    pub(crate) active_pane_geometry: PaneGeometry,
    pub(crate) raw_passthrough: bool,
    pub(crate) kitty_graphics_passthrough: bool,
    pub(crate) sixel_passthrough: bool,
    pub(crate) persistent_overlay_state_id: Option<u64>,
    pub(crate) live_pane: Option<Box<LivePaneRender>>,
}

impl AttachTarget {
    pub(crate) fn is_coalescible_render_refresh(&self) -> bool {
        self.pane_master.is_none() && self.persistent_overlay_state_id.is_none()
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
}

impl AttachSessionUpgrade {
    pub(crate) fn new(
        session_id: rmux_proto::SessionId,
        target: AttachTarget,
        control_tx: mpsc::UnboundedSender<AttachControl>,
        control_rx: mpsc::UnboundedReceiver<AttachControl>,
        flags: ClientFlags,
        client_size: Option<TerminalSize>,
        render_stream: bool,
    ) -> Self {
        let control_backlog = Arc::new(AtomicUsize::new(0));
        Self {
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
        }
    }
}

pub(super) struct OpenAttachTarget {
    pub(super) session_name: rmux_proto::SessionName,
    pub(super) predicted_echo: VecDeque<u8>,
    pub(super) predicted_echo_started_at: Option<Instant>,
    pub(super) pane_output: Option<PaneOutputReceiver>,
    pub(super) render_frame: Vec<u8>,
    pub(super) outer_terminal: OuterTerminal,
    pub(super) cursor_style: u32,
    pub(super) active_pane_geometry: PaneGeometry,
    pub(super) raw_passthrough: bool,
    pub(super) kitty_graphics_passthrough: bool,
    pub(super) sixel_passthrough: bool,
    pub(super) persistent_overlay_state_id: Option<u64>,
    pub(super) live_pane: Option<Box<LivePaneRender>>,
    pub(super) render_stream: bool,
}

#[derive(Clone)]
pub(crate) struct PaneOutputSender {
    inner: Arc<PaneOutputInner>,
}

struct PaneOutputInner {
    state: Mutex<PaneOutputState>,
    generation: AtomicU64,
    fast_epoch: AtomicU64,
    receiver_count: AtomicUsize,
    fast_receiver_count: AtomicUsize,
    fast_receivers: Mutex<Vec<mpsc::Sender<FastPaneOutput>>>,
    notify: Notify,
    #[cfg(test)]
    fast_send_pause: Mutex<Option<Arc<FastSendPause>>>,
}

#[cfg(test)]
struct FastSendPause {
    reached: std::sync::Barrier,
    release: std::sync::Barrier,
}

pub(crate) struct PaneOutputReceiver {
    inner: Arc<PaneOutputInner>,
    cursor: OutputCursor,
    passthrough_floor_sequence: u64,
    fast_rx: Option<mpsc::Receiver<FastPaneOutput>>,
}

impl PaneOutputReceiver {
    pub(super) fn shares_pane_source_with(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.inner, &other.inner)
    }
}

#[derive(Debug, Clone)]
struct FastPaneOutput {
    epoch: u64,
    sequence: u64,
    bytes: Arc<[u8]>,
}

struct PaneOutputState {
    ring: OutputRing,
    passthroughs: VecDeque<PaneOutputPassthroughs>,
    retained_passthrough_bytes: usize,
}

struct PaneOutputPassthroughs {
    sequence: u64,
    passthroughs: Vec<TerminalPassthrough>,
}

const PANE_OUTPUT_PASSTHROUGH_CAPACITY: usize = 16;
const PANE_OUTPUT_PASSTHROUGH_BYTE_CAPACITY: usize = 16 * 1024 * 1024;
const FAST_PANE_OUTPUT_MAX_BYTES: usize = 16 * 1024;
const FAST_PANE_OUTPUT_CHANNEL_CAPACITY: usize = 64;

impl PaneOutputState {
    fn new(event_capacity: usize, recent_byte_capacity: usize) -> Self {
        Self {
            ring: OutputRing::new(event_capacity, recent_byte_capacity),
            passthroughs: VecDeque::with_capacity(PANE_OUTPUT_PASSTHROUGH_CAPACITY),
            retained_passthrough_bytes: 0,
        }
    }

    fn push(
        &mut self,
        bytes: Arc<[u8]>,
        passthroughs: Vec<TerminalPassthrough>,
        retain_recent: bool,
        retain_passthroughs: bool,
    ) -> u64 {
        let sequence = self
            .ring
            .push_shared_with_recent_retention(bytes, retain_recent);
        if retain_passthroughs && !passthroughs.is_empty() {
            let passthrough_bytes = passthrough_payload_bytes(&passthroughs);
            self.retained_passthrough_bytes = self
                .retained_passthrough_bytes
                .saturating_add(passthrough_bytes);
            self.passthroughs.push_back(PaneOutputPassthroughs {
                sequence,
                passthroughs,
            });
            while self.passthroughs.len() > PANE_OUTPUT_PASSTHROUGH_CAPACITY
                || self.retained_passthrough_bytes > PANE_OUTPUT_PASSTHROUGH_BYTE_CAPACITY
            {
                let Some(evicted) = self.passthroughs.pop_front() else {
                    break;
                };
                self.retained_passthrough_bytes = self
                    .retained_passthrough_bytes
                    .saturating_sub(passthrough_payload_bytes(&evicted.passthroughs));
            }
        }
        sequence
    }

    fn cursor_from_now(&self) -> OutputCursor {
        self.ring.cursor_from_now()
    }

    fn cursor_from_oldest(&self) -> OutputCursor {
        self.ring.cursor_from_oldest()
    }

    fn next_sequence(&self) -> u64 {
        self.ring.next_sequence()
    }

    fn clear_retained(&mut self) {
        self.ring.clear_retained();
        self.passthroughs.clear();
        self.retained_passthrough_bytes = 0;
    }

    fn poll_cursor(
        &self,
        cursor: &mut OutputCursor,
        passthrough_floor_sequence: u64,
    ) -> Option<OutputCursorItem> {
        self.ring
            .poll_cursor(cursor)
            .map(|item| self.attach_passthroughs(item, passthrough_floor_sequence))
    }

    fn poll_cursor_batch(
        &self,
        cursor: &mut OutputCursor,
        passthrough_floor_sequence: u64,
        limit: usize,
    ) -> Vec<OutputCursorItem> {
        self.ring
            .poll_cursor_batch(cursor, limit)
            .into_iter()
            .map(|item| self.attach_passthroughs(item, passthrough_floor_sequence))
            .collect()
    }

    fn attach_passthroughs(
        &self,
        item: OutputCursorItem,
        passthrough_floor_sequence: u64,
    ) -> OutputCursorItem {
        let OutputCursorItem::Event(event) = item else {
            return item;
        };
        if event.sequence() < passthrough_floor_sequence {
            return OutputCursorItem::Event(event);
        }
        let passthroughs = self
            .passthroughs
            .iter()
            .find(|candidate| candidate.sequence == event.sequence())
            .map(|candidate| candidate.passthroughs.clone())
            .unwrap_or_default();
        OutputCursorItem::Event(event.with_passthroughs(passthroughs))
    }
}

impl std::fmt::Debug for PaneOutputSender {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaneOutputSender")
            .finish_non_exhaustive()
    }
}

impl std::fmt::Debug for PaneOutputReceiver {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PaneOutputReceiver")
            .field("cursor", &self.cursor)
            .field(
                "passthrough_floor_sequence",
                &self.passthrough_floor_sequence,
            )
            .field("fast_path", &self.fast_rx.is_some())
            .finish_non_exhaustive()
    }
}

impl PaneOutputSender {
    #[cfg(test)]
    pub(crate) fn send(&self, bytes: Vec<u8>) -> u64 {
        self.push_for_generation(None, bytes, Vec::new())
            .expect("unguarded pane output send should always be accepted")
    }

    pub(crate) fn send_for_generation(
        &self,
        generation: Option<u64>,
        bytes: Vec<u8>,
    ) -> Option<u64> {
        self.push_for_generation(generation, bytes, Vec::new())
    }

    #[cfg(test)]
    pub(crate) fn send_for_generation_with_passthroughs(
        &self,
        generation: Option<u64>,
        bytes: Vec<u8>,
        passthroughs: Vec<TerminalPassthrough>,
    ) -> Option<u64> {
        self.push_for_generation(generation, bytes, passthroughs)
    }

    pub(crate) fn publish_for_generation<R>(
        &self,
        generation: Option<u64>,
        bytes: Vec<u8>,
        build_side_effects: impl FnOnce(&[u8]) -> (R, Vec<TerminalPassthrough>),
    ) -> Option<(u64, R)> {
        let fast_receiver_count = self.inner.fast_receiver_count.load(Ordering::Acquire);
        let (sequence, result, fast_bytes, fast_epoch) = {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("pane output state mutex must not be poisoned");
            if !generation_matches(self.current_generation(), generation) {
                return None;
            }
            let (result, passthroughs) = build_side_effects(&bytes);
            let retain_passthroughs = self.inner.receiver_count.load(Ordering::Acquire) > 0;
            let bytes: Arc<[u8]> = bytes.into();
            let fast_bytes = fast_output_candidate(fast_receiver_count, &bytes, &passthroughs);
            let sequence = state.push(bytes, passthroughs, true, retain_passthroughs);
            let fast_epoch = self.inner.fast_epoch.load(Ordering::Acquire);
            (sequence, result, fast_bytes, fast_epoch)
        };
        #[cfg(test)]
        self.pause_before_fast_send();
        let fast_delivered = fast_bytes
            .map(|bytes| self.try_send_fast_output(fast_epoch, sequence, bytes))
            .unwrap_or(false);
        self.notify_receivers_after_fast(fast_delivered);
        Some((sequence, result))
    }

    pub(crate) fn accepts_generation(&self, generation: Option<u64>) -> bool {
        generation_matches(self.current_generation(), generation)
    }

    pub(crate) fn set_generation(&self, generation: u64) {
        // Keep generation switches ordered with generation-guarded ring
        // pushes, so stale readers cannot pass a check from the old process
        // generation and then publish after a respawn.
        let _ring = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        self.inner.generation.store(generation, Ordering::SeqCst);
        self.inner.fast_epoch.fetch_add(1, Ordering::AcqRel);
    }

    pub(crate) fn current_generation(&self) -> u64 {
        self.inner.generation.load(Ordering::SeqCst)
    }

    pub(crate) fn subscribe(&self) -> PaneOutputReceiver {
        let state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        let cursor = state.cursor_from_now();
        let passthrough_floor_sequence = cursor.next_sequence();
        self.receiver(cursor, passthrough_floor_sequence, None)
    }

    pub(crate) fn subscribe_from_oldest(&self) -> PaneOutputReceiver {
        let state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        self.receiver(state.cursor_from_oldest(), state.next_sequence(), None)
    }

    #[allow(dead_code)]
    pub(crate) fn subscribe_from_sequence(&self, sequence: u64) -> PaneOutputReceiver {
        let state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        self.receiver(
            OutputRing::cursor_from_sequence(sequence),
            state.next_sequence(),
            None,
        )
    }

    pub(crate) fn subscribe_live_from_now(&self) -> (u64, PaneOutputReceiver) {
        let state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        let sequence = state.next_sequence();
        let receiver = self.register_live_receiver(sequence);
        (sequence, receiver)
    }

    #[cfg(test)]
    pub(crate) fn subscribe_live_from_sequence(&self, sequence: u64) -> PaneOutputReceiver {
        let _state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        self.register_live_receiver(sequence)
    }

    fn register_live_receiver(&self, sequence: u64) -> PaneOutputReceiver {
        let (fast_tx, fast_rx) = mpsc::channel(FAST_PANE_OUTPUT_CHANNEL_CAPACITY);
        self.inner
            .fast_receivers
            .lock()
            .expect("pane output fast receiver list must not be poisoned")
            .push(fast_tx);
        self.inner
            .fast_receiver_count
            .fetch_add(1, Ordering::Relaxed);
        self.receiver(
            OutputRing::cursor_from_sequence(sequence),
            sequence,
            Some(fast_rx),
        )
    }

    #[cfg_attr(not(all(any(unix, windows), feature = "web")), allow(dead_code))]
    pub(crate) fn capture_with_next_sequence<T>(&self, capture: impl FnOnce() -> T) -> (u64, T) {
        let state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        let captured = capture();
        (state.next_sequence(), captured)
    }

    pub(crate) fn clear_retained(&self) {
        let mut state = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        state.clear_retained();
        self.inner.fast_epoch.fetch_add(1, Ordering::AcqRel);
        drop(state);
        self.notify_receivers();
    }

    fn push_for_generation(
        &self,
        generation: Option<u64>,
        bytes: Vec<u8>,
        passthroughs: Vec<TerminalPassthrough>,
    ) -> Option<u64> {
        let fast_receiver_count = self.inner.fast_receiver_count.load(Ordering::Acquire);
        let (sequence, fast_bytes, fast_epoch) = {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("pane output state mutex must not be poisoned");
            if !generation_matches(self.current_generation(), generation) {
                return None;
            }
            let retain_passthroughs = self.inner.receiver_count.load(Ordering::Acquire) > 0;
            let bytes: Arc<[u8]> = bytes.into();
            let fast_bytes = fast_output_candidate(fast_receiver_count, &bytes, &passthroughs);
            let sequence = state.push(bytes, passthroughs, true, retain_passthroughs);
            let fast_epoch = self.inner.fast_epoch.load(Ordering::Acquire);
            (sequence, fast_bytes, fast_epoch)
        };
        #[cfg(test)]
        self.pause_before_fast_send();
        let fast_delivered = fast_bytes
            .map(|bytes| self.try_send_fast_output(fast_epoch, sequence, bytes))
            .unwrap_or(false);
        self.notify_receivers_after_fast(fast_delivered);
        Some(sequence)
    }

    fn receiver(
        &self,
        cursor: OutputCursor,
        passthrough_floor_sequence: u64,
        fast_rx: Option<mpsc::Receiver<FastPaneOutput>>,
    ) -> PaneOutputReceiver {
        self.inner.receiver_count.fetch_add(1, Ordering::Relaxed);
        PaneOutputReceiver {
            inner: Arc::clone(&self.inner),
            cursor,
            passthrough_floor_sequence,
            fast_rx,
        }
    }

    fn notify_receivers(&self) {
        match self.inner.receiver_count.load(Ordering::Acquire) {
            0 => {}
            1 => self.inner.notify.notify_one(),
            _ => self.inner.notify.notify_waiters(),
        }
    }

    fn notify_receivers_after_fast(&self, fast_delivered: bool) {
        let receiver_count = self.inner.receiver_count.load(Ordering::Acquire);
        if receiver_count == 0 {
            return;
        }
        if fast_delivered
            && receiver_count == self.inner.fast_receiver_count.load(Ordering::Acquire)
        {
            return;
        }
        match receiver_count {
            1 => self.inner.notify.notify_one(),
            _ => self.inner.notify.notify_waiters(),
        }
    }

    fn try_send_fast_output(&self, epoch: u64, sequence: u64, bytes: Arc<[u8]>) -> bool {
        let mut receivers = self
            .inner
            .fast_receivers
            .lock()
            .expect("pane output fast receiver list must not be poisoned");
        if receivers.len() == 1 {
            return match receivers[0].try_send(FastPaneOutput {
                epoch,
                sequence,
                bytes,
            }) {
                Ok(()) => true,
                Err(TrySendError::Full(_)) => false,
                Err(TrySendError::Closed(_)) => {
                    receivers.clear();
                    false
                }
            };
        }

        let mut delivered = 0_usize;
        let mut missed = false;
        receivers.retain(|receiver| {
            match receiver.try_send(FastPaneOutput {
                epoch,
                sequence,
                bytes: Arc::clone(&bytes),
            }) {
                Ok(()) => {
                    delivered = delivered.saturating_add(1);
                    true
                }
                Err(TrySendError::Full(_)) => {
                    missed = true;
                    true
                }
                Err(TrySendError::Closed(_)) => false,
            }
        });
        delivered > 0 && !missed
    }

    #[cfg(test)]
    fn install_fast_send_pause(&self) -> Arc<FastSendPause> {
        let pause = Arc::new(FastSendPause {
            reached: std::sync::Barrier::new(2),
            release: std::sync::Barrier::new(2),
        });
        *self
            .inner
            .fast_send_pause
            .lock()
            .expect("fast send pause mutex must not be poisoned") = Some(Arc::clone(&pause));
        pause
    }

    #[cfg(test)]
    fn pause_before_fast_send(&self) {
        let pause = self
            .inner
            .fast_send_pause
            .lock()
            .expect("fast send pause mutex must not be poisoned")
            .take();
        if let Some(pause) = pause {
            pause.reached.wait();
            pause.release.wait();
        }
    }
}

fn passthrough_payload_bytes(passthroughs: &[TerminalPassthrough]) -> usize {
    passthroughs.iter().fold(0_usize, |total, passthrough| {
        total.saturating_add(passthrough.payload().len())
    })
}

fn generation_matches(current: u64, generation: Option<u64>) -> bool {
    match generation {
        None => true,
        Some(generation) => current == generation,
    }
}

fn fast_output_candidate(
    fast_receiver_count: usize,
    bytes: &Arc<[u8]>,
    passthroughs: &[TerminalPassthrough],
) -> Option<Arc<[u8]>> {
    if fast_receiver_count == 0
        || !passthroughs.is_empty()
        || bytes.len() > FAST_PANE_OUTPUT_MAX_BYTES
    {
        return None;
    }
    Some(Arc::clone(bytes))
}

impl PaneOutputReceiver {
    pub(crate) async fn recv(&mut self) -> OutputCursorItem {
        loop {
            if let Some(item) = self.try_recv_fast() {
                return item;
            }
            let inner = Arc::clone(&self.inner);
            let notified = inner.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(item) = self.try_recv_fast() {
                return item;
            }
            if let Some(item) = self.try_recv() {
                return item;
            }
            if self.fast_rx.is_some() {
                let fast = {
                    let fast_rx = self.fast_rx.as_mut().expect("fast receiver checked");
                    tokio::select! {
                        fast = fast_rx.recv() => fast,
                        _ = notified => None,
                    }
                };
                if let Some(fast) = fast {
                    if let Some(item) = self.accept_fast_item(fast) {
                        return item;
                    }
                }
            } else {
                notified.await;
            }
        }
    }

    fn try_recv_fast(&mut self) -> Option<OutputCursorItem> {
        loop {
            let fast = match self.fast_rx.as_mut()?.try_recv() {
                Ok(fast) => fast,
                Err(TryRecvError::Empty) => return None,
                Err(TryRecvError::Disconnected) => {
                    self.disable_fast_rx();
                    return None;
                }
            };
            if let Some(item) = self.accept_fast_item(fast) {
                return Some(item);
            }
        }
    }

    fn accept_fast_item(&mut self, fast: FastPaneOutput) -> Option<OutputCursorItem> {
        if fast.epoch != self.inner.fast_epoch.load(Ordering::Acquire) {
            return None;
        }
        if !self.cursor.advance_past_sequence(fast.sequence) {
            return None;
        }
        Some(OutputCursorItem::Event(OutputEvent::from_shared(
            fast.sequence,
            fast.bytes,
            Vec::new(),
        )))
    }

    fn disable_fast_rx(&mut self) {
        if self.fast_rx.take().is_some() {
            self.inner
                .fast_receiver_count
                .fetch_sub(1, Ordering::Relaxed);
            self.inner
                .fast_receivers
                .lock()
                .expect("pane output fast receiver list must not be poisoned")
                .retain(|receiver| !receiver.is_closed());
        }
    }

    pub(crate) fn try_recv(&mut self) -> Option<OutputCursorItem> {
        self.inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned")
            .poll_cursor(&mut self.cursor, self.passthrough_floor_sequence)
    }

    pub(crate) fn try_recv_batch(&mut self, limit: usize) -> Vec<OutputCursorItem> {
        let mut items = Vec::new();
        for _ in 0..limit {
            let Some(item) = self.try_recv_fast() else {
                break;
            };
            items.push(item);
        }

        let remaining = limit.saturating_sub(items.len());
        if remaining == 0 {
            return items;
        }
        let mut retained = self
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned")
            .poll_cursor_batch(&mut self.cursor, self.passthrough_floor_sequence, remaining);
        items.append(&mut retained);
        items
    }

    pub(crate) const fn cursor(&self) -> &OutputCursor {
        &self.cursor
    }
}

impl Drop for PaneOutputReceiver {
    fn drop(&mut self) {
        self.disable_fast_rx();
        self.inner.receiver_count.fetch_sub(1, Ordering::Relaxed);
    }
}

pub(crate) fn pane_output_channel() -> PaneOutputSender {
    pane_output_channel_with_limits(
        DEFAULT_OUTPUT_RING_CAPACITY,
        DEFAULT_RECENT_LIVE_BUFFER_CAPACITY,
    )
}

pub(crate) fn pane_output_channel_with_limits(
    event_capacity: usize,
    recent_byte_capacity: usize,
) -> PaneOutputSender {
    PaneOutputSender {
        inner: Arc::new(PaneOutputInner {
            state: Mutex::new(PaneOutputState::new(event_capacity, recent_byte_capacity)),
            generation: AtomicU64::new(0),
            fast_epoch: AtomicU64::new(0),
            receiver_count: AtomicUsize::new(0),
            fast_receiver_count: AtomicUsize::new(0),
            fast_receivers: Mutex::new(Vec::new()),
            notify: Notify::new(),
            #[cfg(test)]
            fast_send_pause: Mutex::new(None),
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_generation_output_is_not_published() {
        let sender = pane_output_channel_with_limits(4, 64);
        sender.set_generation(1);
        let mut receiver = sender.subscribe();

        assert_eq!(
            sender.send_for_generation(Some(1), b"old".to_vec()),
            Some(0)
        );
        let Some(OutputCursorItem::Event(event)) = receiver.try_recv() else {
            panic!("receiver should see the accepted generation");
        };
        assert_eq!(event.sequence(), 0);
        assert_eq!(event.bytes(), b"old");

        sender.set_generation(2);
        sender.clear_retained();
        assert_eq!(sender.send_for_generation(Some(1), b"stale".to_vec()), None);
        assert!(
            receiver.try_recv().is_none(),
            "stale generation output must not be retained or delivered"
        );

        assert_eq!(
            sender.send_for_generation(Some(2), b"fresh".to_vec()),
            Some(1)
        );
        let Some(OutputCursorItem::Event(event)) = receiver.try_recv() else {
            panic!("receiver should see the fresh generation");
        };
        assert_eq!(event.sequence(), 1);
        assert_eq!(event.bytes(), b"fresh");
    }

    #[test]
    fn live_passthroughs_are_attached_to_existing_receivers() {
        let sender = pane_output_channel_with_limits(4, 64);
        let mut receiver = sender.subscribe();

        sender.send_for_generation_with_passthroughs(
            None,
            b"image".to_vec(),
            vec![TerminalPassthrough::kitty_graphics(1, 2, b"Gf=100;AAAA")],
        );

        let Some(OutputCursorItem::Event(event)) = receiver.try_recv() else {
            panic!("receiver should see live output");
        };
        assert_eq!(event.bytes(), b"image");
        assert_eq!(event.passthroughs().len(), 1);
        assert_eq!(event.passthroughs()[0].cursor_x(), 1);
        assert_eq!(event.passthroughs()[0].payload(), b"Gf=100;AAAA");
    }

    #[test]
    fn reserved_live_subscription_keeps_passthroughs_emitted_before_transport_open() {
        let sender = pane_output_channel_with_limits(4, 64);
        let (start_sequence, mut receiver) = sender.subscribe_live_from_now();
        assert_eq!(start_sequence, 0);

        sender.send_for_generation_with_passthroughs(
            None,
            b"frame".to_vec(),
            vec![TerminalPassthrough::kitty_graphics(
                0,
                0,
                b"reserved-live-image",
            )],
        );

        let Some(OutputCursorItem::Event(event)) = receiver.try_recv() else {
            panic!("reserved live receiver should see pre-open output");
        };
        assert_eq!(event.passthroughs().len(), 1);
        assert_eq!(event.passthroughs()[0].payload(), b"reserved-live-image");
    }

    #[test]
    fn detached_output_keeps_recent_recovery_buffer_for_late_waiters() {
        let sender = pane_output_channel_with_limits(4, 64);

        sender.send(b"detached".to_vec());

        {
            let state = sender
                .inner
                .state
                .lock()
                .expect("pane output state mutex must not be poisoned");
            assert_eq!(state.ring.retained_len(), 1);
            assert_eq!(
                state.ring.recent_len(),
                b"detached".len(),
                "late waiters and lag reports need recent output even when no receiver was live"
            );
        }

        let mut receiver = sender.subscribe_from_oldest();
        let Some(OutputCursorItem::Event(event)) = receiver.try_recv() else {
            panic!("oldest subscriptions should still replay retained detached events");
        };
        assert_eq!(event.sequence(), 0);
        assert_eq!(event.bytes(), b"detached");
    }

    #[test]
    fn live_output_keeps_recent_recovery_buffer_for_slow_receivers() {
        let sender = pane_output_channel_with_limits(4, 64);
        let _receiver = sender.subscribe();

        sender.send(b"live".to_vec());

        let state = sender
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        assert_eq!(state.ring.retained_len(), 1);
        assert_eq!(state.ring.recent_len(), b"live".len());
    }

    #[test]
    fn passthroughs_are_not_replayed_to_oldest_subscribers() {
        let sender = pane_output_channel_with_limits(4, 64);

        sender.send_for_generation_with_passthroughs(
            None,
            b"historic-image".to_vec(),
            vec![TerminalPassthrough::kitty_graphics(0, 0, b"Gf=100;AAAA")],
        );
        let mut receiver = sender.subscribe_from_oldest();

        let Some(OutputCursorItem::Event(event)) = receiver.try_recv() else {
            panic!("receiver should replay retained bytes");
        };
        assert_eq!(event.bytes(), b"historic-image");
        assert!(
            event.passthroughs().is_empty(),
            "kitty passthrough is live-only and must not replay from retained output"
        );
    }

    #[test]
    fn passthroughs_are_not_retained_without_live_receivers() {
        let sender = pane_output_channel_with_limits(4, 64);

        sender.send_for_generation_with_passthroughs(
            None,
            b"detached-image".to_vec(),
            vec![TerminalPassthrough::kitty_graphics(
                0,
                0,
                vec![b'A'; 1024 * 1024],
            )],
        );

        let state = sender
            .inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned");
        assert!(state.passthroughs.is_empty());
        assert_eq!(state.retained_passthrough_bytes, 0);
        assert_eq!(
            state.ring.retained_len(),
            1,
            "plain pane bytes stay replayable"
        );
    }

    #[test]
    fn live_passthrough_retention_has_an_aggregate_byte_budget() {
        let sender = pane_output_channel_with_limits(8, 128);
        let mut receiver = sender.subscribe();
        let payload_bytes = 5 * 1024 * 1024;

        for index in 0..4_u8 {
            sender.send_for_generation_with_passthroughs(
                None,
                vec![index],
                vec![TerminalPassthrough::kitty_graphics(
                    0,
                    0,
                    vec![index; payload_bytes],
                )],
            );
        }

        {
            let state = sender
                .inner
                .state
                .lock()
                .expect("pane output state mutex must not be poisoned");
            assert!(
                state.retained_passthrough_bytes <= PANE_OUTPUT_PASSTHROUGH_BYTE_CAPACITY,
                "retained live-only side effects must stay within the per-pane byte budget"
            );
            assert_eq!(state.passthroughs.len(), 3);
        }

        let Some(OutputCursorItem::Event(first)) = receiver.try_recv() else {
            panic!("receiver should see the first pane-output event");
        };
        assert!(
            first.passthroughs().is_empty(),
            "the byte budget should evict the oldest side effect"
        );
        let mut last = first;
        while let Some(OutputCursorItem::Event(event)) = receiver.try_recv() {
            last = event;
        }
        assert_eq!(last.passthroughs().len(), 1);
        assert_eq!(last.passthroughs()[0].payload().len(), payload_bytes);
    }

    #[test]
    fn live_passthrough_retention_is_bounded() {
        let sender = pane_output_channel_with_limits(PANE_OUTPUT_PASSTHROUGH_CAPACITY + 2, 1024);
        let mut receiver = sender.subscribe();

        for index in 0..=PANE_OUTPUT_PASSTHROUGH_CAPACITY {
            sender.send_for_generation_with_passthroughs(
                None,
                format!("event-{index}").into_bytes(),
                vec![TerminalPassthrough::kitty_graphics(
                    0,
                    0,
                    format!("Gf=100;{index}").into_bytes(),
                )],
            );
        }

        let Some(OutputCursorItem::Event(first)) = receiver.try_recv() else {
            panic!("receiver should see the first retained event");
        };
        assert_eq!(first.sequence(), 0);
        assert!(
            first.passthroughs().is_empty(),
            "old live passthrough side effects should be dropped when the bounded queue rotates"
        );

        let mut latest = first;
        while let Some(OutputCursorItem::Event(event)) = receiver.try_recv() {
            latest = event;
        }
        assert_eq!(latest.sequence(), PANE_OUTPUT_PASSTHROUGH_CAPACITY as u64);
        assert_eq!(latest.passthroughs().len(), 1);
    }

    #[test]
    fn receiver_count_tracks_live_subscribers() {
        let sender = pane_output_channel_with_limits(4, 64);
        assert_eq!(sender.inner.receiver_count.load(Ordering::Relaxed), 0);
        assert_eq!(sender.inner.fast_receiver_count.load(Ordering::Relaxed), 0);

        let first = sender.subscribe();
        assert_eq!(sender.inner.receiver_count.load(Ordering::Relaxed), 1);
        assert_eq!(sender.inner.fast_receiver_count.load(Ordering::Relaxed), 0);
        {
            let _second = sender.subscribe_from_oldest();
            assert_eq!(sender.inner.receiver_count.load(Ordering::Relaxed), 2);
            assert_eq!(sender.inner.fast_receiver_count.load(Ordering::Relaxed), 0);
            let _live = sender.subscribe_live_from_sequence(0);
            assert_eq!(sender.inner.receiver_count.load(Ordering::Relaxed), 3);
            assert_eq!(sender.inner.fast_receiver_count.load(Ordering::Relaxed), 1);
        }
        assert_eq!(sender.inner.receiver_count.load(Ordering::Relaxed), 1);
        assert_eq!(sender.inner.fast_receiver_count.load(Ordering::Relaxed), 0);

        drop(first);
        assert_eq!(sender.inner.receiver_count.load(Ordering::Relaxed), 0);
        assert_eq!(sender.inner.fast_receiver_count.load(Ordering::Relaxed), 0);
    }

    #[test]
    fn dropped_live_receivers_are_pruned_without_new_output() {
        let sender = pane_output_channel_with_limits(4, 64);

        for _ in 0..32 {
            drop(sender.subscribe_live_from_sequence(0));
        }

        assert_eq!(sender.inner.receiver_count.load(Ordering::Relaxed), 0);
        assert_eq!(sender.inner.fast_receiver_count.load(Ordering::Relaxed), 0);
        assert!(
            sender
                .inner
                .fast_receivers
                .lock()
                .expect("pane output fast receiver list must not be poisoned")
                .is_empty(),
            "closed fast senders must not accumulate while panes are quiet"
        );
    }

    #[tokio::test]
    async fn live_receiver_consumes_fast_output() {
        let sender = pane_output_channel_with_limits(4, 64);
        let mut receiver = sender.subscribe_live_from_sequence(0);

        assert_eq!(sender.send(b"fast".to_vec()), 0);

        let item = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .expect("live receiver should not block on fast output");
        let OutputCursorItem::Event(event) = item else {
            panic!("live receiver should see the fast output event");
        };
        assert_eq!(event.sequence(), 0);
        assert_eq!(event.bytes(), b"fast");
        assert_eq!(receiver.cursor().next_sequence(), 1);
    }

    #[tokio::test]
    async fn live_receiver_uses_bounded_ring_for_large_output() {
        let sender = pane_output_channel_with_limits(4, 64);
        let mut receiver = sender.subscribe_live_from_sequence(0);
        let large = vec![b'x'; FAST_PANE_OUTPUT_MAX_BYTES + 1];

        assert_eq!(sender.send(large.clone()), 0);
        assert!(
            receiver
                .fast_rx
                .as_mut()
                .expect("live receiver should have fast channel")
                .try_recv()
                .is_err(),
            "large output must not be duplicated into the fast receiver queue"
        );

        let item = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .expect("live receiver should fall back to retained output");
        let OutputCursorItem::Event(event) = item else {
            panic!("large retained event should be delivered from the bounded ring");
        };
        assert_eq!(event.sequence(), 0);
        assert_eq!(event.bytes(), large.as_slice());
        assert_eq!(receiver.cursor().next_sequence(), 1);
    }

    #[test]
    fn live_fast_output_queue_capacity_is_bounded() {
        let sender = pane_output_channel_with_limits(256, 64);
        let receiver = sender.subscribe_live_from_sequence(0);

        for _ in 0..(FAST_PANE_OUTPUT_CHANNEL_CAPACITY + 8) {
            sender.send(b"x".to_vec());
        }

        let fast_rx = receiver.fast_rx.as_ref().expect("live receiver");
        assert_eq!(fast_rx.len(), FAST_PANE_OUTPUT_CHANNEL_CAPACITY);
        assert_eq!(fast_rx.max_capacity(), FAST_PANE_OUTPUT_CHANNEL_CAPACITY);
    }

    #[test]
    fn live_receiver_batches_fast_output_before_retained_fallback() {
        let sender = pane_output_channel_with_limits(256, 64);
        let mut receiver = sender.subscribe_live_from_sequence(0);

        for byte in [b'a', b'b', b'c'] {
            assert_eq!(sender.send(vec![byte]), u64::from(byte - b'a'));
        }

        let batch = receiver.try_recv_batch(8);
        let events = batch
            .into_iter()
            .map(|item| {
                let OutputCursorItem::Event(event) = item else {
                    panic!("fast output should not report a gap");
                };
                (event.sequence(), event.bytes().to_vec())
            })
            .collect::<Vec<_>>();

        assert_eq!(
            events,
            vec![(0, b"a".to_vec()), (1, b"b".to_vec()), (2, b"c".to_vec())]
        );
        assert_eq!(receiver.cursor().next_sequence(), 3);
        assert!(
            receiver.try_recv().is_none(),
            "batching fast output must not replay the same retained events"
        );
    }

    #[tokio::test]
    async fn live_fast_output_is_invalidated_when_retained_output_is_cleared() {
        let sender = pane_output_channel_with_limits(4, 64);
        let mut receiver = sender.subscribe_live_from_sequence(0);

        assert_eq!(sender.send(b"stale".to_vec()), 0);
        sender.clear_retained();
        assert_eq!(sender.send(b"fresh".to_vec()), 1);

        let item = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .expect("live receiver should report the retained-output gap");
        let OutputCursorItem::Gap(gap) = item else {
            panic!("stale fast output must not be delivered after clear_retained");
        };
        assert_eq!(gap.expected_sequence(), 0);
        assert_eq!(gap.resume_sequence(), 1);

        let item = tokio::time::timeout(std::time::Duration::from_secs(1), receiver.recv())
            .await
            .expect("live receiver should resume with fresh output");
        let OutputCursorItem::Event(event) = item else {
            panic!("live receiver should resume with fresh output");
        };
        assert_eq!(event.sequence(), 1);
        assert_eq!(event.bytes(), b"fresh");
    }

    #[test]
    fn fast_output_captured_before_clear_is_rejected_after_clear() {
        let sender = pane_output_channel_with_limits(4, 64);
        let mut receiver = sender.subscribe_live_from_sequence(0);
        let stale_epoch = sender.inner.fast_epoch.load(Ordering::Acquire);

        sender.clear_retained();
        assert!(sender.try_send_fast_output(stale_epoch, 0, Arc::<[u8]>::from(&b"stale"[..]),));

        assert!(
            receiver.try_recv_fast().is_none(),
            "a fast item captured before the retained-output boundary must be rejected"
        );
        assert_eq!(receiver.cursor().next_sequence(), 0);
    }

    fn assert_clear_invalidates_paused_fast_send(publish_with_side_effects: bool) {
        let sender = pane_output_channel_with_limits(4, 64);
        let mut receiver = sender.subscribe_live_from_sequence(0);
        let pause = sender.install_fast_send_pause();
        let publisher = sender.clone();
        let publishing = std::thread::spawn(move || {
            if publish_with_side_effects {
                publisher
                    .publish_for_generation(None, b"stale".to_vec(), |_| ((), Vec::new()))
                    .map(|(sequence, ())| sequence)
            } else {
                publisher.send_for_generation(None, b"stale".to_vec())
            }
        });

        pause.reached.wait();
        sender.clear_retained();
        pause.release.wait();
        assert_eq!(publishing.join().expect("publisher thread joins"), Some(0));

        assert!(
            receiver.try_recv_fast().is_none(),
            "clear must invalidate the paused old-generation fast item"
        );
        let Some(OutputCursorItem::Gap(gap)) = receiver.try_recv() else {
            panic!("clear must replace the old-generation retained item with a gap");
        };
        assert_eq!(gap.expected_sequence(), 0);
        assert_eq!(gap.resume_sequence(), 1);
    }

    #[test]
    fn push_captures_fast_epoch_before_releasing_output_state() {
        assert_clear_invalidates_paused_fast_send(false);
    }

    #[test]
    fn publish_captures_fast_epoch_before_releasing_output_state() {
        assert_clear_invalidates_paused_fast_send(true);
    }
}
