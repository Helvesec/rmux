//! Ordinary input of a client queued behind the bindings its input path was released from.
//!
//! Whether a frame queues is decided when it arrives. Deciding and inserting happen under the
//! registry lock that the end of a released binding also takes, so a frame is either queued
//! before the last end - which then starts its replay - or not queued at all; one replay step
//! (take the next frame, flush what the replay retained, or pause and put the retained input back
//! in front) is likewise a single critical section, so a binding ending during a pause always
//! finds the queue it has to replay.

use std::sync::PoisonError;

use rmux_core::kitty_keyboard::Event;
use rmux_proto::AttachedWindowsConsoleKey;

use super::super::super::RequestHandler;
use super::super::ActiveAttachIdentity;
use super::{AttachedInputWaits, QueuedFrame, QueuedUnit};

mod queued_identity;
pub(in crate::handler) use queued_identity::check_queued_input_target;
pub(super) use queued_identity::QueuedInputIdentity;

mod queued_split;
use queued_split::FocusSplit;
pub(super) use queued_split::PasteScan;
pub(in crate::handler) use queued_split::UnfinishedTail;

tokio::task_local! {
    static REPLAYING_QUEUED_INPUT: u32;
}

/// How an input frame of a client whose ordinary input may have to wait goes on.
pub(in crate::handler) enum OrderedInput {
    /// Nothing has to wait: the input path handles the whole frame now, part after part.
    HandleNow(FrameCursor),
    /// The ordinary input waits - queued behind the released bindings, or retained as the start
    /// of a focus report until its next bytes arrive - while the focus reports the frame held act
    /// now.
    Waits { focus_reports: Vec<u8> },
}

/// Where a frame the input path handles now stands against the client's released bindings, from
/// one part of it to the next.
#[derive(Debug, Clone, Copy)]
pub(in crate::handler) struct FrameCursor {
    /// Ordinary input of the client already waited when the frame arrived, so a surface took it:
    /// what a part hands back meets the surfaces as they are then. Otherwise the frame went the
    /// ordinary way, and a surface its own keys open does not take the rest of it.
    waited_on_arrival: bool,
    /// A part of the frame waits behind released bindings; everything after it waits too.
    part_waits: bool,
    /// While a unit is replayed: how many units its rest already went back as, at the front of
    /// the queue; later parts go right after them.
    requeued: usize,
}

impl FrameCursor {
    /// A frame, or input handled like one, that arrived while ordinary input `waited` or not.
    pub(in crate::handler) fn new(waited_on_arrival: bool) -> Self {
        Self {
            waited_on_arrival,
            part_waits: false,
            requeued: 0,
        }
    }
}

/// A part of a frame the input path handles now.
pub(in crate::handler) enum FramePart {
    Bytes(Vec<u8>),
    KeyboardEvent(Event),
}

/// How an input frame reaches the input path.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) enum InputArrival {
    /// Bytes the client sent; the next bytes may complete a sequence they end with.
    Live,
    /// Retained input the escape time flushed; nothing completes it any more.
    EscapeFlush,
}

/// What a replay does next.
enum ReplayStep {
    /// Replay this frame.
    Frame(QueuedFrame),
    /// Nothing more is queued: flush the input the replay still retains, then look again.
    FlushRetained,
    /// A released binding runs again; the retained input went back in front of the queue and the
    /// end of that binding replays it.
    Paused,
    /// Nothing is queued or retained any more.
    Done,
}

impl AttachedInputWaits {
    /// If ordinary input of `attach_pid` has to wait - a released binding runs, or earlier input
    /// still waits or is being replayed - lets `decide` split the input against the bracketed
    /// paste the queued input leaves open and queues the units it returns, in order; `None` when
    /// nothing has to wait.
    fn queue_if_ordered<T>(
        &self,
        attach_pid: u32,
        decide: impl FnOnce(&PasteScan) -> (Vec<QueuedFrame>, T),
    ) -> Option<T> {
        let mut waits = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if !waits.released_work.contains_key(&attach_pid)
            && !waits.queued_input.contains_key(&attach_pid)
        {
            return None;
        }
        let paste = waits
            .queued_input
            .get(&attach_pid)
            .map(|queued| queued.paste.clone())
            .unwrap_or_default();
        let (frames, decided) = decide(&paste);
        if !frames.is_empty() {
            let queued = waits.queued_input.entry(attach_pid).or_default();
            for frame in frames {
                queued.paste.follow(&frame.unit);
                queued.frames.push_back(frame);
            }
        }
        Some(decided)
    }

    /// Whether ordinary input of `attach_pid` may have to wait; decided again by
    /// `queue_if_ordered` before anything is queued.
    fn may_order_ordinary_input(&self, attach_pid: u32) -> bool {
        let waits = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        waits.released_work.contains_key(&attach_pid)
            || waits.queued_input.contains_key(&attach_pid)
    }

    /// The next step of `attach_pid`'s replay, which retains `retained` from the frames it
    /// replayed as `identity`.
    fn next_replay_step(
        &self,
        attach_pid: u32,
        retained: &mut Vec<u8>,
        identity: Option<QueuedInputIdentity>,
    ) -> ReplayStep {
        let mut waits = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        let released_work_runs = waits.released_work.contains_key(&attach_pid);
        let Some(queued) = waits.queued_input.get_mut(&attach_pid) else {
            return ReplayStep::Done;
        };
        if released_work_runs {
            if let Some(identity) = identity.filter(|_| !retained.is_empty()) {
                queued.frames.push_front(QueuedFrame {
                    identity,
                    unit: QueuedUnit::Live {
                        bytes: std::mem::take(retained),
                        windows_console_key: None,
                    },
                });
            }
            if queued.frames.is_empty() {
                waits.queued_input.remove(&attach_pid);
            } else {
                queued.replaying = false;
            }
            return ReplayStep::Paused;
        }
        if let Some(frame) = queued.frames.pop_front() {
            return ReplayStep::Frame(frame);
        }
        if !retained.is_empty() {
            return ReplayStep::FlushRetained;
        }
        waits.queued_input.remove(&attach_pid);
        ReplayStep::Done
    }

    /// While `attach_pid`'s replay replays a unit: if a binding one of its parts released still
    /// runs, or a part of it already went back, puts `frames` - the rest of the unit - back in the
    /// queue, in order, after what went back before them and ahead of the units still queued; says
    /// whether it did. The replay then pauses, and the end of that binding resumes it.
    fn requeue_rest_in_replay(
        &self,
        attach_pid: u32,
        frames: Vec<QueuedFrame>,
        cursor: &mut FrameCursor,
    ) -> bool {
        let mut waits = self.0.lock().unwrap_or_else(PoisonError::into_inner);
        if !cursor.part_waits && !waits.released_work.contains_key(&attach_pid) {
            return false;
        }
        let queued = waits.queued_input.entry(attach_pid).or_default();
        for frame in frames {
            queued.frames.insert(cursor.requeued, frame);
            cursor.requeued += 1;
        }
        cursor.part_waits = true;
        true
    }

    /// Whether a binding released while `attach_pid`'s input is replayed still runs.
    fn released_work_runs(&self, attach_pid: u32) -> bool {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .released_work
            .contains_key(&attach_pid)
    }

    fn discard_queued_input(&self, attach_pid: u32) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .queued_input
            .remove(&attach_pid);
    }

    #[cfg(test)]
    pub(in crate::handler) fn released_work_count(&self, attach_pid: u32) -> usize {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .released_work
            .get(&attach_pid)
            .copied()
            .unwrap_or(0)
    }

    #[cfg(test)]
    pub(in crate::handler) fn queued_frame_count(&self, attach_pid: u32) -> Option<usize> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .queued_input
            .get(&attach_pid)
            .map(|queued| queued.frames.len())
    }
}

impl RequestHandler {
    /// Decides whether this input frame of the client waits behind the bindings its input path was
    /// released from. Input that a popup, menu, prompt or display-panes takes goes on at once, and
    /// so do the focus reports of a live frame: only the ordinary input around them waits.
    pub(in crate::handler) async fn order_input_behind_released_work(
        &self,
        identity: ActiveAttachIdentity,
        pending_input: &mut Vec<u8>,
        bytes: &[u8],
        windows_console_key: Option<AttachedWindowsConsoleKey>,
        arrival: InputArrival,
        unfinished: UnfinishedTail,
    ) -> OrderedInput {
        let attach_pid = identity.attach_pid();
        if replays_queued_input(attach_pid) || (pending_input.is_empty() && bytes.is_empty()) {
            return OrderedInput::HandleNow(FrameCursor::new(false));
        }
        {
            let active_attach = self.active_attach.lock().await;
            if !active_attach
                .attached_input_waits
                .may_order_ordinary_input(attach_pid)
            {
                return OrderedInput::HandleNow(FrameCursor::new(false));
            }
            if surface_takes_arriving_input(&active_attach, attach_pid) {
                return OrderedInput::HandleNow(FrameCursor::new(true));
            }
        }
        #[cfg(test)]
        test_barrier::pass(test_barrier::Point::QueueDecision, attach_pid).await;
        let mut input = pending_input.clone();
        input.extend_from_slice(bytes);
        let queued_identity = self.capture_queued_input_identity(identity).await;
        let active_attach = self.active_attach.lock().await;
        let decided = active_attach
            .attached_input_waits
            .queue_if_ordered(attach_pid, |paste| {
                let split = match (arrival, windows_console_key) {
                    (InputArrival::Live, None) => FocusSplit::of_live(&input, paste, unfinished),
                    _ => FocusSplit::whole(input),
                };
                // Focus reports only, or the start of one retained: nothing is queued, so a
                // binding ending now leaves nothing behind.
                let frames = (!split.ordinary.is_empty())
                    .then_some(QueuedFrame {
                        identity: queued_identity,
                        unit: match arrival {
                            InputArrival::Live => QueuedUnit::Live {
                                bytes: split.ordinary,
                                windows_console_key,
                            },
                            InputArrival::EscapeFlush => QueuedUnit::EscapeFlushed(split.ordinary),
                        },
                    })
                    .into_iter()
                    .collect();
                (frames, (split.focus_reports, split.retained))
            });
        let Some((focus_reports, retained)) = decided else {
            return OrderedInput::HandleNow(FrameCursor::new(false));
        };
        *pending_input = retained;
        OrderedInput::Waits { focus_reports }
    }

    /// Whether the rest of a frame, after a part of it changed the client's state - a surface that
    /// took the frame closed, a binding was released - may have to wait behind released bindings;
    /// `queue_rest_of_frame` decides it for good.
    pub(in crate::handler) async fn rest_of_frame_may_wait(
        &self,
        identity: ActiveAttachIdentity,
        cursor: &FrameCursor,
    ) -> bool {
        let attach_pid = identity.attach_pid();
        let active_attach = self.active_attach.lock().await;
        if replays_queued_input(attach_pid) {
            // A replayed part may itself have released a binding: the rest of the unit then waits
            // behind it, still bound for the key tables and the pane.
            return cursor.part_waits
                || active_attach
                    .attached_input_waits
                    .released_work_runs(attach_pid);
        }
        if !active_attach
            .attached_input_waits
            .may_order_ordinary_input(attach_pid)
        {
            return false;
        }
        // A frame that arrived while nothing waited went the ordinary way: a binding of its own
        // was released, and tmux queues the rest of the frame behind it, whatever surface that
        // binding opened.
        cursor.part_waits
            || !cursor.waited_on_arrival
            || !surface_takes_arriving_input(&active_attach, attach_pid)
    }

    /// Queues `parts`, the rest of a frame, after the input the frame retained, if ordinary input
    /// still has to wait; returns the focus reports among them, which act now, or `None` when the
    /// input path handles the rest now. A trailing start of a focus report stays retained.
    pub(in crate::handler) async fn queue_rest_of_frame(
        &self,
        identity: ActiveAttachIdentity,
        cursor: &mut FrameCursor,
        pending_input: &mut Vec<u8>,
        parts: Vec<FramePart>,
        unfinished: UnfinishedTail,
    ) -> Option<Vec<u8>> {
        let leading = pending_input.clone();
        let queued_identity = self.capture_queued_input_identity(identity).await;
        let active_attach = self.active_attach.lock().await;
        if replays_queued_input(identity.attach_pid()) {
            // Replayed bytes were split at admission: the rest goes back as it is.
            let frames = replayed_rest_units(queued_identity, leading, parts);
            if !active_attach.attached_input_waits.requeue_rest_in_replay(
                identity.attach_pid(),
                frames,
                cursor,
            ) {
                return None;
            }
            pending_input.clear();
            return Some(Vec::new());
        }
        let (focus_reports, retained) = active_attach
            .attached_input_waits
            .queue_if_ordered(identity.attach_pid(), |paste| {
                split_rest_of_frame(queued_identity, leading, parts, paste, unfinished)
            })?;
        *pending_input = retained;
        cursor.part_waits = true;
        Some(focus_reports)
    }

    /// Replays, in arrival order, the input of `attach_pid` queued behind its released bindings.
    pub(super) async fn replay_queued_input(&self, attach_pid: u32) {
        REPLAYING_QUEUED_INPUT
            .scope(attach_pid, async {
                let mut retained = Vec::new();
                let mut identity = None;
                loop {
                    #[cfg(test)]
                    if identity.is_some() {
                        test_barrier::pass(test_barrier::Point::ReplayStep, attach_pid).await;
                    }
                    let step = self
                        .active_attach
                        .lock()
                        .await
                        .attached_input_waits
                        .next_replay_step(attach_pid, &mut retained, identity.clone());
                    let replayed = match step {
                        ReplayStep::Frame(QueuedFrame {
                            identity: frame_identity,
                            unit,
                        }) => {
                            if let Some(previous) = identity.as_ref() {
                                let state = self.state.lock().await;
                                // Reindex and rename change addresses without
                                // retiring the pane that owns retained bytes.
                                if !frame_identity.continues_origin(previous, &state) {
                                    retained.clear();
                                }
                            }
                            identity = Some(frame_identity.clone());
                            self.replay_queued_unit(frame_identity, &mut retained, unit)
                                .await
                        }
                        ReplayStep::FlushRetained => {
                            let Some(identity) = identity.clone() else {
                                return;
                            };
                            self.replay_queued_unit(
                                identity,
                                &mut retained,
                                QueuedUnit::EscapeFlushed(Vec::new()),
                            )
                            .await
                        }
                        ReplayStep::Paused | ReplayStep::Done => return,
                    };
                    if replayed.is_err() {
                        // The client is gone; what it still had queued goes with it.
                        self.active_attach
                            .lock()
                            .await
                            .attached_input_waits
                            .discard_queued_input(attach_pid);
                        return;
                    }
                }
            })
            .await;
    }
}

/// Whether a popup, menu, prompt or display-panes of the client takes input that arrives now.
fn surface_takes_arriving_input(
    active_attach: &super::super::ActiveAttachState,
    attach_pid: u32,
) -> bool {
    active_attach.by_pid.get(&attach_pid).is_some_and(|active| {
        active.overlay.is_some() || active.prompt.is_some() || active.display_panes.is_some()
    })
}

/// The units the rest of a replayed unit goes back as, after `leading`, the input the replay
/// retained.
fn replayed_rest_units(
    identity: QueuedInputIdentity,
    leading: Vec<u8>,
    parts: Vec<FramePart>,
) -> Vec<QueuedFrame> {
    let live = |bytes| QueuedUnit::Live {
        bytes,
        windows_console_key: None,
    };
    (!leading.is_empty())
        .then(|| live(leading))
        .into_iter()
        .chain(parts.into_iter().map(|part| match part {
            FramePart::Bytes(bytes) => live(bytes),
            FramePart::KeyboardEvent(event) => QueuedUnit::KeyboardEvent(event),
        }))
        .map(|unit| QueuedFrame {
            identity: identity.clone(),
            unit,
        })
        .collect()
}

/// The units the rest of a frame queues as, after `leading`, the input the frame retained: raw
/// bytes split against the paste the queued input leaves open, decoded events as they are. Only
/// the frame's last bytes may keep the start of a focus report retained.
fn split_rest_of_frame(
    identity: QueuedInputIdentity,
    mut leading: Vec<u8>,
    parts: Vec<FramePart>,
    paste: &PasteScan,
    unfinished: UnfinishedTail,
) -> (Vec<QueuedFrame>, (Vec<u8>, Vec<u8>)) {
    let mut scan = paste.clone();
    let mut frames = Vec::new();
    let mut focus_reports = Vec::new();
    let mut retained = Vec::new();
    let mut queue = |scan: &mut PasteScan, unit: QueuedUnit| {
        scan.follow(&unit);
        frames.push(QueuedFrame {
            identity: identity.clone(),
            unit,
        });
    };
    let last = parts.len().saturating_sub(1);
    for (index, part) in parts.into_iter().enumerate() {
        match part {
            FramePart::Bytes(bytes) => {
                let input = [std::mem::take(&mut leading).as_slice(), &bytes].concat();
                let mut split = FocusSplit::of_live(&input, &scan, unfinished);
                if index < last {
                    split.ordinary.append(&mut split.retained);
                }
                focus_reports.append(&mut split.focus_reports);
                if !split.ordinary.is_empty() {
                    let bytes = split.ordinary;
                    queue(
                        &mut scan,
                        QueuedUnit::Live {
                            bytes,
                            windows_console_key: None,
                        },
                    );
                }
                retained = split.retained;
            }
            FramePart::KeyboardEvent(event) => {
                if !leading.is_empty() {
                    let bytes = std::mem::take(&mut leading);
                    queue(
                        &mut scan,
                        QueuedUnit::Live {
                            bytes,
                            windows_console_key: None,
                        },
                    );
                }
                queue(&mut scan, QueuedUnit::KeyboardEvent(event));
            }
        }
    }
    if !leading.is_empty() {
        queue(
            &mut scan,
            QueuedUnit::Live {
                bytes: leading,
                windows_console_key: None,
            },
        );
    }
    (frames, (focus_reports, retained))
}

/// Whether the current task replays the queued input of the client `attach_pid`.
pub(in crate::handler) fn replays_queued_input(attach_pid: u32) -> bool {
    REPLAYING_QUEUED_INPUT
        .try_with(|replaying| *replaying == attach_pid)
        .unwrap_or(false)
}

#[cfg(test)]
#[path = "../../../handler_attach/queued_rest_split_tests.rs"]
mod rest_split_tests;
/// Pauses a test's client at one causal point of the queue.
#[cfg(test)]
#[path = "../../../handler_attach/queued_test_barrier.rs"]
pub(in crate::handler) mod test_barrier;
