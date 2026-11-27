use rmux_proto::RmuxError;

use super::super::super::client_support::most_recent_client;
use super::super::super::control_support::ManagedClient as ManagedDisplayClient;
use super::super::super::{ControlClientIdentity, RequestHandler};
use super::DisplayMessageClient;
use crate::control_notifications::format_control_message_line;

impl RequestHandler {
    pub(super) async fn display_message_client_from_managed(
        &self,
        client: ManagedDisplayClient,
    ) -> Option<DisplayMessageClient> {
        match client {
            ManagedDisplayClient::Attach { pid, attach_id } => {
                let active_attach = self.active_attach.lock().await;
                active_attach
                    .by_pid
                    .get(&pid)
                    .filter(|active| {
                        active.id == attach_id
                            && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    })
                    .map(|active| DisplayMessageClient::Attached(active.identity(pid)))
            }
            ManagedDisplayClient::Control(identity) => {
                Some(DisplayMessageClient::Control(identity))
            }
        }
    }

    /// The most recently active client of `session`, or of any session
    /// (tmux `cmd_find_best_client()` order), as a display-message client.
    pub(super) async fn recent_display_message_client(
        &self,
        session: Option<&rmux_proto::SessionName>,
    ) -> Option<DisplayMessageClient> {
        let clients = self.live_clients().await;
        let client = most_recent_client(&clients, session)?.client;
        self.display_message_client_from_managed(client).await
    }

    pub(super) async fn display_message_client_session(
        &self,
        client: DisplayMessageClient,
    ) -> Result<Option<(rmux_proto::SessionName, rmux_proto::SessionId)>, RmuxError> {
        match client {
            DisplayMessageClient::Attached(identity) => self
                .attached_session_identity_for_identity(identity)
                .await
                .map(Some),
            DisplayMessageClient::Control(identity) => {
                let active_control = self.active_control.lock().await;
                let active = active_control
                    .by_pid
                    .get(&identity.requester_pid())
                    .filter(|active| {
                        active.id == identity.control_id()
                            && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                    })
                    .ok_or_else(|| RmuxError::Server("control client disappeared".to_owned()))?;
                match (&active.session_name, active.session_id) {
                    (Some(session_name), Some(session_id)) => {
                        Ok(Some((session_name.clone(), session_id)))
                    }
                    (None, None) => Ok(None),
                    _ => Err(RmuxError::Server(
                        "control client has an invalid session identity".to_owned(),
                    )),
                }
            }
        }
    }

    pub(super) async fn send_display_message_to_control_session_identity(
        &self,
        session_name: &rmux_proto::SessionName,
        session_id: rmux_proto::SessionId,
        message: &str,
    ) -> bool {
        let identities = {
            let active_control = self.active_control.lock().await;
            active_control
                .by_pid
                .iter()
                .filter(|(_, active)| {
                    active.session_name.as_ref() == Some(session_name)
                        && active.session_id == Some(session_id)
                        && !active.closing.load(std::sync::atomic::Ordering::SeqCst)
                })
                .map(|(pid, active)| ControlClientIdentity::new(*pid, active.id))
                .collect::<Vec<_>>()
        };
        let line = format_control_message_line(message);
        let mut delivered = false;
        for identity in identities {
            delivered |= self
                .send_control_response_notification_to_queue(identity, line.clone())
                .await;
        }
        delivered
    }
}
