use std::collections::HashMap;
use std::time::SystemTime;

use rmux_core::PaneId;
use rmux_proto::{SessionId, SessionName, WebShareSummary};

use super::super::record::{
    WebSessionTarget, WebShareConnectRole, WebShareRecord, WebShareRevokeReason, WebShareTarget,
};
use super::super::secrets::SecretHash;

#[derive(Debug)]
pub(crate) struct ExpiredWebShare {
    pub(crate) share_id: String,
    pub(crate) kill_session: Option<WebSessionTarget>,
}

#[derive(Debug)]
pub(crate) enum WebShareExpiryPoll {
    Expired(ExpiredWebShare),
    Pending(SystemTime),
    Gone,
}

#[derive(Debug)]
pub(super) struct WebShareState {
    pub(super) records: HashMap<String, WebShareRecord>,
    pub(super) expired_actions: HashMap<String, ExpiredWebShare>,
    pub(super) token_ids: HashMap<String, WebCapability>,
    pub(super) listener: WebListenerState,
}

impl Default for WebShareState {
    fn default() -> Self {
        Self {
            records: HashMap::new(),
            expired_actions: HashMap::new(),
            token_ids: HashMap::new(),
            listener: WebListenerState::Unavailable("not started".to_owned()),
        }
    }
}

#[derive(Debug)]
pub(super) enum WebListenerState {
    Available,
    Unavailable(String),
}

impl WebListenerState {
    pub(super) const fn is_available(&self) -> bool {
        matches!(self, Self::Available)
    }
}

#[derive(Debug, Clone)]
pub(super) struct WebCapability {
    pub(super) share_id: String,
    pub(super) role: WebShareConnectRole,
    pub(super) secret_hash: SecretHash,
}

impl WebShareState {
    pub(super) fn insert(&mut self, record: WebShareRecord) {
        if let Some(hash) = record.spectator_token_hash {
            self.token_ids.insert(
                hash.token_id(),
                WebCapability {
                    share_id: record.share_id.clone(),
                    role: WebShareConnectRole::Spectator,
                    secret_hash: hash,
                },
            );
        }
        if let Some(hash) = record.operator_token_hash {
            self.token_ids.insert(
                hash.token_id(),
                WebCapability {
                    share_id: record.share_id.clone(),
                    role: WebShareConnectRole::Operator,
                    secret_hash: hash,
                },
            );
        }
        self.records.insert(record.share_id.clone(), record);
    }

    pub(super) fn remove(&mut self, share_id: &str, reason: WebShareRevokeReason) -> bool {
        self.records
            .remove(share_id)
            .map(|record| {
                self.remove_tokens(&record);
                record.revoke(reason);
                true
            })
            .unwrap_or(false)
    }

    pub(super) fn clear(&mut self, reason: WebShareRevokeReason) -> u32 {
        let stopped = u32::try_from(self.records.len()).unwrap_or(u32::MAX);
        for (_, record) in self.records.drain() {
            record.revoke(reason);
        }
        self.expired_actions.clear();
        self.token_ids.clear();
        self.records.clear();
        stopped
    }

    pub(super) fn remove_targets_for_sessions(
        &mut self,
        sessions: &[(SessionName, SessionId)],
        reason: WebShareRevokeReason,
    ) -> u32 {
        let share_ids = self
            .records
            .iter()
            .filter(|(_, record)| target_matches_removed_session(&record.target, sessions))
            .map(|(share_id, _)| share_id.clone())
            .collect::<Vec<_>>();
        let removed = u32::try_from(share_ids.len()).unwrap_or(u32::MAX);
        for share_id in share_ids {
            let _ = self.remove(&share_id, reason);
        }
        removed
    }

    pub(super) fn remove_targets_for_panes(
        &mut self,
        pane_ids: &[PaneId],
        reason: WebShareRevokeReason,
    ) -> u32 {
        let share_ids = self
            .records
            .iter()
            .filter(|(_, record)| {
                matches!(
                    &record.target,
                    WebShareTarget::Pane(target)
                        if target.pane_id().is_some_and(|pane_id| pane_ids.contains(&pane_id))
                )
            })
            .map(|(share_id, _)| share_id.clone())
            .collect::<Vec<_>>();
        let removed = u32::try_from(share_ids.len()).unwrap_or(u32::MAX);
        for share_id in share_ids {
            let _ = self.remove(&share_id, reason);
        }
        removed
    }

    pub(super) fn rename_session_targets(
        &mut self,
        old_name: &SessionName,
        new_name: &SessionName,
        session_id: SessionId,
    ) -> u32 {
        let mut renamed = 0_u32;
        for record in self.records.values_mut() {
            let before = record.target.clone();
            record.target.rename_session(old_name, new_name, session_id);
            if record.target != before {
                renamed = renamed.saturating_add(1);
            }
        }
        renamed
    }

    pub(super) fn capability_by_token_id(&self, token_id: &str) -> Option<WebCapability> {
        self.token_ids.get(token_id).cloned()
    }

    pub(super) fn summaries(&self) -> Vec<WebShareSummary> {
        let mut summaries = self
            .records
            .values()
            .map(WebShareRecord::summary)
            .collect::<Vec<_>>();
        summaries.sort_by(|left, right| left.share_id.cmp(&right.share_id));
        summaries
    }

    pub(super) fn summary(&self, share_id: &str) -> Option<WebShareSummary> {
        self.records.get(share_id).map(WebShareRecord::summary)
    }

    pub(super) fn prune_expired(&mut self) {
        let now = SystemTime::now();
        let expired = self
            .records
            .iter()
            .filter(|(_, record)| record.expires_at.is_some_and(|expires| expires <= now))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in expired {
            if let Some(record) = self.records.remove(&id) {
                self.remove_tokens(&record);
                let expired = expire_record(record);
                self.expired_actions.insert(id, expired);
            }
        }
    }

    pub(super) fn poll_expiry(&mut self, share_id: &str) -> WebShareExpiryPoll {
        if let Some(expired) = self.expired_actions.remove(share_id) {
            return WebShareExpiryPoll::Expired(expired);
        }
        let now = SystemTime::now();
        let Some(expires_at) = self
            .records
            .get(share_id)
            .and_then(|record| record.expires_at)
        else {
            return WebShareExpiryPoll::Gone;
        };
        if expires_at > now {
            return WebShareExpiryPoll::Pending(expires_at);
        }
        let Some(record) = self.records.remove(share_id) else {
            return WebShareExpiryPoll::Gone;
        };
        self.remove_tokens(&record);
        WebShareExpiryPoll::Expired(expire_record(record))
    }

    fn remove_tokens(&mut self, record: &WebShareRecord) {
        if let Some(hash) = record.spectator_token_hash {
            self.token_ids.remove(&hash.token_id());
        }
        if let Some(hash) = record.operator_token_hash {
            self.token_ids.remove(&hash.token_id());
        }
    }
}

fn expire_record(record: WebShareRecord) -> ExpiredWebShare {
    let expired = ExpiredWebShare {
        share_id: record.share_id.clone(),
        kill_session: match (&record.target, record.kill_session_on_expire) {
            (WebShareTarget::Session(target), true) => Some(target.clone()),
            _ => None,
        },
    };
    record.revoke(WebShareRevokeReason::TtlExpired);
    expired
}

fn target_matches_removed_session(
    target: &WebShareTarget,
    sessions: &[(SessionName, SessionId)],
) -> bool {
    match target {
        WebShareTarget::Pane(target) => sessions
            .iter()
            .any(|(session_name, _)| target.session_name() == session_name),
        WebShareTarget::Session(target) => sessions
            .iter()
            .any(|(_, session_id)| target.id() == *session_id),
    }
}
