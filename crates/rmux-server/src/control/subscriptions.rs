use std::collections::{HashMap, HashSet};
use std::io;
use std::time::Instant;

use rmux_core::events::OutputCursorItem;
use rmux_proto::{format_extended_output_line, format_output_line, SessionName};
use tokio::sync::mpsc;
use tracing::warn;

use super::{ControlClientFlags, ControlOutputQueue, ControlPaneOutputStates, PaneEventSender};
use crate::handler::{ControlClientIdentity, RequestHandler};
use crate::pane_io::{PaneOutputReadDemandGuard, PaneOutputSender};

pub(super) enum PaneSubscriptionStart<'a> {
    Oldest,
    Now,
    Sequences(&'a [(u32, u64)]),
}

#[derive(Debug)]
pub(super) enum PaneEvent {
    PauseAfter {
        pane_id: u32,
        epoch: u64,
    },
    Data {
        pane_id: u32,
        epoch: u64,
        bytes: Vec<u8>,
        received_at: Instant,
        retention: Option<super::pane_event_delivery::PaneEventRetention>,
    },
    CursorGap {
        pane_id: u32,
        epoch: u64,
        expected_sequence: u64,
        resume_sequence: u64,
        missed_events: u64,
        retention: Option<super::pane_event_delivery::PaneEventRetention>,
    },
    // An indivisible source record exceeding the client budget is terminal.
    Lagged {
        pane_id: u32,
        epoch: u64,
        expected_sequence: u64,
        resume_sequence: u64,
        missed_events: u64,
        retention: Option<super::pane_event_delivery::PaneEventRetention>,
    },
}

impl PaneEvent {
    pub(super) const fn pane_id(&self) -> u32 {
        match self {
            Self::Data { pane_id, .. }
            | Self::PauseAfter { pane_id, .. }
            | Self::CursorGap { pane_id, .. }
            | Self::Lagged { pane_id, .. } => *pane_id,
        }
    }
}

#[derive(Debug)]
pub(super) struct PaneSubscription {
    stop_tx: tokio::sync::oneshot::Sender<()>,
    epoch: u64,
    _read_demand: PaneOutputReadDemandGuard,
}

pub(super) struct PaneSubscriptionState<'a> {
    pub(super) subscriptions: &'a mut HashMap<u32, PaneSubscription>,
    pub(super) reader_demands: &'a mut HashMap<u32, PaneOutputReadDemandGuard>,
    pub(super) pane_output_states: &'a mut ControlPaneOutputStates,
}

pub(super) async fn refresh_subscriptions(
    handler: &RequestHandler,
    control_identity: ControlClientIdentity,
    session_name: Option<&SessionName>,
    state: PaneSubscriptionState<'_>,
    pane_event_tx: PaneEventSender,
    start: PaneSubscriptionStart<'_>,
) {
    let PaneSubscriptionState {
        subscriptions,
        reader_demands,
        pane_output_states,
    } = state;
    let live_pane_ids = handler.control_pane_output_ids().await;
    pane_output_states.prune_missing(&live_pane_ids);
    let Some(session_name) = session_name else {
        clear_subscriptions(subscriptions);
        reader_demands.clear();
        return;
    };
    let panes = handler
        .control_session_panes_for_identity(control_identity, session_name)
        .await
        .unwrap_or_default();
    let desired = panes
        .iter()
        .filter_map(|(pane_id, _)| {
            pane_output_states
                .wants_subscription(*pane_id)
                .then_some(*pane_id)
        })
        .collect::<HashSet<_>>();
    let paused = panes
        .iter()
        .filter_map(|(pane_id, _)| pane_output_states.is_paused(*pane_id).then_some(*pane_id))
        .collect::<HashSet<_>>();
    for (_, subscription) in subscriptions.extract_if(|pane_id, subscription| {
        !desired.contains(pane_id) || subscription.epoch != pane_output_states.epoch_for(*pane_id)
    }) {
        let _ = subscription.stop_tx.send(());
    }
    reader_demands.retain(|pane_id, _| paused.contains(pane_id));

    for (pane_id, sender) in panes {
        pane_output_states.ensure_pane_epoch(pane_id);
        reconcile_pane_subscription(
            pane_id,
            sender,
            subscriptions,
            reader_demands,
            pane_output_states,
            pane_event_tx.clone(),
            &start,
        );
    }
}

pub(super) async fn refresh_subscription_for_pane(
    handler: &RequestHandler,
    control_identity: ControlClientIdentity,
    session_name: Option<&SessionName>,
    state: PaneSubscriptionState<'_>,
    pane_event_tx: PaneEventSender,
    pane_id: u32,
    start: PaneSubscriptionStart<'_>,
) {
    let PaneSubscriptionState {
        subscriptions,
        reader_demands,
        pane_output_states,
    } = state;
    let live_pane_ids = handler.control_pane_output_ids().await;
    pane_output_states.prune_missing(&live_pane_ids);
    let Some(session_name) = session_name else {
        stop_pane_subscription(subscriptions, pane_id);
        reader_demands.remove(&pane_id);
        return;
    };
    let sender = handler
        .control_session_panes_for_identity(control_identity, session_name)
        .await
        .ok()
        .and_then(|panes| {
            panes
                .into_iter()
                .find_map(|(candidate, sender)| (candidate == pane_id).then_some(sender))
        });
    let Some(sender) = sender else {
        stop_pane_subscription(subscriptions, pane_id);
        reader_demands.remove(&pane_id);
        return;
    };
    reconcile_pane_subscription(
        pane_id,
        sender,
        subscriptions,
        reader_demands,
        pane_output_states,
        pane_event_tx,
        &start,
    );
}

fn reconcile_pane_subscription(
    pane_id: u32,
    sender: PaneOutputSender,
    subscriptions: &mut HashMap<u32, PaneSubscription>,
    reader_demands: &mut HashMap<u32, PaneOutputReadDemandGuard>,
    pane_output_states: &ControlPaneOutputStates,
    pane_event_tx: PaneEventSender,
    start: &PaneSubscriptionStart<'_>,
) {
    if pane_output_states.is_paused(pane_id) {
        stop_pane_subscription(subscriptions, pane_id);
        reader_demands
            .entry(pane_id)
            .or_insert_with(|| sender.acquire_read_demand());
        return;
    }
    reader_demands.remove(&pane_id);
    if !pane_output_states.wants_subscription(pane_id) {
        stop_pane_subscription(subscriptions, pane_id);
        return;
    }

    let epoch = pane_output_states.epoch_for(pane_id);
    if subscriptions
        .get(&pane_id)
        .is_some_and(|subscription| subscription.epoch == epoch)
    {
        return;
    }
    stop_pane_subscription(subscriptions, pane_id);

    let receiver = match start {
        PaneSubscriptionStart::Oldest => sender.subscribe_from_oldest(),
        PaneSubscriptionStart::Now => sender.subscribe(),
        PaneSubscriptionStart::Sequences(sequences) => sequences
            .iter()
            .find_map(|(candidate, sequence)| {
                (*candidate == pane_id).then(|| sender.subscribe_from_sequence(*sequence))
            })
            .unwrap_or_else(|| sender.subscribe_from_oldest()),
    };
    let (stop_tx, stop_rx) = tokio::sync::oneshot::channel();
    pane_event_tx.subscribe(pane_id, epoch, receiver, sender.clone(), stop_rx);
    subscriptions.insert(
        pane_id,
        PaneSubscription {
            stop_tx,
            epoch,
            _read_demand: sender.acquire_read_demand(),
        },
    );
}

fn stop_pane_subscription(subscriptions: &mut HashMap<u32, PaneSubscription>, pane_id: u32) {
    if let Some(subscription) = subscriptions.remove(&pane_id) {
        let _ = subscription.stop_tx.send(());
    }
}

pub(super) fn handle_pane_event(
    event: PaneEvent,
    output_queue: &mut ControlOutputQueue,
    paused_panes: &mut HashSet<u32>,
    pane_output_states: &mut ControlPaneOutputStates,
    flags: ControlClientFlags,
) -> io::Result<bool> {
    if flags.no_output {
        return Ok(false);
    }

    match event {
        PaneEvent::PauseAfter { pane_id, epoch } => {
            if pane_output_states.accepts_event(pane_id, epoch)
                && pane_output_states.apply_pause_after(pane_id, output_queue)
            {
                paused_panes.insert(pane_id);
            }
        }
        PaneEvent::Data {
            pane_id,
            epoch,
            bytes,
            received_at,
            retention,
        } => {
            if !pane_output_states.accepts_event(pane_id, epoch) {
                return Ok(false);
            }
            // A pending event can age in the channel while a prior transport
            // write is blocked. Include that wait as well as queued pane output;
            // pause-after is a time limit, independent of the burst size.
            let pending_age = output_queue
                .oldest_pane_output_age(pane_id)
                .unwrap_or_default()
                .max(received_at.elapsed());
            if flags.pause_after_millis.is_some_and(|limit| {
                let age_ms = pending_age.as_millis();
                age_ms != 0 && age_ms >= u128::from(limit)
            }) && pane_output_states.apply_pause_after(pane_id, output_queue)
            {
                paused_panes.insert(pane_id);
                return Ok(false);
            }
            let age_ms = received_at.elapsed().as_millis().min(u128::from(u64::MAX)) as u64;
            let line = if flags.uses_extended_output() {
                format_extended_output_line(pane_id, age_ms, &bytes)
            } else {
                format_output_line(pane_id, &bytes)
            };
            let age =
                retention.and_then(super::pane_event_delivery::PaneEventRetention::into_output_age);
            output_queue.enqueue_pane_output(pane_id, line.into_bytes(), age);
        }
        PaneEvent::CursorGap {
            pane_id,
            epoch,
            expected_sequence,
            resume_sequence,
            missed_events,
            retention: _retention,
        } => {
            if !pane_output_states.accepts_event(pane_id, epoch) {
                return Ok(false);
            }
            warn!(
                pane_id,
                expected_sequence,
                resume_sequence,
                missed_events,
                "pausing control pane after output cursor retention gap"
            );
            // The bounded journal can evict a stalled cursor while another
            // client keeps reading. Pause only this pane: invalidate old events
            // and retain the control connection for commands and resumption.
            if pane_output_states.apply_pause_after(pane_id, output_queue) {
                paused_panes.insert(pane_id);
            }
        }
        PaneEvent::Lagged {
            pane_id,
            epoch,
            expected_sequence,
            resume_sequence,
            missed_events,
            retention: _retention,
        } => {
            if !pane_output_states.accepts_event(pane_id, epoch) {
                return Ok(false);
            }
            warn!(
                pane_id,
                expected_sequence,
                resume_sequence,
                missed_events,
                "control pane output cursor lagged"
            );
            output_queue.enqueue_exit(Some("too far behind"));
            return Ok(true);
        }
    }

    Ok(false)
}

pub(super) async fn drain_ready_pane_events(
    pane_event_rx: &mut mpsc::Receiver<PaneEvent>,
    pane_event_tx: &PaneEventSender,
    output_queue: &mut ControlOutputQueue,
    paused_panes: &mut HashSet<u32>,
    pane_output_states: &mut ControlPaneOutputStates,
    flags: ControlClientFlags,
) -> io::Result<bool> {
    // Freeze source frontiers, including records not yet copied. Keep their
    // admission fences through the final finite channel snapshot, so live
    // output cannot indefinitely postpone a reply/notification/error.
    let barrier = pane_event_tx.pending_barrier();
    tokio::pin!(barrier);
    loop {
        tokio::select! {
            biased;
            retention = &mut barrier => {
                let _fence = retention;
                return drain_channel_pane_events(pane_event_rx, output_queue,
                    paused_panes, pane_output_states, flags);
            }
            event = pane_event_rx.recv() => {
                let Some(event) = event else { return Ok(false) };
                if handle_pane_event(event, output_queue, paused_panes, pane_output_states, flags)? {
                    return Ok(true);
                }
            }
        }
    }
}

pub(super) fn drain_channel_pane_events(
    pane_event_rx: &mut mpsc::Receiver<PaneEvent>,
    output_queue: &mut ControlOutputQueue,
    paused_panes: &mut HashSet<u32>,
    pane_output_states: &mut ControlPaneOutputStates,
    flags: ControlClientFlags,
) -> io::Result<bool> {
    for _ in 0..pane_event_rx.len() {
        let Ok(event) = pane_event_rx.try_recv() else {
            return Ok(false);
        };
        if handle_pane_event(event, output_queue, paused_panes, pane_output_states, flags)? {
            return Ok(true);
        }
    }
    Ok(false)
}

fn clear_subscriptions(subscriptions: &mut HashMap<u32, PaneSubscription>) {
    for (_, subscription) in subscriptions.drain() {
        let _ = subscription.stop_tx.send(());
    }
}

pub(super) fn cursor_item_event(pane_id: u32, epoch: u64, item: OutputCursorItem) -> PaneEvent {
    match item {
        OutputCursorItem::Event(event) => PaneEvent::Data {
            pane_id,
            epoch,
            bytes: event.into_bytes(),
            received_at: Instant::now(),
            retention: None,
        },
        OutputCursorItem::Gap(gap) => PaneEvent::CursorGap {
            pane_id,
            epoch,
            expected_sequence: gap.expected_sequence(),
            resume_sequence: gap.resume_sequence(),
            missed_events: gap.missed_events(),
            retention: None,
        },
    }
}

#[cfg(test)]
mod fifo_tests;
