use std::collections::VecDeque;

use tokio::time::Instant;

use super::PendingClipboardQuery;
use crate::handler::attach_support::{ActiveAttachIdentity, ActiveAttachState};

pub(super) fn expire_pending_at(
    pending: &mut VecDeque<PendingClipboardQuery>,
    now: Instant,
) -> Vec<ActiveAttachIdentity> {
    let mut expired_attaches = Vec::new();
    for pending_query in pending.iter() {
        if pending_query.expires_at <= now
            && !expired_attaches
                .iter()
                .any(|expired| same_attach_generation(*expired, pending_query.attach))
        {
            expired_attaches.push(pending_query.attach);
        }
    }
    if !expired_attaches.is_empty() {
        // OSC 52 responses carry no request token. Once any request for an
        // attach generation times out, retaining a newer request would let the
        // late response consume that newer slot. Cancel the generation as one
        // unit and fail closed until the client reconnects.
        pending.retain(|pending_query| {
            !expired_attaches
                .iter()
                .any(|expired| same_attach_generation(*expired, pending_query.attach))
        });
    }
    expired_attaches
}

pub(super) fn mark_clipboard_attaches_desynchronized(
    active_attach: &mut ActiveAttachState,
    expired_attaches: &[ActiveAttachIdentity],
) {
    for identity in expired_attaches {
        let Some(active) = active_attach
            .by_pid
            .get_mut(&identity.attach_pid())
            .filter(|active| identity.matches_active(active))
        else {
            continue;
        };
        if active.clipboard_queries_desynchronized {
            continue;
        }
        active.clipboard_queries_desynchronized = true;
        tracing::warn!(
            attach_pid = identity.attach_pid(),
            attach_id = identity.attach_id(),
            "disabled clipboard queries for a timed-out attach generation"
        );
    }
}

pub(super) fn same_attach_generation(
    left: ActiveAttachIdentity,
    right: ActiveAttachIdentity,
) -> bool {
    left.attach_pid() == right.attach_pid() && left.attach_id() == right.attach_id()
}
