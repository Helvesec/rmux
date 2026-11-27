//! Drop guards that roll back pane stream registry reservations on early exit.

use std::sync::{Arc, Mutex as StdMutex, Weak};

use rmux_proto::PaneRawRebaseReason;

use super::super::subscription_support::OutputSubscriptionState;
use super::{PendingSurfaceRefresh, SurfaceRefreshToken};

pub(super) struct SurfaceInitializationGuard {
    subscriptions: Weak<StdMutex<OutputSubscriptionState>>,
    pub(super) token: u64,
}

impl SurfaceInitializationGuard {
    pub(super) fn new(subscriptions: &Arc<StdMutex<OutputSubscriptionState>>, token: u64) -> Self {
        Self {
            subscriptions: Arc::downgrade(subscriptions),
            token,
        }
    }
}

impl Drop for SurfaceInitializationGuard {
    fn drop(&mut self) {
        let Some(subscriptions) = self.subscriptions.upgrade() else {
            return;
        };
        subscriptions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .finish_surface_initialization(self.token);
    }
}

pub(super) struct StreamReservationGuard {
    subscriptions: Weak<StdMutex<OutputSubscriptionState>>,
    subscription_id: rmux_proto::PaneOutputSubscriptionId,
    armed: bool,
}

impl StreamReservationGuard {
    pub(super) fn new(
        subscriptions: &Arc<StdMutex<OutputSubscriptionState>>,
        subscription_id: rmux_proto::PaneOutputSubscriptionId,
    ) -> Self {
        Self {
            subscriptions: Arc::downgrade(subscriptions),
            subscription_id,
            armed: true,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for StreamReservationGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(subscriptions) = self.subscriptions.upgrade() else {
            return;
        };
        subscriptions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .remove_subscription(self.subscription_id);
    }
}

pub(super) struct SurfaceRefreshGuard {
    subscriptions: Weak<StdMutex<OutputSubscriptionState>>,
    pane_id: rmux_core::PaneId,
    token: SurfaceRefreshToken,
    pending: PendingSurfaceRefresh,
    armed: bool,
}

impl SurfaceRefreshGuard {
    pub(super) fn new(
        subscriptions: &Arc<StdMutex<OutputSubscriptionState>>,
        pane_id: rmux_core::PaneId,
        token: SurfaceRefreshToken,
        pending: PendingSurfaceRefresh,
    ) -> Self {
        Self {
            subscriptions: Arc::downgrade(subscriptions),
            pane_id,
            token,
            pending,
            armed: true,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for SurfaceRefreshGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(subscriptions) = self.subscriptions.upgrade() else {
            return;
        };
        subscriptions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .cancel_surface_refresh(self.pane_id, self.token, self.pending);
    }
}

pub(super) struct RawRebaseGuard {
    subscriptions: Weak<StdMutex<OutputSubscriptionState>>,
    subscription_id: rmux_proto::PaneOutputSubscriptionId,
    token: u64,
    reason: PaneRawRebaseReason,
    armed: bool,
}

impl RawRebaseGuard {
    pub(super) fn new(
        subscriptions: &Arc<StdMutex<OutputSubscriptionState>>,
        subscription_id: rmux_proto::PaneOutputSubscriptionId,
        token: u64,
        reason: PaneRawRebaseReason,
    ) -> Self {
        Self {
            subscriptions: Arc::downgrade(subscriptions),
            subscription_id,
            token,
            reason,
            armed: true,
        }
    }

    pub(super) fn disarm(&mut self) {
        self.armed = false;
    }
}

impl Drop for RawRebaseGuard {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let Some(subscriptions) = self.subscriptions.upgrade() else {
            return;
        };
        subscriptions
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .cancel_raw_rebase(self.subscription_id, self.token, self.reason);
    }
}
