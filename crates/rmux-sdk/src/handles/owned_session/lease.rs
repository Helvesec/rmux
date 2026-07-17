use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use tokio::sync::watch;
use tokio::task::JoinHandle;

use crate::transport::TransportClient;
use crate::{Result, RmuxError, Session, SessionId, SessionName};
use rmux_proto::{
    CreateSessionLeaseRequest, ReleaseSessionLeaseRequest, RenewSessionLeaseRequest, Request,
    Response, CAPABILITY_SDK_SESSION_LEASE, CAPABILITY_SDK_SESSION_LEASE_BY_ID_V2,
};

use super::stable_session_target;

const MIN_LEASE_RENEW_INTERVAL: Duration = Duration::from_millis(100);
const MAX_LEASE_RENEW_RETRY_INTERVAL: Duration = Duration::from_millis(250);

#[derive(Debug)]
pub(super) struct OwnedSessionLease {
    display_name: SessionName,
    lease_target: SessionName,
    token: u64,
    transport: TransportClient,
    task: JoinHandle<()>,
    lost: Arc<AtomicBool>,
    state_tx: watch::Sender<super::LeaseState>,
}

impl OwnedSessionLease {
    pub(super) async fn start(
        session: &Session,
        session_id: SessionId,
        ttl: Duration,
        operation_transport: &TransportClient,
    ) -> Result<Self> {
        let ttl_millis = ttl_millis(ttl)?;
        let operation_transport = connect_lease_transport(session, operation_transport).await?;
        crate::capabilities::require_with_handshake(
            &operation_transport,
            &[CAPABILITY_SDK_SESSION_LEASE_BY_ID_V2],
            &[
                CAPABILITY_SDK_SESSION_LEASE,
                CAPABILITY_SDK_SESSION_LEASE_BY_ID_V2,
            ],
        )
        .await?;
        let lease_target = stable_session_target(session_id);
        let response = operation_transport
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
        let transport = operation_transport.reusable();
        let renew_transport = transport.clone();
        let renew_session_name = lease_target.clone();
        let lost = Arc::new(AtomicBool::new(false));
        let renew_lost = Arc::clone(&lost);
        let (state_tx, _) = watch::channel(super::LeaseState::Active);
        let renew_state_tx = state_tx.clone();
        let renew_interval = (ttl / 3).max(MIN_LEASE_RENEW_INTERVAL);
        let task = tokio::spawn(async move {
            let mut last_renew_success = tokio::time::Instant::now();
            loop {
                tokio::time::sleep(renew_interval).await;
                let now = tokio::time::Instant::now();
                let deadline = renewal_attempt_deadline(last_renew_success, now, ttl);
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
                    let _ = renew_state_tx.send(super::LeaseState::Lost);
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

    pub(super) fn is_lost(&self) -> bool {
        self.lost.load(Ordering::Acquire)
    }

    pub(super) fn state(&self) -> super::LeaseState {
        if self.is_lost() {
            super::LeaseState::Lost
        } else {
            *self.state_tx.borrow()
        }
    }

    pub(super) fn subscribe(&self) -> watch::Receiver<super::LeaseState> {
        self.state_tx.subscribe()
    }

    pub(super) fn mark_not_leased(&self) {
        let _ = self.state_tx.send(super::LeaseState::NotLeased);
    }

    pub(super) async fn release_confirmed(&self) -> Result<bool> {
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
            let _ = self.state_tx.send(super::LeaseState::Lost);
            Err(self.lost_error())
        }
    }

    fn lost_error(&self) -> RmuxError {
        RmuxError::from(rmux_proto::RmuxError::owned_session_lease_lost(
            self.display_name.clone(),
        ))
    }
}

fn renewal_attempt_deadline(
    last_success: tokio::time::Instant,
    now: tokio::time::Instant,
    ttl: Duration,
) -> tokio::time::Instant {
    let lease_deadline = last_success + ttl;
    if now >= lease_deadline {
        // A system-wide suspend can advance the monotonic clock while neither
        // the SDK nor daemon had an opportunity to run. Always give the
        // resumed owner one bounded renewal attempt; a daemon that remained
        // active has already reaped the lease and will reject it.
        now + ttl
    } else {
        lease_deadline
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
        let now = tokio::time::Instant::now();
        if now >= deadline {
            return false;
        }
        match tokio::time::timeout_at(
            deadline,
            renew_lease_once(transport, session_name, token, ttl_millis),
        )
        .await
        {
            Err(_) => return false,
            Ok(Ok(true)) => return true,
            Ok(Ok(false)) => return false,
            Ok(Err(_)) => {
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

async fn connect_lease_transport(
    session: &Session,
    operation_transport: &TransportClient,
) -> Result<TransportClient> {
    // Unit fixtures use an in-memory transport and no connectable endpoint.
    // Public SDK handles always retain the resolved endpoint, and integration
    // tests exercise this production branch through a real local listener.
    #[cfg(test)]
    if session.transport().is_fixture_transport() {
        return Ok(operation_transport.clone());
    }

    let timeout =
        crate::bootstrap::discovery::resolve_timeout(None, session.configured_default_timeout());
    let deadline = operation_transport
        .operation_deadline()
        .unwrap_or_else(|| crate::transport::OperationDeadline::from_timeout(timeout));
    let transport = super::super::connect_transport_to_endpoint(
        session.endpoint(),
        deadline.remaining_timeout(),
    )
    .await?;
    Ok(transport
        .with_default_timeout(timeout)
        .with_operation_deadline(deadline))
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

fn ttl_millis(ttl: Duration) -> Result<u64> {
    validate_lease_ttl(ttl)?;
    let millis = u64::try_from(ttl.as_millis()).map_err(|_| {
        RmuxError::protocol(rmux_proto::RmuxError::Server(
            "owned session lease ttl is too large".to_owned(),
        ))
    })?;
    Ok(millis)
}

pub(super) fn validate_lease_ttl(ttl: Duration) -> Result<()> {
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
