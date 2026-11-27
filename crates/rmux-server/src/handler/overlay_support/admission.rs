//! Display-popup admission: one admission in flight per attached client.
//!
//! tmux runs a display-popup to completion before it reads the next command, so a second popup
//! for a client either finds the first shown and leaves, or comes after it. RMUX builds a popup
//! across awaits, so the overlay check also reserves the client's slot, under the same attach
//! lock; a concurrent request then sees the reservation or the installed overlay and leaves
//! before it validates, builds or spawns anything.

use std::collections::HashSet;
use std::sync::{Arc, Mutex as StdMutex, PoisonError};

use super::super::attach_support::ActiveAttachIdentity;
use super::super::RequestHandler;
use super::state::ClientOverlayState;

/// A reserved admission. Dropping it, on every path including a dropped request, frees the slot.
pub(super) struct PopupAdmission {
    admissions: Arc<StdMutex<HashSet<u32>>>,
    attach_pid: u32,
}

impl Drop for PopupAdmission {
    fn drop(&mut self) {
        self.admissions
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.attach_pid);
    }
}

impl RequestHandler {
    /// Admits a display-popup unless the client already shows an overlay or another popup
    /// admission for it is in flight.
    pub(super) async fn admit_display_popup(
        &self,
        attach_identity: ActiveAttachIdentity,
    ) -> Option<PopupAdmission> {
        let admissions = Arc::clone(&self.state.lock().await.popup_admissions);
        let attach_pid = attach_identity.attach_pid();
        let active_attach = self.active_attach.lock().await;
        let existing_overlay = matches!(
            active_attach
                .by_pid
                .get(&attach_pid)
                .filter(|active| attach_identity.matches_active(active))
                .and_then(|active| active.overlay.as_ref()),
            Some(ClientOverlayState::Menu(_) | ClientOverlayState::Popup(_))
        );
        let reserved = !existing_overlay
            && admissions
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(attach_pid);
        // Only the call that inserted the reservation may hold the admission that removes it.
        if !reserved {
            return None;
        }
        Some(PopupAdmission {
            admissions,
            attach_pid,
        })
    }
}
