use std::sync::Arc;
use std::time::Instant;

use rmux_core::events::PaneOutputSubscriptionKey;
use rmux_proto::{
    PaneRawRebase, PaneRawRebaseReason, PaneStreamEndReason, PaneStreamMode, PaneSurfaceFrame,
};

use crate::pane_io::{PaneBoundary, PaneObservationItem, PaneOutputReceiver};
use crate::pane_recovery::PaneDynamicColors;

#[derive(Debug)]
pub(in crate::handler) struct RawPaneStream {
    pub(in crate::handler) receiver: PaneOutputReceiver,
    pub(in crate::handler) epoch: u64,
    pub(in crate::handler) include_snapshot: bool,
    rebasing: bool,
    rebase_token: u64,
    pending_rebase: Option<PaneRawRebaseReason>,
    pending_observation: Option<PaneObservationItem>,
    end_reason: Option<PaneStreamEndReason>,
}

impl RawPaneStream {
    pub(in crate::handler) fn new(
        receiver: PaneOutputReceiver,
        epoch: u64,
        include_snapshot: bool,
        end_reason: Option<PaneStreamEndReason>,
    ) -> Self {
        Self {
            receiver,
            epoch,
            include_snapshot,
            rebasing: false,
            rebase_token: 0,
            pending_rebase: None,
            pending_observation: None,
            end_reason,
        }
    }

    pub(in crate::handler) const fn is_rebasing(&self) -> bool {
        self.rebasing
    }

    pub(in crate::handler) const fn pending_rebase(&self) -> Option<PaneRawRebaseReason> {
        self.pending_rebase
    }

    pub(in crate::handler) fn take_pending_observation(&mut self) -> Option<PaneObservationItem> {
        self.pending_observation.take()
    }

    pub(in crate::handler) fn defer_observation(&mut self, item: PaneObservationItem) {
        debug_assert!(self.pending_observation.is_none());
        self.pending_observation = Some(item);
    }

    pub(in crate::handler) const fn end_reason(&self) -> Option<PaneStreamEndReason> {
        self.end_reason
    }

    pub(in crate::handler) fn mark_ending(&mut self, reason: PaneStreamEndReason) {
        self.end_reason = Some(reason);
    }

    pub(in crate::handler) fn begin_rebase(&mut self) -> Option<u64> {
        if self.rebasing {
            return None;
        }
        self.rebasing = true;
        self.pending_rebase = None;
        self.rebase_token = self.rebase_token.saturating_add(1);
        Some(self.rebase_token)
    }

    pub(in crate::handler) fn finish_rebase(
        &mut self,
        token: u64,
        receiver: PaneOutputReceiver,
        epoch: u64,
    ) -> bool {
        if !self.rebasing || self.rebase_token != token {
            return false;
        }
        self.receiver = receiver;
        self.epoch = epoch;
        self.rebasing = false;
        true
    }

    pub(in crate::handler) fn cancel_rebase(&mut self, token: u64, reason: PaneRawRebaseReason) {
        if !self.rebasing || self.rebase_token != token {
            return;
        }
        self.rebasing = false;
        self.pending_rebase = Some(reason);
    }
}

#[derive(Debug, Clone, Copy)]
pub(in crate::handler) struct SurfacePaneStream {
    pub(in crate::handler) delivered_revision: u64,
    pub(in crate::handler) delivered_epoch: u64,
    pub(in crate::handler) delivered_lifecycle_revision: u64,
    pub(in crate::handler) end_reason: Option<PaneStreamEndReason>,
}

#[derive(Debug)]
pub(in crate::handler) enum PaneStreamSubscription {
    Reserved {
        mode: PaneStreamMode,
        end_reason: Option<PaneStreamEndReason>,
    },
    Raw(RawPaneStream),
    Surface(SurfacePaneStream),
}

impl PaneStreamSubscription {
    pub(in crate::handler) const fn reserved(mode: PaneStreamMode) -> Self {
        Self::Reserved {
            mode,
            end_reason: None,
        }
    }

    pub(in crate::handler) const fn mode(&self) -> PaneStreamMode {
        match self {
            Self::Reserved { mode, .. } => *mode,
            Self::Raw(_) => PaneStreamMode::Raw,
            Self::Surface(_) => PaneStreamMode::Surface,
        }
    }

    pub(in crate::handler) const fn end_reason(&self) -> Option<PaneStreamEndReason> {
        match self {
            Self::Reserved { end_reason, .. } => *end_reason,
            Self::Raw(stream) => stream.end_reason(),
            Self::Surface(stream) => stream.end_reason,
        }
    }

    pub(in crate::handler) fn mark_ending(&mut self, reason: PaneStreamEndReason) {
        match self {
            Self::Reserved { end_reason, .. } => *end_reason = Some(reason),
            Self::Raw(stream) => stream.mark_ending(reason),
            Self::Surface(stream) => stream.end_reason = Some(reason),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(in crate::handler) struct PaneSurfaceFingerprint {
    size: rmux_proto::TerminalSize,
    visible_line_revisions: Vec<u64>,
    cursor: (u32, u32),
    cursor_style: u32,
    title: String,
    path: String,
    mode: u32,
    alternate: bool,
    scroll_region: (u32, u32),
    history_size: usize,
    history_bytes: usize,
    metadata_revision: u64,
    dynamic_colors: PaneDynamicColors,
}

impl PaneSurfaceFingerprint {
    pub(in crate::handler) fn capture(
        screen: &rmux_core::Screen,
        dynamic_colors: &PaneDynamicColors,
    ) -> Self {
        let size = screen.size();
        Self {
            size,
            visible_line_revisions: (0..usize::from(size.rows))
                .map(|row| screen.visible_line_revision(row).unwrap_or(0))
                .collect(),
            cursor: screen.cursor_position(),
            cursor_style: screen.cursor_style(),
            title: screen.title().to_owned(),
            path: screen.path().to_owned(),
            mode: screen.mode(),
            alternate: screen.is_alternate(),
            scroll_region: screen.scroll_region(),
            history_size: screen.history_size(),
            history_bytes: screen.history_bytes(),
            metadata_revision: screen.metadata_revision(),
            dynamic_colors: dynamic_colors.clone(),
        }
    }
}

#[derive(Debug)]
pub(in crate::handler) struct SurfaceDriver {
    generation: u64,
    pub(in crate::handler) receiver: PaneOutputReceiver,
    pub(in crate::handler) latest: Arc<PaneSurfaceFrame>,
    pub(in crate::handler) fingerprint: PaneSurfaceFingerprint,
    pub(in crate::handler) revision: u64,
    pub(in crate::handler) lifecycle_revision: u64,
    pub(in crate::handler) frame_lifecycle_revision: u64,
    pub(in crate::handler) refreshing: bool,
    pub(in crate::handler) refresh_token: u64,
    pending_refresh: Option<PendingSurfaceRefresh>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) struct SurfaceRefreshToken {
    generation: u64,
    sequence: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(in crate::handler) struct PendingSurfaceRefresh {
    pub(in crate::handler) reset: bool,
    pub(in crate::handler) frame_lifecycle_revision: u64,
}

impl SurfaceDriver {
    pub(in crate::handler) fn new(
        generation: u64,
        receiver: PaneOutputReceiver,
        latest: Arc<PaneSurfaceFrame>,
        fingerprint: PaneSurfaceFingerprint,
    ) -> Self {
        Self {
            generation,
            receiver,
            revision: latest.revision,
            latest,
            fingerprint,
            lifecycle_revision: 0,
            frame_lifecycle_revision: 0,
            refreshing: false,
            refresh_token: 0,
            pending_refresh: None,
        }
    }

    pub(in crate::handler) const fn pending_refresh(&self) -> Option<PendingSurfaceRefresh> {
        self.pending_refresh
    }

    pub(in crate::handler) fn begin_refresh(&mut self) -> Option<SurfaceRefreshToken> {
        if self.refreshing {
            return None;
        }
        self.refreshing = true;
        self.pending_refresh = None;
        self.refresh_token = self.refresh_token.saturating_add(1);
        Some(SurfaceRefreshToken {
            generation: self.generation,
            sequence: self.refresh_token,
        })
    }

    pub(in crate::handler) fn finish_refresh(
        &mut self,
        token: SurfaceRefreshToken,
        receiver: PaneOutputReceiver,
        latest: Arc<PaneSurfaceFrame>,
        fingerprint: PaneSurfaceFingerprint,
        frame_lifecycle_revision: u64,
    ) -> bool {
        if !self.is_current_refresh(token) {
            return false;
        }
        self.receiver = receiver;
        self.revision = latest.revision;
        self.latest = latest;
        self.fingerprint = fingerprint;
        self.frame_lifecycle_revision = frame_lifecycle_revision;
        self.refreshing = false;
        true
    }

    pub(in crate::handler) fn finish_unchanged_refresh(
        &mut self,
        token: SurfaceRefreshToken,
        receiver: PaneOutputReceiver,
        fingerprint: PaneSurfaceFingerprint,
    ) -> bool {
        if !self.is_current_refresh(token) {
            return false;
        }
        self.receiver = receiver;
        self.fingerprint = fingerprint;
        self.refreshing = false;
        true
    }

    pub(in crate::handler) fn cancel_refresh(
        &mut self,
        token: SurfaceRefreshToken,
        pending: PendingSurfaceRefresh,
    ) {
        if !self.is_current_refresh(token) {
            return;
        }
        self.refreshing = false;
        self.pending_refresh = Some(match self.pending_refresh {
            Some(existing) => PendingSurfaceRefresh {
                reset: existing.reset || pending.reset,
                frame_lifecycle_revision: existing
                    .frame_lifecycle_revision
                    .min(pending.frame_lifecycle_revision),
            },
            None => pending,
        });
    }

    fn is_current_refresh(&self, token: SurfaceRefreshToken) -> bool {
        self.refreshing
            && self.generation == token.generation
            && self.refresh_token == token.sequence
    }
}

#[derive(Debug, Clone)]
pub(in crate::handler) struct CachedRawRebase {
    pub(in crate::handler) boundary: PaneBoundary,
    pub(in crate::handler) rebase: PaneRawRebase,
}

#[derive(Debug, Clone, Copy)]
pub(in crate::handler) struct EndedPaneStream {
    pub(in crate::handler) connection_id: u64,
    pub(in crate::handler) reason: PaneStreamEndReason,
    pub(in crate::handler) ended_at: Instant,
}

impl EndedPaneStream {
    pub(in crate::handler) const fn new(
        connection_id: u64,
        reason: PaneStreamEndReason,
        ended_at: Instant,
    ) -> Self {
        Self {
            connection_id,
            reason,
            ended_at,
        }
    }
}

#[derive(Debug, Clone)]
pub(in crate::handler) struct PaneStreamSource {
    pub(in crate::handler) target: rmux_proto::PaneTarget,
    pub(in crate::handler) key: PaneOutputSubscriptionKey,
    pub(in crate::handler) output: crate::pane_io::PaneOutputSender,
    pub(in crate::handler) transcript: crate::pane_transcript::SharedPaneTranscript,
    pub(in crate::handler) generation: u64,
}

impl PaneStreamSource {
    pub(in crate::handler) fn rekey(&mut self, current_key: PaneOutputSubscriptionKey) {
        if self.target.session_name() == self.key.runtime_session_name() {
            self.target = rmux_proto::PaneTarget::with_window(
                current_key.runtime_session_name().clone(),
                self.target.window_index(),
                self.target.pane_index(),
            );
        }
        self.key = current_key;
    }
}
