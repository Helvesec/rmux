use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use rmux_proto::{
    CreateSessionLeaseResponse, ErrorResponse, KillSessionRequest, ReleaseSessionLeaseResponse,
    RenewSessionLeaseResponse, Response, RmuxError, SessionName,
};

use super::RequestHandler;

const LEASE_REAPER_INTERVAL: Duration = Duration::from_millis(100);

#[derive(Debug)]
struct SessionLease {
    token: u64,
    deadline: Instant,
}

/// Daemon-side owner lease registry for app-owned sessions.
#[derive(Debug, Default)]
pub(crate) struct SessionLeaseStore {
    leases: HashMap<SessionName, SessionLease>,
    next_token: u64,
}

impl SessionLeaseStore {
    fn create(&mut self, session_name: SessionName, ttl: Duration) -> u64 {
        self.next_token = self.next_token.saturating_add(1).max(1);
        let token = self.next_token;
        self.leases.insert(
            session_name,
            SessionLease {
                token,
                deadline: Instant::now() + ttl,
            },
        );
        token
    }

    fn renew(&mut self, session_name: &SessionName, token: u64, ttl: Duration) -> bool {
        let Some(lease) = self.leases.get_mut(session_name) else {
            return false;
        };
        if lease.token != token {
            return false;
        }
        lease.deadline = Instant::now() + ttl;
        true
    }

    fn release(&mut self, session_name: &SessionName, token: u64) -> bool {
        if self
            .leases
            .get(session_name)
            .is_none_or(|lease| lease.token != token)
        {
            return false;
        }
        self.leases.remove(session_name);
        true
    }

    fn remove_sessions(&mut self, session_names: &[SessionName]) {
        for session_name in session_names {
            self.leases.remove(session_name);
        }
    }

    fn expired(&mut self, now: Instant) -> Vec<SessionName> {
        let mut expired = self
            .leases
            .iter()
            .filter(|(_, lease)| lease.deadline <= now)
            .map(|(session_name, _)| session_name.clone())
            .collect::<Vec<_>>();
        expired.sort_by(|left, right| left.as_str().cmp(right.as_str()));
        for session_name in &expired {
            self.leases.remove(session_name);
        }
        expired
    }
}

impl RequestHandler {
    pub(in crate::handler) async fn handle_create_session_lease(
        &self,
        request: rmux_proto::CreateSessionLeaseRequest,
    ) -> Response {
        let ttl = match duration_from_millis(request.ttl_millis) {
            Ok(ttl) => ttl,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        {
            let state = self.state.lock().await;
            if !state.sessions.contains_session(&request.session_name) {
                return Response::Error(ErrorResponse {
                    error: RmuxError::SessionNotFound(request.session_name.to_string()),
                });
            }
        }

        self.ensure_session_lease_janitor_started();
        let token = self
            .session_leases
            .lock()
            .expect("session lease mutex must not be poisoned")
            .create(request.session_name, ttl);

        Response::CreateSessionLease(CreateSessionLeaseResponse {
            token,
            ttl_millis: request.ttl_millis,
        })
    }

    pub(in crate::handler) async fn handle_renew_session_lease(
        &self,
        request: rmux_proto::RenewSessionLeaseRequest,
    ) -> Response {
        let ttl = match duration_from_millis(request.ttl_millis) {
            Ok(ttl) => ttl,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        let renewed = self
            .session_leases
            .lock()
            .expect("session lease mutex must not be poisoned")
            .renew(&request.session_name, request.token, ttl);
        if !renewed {
            return Response::Error(ErrorResponse {
                error: lease_lost_error(&request.session_name),
            });
        }
        Response::RenewSessionLease(RenewSessionLeaseResponse { renewed })
    }

    pub(in crate::handler) async fn handle_release_session_lease(
        &self,
        request: rmux_proto::ReleaseSessionLeaseRequest,
    ) -> Response {
        let released = self
            .session_leases
            .lock()
            .expect("session lease mutex must not be poisoned")
            .release(&request.session_name, request.token);
        Response::ReleaseSessionLease(ReleaseSessionLeaseResponse { released })
    }

    pub(in crate::handler) fn remove_session_leases(&self, session_names: &[SessionName]) {
        self.session_leases
            .lock()
            .expect("session lease mutex must not be poisoned")
            .remove_sessions(session_names);
    }

    fn ensure_session_lease_janitor_started(&self) {
        if self
            .session_lease_janitor_started
            .swap(true, Ordering::SeqCst)
        {
            return;
        }

        let weak = self.downgrade();
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(LEASE_REAPER_INTERVAL).await;
                let Some(handler) = weak.upgrade() else {
                    break;
                };
                handler.reap_expired_session_leases().await;
            }
        });
    }

    async fn reap_expired_session_leases(&self) {
        let expired = self
            .session_leases
            .lock()
            .expect("session lease mutex must not be poisoned")
            .expired(Instant::now());

        for session_name in expired {
            let response = self
                .handle_kill_session(KillSessionRequest {
                    target: session_name,
                    kill_all_except_target: false,
                    clear_alerts: false,
                    kill_group: false,
                })
                .await;
            if !matches!(
                response,
                Response::KillSession(_) | Response::Error(ErrorResponse { .. })
            ) {
                tracing::debug!(?response, "unexpected lease reaper response");
            }
        }
    }
}

fn duration_from_millis(ttl_millis: u64) -> Result<Duration, RmuxError> {
    if ttl_millis == 0 {
        return Err(RmuxError::Server(
            "session lease ttl must be greater than zero".to_owned(),
        ));
    }
    if ttl_millis < rmux_proto::MIN_SESSION_LEASE_TTL_MILLIS {
        return Err(RmuxError::Server(format!(
            "session lease ttl must be at least {}ms",
            rmux_proto::MIN_SESSION_LEASE_TTL_MILLIS
        )));
    }
    Ok(Duration::from_millis(ttl_millis))
}

fn lease_lost_error(session_name: &SessionName) -> RmuxError {
    RmuxError::owned_session_lease_lost(session_name.clone())
}
