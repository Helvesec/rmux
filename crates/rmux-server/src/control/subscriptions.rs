use std::collections::{HashMap, HashSet};
use std::io;
use std::time::Instant;

use rmux_core::events::OutputCursorItem;
use rmux_proto::{
    format_extended_output_line, format_output_line, format_pause_line, SessionName,
    CONTROL_BUFFER_HIGH,
};
use tokio::sync::mpsc;
use tracing::warn;

use super::{ControlClientFlags, ControlOutputQueue};
use crate::handler::RequestHandler;

#[derive(Debug)]
pub(super) enum PaneEvent {
    Data {
        pane_id: u32,
        bytes: Vec<u8>,
        received_at: Instant,
    },
    Lagged {
        pane_id: u32,
        expected_sequence: u64,
        resume_sequence: u64,
        missed_events: u64,
    },
}

#[derive(Debug)]
pub(super) struct PaneSubscription {
    stop_tx: tokio::sync::oneshot::Sender<()>,
}

pub(super) async fn refresh_subscriptions(
    handler: &RequestHandler,
    session_name: Option<&SessionName>,
    subscriptions: &mut HashMap<u32, PaneSubscription>,
    pane_event_tx: mpsc::Sender<PaneEvent>,
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
        let mut receiver = sender.subscribe_from_oldest();
        let (stop_tx, mut stop_rx) = tokio::sync::oneshot::channel();
        let pane_event_tx = pane_event_tx.clone();
        tokio::spawn(async move {
            for event in retained_output_events(pane_id, &mut receiver) {
                tokio::select! {
                    _ = &mut stop_rx => return,
                    result = pane_event_tx.send(event) => {
                        if result.is_err() {
                            return;
                        }
                    }
                }
            }
            loop {
                tokio::select! {
                    _ = &mut stop_rx => return,
                    item = receiver.recv() => {
                        let event = cursor_item_event(pane_id, item);
                        tokio::select! {
                            _ = &mut stop_rx => return,
                            result = pane_event_tx.send(event) => {
                                if result.is_err() {
                                    return;
                                }
                            }
                        }
                    }
                }
            }
        });
        subscriptions.insert(pane_id, PaneSubscription { stop_tx });
    }
}

pub(super) fn handle_pane_event(
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
        PaneEvent::Lagged {
            pane_id,
            expected_sequence,
            resume_sequence,
            missed_events,
        } => {
            warn!(
                pane_id,
                expected_sequence,
                resume_sequence,
                missed_events,
                "control pane output cursor lagged"
            );
        }
    }

    Ok(())
}

pub(super) fn drain_ready_pane_events(
    pane_event_rx: &mut mpsc::Receiver<PaneEvent>,
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

fn retained_output_events(
    pane_id: u32,
    receiver: &mut crate::pane_io::PaneOutputReceiver,
) -> Vec<PaneEvent> {
    let mut events = Vec::new();
    let mut initial_bytes = Vec::new();
    while let Some(item) = receiver.try_recv() {
        match item {
            OutputCursorItem::Event(event) => {
                initial_bytes.extend(event.into_bytes());
            }
            OutputCursorItem::Gap(gap) => {
                push_initial_bytes(pane_id, &mut initial_bytes, &mut events);
                events.push(PaneEvent::Lagged {
                    pane_id,
                    expected_sequence: gap.expected_sequence(),
                    resume_sequence: gap.resume_sequence(),
                    missed_events: gap.missed_events(),
                });
            }
        }
    }
    push_initial_bytes(pane_id, &mut initial_bytes, &mut events);
    events
}

fn push_initial_bytes(pane_id: u32, initial_bytes: &mut Vec<u8>, events: &mut Vec<PaneEvent>) {
    if initial_bytes.is_empty() {
        return;
    }
    events.push(PaneEvent::Data {
        pane_id,
        bytes: std::mem::take(initial_bytes),
        received_at: Instant::now(),
    });
}

fn cursor_item_event(pane_id: u32, item: OutputCursorItem) -> PaneEvent {
    match item {
        OutputCursorItem::Event(event) => PaneEvent::Data {
            pane_id,
            bytes: event.into_bytes(),
            received_at: Instant::now(),
        },
        OutputCursorItem::Gap(gap) => PaneEvent::Lagged {
            pane_id,
            expected_sequence: gap.expected_sequence(),
            resume_sequence: gap.resume_sequence(),
            missed_events: gap.missed_events(),
        },
    }
}
