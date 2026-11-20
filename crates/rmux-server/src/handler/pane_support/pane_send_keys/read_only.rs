//! tmux 3.7b `cmd_send_keys_exec()` refuses input from a read-only target
//! client: only `send-keys -X` (copy-mode commands) stays available to it.

use std::sync::atomic::Ordering;

use rmux_proto::{ErrorResponse, Response, RmuxError};

use super::super::super::client_support::{best_client, LiveClient};
use super::super::super::control_support::{current_control_queue_identity, ManagedClient};
use super::super::super::target_support::{inherited_pane_target, requester_environment_context};
use super::super::super::{current_expected_attach_identity, RequestHandler};

/// The client one `send-keys` runs for, once its read-only flag allowed it.
pub(super) struct SendKeysTargetClient {
    /// The attached terminal client named by `-c` or bound by the queue.
    pub(super) attach_pid: Option<u32>,
}

impl RequestHandler {
    /// Resolves `-c` like tmux `cmd_find_client()` (quietly: `Ok(None)` is a
    /// name that finds no client, and the keys go nowhere), then refuses a
    /// read-only target client. The client never chooses the default target
    /// pane; the copy-mode, key-table and mouse routes still need a terminal
    /// client.
    pub(super) async fn judge_send_keys_target_client(
        &self,
        requester_pid: u32,
        request: &rmux_proto::SendKeysExtRequest,
        target_client: Option<&str>,
    ) -> Result<Option<SendKeysTargetClient>, RmuxError> {
        let resolved = self
            .find_send_keys_target_client(requester_pid, target_client)
            .await?;
        if resolved.is_none() && target_client.is_some() {
            return Ok(None);
        }
        self.refuse_send_keys_from_read_only_client(
            requester_pid,
            request.copy_mode_command,
            resolved.as_ref(),
        )
        .await?;
        let attach_pid = match resolved.map(|client| client.client) {
            Some(ManagedClient::Attach { pid, .. }) => Some(pid),
            Some(ManagedClient::Control(_))
                if request.copy_mode_command
                    || request.dispatch_key_table
                    || request.forward_mouse_event =>
            {
                return Err(RmuxError::Server(
                    "send-keys requires an attached client".to_owned(),
                ));
            }
            Some(ManagedClient::Control(_)) | None => None,
        };
        Ok(Some(SendKeysTargetClient { attach_pid }))
    }

    /// The plain `send-keys -t <pane> <keys>` route, which carries no `-X` and
    /// no `-c`, under the same read-only check as the extended routes.
    pub(in crate::handler) async fn handle_send_keys_for_requester(
        &self,
        requester_pid: u32,
        request: rmux_proto::SendKeysRequest,
    ) -> Response {
        if let Err(error) = self
            .refuse_send_keys_from_read_only_client(requester_pid, false, None)
            .await
        {
            return Response::Error(ErrorResponse { error });
        }
        self.handle_send_keys(request).await
    }

    /// tmux judges its target client. With `-c` that is the named client,
    /// attached terminal or control client alike (`cmd_find_client()`).
    /// Without it, it is `cmd_find_current_client()`: the requesting client
    /// when it is attached, so a control client attached with `-r` or `-f
    /// read-only` may not send keys from its own queue; for a requester without
    /// a client of its own (a plain CLI, or a request off a control queue), the
    /// best client of the session holding the requester's pane, whose
    /// read-only flag then refuses the keys just the same.
    async fn refuse_send_keys_from_read_only_client(
        &self,
        requester_pid: u32,
        copy_mode_command: bool,
        target_client: Option<&LiveClient>,
    ) -> Result<(), RmuxError> {
        if copy_mode_command {
            return Ok(());
        }
        let read_only = if let Some(client) = target_client {
            client.read_only
        } else if current_control_queue_identity(requester_pid).is_some() {
            self.control_queue_is_read_only(requester_pid).await
        } else if current_expected_attach_identity().is_some()
            || self.requester_owns_live_attach(requester_pid).await
        {
            false
        } else {
            self.current_client_is_read_only(requester_pid).await
        };
        if read_only {
            return Err(RmuxError::Server("client is read-only".to_owned()));
        }
        Ok(())
    }

    async fn requester_owns_live_attach(&self, requester_pid: u32) -> bool {
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&requester_pid)
            .is_some_and(|active| !active.closing.load(Ordering::SeqCst))
    }

    /// The read-only flag of tmux's current client for a requester that has
    /// none: the best client of the session holding the requester's pane
    /// (tmux `cmd_find_best_client()`).
    async fn current_client_is_read_only(&self, requester_pid: u32) -> bool {
        let clients = self.live_clients().await;
        if !clients.iter().any(|client| client.read_only) {
            return false;
        }
        let pane_session = {
            let context = requester_environment_context(requester_pid, &self.socket_path());
            let state = self.state.lock().await;
            inherited_pane_target(&state.sessions, context, None)
                .ok()
                .flatten()
                .map(|target| target.session_name().clone())
        };
        best_client(&clients, pane_session.as_ref()).is_some_and(|client| client.read_only)
    }
}
