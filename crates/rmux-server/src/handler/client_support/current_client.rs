//! tmux 3.7b's client searches, shared by every command that needs a client
//! it did not get from its own queue: `cmd_find_client()` finds a client by
//! name over attached and control clients alike, and `cmd_find_best_client()`
//! takes the most recently active one. display-message, the OSC 52 clipboard
//! query and send-keys all choose their client through this one ordering.
//! send-keys also takes its default target from the requester's own state
//! here, tmux `cmd_find_from_client()`, whatever client `-c` names.

use std::sync::atomic::Ordering;

use rmux_proto::{RmuxError, SessionName};

use super::super::attach_support::{ActiveAttach, ClientFlags};
use super::super::control_support::{
    current_control_queue_identity, ActiveControl, ControlClientIdentity, ManagedClient,
};
use super::super::target_support::{inherited_pane_target, requester_environment_context};
use super::super::{
    attached_client_matches_target, control_client_target_pid, normalize_target_client,
    RequestHandler,
};
use crate::pane_terminals::HandlerState;

/// tmux `cmd_find_client_better()` compares `activity_time` with a strict
/// `>`: on an exact tie the client met first in tmux's client list stays the
/// best. rmux draws every terminal and control client's activity sequence from
/// one server-wide counter, so two live clients never tie; the terminal flag
/// and the registration id only keep the order total.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub(in crate::handler) struct ClientActivityOrder {
    sequence: u64,
    terminal: bool,
    registration: u64,
}

impl ClientActivityOrder {
    pub(in crate::handler) fn of_attach(active: &ActiveAttach) -> Self {
        Self {
            sequence: active.last_activity_sequence,
            terminal: true,
            registration: active.id,
        }
    }

    fn of_control(active: &ActiveControl) -> Self {
        Self {
            sequence: active.last_activity_sequence,
            terminal: false,
            registration: active.id,
        }
    }
}

/// The most recently active of `candidates`, by tmux's client ordering.
pub(in crate::handler) fn most_recently_active<T>(
    candidates: impl IntoIterator<Item = (ClientActivityOrder, T)>,
) -> Option<T> {
    candidates
        .into_iter()
        .max_by_key(|(order, _)| *order)
        .map(|(_, client)| client)
}

/// One live client, terminal or control, as tmux's client searches see it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) struct LiveClient {
    pub(in crate::handler) client: ManagedClient,
    pub(in crate::handler) session: Option<SessionName>,
    /// tmux `CLIENT_READONLY`.
    pub(in crate::handler) read_only: bool,
    order: ClientActivityOrder,
}

impl LiveClient {
    fn attach(pid: u32, active: &ActiveAttach) -> Option<Self> {
        (!active.closing.load(Ordering::SeqCst)).then(|| Self {
            client: ManagedClient::Attach {
                pid,
                attach_id: active.id,
            },
            session: Some(active.session_name.clone()),
            read_only: active.flags.contains(ClientFlags::READONLY),
            order: ClientActivityOrder::of_attach(active),
        })
    }

    fn control(pid: u32, active: &ActiveControl) -> Option<Self> {
        (!active.closing.load(Ordering::SeqCst)).then(|| Self {
            client: ManagedClient::Control(ControlClientIdentity::new(pid, active.id)),
            session: active.session_name.clone(),
            read_only: active.flags.read_only,
            order: ClientActivityOrder::of_control(active),
        })
    }
}

/// The most recently active client attached to `session`, or to any session
/// when `session` is `None`. A client without a session is never a candidate,
/// as in tmux `cmd_find_best_client()`.
pub(in crate::handler) fn most_recent_client<'a>(
    clients: &'a [LiveClient],
    session: Option<&SessionName>,
) -> Option<&'a LiveClient> {
    most_recently_active(
        clients
            .iter()
            .filter(|client| {
                client
                    .session
                    .as_ref()
                    .is_some_and(|attached| session.is_none_or(|wanted| attached == wanted))
            })
            .map(|client| (client.order, client)),
    )
}

/// tmux `cmd_find_best_client(s)`: the most recent client of `session`, or of
/// any session when no client is attached to it.
pub(in crate::handler) fn best_client<'a>(
    clients: &'a [LiveClient],
    session: Option<&SessionName>,
) -> Option<&'a LiveClient> {
    session
        .and_then(|session| most_recent_client(clients, Some(session)))
        .or_else(|| most_recent_client(clients, None))
}

/// tmux `cmd_find_best_session()` over every session: the most recently
/// active one, which `cmd_find_from_nothing()` takes as the current session.
fn most_recently_active_session(state: &HandlerState) -> Option<SessionName> {
    state
        .sessions
        .iter()
        .max_by_key(|session| session.recency())
        .map(|session| session.name().clone())
}

impl RequestHandler {
    /// tmux `cmd_find_from_client()` for the requester's own client, which
    /// gives send-keys its default target: the session of the requester's own
    /// terminal or control client; for a requester without one, the session
    /// holding the pane it runs in; else `cmd_find_from_nothing()`, the most
    /// recently active session. A client named by `-c` never takes part.
    pub(in crate::handler) async fn requester_current_session(
        &self,
        requester_pid: u32,
    ) -> Option<SessionName> {
        if current_control_queue_identity(requester_pid).is_some() {
            return self.current_session_candidate(requester_pid).await;
        }
        if let Some(active) = self.active_attach.lock().await.by_pid.get(&requester_pid) {
            return Some(active.session_name.clone());
        }
        let context = requester_environment_context(requester_pid, &self.socket_path());
        let state = self.state.lock().await;
        inherited_pane_target(&state.sessions, context, None)
            .ok()
            .flatten()
            .map(|target| target.session_name().clone())
            .or_else(|| most_recently_active_session(&state))
    }

    /// Every client a best-client search may choose: attached terminals that
    /// are neither suspended nor closing, and control clients not closing.
    pub(in crate::handler) async fn live_clients(&self) -> Vec<LiveClient> {
        let mut clients = {
            let active_attach = self.active_attach.lock().await;
            active_attach
                .by_pid
                .iter()
                .filter(|(_, active)| !active.suspended)
                .filter_map(|(&pid, active)| LiveClient::attach(pid, active))
                .collect::<Vec<_>>()
        };
        let active_control = self.active_control.lock().await;
        clients.extend(
            active_control
                .by_pid
                .iter()
                .filter_map(|(&pid, active)| LiveClient::control(pid, active)),
        );
        clients
    }

    /// The client behind `client` while it is the same registration and not
    /// closing; a suspended attached client is still found, as by name.
    pub(in crate::handler) async fn live_client(
        &self,
        client: ManagedClient,
    ) -> Option<LiveClient> {
        match client {
            ManagedClient::Attach { pid, attach_id } => {
                let active_attach = self.active_attach.lock().await;
                active_attach
                    .by_pid
                    .get(&pid)
                    .filter(|active| active.id == attach_id)
                    .and_then(|active| LiveClient::attach(pid, active))
            }
            ManagedClient::Control(identity) => {
                let active_control = self.active_control.lock().await;
                active_control
                    .by_pid
                    .get(&identity.requester_pid())
                    .filter(|active| active.id == identity.control_id())
                    .and_then(|active| LiveClient::control(identity.requester_pid(), active))
            }
        }
    }

    /// tmux `cmd_find_client()` for a `-c` name: an attached client by PID or
    /// name first, then a control client. `=` is the requester's own client.
    /// `Ok(None)` is tmux's quiet "can't find client".
    pub(in crate::handler) async fn find_named_client(
        &self,
        requester_pid: u32,
        target_client: &str,
        command_name: &str,
    ) -> Result<Option<ManagedClient>, RmuxError> {
        let target_client = normalize_target_client(target_client);
        if target_client == "=" {
            return self
                .resolve_target_managed_client(requester_pid, Some(target_client), command_name)
                .await
                .map(Some);
        }

        {
            let active_attach = self.active_attach.lock().await;
            let attached = if let Ok(pid) = target_client.parse::<u32>() {
                active_attach.by_pid.get(&pid).and_then(|active| {
                    (!active.closing.load(Ordering::SeqCst)).then_some(ManagedClient::Attach {
                        pid,
                        attach_id: active.id,
                    })
                })
            } else {
                active_attach
                    .by_pid
                    .iter()
                    .filter(|(_, active)| !active.closing.load(Ordering::SeqCst))
                    .find(|(_, active)| {
                        attached_client_matches_target(&active.client_name, target_client)
                    })
                    .map(|(&pid, active)| ManagedClient::Attach {
                        pid,
                        attach_id: active.id,
                    })
            };
            if attached.is_some() {
                return Ok(attached);
            }
        }

        let active_control = self.active_control.lock().await;
        Ok(control_client_target_pid(target_client).and_then(|pid| {
            active_control.by_pid.get(&pid).and_then(|active| {
                (!active.closing.load(Ordering::SeqCst)).then_some(ManagedClient::Control(
                    ControlClientIdentity::new(pid, active.id),
                ))
            })
        }))
    }
}

#[cfg(test)]
mod tests {
    use super::{best_client, most_recent_client, ClientActivityOrder, LiveClient};
    use crate::handler::control_support::{ControlClientIdentity, ManagedClient};
    use rmux_proto::SessionName;

    fn client(session: Option<&str>, sequence: u64, terminal: bool, id: u64) -> LiveClient {
        LiveClient {
            client: if terminal {
                ManagedClient::Attach {
                    pid: 100,
                    attach_id: id,
                }
            } else {
                ManagedClient::Control(ControlClientIdentity::new(200, id))
            },
            session: session.map(|name| SessionName::new(name).expect("valid session name")),
            read_only: false,
            order: ClientActivityOrder {
                sequence,
                terminal,
                registration: id,
            },
        }
    }

    fn session(name: &str) -> SessionName {
        SessionName::new(name).expect("valid session name")
    }

    #[test]
    fn the_most_recent_client_wins_and_the_order_stays_total() {
        let clients = [
            client(Some("a"), 4, false, 1),
            client(Some("a"), 4, true, 2),
            client(Some("a"), 3, true, 3),
        ];
        assert_eq!(most_recent_client(&clients, None), Some(&clients[1]));
        let later_control = [
            client(Some("a"), 5, false, 1),
            client(Some("a"), 4, true, 2),
        ];
        assert_eq!(
            most_recent_client(&later_control, None),
            Some(&later_control[0])
        );
    }

    #[test]
    fn a_client_without_a_session_is_never_the_best_client() {
        let clients = [client(None, 9, false, 1), client(Some("b"), 1, false, 2)];
        assert_eq!(most_recent_client(&clients, None), Some(&clients[1]));
        assert_eq!(most_recent_client(&clients[..1], None), None);
    }

    #[test]
    fn best_client_prefers_the_session_then_falls_back_to_any_session() {
        let clients = [client(Some("a"), 1, true, 1), client(Some("b"), 2, true, 2)];
        assert_eq!(
            best_client(&clients, Some(&session("a"))),
            Some(&clients[0])
        );
        assert_eq!(
            best_client(&clients, Some(&session("c"))),
            Some(&clients[1])
        );
        assert_eq!(most_recent_client(&clients, Some(&session("c"))), None);
        assert_eq!(best_client(&clients, None), Some(&clients[1]));
    }
}
