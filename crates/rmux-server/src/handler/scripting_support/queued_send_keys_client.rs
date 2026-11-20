//! The attached client selected by queued ordinary pane input.

use std::future::Future;
use std::sync::atomic::Ordering;

use rmux_proto::{Request, RmuxError};

use super::super::attach_support::{ActiveAttachIdentity, ActiveAttachState};
use super::super::client_support::LiveClient;
use super::super::control_support::{current_control_queue_identity, ManagedClient};
use super::super::{
    attached_client_matches_target, current_expected_attach_identity, normalize_target_client,
    RequestHandler,
};
use super::queue::QueueInvocation;

tokio::task_local! {
    static QUEUED_SEND_KEYS_CLIENT: Option<ActiveAttachIdentity>;
}

pub(super) fn bind_client(
    invocation: &mut QueueInvocation,
    active_attach: &ActiveAttachState,
    requester_pid: u32,
) -> Result<Option<ActiveAttachIdentity>, RmuxError> {
    let QueueInvocation::Request(Request::SendKeysExt2(request)) = invocation else {
        return Ok(None);
    };
    if request.dispatch_key_table || request.forward_mouse_event {
        return Ok(None);
    }
    let Some(client) = request
        .target_client
        .as_deref()
        .map(normalize_target_client)
    else {
        return Ok(None);
    };
    let expected_client = (client == "=")
        .then(current_expected_attach_identity)
        .flatten();
    let pid = if client == "=" {
        if let Some(identity) = expected_client {
            Some(identity.attach_pid())
        } else if current_control_queue_identity(requester_pid).is_some() {
            return Err(RmuxError::Server(
                "send-keys requires an attached client".to_owned(),
            ));
        } else {
            Some(active_attach.resolve_attached_client_pid(requester_pid, "send-keys")?)
        }
    } else if let Ok(pid) = client.parse::<u32>() {
        active_attach.by_pid.contains_key(&pid).then_some(pid)
    } else {
        active_attach.by_pid.iter().find_map(|(&pid, active)| {
            attached_client_matches_target(&active.client_name, client).then_some(pid)
        })
    };
    let Some(pid) = pid else {
        // Keep the existing missing-client/no-op and control-client error path.
        return Ok(None);
    };
    // The queue's earlier validation released its lock. Bind only the same
    // registration under the admission lock, never a replacement at this PID.
    let active = active_attach.by_pid.get(&pid).ok_or_else(client_changed)?;
    if expected_client.is_some_and(|identity| !identity.matches_active(active))
        || active.closing.load(Ordering::SeqCst)
    {
        return Err(client_changed());
    }
    request.target_client = Some(pid.to_string());
    Ok(Some(active.identity(pid)))
}

pub(super) async fn with_client<T, F>(client: Option<ActiveAttachIdentity>, future: F) -> T
where
    F: Future<Output = T>,
{
    QUEUED_SEND_KEYS_CLIENT.scope(client, future).await
}

fn client_changed() -> RmuxError {
    RmuxError::Server("send-keys target client identity changed before queued input".to_owned())
}

impl RequestHandler {
    pub(in crate::handler) async fn validate_queued_send_keys_client(
        &self,
    ) -> Result<Option<u32>, RmuxError> {
        Ok(self
            .validate_queued_send_keys_identity()
            .await?
            .map(ActiveAttachIdentity::attach_pid))
    }

    async fn validate_queued_send_keys_identity(
        &self,
    ) -> Result<Option<ActiveAttachIdentity>, RmuxError> {
        let Some(identity) = QUEUED_SEND_KEYS_CLIENT
            .try_with(|client| *client)
            .ok()
            .flatten()
        else {
            return Ok(None);
        };
        let active_attach = self.active_attach.lock().await;
        active_attach
            .by_pid
            .get(&identity.attach_pid())
            .filter(|active| {
                identity.matches_active_lifetime(active, identity.session_id())
                    && !active.closing.load(Ordering::SeqCst)
            })
            .map(|_| Some(identity))
            .ok_or_else(client_changed)
    }

    /// tmux `cmd_find_client()` for `send-keys -c`, quiet: the client bound by
    /// the queue, else the named attached or control client. `Ok(None)` is a
    /// name that finds no client. `=` stays an attached-client lookup.
    pub(in crate::handler) async fn find_send_keys_target_client(
        &self,
        requester_pid: u32,
        target_client: Option<&str>,
    ) -> Result<Option<LiveClient>, RmuxError> {
        let client = if let Some(identity) = self.validate_queued_send_keys_identity().await? {
            ManagedClient::Attach {
                pid: identity.attach_pid(),
                attach_id: identity.attach_id(),
            }
        } else {
            let Some(target_client) = target_client else {
                return Ok(None);
            };
            if normalize_target_client(target_client) == "=" {
                let identity = self
                    .resolve_target_attach_client_identity(
                        requester_pid,
                        Some(target_client),
                        "send-keys",
                    )
                    .await?;
                ManagedClient::Attach {
                    pid: identity.attach_pid(),
                    attach_id: identity.attach_id(),
                }
            } else {
                match self
                    .find_named_client(requester_pid, target_client, "send-keys")
                    .await?
                {
                    Some(client) => client,
                    None => return Ok(None),
                }
            }
        };
        Ok(self.live_client(client).await)
    }
}
