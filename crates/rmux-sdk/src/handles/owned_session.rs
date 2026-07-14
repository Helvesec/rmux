//! App-owned session guard.

mod signals;
#[cfg(test)]
mod tests;

use std::future::{Future, IntoFuture};
use std::ops::Deref;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::transport::{DropGuard, TransportClient};
use crate::{EnsureSession, Result, RmuxError, Session, SessionId, SessionName};
use rmux_proto::{
    CreateSessionLeaseRequest, KillSessionRequest, ReleaseSessionLeaseRequest,
    RenewSessionLeaseRequest, Request, Response, CAPABILITY_SDK_OWNED_SESSION_STABLE_IDENTITY,
    CAPABILITY_SDK_SESSION_LEASE, CAPABILITY_SDK_SESSION_LEASE_BY_ID_V2,
};

use super::Rmux;
pub use signals::OwnedSessionSignalHandlers;

const DEFAULT_LEASE_TTL: Duration = Duration::from_secs(5);
const MIN_LEASE_RENEW_INTERVAL: Duration = Duration::from_millis(100);
const MAX_LEASE_RENEW_RETRY_INTERVAL: Duration = Duration::from_millis(250);

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
        crate::ensure::preflight_owned_session_capabilities(self.rmux, capabilities).await?;

        if self.replace_existing {
            match self.rmux.session(self.name.clone()).await {
                Ok(session) => {
                    let _ = session.kill().await?;
                }
                Err(error) if is_missing_session(&error) => {}
                Err(error) => return Err(error),
            }
        }

        let (session, session_id) = crate::ensure::create_owned_session(
            self.rmux,
            EnsureSession::named(self.name).create_only().detached(true),
            capabilities,
        )
        .await?;
        let mut creation_rollback = DropGuard::best_effort(
            session.transport().clone(),
            session_identity_kill_request(session_id),
        );
        let lease = if self.cleanup_policy == CleanupPolicy::KillOnOwnerExit {
            match OwnedSessionLease::start(&session, session_id, self.lease_ttl).await {
                Ok(lease) => Some(lease),
                Err(error) => {
                    return Err(rollback_owned_session_creation(
                        &session,
                        session_id,
                        error,
                        &mut creation_rollback,
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

#[derive(Debug)]
struct OwnedSessionLease {
    display_name: SessionName,
    lease_target: SessionName,
    token: u64,
    transport: TransportClient,
    task: JoinHandle<()>,
    lost: Arc<AtomicBool>,
    state_tx: watch::Sender<LeaseState>,
}

impl OwnedSessionLease {
    async fn start(session: &Session, session_id: SessionId, ttl: Duration) -> Result<Self> {
        let ttl_millis = ttl_millis(ttl)?;
        let transport = session.transport().clone();
        crate::capabilities::require_with_handshake(
            &transport,
            &[CAPABILITY_SDK_SESSION_LEASE_BY_ID_V2],
            &[
                CAPABILITY_SDK_SESSION_LEASE,
                CAPABILITY_SDK_SESSION_LEASE_BY_ID_V2,
            ],
        )
        .await?;
        let lease_target = stable_session_target(session_id);
        let response = transport
            .request(Request::CreateSessionLease(CreateSessionLeaseRequest {
                session_name: lease_target.clone(),
                ttl_millis,
            }))
            .await?;
        let Response::CreateSessionLease(response) = response else {
            return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(
                "daemon returned unexpected response for session lease create".to_owned(),
            )));
        };

        let token = response.token;
        let renew_transport = transport.clone();
        let renew_session_name = lease_target.clone();
        let lost = Arc::new(AtomicBool::new(false));
        let renew_lost = Arc::clone(&lost);
        let (state_tx, _) = watch::channel(LeaseState::Active);
        let renew_state_tx = state_tx.clone();
        let renew_interval = (ttl / 3).max(MIN_LEASE_RENEW_INTERVAL);
        let task = tokio::spawn(async move {
            let mut last_renew_success = tokio::time::Instant::now();
            loop {
                tokio::time::sleep(renew_interval).await;
                let deadline = last_renew_success + ttl;
                if !renew_lease_with_retries(
                    &renew_transport,
                    &renew_session_name,
                    token,
                    ttl_millis,
                    deadline,
                )
                .await
                {
                    renew_lost.store(true, Ordering::Release);
                    let _ = renew_state_tx.send(LeaseState::Lost);
                    break;
                }
                last_renew_success = tokio::time::Instant::now();
            }
        });

        Ok(Self {
            display_name: session.name().clone(),
            lease_target,
            token,
            transport,
            task,
            lost,
            state_tx,
        })
    }

    fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    fn state(&self) -> LeaseState {
        if self.is_lost() {
            LeaseState::Lost
        } else {
            *self.state_tx.borrow()
        }
    }

    fn subscribe(&self) -> watch::Receiver<LeaseState> {
        self.state_tx.subscribe()
    }

    fn mark_not_leased(&self) {
        let _ = self.state_tx.send(LeaseState::NotLeased);
    }

    async fn release_confirmed(&self) -> Result<bool> {
        if self.is_lost() {
            return Err(self.lost_error());
        }

        let response = self
            .transport
            .request(Request::ReleaseSessionLease(ReleaseSessionLeaseRequest {
                session_name: self.lease_target.clone(),
                token: self.token,
            }))
            .await?;
        let Response::ReleaseSessionLease(response) = response else {
            return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(
                "daemon returned unexpected response for session lease release".to_owned(),
            )));
        };
        if response.released {
            self.mark_not_leased();
            Ok(true)
        } else {
            self.lost.store(true, Ordering::Release);
            let _ = self.state_tx.send(LeaseState::Lost);
            Err(self.lost_error())
        }
    }

    fn lost_error(&self) -> RmuxError {
        RmuxError::from(rmux_proto::RmuxError::owned_session_lease_lost(
            self.display_name.clone(),
        ))
    }
}

async fn rollback_owned_session_creation(
    session: &Session,
    session_id: SessionId,
    source_error: RmuxError,
    rollback: &mut DropGuard,
) -> RmuxError {
    match kill_session_identity_confirmed(session, session_id).await {
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
    match session
        .transport()
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

async fn renew_lease_with_retries(
    transport: &TransportClient,
    session_name: &SessionName,
    token: u64,
    ttl_millis: u64,
    deadline: tokio::time::Instant,
) -> bool {
    let mut delay = MIN_LEASE_RENEW_INTERVAL;

    loop {
        match renew_lease_once(transport, session_name, token, ttl_millis).await {
            Ok(true) => return true,
            Ok(false) => return false,
            Err(_) => {
                let now = tokio::time::Instant::now();
                if now >= deadline {
                    return false;
                }
                let remaining = deadline - now;
                tokio::time::sleep(delay.min(remaining)).await;
                delay = delay
                    .saturating_add(delay)
                    .min(MAX_LEASE_RENEW_RETRY_INTERVAL);
            }
        }
    }
}

async fn renew_lease_once(
    transport: &TransportClient,
    session_name: &SessionName,
    token: u64,
    ttl_millis: u64,
) -> Result<bool> {
    match transport
        .request(Request::RenewSessionLease(RenewSessionLeaseRequest {
            session_name: session_name.clone(),
            token,
            ttl_millis,
        }))
        .await?
    {
        Response::RenewSessionLease(response) => Ok(response.renewed),
        response => Err(RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
            "daemon returned `{}` response for session lease renew",
            response.command_name()
        )))),
    }
}

impl Drop for OwnedSessionLease {
    fn drop(&mut self) {
        self.task.abort();
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

fn ttl_millis(ttl: Duration) -> Result<u64> {
    validate_lease_ttl(ttl)?;
    let millis = u64::try_from(ttl.as_millis()).map_err(|_| {
        RmuxError::protocol(rmux_proto::RmuxError::Server(
            "owned session lease ttl is too large".to_owned(),
        ))
    })?;
    Ok(millis)
}

fn validate_lease_ttl(ttl: Duration) -> Result<()> {
    let millis = u64::try_from(ttl.as_millis()).map_err(|_| {
        RmuxError::protocol(rmux_proto::RmuxError::Server(
            "owned session lease ttl is too large".to_owned(),
        ))
    })?;
    if millis == 0 {
        return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(
            "owned session lease ttl must be greater than zero".to_owned(),
        )));
    }
    if millis < rmux_proto::MIN_SESSION_LEASE_TTL_MILLIS {
        return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
            "owned session lease ttl must be at least {}ms",
            rmux_proto::MIN_SESSION_LEASE_TTL_MILLIS
        ))));
    }
    Ok(())
}

fn is_missing_session(error: &RmuxError) -> bool {
    matches!(
        error,
        RmuxError::Protocol {
            source: rmux_proto::RmuxError::SessionNotFound(_),
        }
    )
}
