use std::collections::{BTreeSet, HashMap};
use std::io;
use std::sync::atomic::Ordering;
use std::sync::{Arc, Mutex};

use rmux_core::{input::mode, LifecycleEvent, PaneId};
use rmux_proto::PaneTarget;
use tokio::sync::Notify;

use super::attach_support::{ActiveAttachIdentity, ActiveAttachState};
use super::pane_support::{
    prepare_committed_focus_input_write, write_attached_bytes_to_target_io, PaneInputWrite,
};
use super::{
    defer_lifecycle_event, prepare_deferred_lifecycle_event, DeferredLifecycleEvent,
    QueuedLifecycleEvent, RequestHandler,
};
use crate::pane_terminals::HandlerState;

/// Provenance of an attach's focus reports.
///
/// This is server-internal and deliberately does not infer provenance from a
/// synthetic pid or from transport behavior.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) enum AttachFocusOrigin {
    Native,
    Virtual,
}

/// Canonical identity of one pane runtime incarnation.
///
/// Pane ids are allocated globally by the session store. Resolving the runtime
/// owner is still required to read its output generation, but that owner's
/// session identity is not part of the key: transferring ownership of the same
/// linked runtime must not manufacture an out/in edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub(in crate::handler) struct PaneFocusRuntimeIdentity {
    pane_id: PaneId,
    output_generation: u64,
}

impl PaneFocusRuntimeIdentity {
    fn for_target(state: &HandlerState, target: &PaneTarget) -> Option<Self> {
        let session = state.sessions.session(target.session_name())?;
        let window = session.window_at(target.window_index())?;
        let pane = window.pane(target.pane_index())?;
        let runtime_session_name =
            state.runtime_session_name_for_window(target.session_name(), target.window_index());
        Some(Self {
            pane_id: pane.id(),
            output_generation: state.pane_output_generation(&runtime_session_name, pane.id()),
        })
    }

    fn current_target(self, state: &HandlerState, preferred: &PaneTarget) -> Option<PaneTarget> {
        if Self::for_target(state, preferred) == Some(self) {
            return Some(preferred.clone());
        }
        state
            .sessions
            .iter()
            .filter_map(|session| {
                session.windows().find_map(|(window_index, window)| {
                    window
                        .panes()
                        .iter()
                        .filter(|pane| pane.id() == self.pane_id)
                        .min_by_key(|pane| pane.index())
                        .and_then(|pane| {
                            let target = PaneTarget::with_window(
                                session.name().clone(),
                                window_index,
                                pane.index(),
                            );
                            (Self::for_target(state, &target) == Some(self))
                                .then_some((session.id(), target))
                        })
                })
            })
            .max_by_key(|(session_id, _)| *session_id)
            .map(|(_, target)| target)
    }
}

#[derive(Debug, Default)]
pub(in crate::handler) struct FocusTransportOrder {
    admission: Mutex<u64>,
    coordinator: Arc<FocusTransportCoordinator>,
}

impl FocusTransportOrder {
    fn reserve(&self) -> FocusTransportTicket {
        let sequence = {
            let mut next = self
                .admission
                .lock()
                .expect("focus transport admission must not be poisoned");
            let sequence = *next;
            *next = next.wrapping_add(1);
            sequence
        };
        FocusTransportTicket {
            position: Some(FocusTransportPosition {
                sequence,
                coordinator: Arc::clone(&self.coordinator),
            }),
        }
    }
}

#[derive(Debug, Default)]
struct FocusTransportCoordinator {
    state: Mutex<FocusTransportCoordinatorState>,
    advanced: Notify,
}

#[derive(Debug, Default)]
struct FocusTransportCoordinatorState {
    next_sequence: u64,
    finished_out_of_order: BTreeSet<u64>,
}

impl FocusTransportCoordinator {
    async fn wait_for_turn(&self, sequence: u64) {
        loop {
            let advanced = self.advanced.notified();
            tokio::pin!(advanced);
            advanced.as_mut().enable();
            if self
                .state
                .lock()
                .expect("focus transport coordinator must not be poisoned")
                .next_sequence
                == sequence
            {
                return;
            }
            advanced.await;
        }
    }

    fn finish(&self, sequence: u64) {
        let advanced = {
            let mut state = self
                .state
                .lock()
                .expect("focus transport coordinator must not be poisoned");
            if sequence < state.next_sequence {
                false
            } else if sequence == state.next_sequence {
                state.next_sequence = state.next_sequence.wrapping_add(1);
                while {
                    let next = state.next_sequence;
                    state.finished_out_of_order.remove(&next)
                } {
                    state.next_sequence = state.next_sequence.wrapping_add(1);
                }
                true
            } else {
                state.finished_out_of_order.insert(sequence);
                false
            }
        };
        if advanced {
            self.advanced.notify_waiters();
        }
    }
}

#[derive(Debug)]
struct FocusTransportPosition {
    sequence: u64,
    coordinator: Arc<FocusTransportCoordinator>,
}

impl Drop for FocusTransportPosition {
    fn drop(&mut self) {
        self.coordinator.finish(self.sequence);
    }
}

struct FocusTransportTicket {
    position: Option<FocusTransportPosition>,
}

impl FocusTransportTicket {
    async fn wait_for_turn(mut self) -> FocusTransportTurn {
        let position = self
            .position
            .as_ref()
            .expect("focus transport ticket must own its position");
        position.coordinator.wait_for_turn(position.sequence).await;
        FocusTransportTurn {
            _position: self.position.take(),
        }
    }
}

struct FocusTransportTurn {
    _position: Option<FocusTransportPosition>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) enum AttachFocusReport {
    In,
    Out,
}

impl AttachFocusReport {
    const fn focused(self) -> bool {
        matches!(self, Self::In)
    }
}

struct FocusDelta {
    runtime: PaneFocusRuntimeIdentity,
    target: PaneTarget,
    focused: bool,
    historical_out: Option<DeferredLifecycleEvent>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) struct FocusedPaneState {
    pub(in crate::handler) target: PaneTarget,
    historical_out: DeferredLifecycleEvent,
}

impl FocusedPaneState {
    pub(in crate::handler) fn rename_session_target(
        &mut self,
        old_name: &rmux_proto::SessionName,
        new_name: &rmux_proto::SessionName,
    ) {
        if self.target.session_name() != old_name {
            return;
        }
        self.target = PaneTarget::with_window(
            new_name.clone(),
            self.target.window_index(),
            self.target.pane_index(),
        );
        self.historical_out
            .rename_pane_event_session(old_name, new_name);
    }
}

enum FocusLifecycleEvent {
    Current(LifecycleEvent),
    Historical(Box<DeferredLifecycleEvent>),
}

struct FocusWrite {
    write: PaneInputWrite,
    bytes: Vec<u8>,
}

pub(in crate::handler) struct FocusEffects {
    writes: Vec<FocusWrite>,
    events: Vec<QueuedLifecycleEvent>,
    transport: Option<FocusTransportTicket>,
}

impl std::fmt::Debug for FocusEffects {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("FocusEffects")
            .field("writes", &self.writes.len())
            .field("events", &self.events.len())
            .field("transport_reserved", &self.transport.is_some())
            .finish()
    }
}

impl FocusEffects {
    pub(in crate::handler) async fn execute(self, handler: &RequestHandler) -> io::Result<bool> {
        let FocusEffects {
            writes,
            events,
            transport,
        } = self;
        let forwarded = !writes.is_empty();
        let mut first_error = None;
        let transport_turn = match transport {
            Some(ticket) => Some(ticket.wait_for_turn().await),
            None => None,
        };
        for write in writes {
            if let Err(error) = write_attached_bytes_to_target_io(write.write, write.bytes).await {
                first_error.get_or_insert_with(|| io::Error::other(error));
            }
        }
        // A hook may re-enter focus reporting. Release transport ordering once
        // every byte in this committed batch has drained, before hook work.
        drop(transport_turn);
        for event in events {
            handler.emit_prepared_and_wait(event).await;
        }
        match first_error {
            Some(error) => Err(error),
            None => Ok(forwarded),
        }
    }
}

enum ClientFocusProjection {
    None,
    Report {
        report: AttachFocusReport,
        session_name: rmux_proto::SessionName,
        client_name: String,
    },
}

impl RequestHandler {
    /// Publishes one native report and derives all pane effects from the
    /// resulting aggregate. Repeated reports still produce their client hook,
    /// but a pane report and pane hook require an effective edge.
    pub(in crate::handler) async fn apply_attached_focus_report(
        &self,
        identity: ActiveAttachIdentity,
        report: AttachFocusReport,
    ) -> io::Result<bool> {
        let effects = {
            // The established lock order is state then active_attach. The
            // entire publication point is covered by that pair; transport and
            // hooks are drained only after both guards are released.
            let mut state = self.state.lock().await;
            let mut active_attach = self.active_attach.lock().await;
            let Some(active) =
                active_attach
                    .by_pid
                    .get_mut(&identity.attach_pid())
                    .filter(|active| {
                        identity.matches_active(active)
                            && !active.closing.load(Ordering::SeqCst)
                            && active.focus_origin == AttachFocusOrigin::Native
                    })
            else {
                return Ok(false);
            };
            if state
                .sessions
                .session(&active.session_name)
                .is_none_or(|session| session.id() != active.session_id)
            {
                return Ok(false);
            }
            active.reported_focused = report.focused();
            let projection = ClientFocusProjection::Report {
                report,
                session_name: active.session_name.clone(),
                client_name: active.client_name.clone(),
            };
            prepare_focus_effects(&mut state, &mut active_attach, projection)
        };
        effects.execute(self).await
    }
}

pub(in crate::handler) fn prepare_server_focus_effects(
    state: &mut HandlerState,
    active_attach: &mut ActiveAttachState,
) -> FocusEffects {
    prepare_focus_effects(state, active_attach, ClientFocusProjection::None)
}

fn prepare_focus_effects(
    state: &mut HandlerState,
    active_attach: &mut ActiveAttachState,
    client: ClientFocusProjection,
) -> FocusEffects {
    let deltas = reconcile_focus_state(state, active_attach);
    let mut writes = Vec::new();
    let mut pane_events = Vec::new();

    for delta in deltas {
        let current_target = delta.runtime.current_target(state, &delta.target);
        let event_target = current_target.as_ref().unwrap_or(&delta.target).clone();
        let bytes = if delta.focused {
            b"\x1b[I".as_slice()
        } else {
            b"\x1b[O".as_slice()
        };
        // Focus moves for selections that never name this pane. A poisoned
        // transcript reports no focus mode, so it gets no focus write.
        if current_target.as_ref().is_some_and(|target| {
            state.transcript_handle(target).is_ok_and(|transcript| {
                transcript
                    .lock_for_cross_pane_sweep()
                    .is_some_and(|transcript| transcript.mode() & mode::MODE_FOCUSON != 0)
            })
        }) {
            let target = current_target
                .as_ref()
                .expect("checked current focus target");
            if let Ok(write) = prepare_committed_focus_input_write(
                state,
                target,
                delta.runtime.pane_id,
                delta.runtime.output_generation,
                bytes,
            ) {
                writes.push(FocusWrite {
                    write,
                    bytes: bytes.to_vec(),
                });
            }
        }
        let event = if delta.focused {
            FocusLifecycleEvent::Current(LifecycleEvent::PaneFocusIn {
                target: event_target,
            })
        } else {
            delta.historical_out.map_or_else(
                || {
                    FocusLifecycleEvent::Current(LifecycleEvent::PaneFocusOut {
                        target: event_target,
                    })
                },
                |event| {
                    if current_target.is_none() {
                        event.retire_historical_pane_target();
                    }
                    FocusLifecycleEvent::Historical(Box::new(event))
                },
            )
        };
        pane_events.push(event);
    }

    let mut lifecycle_events = Vec::new();
    match client {
        ClientFocusProjection::None => lifecycle_events.extend(pane_events),
        ClientFocusProjection::Report {
            report: AttachFocusReport::In,
            session_name,
            client_name,
        } => {
            lifecycle_events.push(FocusLifecycleEvent::Current(
                LifecycleEvent::ClientFocusIn {
                    session_name,
                    client_name: Some(client_name),
                },
            ));
            lifecycle_events.extend(pane_events);
        }
        ClientFocusProjection::Report {
            report: AttachFocusReport::Out,
            session_name,
            client_name,
        } => {
            lifecycle_events.extend(pane_events);
            lifecycle_events.push(FocusLifecycleEvent::Current(
                LifecycleEvent::ClientFocusOut {
                    session_name,
                    client_name: Some(client_name),
                },
            ));
        }
    }
    let mut hook_snapshot = state.hooks.clone();
    let events = lifecycle_events
        .into_iter()
        .map(|event| {
            let deferred = match event {
                FocusLifecycleEvent::Current(event) => defer_lifecycle_event(state, &event),
                FocusLifecycleEvent::Historical(event) => *event,
            };
            prepare_deferred_lifecycle_event(state, &mut hook_snapshot, deferred)
        })
        .collect();

    let transport = (!writes.is_empty()).then(|| active_attach.focus_transport_order.reserve());
    FocusEffects {
        writes,
        events,
        transport,
    }
}

fn reconcile_focus_state(
    state: &HandlerState,
    active_attach: &mut ActiveAttachState,
) -> Vec<FocusDelta> {
    let focused_now = collect_focus_contributions(state, active_attach);
    let mut removed = active_attach
        .focused_panes
        .iter()
        .filter(|(runtime, _)| !focused_now.contains_key(runtime))
        .map(|(runtime, focused)| FocusDelta {
            runtime: *runtime,
            target: focused.target.clone(),
            focused: false,
            historical_out: Some(focused.historical_out.clone()),
        })
        .collect::<Vec<_>>();
    let mut added = focused_now
        .iter()
        .filter(|(runtime, _)| !active_attach.focused_panes.contains_key(runtime))
        .map(|(runtime, target)| FocusDelta {
            runtime: *runtime,
            target: target.clone(),
            focused: true,
            historical_out: None,
        })
        .collect::<Vec<_>>();
    removed.sort_by_key(|delta| delta.runtime);
    added.sort_by_key(|delta| delta.runtime);
    active_attach.focused_panes = focused_now
        .into_iter()
        .map(|(runtime, target)| {
            let historical_out = defer_lifecycle_event(
                state,
                &LifecycleEvent::PaneFocusOut {
                    target: target.clone(),
                },
            );
            (
                runtime,
                FocusedPaneState {
                    target,
                    historical_out,
                },
            )
        })
        .collect();
    removed.extend(added);
    removed
}

fn collect_focus_contributions(
    state: &HandlerState,
    active_attach: &ActiveAttachState,
) -> HashMap<PaneFocusRuntimeIdentity, PaneTarget> {
    let mut attaches = active_attach.by_pid.values().collect::<Vec<_>>();
    attaches.sort_by_key(|active| active.id);
    let mut focused = HashMap::new();

    for active in attaches {
        if active.focus_origin != AttachFocusOrigin::Native
            || !active.reported_focused
            || active.closing.load(Ordering::SeqCst)
            || active.overlay.is_some()
            || active.display_panes.is_some()
        {
            continue;
        }
        let Some(session) = state
            .sessions
            .session(&active.session_name)
            .filter(|session| session.id() == active.session_id)
        else {
            continue;
        };
        let window_index = session.active_window_index();
        let Some(pane) = session
            .window_at(window_index)
            .and_then(|window| window.active_pane())
        else {
            continue;
        };
        let target =
            PaneTarget::with_window(active.session_name.clone(), window_index, pane.index());
        let Some(runtime) = PaneFocusRuntimeIdentity::for_target(state, &target) else {
            continue;
        };
        focused.entry(runtime).or_insert(target);
    }
    focused
}

#[cfg(test)]
mod tests {
    use super::FocusTransportOrder;

    #[tokio::test]
    async fn transport_turn_preserves_committed_out_then_in_order() {
        let order = FocusTransportOrder::default();
        let out_ticket = order.reserve();
        let in_ticket = order.reserve();

        let out_turn = out_ticket.wait_for_turn().await;
        let mut in_wait = Box::pin(in_ticket.wait_for_turn());
        tokio::select! {
            biased;
            _ = &mut in_wait => panic!("focus-in overtook the admitted focus-out write"),
            _ = tokio::task::yield_now() => {}
        }

        let mut wire = Vec::new();
        wire.extend_from_slice(b"\x1b[O");
        drop(out_turn);
        let in_turn = in_wait.await;
        wire.extend_from_slice(b"\x1b[I");
        drop(in_turn);

        assert_eq!(wire, b"\x1b[O\x1b[I");
    }

    #[tokio::test]
    async fn cancelling_an_admitted_transport_ticket_releases_its_successor() {
        let order = FocusTransportOrder::default();
        let cancelled = order.reserve();
        let successor = order.reserve();

        drop(cancelled);
        let successor_turn = successor.wait_for_turn().await;
        drop(successor_turn);
    }
}
