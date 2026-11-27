//! `new-session` run by an attached client moves that client into the session.
//!
//! tmux 3.7b `cmd_new_session` ends, without `-d`, with
//! `server_client_set_session` on the client that runs the command, and `-A`
//! on an existing session goes through `cmd_attach_session`, which switches an
//! attached client. A key binding, a `command-prompt` template, the `:` prompt
//! and a `{ ... }` block therefore all leave that client on the new session.
//!
//! Only an attach registration is moved here. A control client is attached by
//! `prepare_created_session_control_attach`, and a requester with no
//! registration (a command-line client) attaches itself from the response.

use rmux_proto::{Response, RmuxError, SessionId, SessionName};

use super::super::{validate_expected_attach_identity, RequestHandler};
use super::switching::{SwitchManagedClientIdentity, SwitchSessionIdentity};

impl RequestHandler {
    /// Resolves the attached client running this `new-session`, before any
    /// session exists, with the same identity checks as `attach-session`.
    pub(in crate::handler) async fn new_session_attached_requester(
        &self,
        requester_pid: u32,
    ) -> Result<Option<SwitchManagedClientIdentity>, RmuxError> {
        if let Some(identity) = validate_expected_attach_identity(self, requester_pid).await? {
            return Ok(Some(SwitchManagedClientIdentity::Attach {
                pid: identity.attach_pid(),
                attach_id: identity.attach_id(),
            }));
        }
        Ok(match self.managed_client_for_pid(requester_pid).await {
            Some(client @ SwitchManagedClientIdentity::Attach { .. }) => Some(client),
            Some(SwitchManagedClientIdentity::Control { .. }) | None => None,
        })
    }

    /// Moves `client` into the session `new-session` created or found.
    ///
    /// `new-session` already applied `update-environment` from this client,
    /// so the switch does not apply it a second time.
    pub(in crate::handler) async fn switch_attached_requester_to_new_session(
        &self,
        requester_pid: u32,
        client: SwitchManagedClientIdentity,
        session_name: &SessionName,
        session_id: SessionId,
    ) -> Result<(), RmuxError> {
        let response = self
            .switch_managed_client_to_session_with_control_flags(
                requester_pid,
                client,
                SwitchSessionIdentity {
                    session_name: session_name.clone(),
                    session_id,
                },
                None,
                true,
                crate::control::ControlFlagChange::Keep,
            )
            .await;
        match response {
            Response::Error(error) => Err(error.error),
            _ => Ok(()),
        }
    }
}
