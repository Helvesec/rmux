use rmux_core::events::{
    OutputCursor, OutputCursorItem, OutputRing, DEFAULT_OUTPUT_RING_CAPACITY,
    DEFAULT_RECENT_LIVE_BUFFER_CAPACITY,
};
use rmux_core::{PaneGeometry, PaneId, TerminalPassthrough};
use rmux_proto::{AttachShellCommand, TerminalSize};
use rmux_pty::PtyMaster;
use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use tokio::sync::{mpsc, Notify};

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
    pub(crate) active_pane_geometry: PaneGeometry,
    pub(crate) kitty_graphics_passthrough: bool,
    pub(crate) sixel_passthrough: bool,
    pub(crate) persistent_overlay_state_id: Option<u64>,
    pub(crate) live_pane: Option<Box<LivePaneRender>>,
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
    pub(super) active_pane_geometry: PaneGeometry,
    pub(super) kitty_graphics_passthrough: bool,
    pub(super) sixel_passthrough: bool,
    pub(super) persistent_overlay_state_id: Option<u64>,
    pub(super) live_pane: Option<Box<LivePaneRender>>,
}

#[derive(Clone)]
pub(crate) struct PaneOutputSender {
    inner: Arc<PaneOutputInner>,
}

struct PaneOutputInner {
    state: Mutex<PaneOutputState>,
    generation: AtomicU64,
    notify: Notify,
}

pub(crate) struct PaneOutputReceiver {
    inner: Arc<PaneOutputInner>,
    cursor: OutputCursor,
    passthrough_floor_sequence: u64,
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

impl PaneOutputState {
    fn new(event_capacity: usize, recent_byte_capacity: usize) -> Self {
        Self {
            ring: OutputRing::new(event_capacity, recent_byte_capacity),
            passthroughs: VecDeque::with_capacity(PANE_OUTPUT_PASSTHROUGH_CAPACITY),
        }
    }

    fn push(&mut self, bytes: Vec<u8>, passthroughs: Vec<TerminalPassthrough>) -> u64 {
        let sequence = self.ring.push(bytes).sequence();
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
        let (sequence, result) = {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("pane output state mutex must not be poisoned");
            if !generation_matches(self.current_generation(), generation) {
                return None;
            }
            let (result, passthroughs) = build_side_effects(&bytes);
            let sequence = state.push(bytes, passthroughs);
            (sequence, result)
        };
        self.inner.notify.notify_waiters();
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
        PaneOutputReceiver {
            inner: Arc::clone(&self.inner),
            cursor,
            passthrough_floor_sequence,
        }
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
        PaneOutputReceiver {
            inner: Arc::clone(&self.inner),
            cursor,
            passthrough_floor_sequence,
        }
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
        PaneOutputReceiver {
            inner: Arc::clone(&self.inner),
            cursor: OutputRing::cursor_from_sequence(sequence),
            passthrough_floor_sequence,
        }
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
        self.inner.notify.notify_waiters();
    }

    fn push_for_generation(
        &self,
        generation: Option<u64>,
        bytes: Vec<u8>,
        passthroughs: Vec<TerminalPassthrough>,
    ) -> Option<u64> {
        let sequence = {
            let mut state = self
                .inner
                .state
                .lock()
                .expect("pane output state mutex must not be poisoned");
            if !generation_matches(self.current_generation(), generation) {
                return None;
            }
            state.push(bytes, passthroughs)
        };
        self.inner.notify.notify_waiters();
        Some(sequence)
    }
}

fn generation_matches(current: u64, generation: Option<u64>) -> bool {
    match generation {
        None => true,
        Some(generation) => current == generation,
    }
}

impl PaneOutputReceiver {
    pub(crate) async fn recv(&mut self) -> OutputCursorItem {
        loop {
            let inner = Arc::clone(&self.inner);
            let notified = inner.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(item) = self.try_recv() {
                return item;
            }
            notified.await;
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
        self.inner
            .state
            .lock()
            .expect("pane output state mutex must not be poisoned")
            .poll_cursor_batch(&mut self.cursor, self.passthrough_floor_sequence, limit)
    }

    pub(crate) const fn cursor(&self) -> &OutputCursor {
        &self.cursor
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
}
