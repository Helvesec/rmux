use rmux_core::events::{
    OutputCursor, OutputCursorItem, OutputEvent, OutputRing, DEFAULT_OUTPUT_RING_CAPACITY,
    DEFAULT_RECENT_LIVE_BUFFER_CAPACITY,
};
use rmux_core::{PaneGeometry, PaneId, TerminalPassthrough};
use rmux_proto::{AttachShellCommand, PaneTarget, TerminalSize};
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
use crate::handler::RequestHandler;
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
    #[cfg(windows)]
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

    #[cfg(windows)]
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
    pub(crate) input_target: PaneTarget,
    pub(crate) pane_master: Option<PtyMaster>,
    pub(crate) pane_output: PaneOutputSender,
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
        render_stream: bool,
    ) -> Self {
        let control_backlog = Arc::new(AtomicUsize::new(0));
        Self {
            response,
            attach: Some(AttachSessionUpgrade {
                target,
                control_tx,
                control_rx,
                control_backlog,
                closing: Arc::new(AtomicBool::new(false)),
                persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
                flags,
                client_size,
                render_stream,
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
    pub(crate) control_backlog: Arc<AtomicUsize>,
    pub(crate) closing: Arc<AtomicBool>,
    pub(crate) persistent_overlay_epoch: Arc<AtomicU64>,
    pub(crate) flags: ClientFlags,
    pub(crate) client_size: Option<TerminalSize>,
    pub(crate) render_stream: bool,
}

pub(super) struct OpenAttachTarget {
    pub(super) session_name: rmux_proto::SessionName,
    pub(super) input_target: PaneTarget,
    pub(super) pane_master: Option<PtyMaster>,
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
}

pub(crate) struct PaneOutputReceiver {
    inner: Arc<PaneOutputInner>,
    cursor: OutputCursor,
    passthrough_floor_sequence: u64,
    fast_rx: Option<mpsc::Receiver<FastPaneOutput>>,
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
}

struct PaneOutputPassthroughs {
    sequence: u64,
    passthroughs: Vec<TerminalPassthrough>,
}

const PANE_OUTPUT_PASSTHROUGH_CAPACITY: usize = 16;
const FAST_PANE_OUTPUT_MAX_BYTES: usize = 16 * 1024;
const FAST_PANE_OUTPUT_CHANNEL_CAPACITY: usize = 64;

impl PaneOutputState {
    fn new(event_capacity: usize, recent_byte_capacity: usize) -> Self {
        Self {
            ring: OutputRing::new(event_capacity, recent_byte_capacity),
            passthroughs: VecDeque::with_capacity(PANE_OUTPUT_PASSTHROUGH_CAPACITY),
        }
    }

    fn push(
        &mut self,
        bytes: Arc<[u8]>,
        passthroughs: Vec<TerminalPassthrough>,
        retain_recent: bool,
    ) -> u64 {
        let sequence = self
            .ring
            .push_shared_with_recent_retention(bytes, retain_recent);
        if !passthroughs.is_empty() {
            self.passthroughs.push_back(PaneOutputPassthroughs {
                sequence,
                passthroughs,
            });
            while self.passthroughs.len() > PANE_OUTPUT_PASSTHROUGH_CAPACITY {
                let _ = self.passthroughs.pop_front();
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
        let (sequence, result, fast_bytes) = {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("pane output state mutex must not be poisoned");
            if !generation_matches(self.current_generation(), generation) {
                return None;
            }
            let (result, passthroughs) = build_side_effects(&bytes);
            let bytes: Arc<[u8]> = bytes.into();
            let fast_bytes = fast_output_candidate(fast_receiver_count, &bytes, &passthroughs);
            let sequence = state.push(bytes, passthroughs, true);
            (sequence, result, fast_bytes)
        };
        let fast_delivered = fast_bytes
            .map(|bytes| self.try_send_fast_output(sequence, bytes))
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
        let (cursor, passthrough_floor_sequence) = {
            let state = self
                .inner
                .state
                .lock()
                .expect("pane output state mutex must not be poisoned");
            let cursor = state.cursor_from_now();
            let passthrough_floor_sequence = cursor.next_sequence();
            (cursor, passthrough_floor_sequence)
        };
        self.receiver(cursor, passthrough_floor_sequence, None)
    }

    pub(crate) fn subscribe_from_oldest(&self) -> PaneOutputReceiver {
        let (cursor, passthrough_floor_sequence) = {
            let state = self
                .inner
                .state
                .lock()
                .expect("pane output state mutex must not be poisoned");
            (state.cursor_from_oldest(), state.next_sequence())
        };
        self.receiver(cursor, passthrough_floor_sequence, None)
    }

    #[allow(dead_code)]
    pub(crate) fn subscribe_from_sequence(&self, sequence: u64) -> PaneOutputReceiver {
        let passthrough_floor_sequence = {
            let state = self
                .inner
                .state
                .lock()
                .expect("pane output state mutex must not be poisoned");
            state.next_sequence()
        };
        self.receiver(
            OutputRing::cursor_from_sequence(sequence),
            passthrough_floor_sequence,
            None,
        )
    }

    pub(crate) fn subscribe_live_from_sequence(&self, sequence: u64) -> PaneOutputReceiver {
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
        self.inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned")
            .clear_retained();
        self.inner.fast_epoch.fetch_add(1, Ordering::AcqRel);
        self.notify_receivers();
    }

    fn push_for_generation(
        &self,
        generation: Option<u64>,
        bytes: Vec<u8>,
        passthroughs: Vec<TerminalPassthrough>,
    ) -> Option<u64> {
        let fast_receiver_count = self.inner.fast_receiver_count.load(Ordering::Acquire);
        let (sequence, fast_bytes) = {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("pane output state mutex must not be poisoned");
            if !generation_matches(self.current_generation(), generation) {
                return None;
            }
            let bytes: Arc<[u8]> = bytes.into();
            let fast_bytes = fast_output_candidate(fast_receiver_count, &bytes, &passthroughs);
            let sequence = state.push(bytes, passthroughs, true);
            (sequence, fast_bytes)
        };
        let fast_delivered = fast_bytes
            .map(|bytes| self.try_send_fast_output(sequence, bytes))
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

    fn try_send_fast_output(&self, sequence: u64, bytes: Arc<[u8]>) -> bool {
        let epoch = self.inner.fast_epoch.load(Ordering::Acquire);
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
}
