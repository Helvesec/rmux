//! App-owned session guard.

mod lease;
mod signals;
#[cfg(test)]
mod tests;

use std::future::{Future, IntoFuture};
use std::ops::Deref;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;

use crate::transport::{DropGuard, TransportClient};
use crate::{EnsureSession, Result, RmuxError, Session, SessionId, SessionName};
use rmux_proto::{
    KillSessionRequest, Request, Response, CAPABILITY_SDK_OWNED_SESSION_STABLE_IDENTITY,
    CAPABILITY_SDK_SESSION_LEASE, CAPABILITY_SDK_SESSION_LEASE_BY_ID_V2,
};

use super::Rmux;
use lease::{validate_lease_ttl, OwnedSessionLease};
pub use signals::OwnedSessionSignalHandlers;

const DEFAULT_LEASE_TTL: Duration = Duration::from_secs(5);

/// Cleanup policy for an [`OwnedSession`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum CleanupPolicy {
    /// Kill the session on explicit cleanup and best-effort Drop.
    #[default]
    KillOnDrop,
    /// Kill the session if the owner stops renewing its daemon-side lease.
    KillOnOwnerExit,
    /// Keep the session alive when the owner is dropped.
    Preserve,
}

/// Observable daemon lease state for an [`OwnedSession`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash)]
#[non_exhaustive]
pub enum LeaseState {
    /// The owned session was not created with a daemon-side lease, or the
    /// lease has been released successfully.
    #[default]
    NotLeased,
    /// The daemon-side lease is active and the SDK heartbeat is renewing it.
    Active,
    /// The SDK heartbeat observed a terminal lease renewal failure.
    Lost,
}

/// Builder returned by [`Rmux::owned_session`].
#[derive(Debug)]
pub struct OwnedSessionBuilder<'a> {
    rmux: &'a Rmux,
    name: SessionName,
    replace_existing: bool,
    cleanup_policy: CleanupPolicy,
    lease_ttl: Duration,
}

impl<'a> OwnedSessionBuilder<'a> {
    pub(crate) const fn new(rmux: &'a Rmux, name: SessionName) -> Self {
        Self {
            rmux,
            name,
            replace_existing: false,
            cleanup_policy: CleanupPolicy::KillOnDrop,
            lease_ttl: DEFAULT_LEASE_TTL,
        }
    }

    /// Kills an existing session with the same name before creating the new
    /// owned session.
    #[must_use]
    pub const fn replace_existing(mut self, replace_existing: bool) -> Self {
        self.replace_existing = replace_existing;
        self
    }

    /// Sets the cleanup policy for the owned session.
    #[must_use]
    pub const fn cleanup_policy(mut self, cleanup_policy: CleanupPolicy) -> Self {
        self.cleanup_policy = cleanup_policy;
        self
    }

    /// Sets the heartbeat lease TTL used by
    /// [`CleanupPolicy::KillOnOwnerExit`].
    #[must_use]
    pub const fn lease_ttl(mut self, ttl: Duration) -> Self {
        self.lease_ttl = ttl;
        self
    }

    async fn run(self) -> Result<OwnedSession> {
        if self.cleanup_policy == CleanupPolicy::KillOnOwnerExit {
            validate_lease_ttl(self.lease_ttl)?;
        }
        let capabilities = match self.cleanup_policy {
            CleanupPolicy::KillOnOwnerExit => &[
                CAPABILITY_SDK_OWNED_SESSION_STABLE_IDENTITY,
                CAPABILITY_SDK_SESSION_LEASE,
                CAPABILITY_SDK_SESSION_LEASE_BY_ID_V2,
            ][..],
            CleanupPolicy::KillOnDrop | CleanupPolicy::Preserve => {
                &[CAPABILITY_SDK_OWNED_SESSION_STABLE_IDENTITY][..]
            }
        };
        let endpoint = self.rmux.resolved_endpoint()?;
        let timeout = self.rmux.resolved_timeout(None);
        let transport = self
            .rmux
            .connect_resolved_transport_for_operation(&endpoint, timeout)
            .await?;
        crate::ensure::preflight_owned_session_capabilities(&transport, capabilities).await?;

        if self.replace_existing
            && super::session::has_session(&transport, self.name.clone()).await?
        {
            let _ = super::session::kill_session(&transport, self.name.clone()).await?;
        }

        let (session, session_id) = crate::ensure::create_owned_session(
            EnsureSession::named(self.name).create_only().detached(true),
            capabilities,
            endpoint,
            self.rmux.configured_default_timeout(),
            transport.clone(),
        )
        .await?;
        let mut creation_rollback = DropGuard::best_effort(
            session.transport().reusable(),
            session_identity_kill_request(session_id),
        );
        let lease = if self.cleanup_policy == CleanupPolicy::KillOnOwnerExit {
            match OwnedSessionLease::start(&session, session_id, self.lease_ttl, &transport).await {
                Ok(lease) => Some(lease),
                Err(error) => {
                    return Err(rollback_owned_session_creation(
                        session_id,
                        error,
                        &mut creation_rollback,
                        &transport,
                    )
                    .await);
                }
            }
        } else {
            None
        };
        let owned = OwnedSession {
            session: Some(session),
            session_id,
            cleanup_policy: self.cleanup_policy,
            lease,
            signal_handler_state: Arc::new(signals::SignalHandlerState::default()),
        };
        creation_rollback.disarm();
        Ok(owned)
    }
}

impl<'a> IntoFuture for OwnedSessionBuilder<'a> {
    type Output = Result<OwnedSession>;
    type IntoFuture = Pin<Box<dyn Future<Output = Self::Output> + Send + 'a>>;

    fn into_future(self) -> Self::IntoFuture {
        Box::pin(self.run())
    }
}

/// A session whose lifetime is owned by the SDK caller.
#[derive(Debug)]
pub struct OwnedSession {
    session: Option<Session>,
    session_id: SessionId,
    cleanup_policy: CleanupPolicy,
    lease: Option<OwnedSessionLease>,
    signal_handler_state: Arc<signals::SignalHandlerState>,
}

impl OwnedSession {
    /// Returns the configured cleanup policy.
    #[must_use]
    pub const fn cleanup_policy(&self) -> CleanupPolicy {
        self.cleanup_policy
    }

    /// Returns true while this owner still contains a live session handle.
    ///
    /// This becomes false after a successful [`Self::cleanup`] or after
    /// [`Self::detach_owned`] consumes the owner.
    #[must_use]
    pub const fn is_active(&self) -> bool {
        self.session.is_some()
    }

    /// Returns true once the daemon-side owner lease renewal task has observed
    /// a terminal lease loss.
    ///
    /// This is only meaningful for [`CleanupPolicy::KillOnOwnerExit`]. A true
    /// value means the daemon may reap the session after the configured TTL.
    #[must_use]
    pub fn lease_lost(&self) -> bool {
        self.lease.as_ref().is_some_and(OwnedSessionLease::is_lost)
    }

    /// Returns the current daemon-side lease state.
    #[must_use]
    pub fn lease_state(&self) -> LeaseState {
        self.lease
            .as_ref()
            .map_or(LeaseState::NotLeased, OwnedSessionLease::state)
    }

    /// Subscribes to daemon-side lease state changes.
    ///
    /// Returns `None` for sessions that were not created with
    /// [`CleanupPolicy::KillOnOwnerExit`].
    #[must_use]
    pub fn lease_state_receiver(&self) -> Option<watch::Receiver<LeaseState>> {
        self.lease.as_ref().map(OwnedSessionLease::subscribe)
    }

    /// Explicitly kills the owned session when the policy is not
    /// [`CleanupPolicy::Preserve`].
    pub async fn cleanup(&mut self) -> Result<bool> {
        if self.session.is_none() {
            return Ok(false);
        }
        match self.cleanup_policy {
            CleanupPolicy::KillOnDrop | CleanupPolicy::KillOnOwnerExit => {
                let session = self
                    .session
                    .as_ref()
                    .expect("active owned session must retain its session handle");
                let killed = kill_session_identity_confirmed(session, self.session_id).await?;
                self.session.take();
                if let Some(lease) = self.lease.as_ref() {
                    lease.mark_not_leased();
                }
                self.lease.take();
                Ok(killed)
            }
            CleanupPolicy::Preserve => Ok(false),
        }
    }

    /// Immediately runs the same cleanup path as [`Self::cleanup`].
    ///
    /// This is a naming convenience for apps that already own their signal or
    /// cancellation handling and want an explicit shutdown hook.
    pub async fn shutdown_now(&mut self) -> Result<bool> {
        self.cleanup().await
    }

    /// Installs opt-in process signal handling for this owned session.
    ///
    /// The SDK never installs signal handlers by default. This helper listens
    /// for Ctrl-C on every platform, and for SIGTERM/SIGHUP on Unix, then asks
    /// the daemon to kill the session. Dropping the returned guard aborts the
    /// background listener. Only one guard may be installed at a time; a second
    /// call returns an error until the first guard is dropped. Preserving or
    /// detaching ownership while the guard remains live atomically disarms its
    /// cleanup action before ownership is released.
    pub fn install_default_signal_handlers(&self) -> Result<OwnedSessionSignalHandlers> {
        let Some(session) = self.session.as_ref() else {
            return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(
                "owned session no longer active".to_owned(),
            )));
        };
        let cleanup_request = match self.cleanup_policy {
            CleanupPolicy::KillOnDrop | CleanupPolicy::KillOnOwnerExit => {
                session_identity_kill_request(self.session_id)
            }
            CleanupPolicy::Preserve => {
                return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(
                    "owned session ownership has already been released".to_owned(),
                )));
            }
        };
        let transport = session.transport().clone();
        let state = Arc::clone(&self.signal_handler_state);
        signals::install_default_signal_handlers(transport, cleanup_request, state)
    }

    /// Switches this owner to preserve mode after confirming ownership release.
    pub async fn preserve(mut self) -> Result<Self> {
        self.signal_handler_state.disarm_for_ownership_release()?;
        self.release_ownership_confirmed().await?;
        self.cleanup_policy = CleanupPolicy::Preserve;
        Ok(self)
    }

    /// Detaches the guard and returns the underlying persistent session.
    pub async fn detach_owned(mut self) -> Result<Session> {
        self.signal_handler_state.disarm_for_ownership_release()?;
        self.release_ownership_confirmed().await?;
        self.cleanup_policy = CleanupPolicy::Preserve;
        Ok(self
            .session
            .take()
            .expect("owned session must contain a session until detached"))
    }

    /// Returns the underlying session handle if the owner still has one.
    #[must_use]
    pub fn try_session(&self) -> Option<&Session> {
        self.session.as_ref()
    }

    /// Returns the underlying session handle.
    ///
    /// Panics after successful [`Self::cleanup`] because there is no longer an
    /// owned session handle. Use [`Self::try_session`] or [`Self::is_active`]
    /// when the owner may have been cleaned up already.
    #[must_use]
    pub fn session(&self) -> &Session {
        self.session
            .as_ref()
            .expect("owned session no longer contains a session")
    }

    async fn release_ownership_confirmed(&mut self) -> Result<()> {
        if let Some(lease) = self.lease.as_ref() {
            lease.release_confirmed().await?;
        }
        self.lease.take();
        Ok(())
    }
}

async fn rollback_owned_session_creation(
    session_id: SessionId,
    source_error: RmuxError,
    rollback: &mut DropGuard,
    operation_transport: &TransportClient,
) -> RmuxError {
    // Lease setup uses its own actor but shares this operation's absolute
    // deadline. The application actor remains healthy when that lease actor
    // times out, so clear only the expired scope and give compensation one
    // fresh request budget on the already-connected transport.
    let cleanup_transport = operation_transport.reusable().begin_operation();
    match kill_session_identity_on_transport(&cleanup_transport, session_id).await {
        Ok(_) => {
            rollback.disarm();
            source_error
        }
        Err(rollback_error) => {
            RmuxError::collect(crate::CollectError::new(vec![source_error, rollback_error]))
        }
    }
}

fn stable_session_target(session_id: SessionId) -> SessionName {
    SessionName::new(session_id.to_string()).expect("formatted session id is a valid target")
}

fn session_identity_kill_request(session_id: SessionId) -> Request {
    Request::KillSession(KillSessionRequest {
        target: stable_session_target(session_id),
        kill_all_except_target: false,
        clear_alerts: false,
        kill_group: false,
    })
}

async fn kill_session_identity_confirmed(session: &Session, session_id: SessionId) -> Result<bool> {
    kill_session_identity_on_transport(session.transport(), session_id).await
}

async fn kill_session_identity_on_transport(
    transport: &TransportClient,
    session_id: SessionId,
) -> Result<bool> {
    match transport
        .request(session_identity_kill_request(session_id))
        .await?
    {
        Response::KillSession(response) => Ok(response.existed),
        response => Err(RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
            "daemon returned `{}` response for owned-session cleanup",
            response.command_name()
        )))),
    }
}

impl Deref for OwnedSession {
    type Target = Session;

    fn deref(&self) -> &Self::Target {
        self.session()
    }
}

impl Drop for OwnedSession {
    fn drop(&mut self) {
        self.lease.take();
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let request = match self.cleanup_policy {
            CleanupPolicy::KillOnDrop | CleanupPolicy::KillOnOwnerExit => {
                session_identity_kill_request(self.session_id)
            }
            CleanupPolicy::Preserve => return,
        };
        let guard = DropGuard::best_effort(session.transport().clone(), request);
        drop(guard);
    }
}
