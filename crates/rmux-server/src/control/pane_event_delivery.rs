use std::collections::{BTreeMap, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
use std::sync::{Arc, Mutex, Weak};
use std::time::Instant;

use rmux_core::events::OutputCursorItem;
use tokio::sync::{mpsc, oneshot, Notify, OwnedSemaphorePermit, Semaphore};

use super::subscriptions::{cursor_item_event, PaneEvent};
#[cfg(unix)]
use super::ControlClientFlags;
use crate::pane_io::{PaneOutputReceiver, PaneOutputSender};

const PENDING_PANE_EVENT_BYTES: usize = 4 * 1024 * 1024;
const PANE_EVENT_OVERHEAD: usize = 2 * std::mem::size_of::<PaneEvent>();

#[cfg(all(test, unix))]
mod tests;

#[derive(Clone)]
pub(super) struct PaneEventSender {
    sender: mpsc::Sender<PaneEvent>,
    pending_budget: Arc<Semaphore>,
    sources: Arc<Mutex<Vec<Weak<Delivery>>>>,
    progress: Arc<Notify>,
    #[cfg(unix)]
    pause_after_millis: Arc<Mutex<Option<u64>>>,
}

impl From<mpsc::Sender<PaneEvent>> for PaneEventSender {
    fn from(sender: mpsc::Sender<PaneEvent>) -> Self {
        Self {
            sender,
            pending_budget: Arc::new(Semaphore::new(PENDING_PANE_EVENT_BYTES)),
            sources: Arc::new(Mutex::new(Vec::new())),
            progress: Arc::new(Notify::new()),
            #[cfg(unix)]
            pause_after_millis: Arc::new(Mutex::new(None)),
        }
    }
}

struct Delivery {
    source: PaneOutputSender,
    sent_next: AtomicU64,
    inflight: Mutex<BTreeMap<u64, RetainedOutput>>,
    fences: Mutex<BTreeMap<u64, usize>>,
    stopped: AtomicBool,
    progress: Arc<Notify>,
}

struct RetainedOutput {
    received_at: Instant,
    admission: Option<OwnedSemaphorePermit>,
}

pub(super) struct PaneEventRetention {
    delivery: Arc<Delivery>,
    sequence: u64,
}

impl PaneEventRetention {
    pub(super) fn into_output_age(self) -> Option<Self> {
        if !cfg!(unix) {
            return None;
        }
        let mut inflight = self.delivery.inflight.lock().expect("pane admissions");
        // Release copy capacity and fences, retaining age until write/discard.
        let output = inflight
            .get_mut(&self.sequence)
            .expect("owned pane admission");
        drop(output.admission.take());
        drop(inflight);
        self.delivery.progress.notify_waiters();
        Some(self)
    }
}

impl std::fmt::Debug for PaneEventRetention {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("PaneEventRetention")
            .field("sequence", &self.sequence)
            .finish()
    }
}

impl Drop for PaneEventRetention {
    fn drop(&mut self) {
        let mut inflight = self.delivery.inflight.lock().expect("pane admissions");
        inflight.remove(&self.sequence);
        drop(inflight);
        self.delivery.progress.notify_waiters();
    }
}

struct DeliveryLifetime(Arc<Delivery>);

impl Drop for DeliveryLifetime {
    fn drop(&mut self) {
        self.0.stopped.store(true, Ordering::Release);
        self.0.progress.notify_waiters();
    }
}

pub(super) struct PendingBarrier {
    boundaries: Vec<(Arc<Delivery>, u64)>,
}

impl Drop for PendingBarrier {
    fn drop(&mut self) {
        for (source, next) in &self.boundaries {
            let mut fences = source.fences.lock().expect("pane admission fences");
            let count = fences.get_mut(next).expect("owned pane fence");
            *count -= 1;
            if *count == 0 {
                fences.remove(next);
            }
            drop(fences);
            source.progress.notify_waiters();
        }
    }
}

impl PaneEventSender {
    #[cfg(unix)]
    pub(super) fn set_pause_after(&self, flags: ControlClientFlags) {
        *self.pause_after_millis.lock().expect("control pause age") = flags.pause_after_millis;
        self.progress.notify_waiters();
    }

    fn pause_deadline(&self, delivery: &Delivery) -> Option<tokio::time::Instant> {
        #[cfg(unix)]
        {
            let limit = (*self.pause_after_millis.lock().expect("control pause age"))?;
            let oldest = delivery
                .inflight
                .lock()
                .expect("pane admissions")
                .first_key_value()
                .map(|(_, output)| output.received_at)?;
            tokio::time::Instant::from_std(oldest)
                .checked_add(std::time::Duration::from_millis(limit.max(1)))
        }
        #[cfg(not(unix))]
        {
            let _ = delivery;
            None
        }
    }

    pub(super) fn subscribe(
        &self,
        pane_id: u32,
        epoch: u64,
        receiver: PaneOutputReceiver,
        source: PaneOutputSender,
        stop: oneshot::Receiver<()>,
    ) {
        #[cfg(unix)]
        let receiver = {
            let mut receiver = receiver;
            receiver.protect_control_cursor();
            receiver
        };
        // Register the cursor before spawning: a reply can run before the task.
        let delivery = Arc::new(Delivery {
            source,
            sent_next: AtomicU64::new(receiver.control_next_sequence()),
            inflight: Mutex::new(BTreeMap::new()),
            fences: Mutex::new(BTreeMap::new()),
            stopped: AtomicBool::new(false),
            progress: self.progress.clone(),
        });
        let mut sources = self.sources.lock().expect("pane delivery sources");
        sources.retain(|source| source.strong_count() != 0);
        sources.push(Arc::downgrade(&delivery));
        drop(sources);
        tokio::spawn(deliver_pane_events(
            pane_id,
            epoch,
            receiver,
            stop,
            self.clone(),
            delivery,
        ));
    }

    pub(super) async fn pending_barrier(&self) -> PendingBarrier {
        // Fence published output, including uncopied bytes; later output cannot extend it.
        let boundaries = self
            .sources
            .lock()
            .expect("pane delivery sources")
            .iter()
            .filter_map(Weak::upgrade)
            .map(|source| {
                let next = source.source.capture_with_next_sequence(|| ()).0;
                *source
                    .fences
                    .lock()
                    .expect("pane admission fences")
                    .entry(next)
                    .or_default() += 1;
                (source, next)
            })
            .collect::<Vec<_>>();
        let barrier = PendingBarrier { boundaries };
        loop {
            let notified = self.progress.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if barrier.boundaries.iter().all(|(source, next)| {
                (source.stopped.load(Ordering::Acquire)
                    || source.sent_next.load(Ordering::Acquire) >= *next)
                    && source
                        .inflight
                        .lock()
                        .expect("pane admissions")
                        .range(..*next)
                        .all(|(_, output)| output.admission.is_none())
            }) {
                return barrier;
            }
            notified.await;
        }
    }
}

async fn deliver_pane_events(
    pane_id: u32,
    epoch: u64,
    mut receiver: PaneOutputReceiver,
    mut stop: oneshot::Receiver<()>,
    sender: PaneEventSender,
    delivery: Arc<Delivery>,
) {
    let _lifetime = DeliveryLifetime(delivery.clone());
    let mut pending = VecDeque::new();
    loop {
        // Cooperate even on immediate admission so sibling cursors keep up.
        tokio::task::coop::consume_budget().await;
        // Arm drain/flag changes before reading the age (no missed wake).
        let progress = sender.progress.notified();
        tokio::pin!(progress);
        progress.as_mut().enable();
        let pause_deadline = sender.pause_deadline(&delivery);
        if pause_deadline.is_some_and(|deadline| deadline <= tokio::time::Instant::now()) {
            break;
        }
        let ready = delivery.source.control_output_ready();
        tokio::pin!(ready);
        // Register before inspecting the cursor so publication cannot miss us.
        ready.as_mut().enable();
        if matches!(
            stop.try_recv(),
            Ok(()) | Err(oneshot::error::TryRecvError::Closed)
        ) || sender.sender.is_closed()
        {
            return;
        }
        if !pending.is_empty() {
            if let Ok(permit) = sender.sender.try_reserve() {
                send_pending(permit, &mut pending, &delivery);
                continue;
            }
        }
        let may_read = delivery
            .fences
            .lock()
            .expect("pane admission fences")
            .first_key_value()
            .is_none_or(|(next, _)| receiver.control_next_sequence() < *next);
        let admitted = if may_read {
            receiver.try_recv_admitted(|bytes| {
                // Oversized indivisible records report lag without a copy or a wait.
                let cost = if bytes > PENDING_PANE_EVENT_BYTES - PANE_EVENT_OVERHEAD {
                    PANE_EVENT_OVERHEAD
                } else {
                    bytes + PANE_EVENT_OVERHEAD
                };
                sender
                    .pending_budget
                    .clone()
                    .try_acquire_many_owned(cost as u32)
                    .ok()
            })
        } else {
            None
        };
        if let Some((item, permit)) = admitted {
            let next = receiver.control_next_sequence();
            let sequence = match &item {
                OutputCursorItem::Event(event) => event.sequence(),
                OutputCursorItem::Gap(gap) => gap.expected_sequence(),
            };
            let mut event = match item {
                OutputCursorItem::Event(event)
                    if event.byte_len() > PENDING_PANE_EVENT_BYTES - PANE_EVENT_OVERHEAD =>
                {
                    PaneEvent::Lagged {
                        pane_id,
                        epoch,
                        expected_sequence: event.sequence(),
                        resume_sequence: next,
                        missed_events: 1,
                        retention: None,
                    }
                }
                item => cursor_item_event(pane_id, epoch, item),
            };
            delivery.inflight.lock().expect("pane admissions").insert(
                sequence,
                RetainedOutput {
                    received_at: Instant::now(),
                    admission: Some(permit),
                },
            );
            let retention = PaneEventRetention {
                delivery: delivery.clone(),
                sequence,
            };
            match &mut event {
                PaneEvent::Data {
                    retention: slot, ..
                }
                | PaneEvent::CursorGap {
                    retention: slot, ..
                }
                | PaneEvent::Lagged {
                    retention: slot, ..
                } => *slot = Some(retention),
                PaneEvent::PauseAfter { .. } => unreachable!("cursor item cannot pause"),
            }
            pending.push_back((event, next));
            continue;
        }
        tokio::select! {
            biased;
            _ = &mut stop => return,
            _ = async {
                match pause_deadline {
                    Some(deadline) => tokio::time::sleep_until(deadline).await,
                    None => std::future::pending().await,
                }
            } => {
                // Recheck age: retention may have drained before this timer was polled.
            },
            permit = sender.sender.reserve(), if !pending.is_empty() => {
                let Ok(permit) = permit else { return };
                send_pending(permit, &mut pending, &delivery);
            }
            _ = progress => {}
            _ = &mut ready => {}
        }
    }
    // Release the PTY independently of a blocked write, then queue pause behind
    // its complete line; never cancel a partial control frame.
    drop(receiver);
    drop(pending);
    drop(_lifetime);
    tokio::select! {
        biased;
        _ = &mut stop => {},
        _ = sender.sender.send(PaneEvent::PauseAfter { pane_id, epoch }) => {},
    }
}

fn send_pending(
    permit: mpsc::Permit<'_, PaneEvent>,
    pending: &mut VecDeque<(PaneEvent, u64)>,
    delivery: &Delivery,
) {
    let (event, next) = pending.pop_front().expect("pending pane event");
    permit.send(event);
    delivery.sent_next.store(next, Ordering::Release);
    delivery.progress.notify_waiters();
}
