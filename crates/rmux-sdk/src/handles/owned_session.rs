//! App-owned session guard.

mod signals;

use std::future::{Future, IntoFuture};
use std::ops::Deref;
use std::pin::Pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::transport::{DropGuard, TransportClient};
use crate::{EnsureSession, Result, RmuxError, Session, SessionName};
use rmux_proto::{
    CreateSessionLeaseRequest, KillSessionRequest, ReleaseSessionLeaseRequest,
    RenewSessionLeaseRequest, Request, Response, CAPABILITY_SDK_SESSION_LEASE,
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

        if self.replace_existing {
            match self.rmux.session(self.name.clone()).await {
                Ok(session) => {
                    let _ = session.kill().await?;
                }
                Err(error) if is_missing_session(&error) => {}
                Err(error) => return Err(error),
            }
        }

        let session = self
            .rmux
            .ensure_session(EnsureSession::named(self.name).create_only().detached(true))
            .await?;
        let lease = if self.cleanup_policy == CleanupPolicy::KillOnOwnerExit {
            Some(OwnedSessionLease::start(&session, self.lease_ttl).await?)
        } else {
            None
        };
        Ok(OwnedSession {
            session: Some(session),
            cleanup_policy: self.cleanup_policy,
            lease,
            signal_handlers_installed: Arc::new(AtomicBool::new(false)),
        })
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
    cleanup_policy: CleanupPolicy,
    lease: Option<OwnedSessionLease>,
    signal_handlers_installed: Arc<AtomicBool>,
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
        let Some(session) = self.session.as_ref() else {
            return Ok(false);
        };
        match self.cleanup_policy {
            CleanupPolicy::KillOnDrop | CleanupPolicy::KillOnOwnerExit => {
                let killed = session.kill().await?;
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
    /// call returns an error until the first guard is dropped.
    pub fn install_default_signal_handlers(&self) -> Result<OwnedSessionSignalHandlers> {
        let Some(session) = self.session.as_ref() else {
            return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(
                "owned session no longer active".to_owned(),
            )));
        };
        if self
            .signal_handlers_installed
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .is_err()
        {
            return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(
                "owned session signal handlers are already installed".to_owned(),
            )));
        }

        let transport = session.transport().clone();
        let target = session.name().clone();
        let installed = Arc::clone(&self.signal_handlers_installed);
        signals::install_default_signal_handlers(transport, target, installed)
    }

    /// Switches this owner to preserve mode after confirming lease release.
    pub async fn preserve(mut self) -> Result<Self> {
        self.release_lease_confirmed().await?;
        self.cleanup_policy = CleanupPolicy::Preserve;
        Ok(self)
    }

    /// Detaches the guard and returns the underlying persistent session.
    pub async fn detach_owned(mut self) -> Result<Session> {
        self.release_lease_confirmed().await?;
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

    async fn release_lease_confirmed(&mut self) -> Result<()> {
        if let Some(lease) = self.lease.as_ref() {
            lease.release_confirmed().await?;
        }
        self.lease.take();
        Ok(())
    }
}

#[derive(Debug)]
struct OwnedSessionLease {
    session_name: SessionName,
    token: u64,
    transport: TransportClient,
    task: JoinHandle<()>,
    lost: Arc<AtomicBool>,
    state_tx: watch::Sender<LeaseState>,
}

impl OwnedSessionLease {
    async fn start(session: &Session, ttl: Duration) -> Result<Self> {
        let ttl_millis = ttl_millis(ttl)?;
        let transport = session.transport().clone();
        crate::capabilities::require(&transport, &[CAPABILITY_SDK_SESSION_LEASE]).await?;
        let response = transport
            .request(Request::CreateSessionLease(CreateSessionLeaseRequest {
                session_name: session.name().clone(),
                ttl_millis,
            }))
            .await?;
        let Response::CreateSessionLease(response) = response else {
            return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(
                "daemon returned unexpected response for session lease create".to_owned(),
            )));
        };

        let session_name = session.name().clone();
        let token = response.token;
        let renew_transport = transport.clone();
        let renew_session_name = session_name.clone();
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
            session_name,
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

    fn mark_lost(&self) {
        self.lost.store(true, Ordering::Release);
        let _ = self.state_tx.send(LeaseState::Lost);
    }

    async fn release_confirmed(&self) -> Result<bool> {
        if self.is_lost() {
            return Err(RmuxError::from(
                rmux_proto::RmuxError::owned_session_lease_lost(self.session_name.clone()),
            ));
        }

        let response = self
            .transport
            .request(Request::ReleaseSessionLease(ReleaseSessionLeaseRequest {
                session_name: self.session_name.clone(),
                token: self.token,
            }))
            .await?;
        let Response::ReleaseSessionLease(response) = response else {
            return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(
                "daemon returned unexpected response for session lease release".to_owned(),
            )));
        };
        if response.released {
            let _ = self.state_tx.send(LeaseState::NotLeased);
            Ok(true)
        } else {
            self.mark_lost();
            Err(RmuxError::from(
                rmux_proto::RmuxError::owned_session_lease_lost(self.session_name.clone()),
            ))
        }
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
        if !matches!(
            self.cleanup_policy,
            CleanupPolicy::KillOnDrop | CleanupPolicy::KillOnOwnerExit
        ) {
            return;
        }
        let Some(session) = self.session.as_ref() else {
            return;
        };
        let guard = DropGuard::best_effort(
            session.transport().clone(),
            Request::KillSession(KillSessionRequest {
                target: session.name().clone(),
                kill_all_except_target: false,
                clear_alerts: false,
                kill_group: false,
            }),
        );
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
