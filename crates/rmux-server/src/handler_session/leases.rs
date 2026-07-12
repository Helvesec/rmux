use std::collections::HashMap;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use rmux_proto::{
    CreateSessionLeaseResponse, ErrorResponse, KillSessionRequest, ReleaseSessionLeaseResponse,
    RenewSessionLeaseResponse, Response, RmuxError, SessionId, SessionName,
};

use super::RequestHandler;

const LEASE_REAPER_INTERVAL: Duration = Duration::from_millis(100);

#[cfg(test)]
#[derive(Debug, Default)]
pub(in crate::handler) struct ExpiredSessionLeaseReapPause {
    pub(in crate::handler) reached: tokio::sync::Notify,
    pub(in crate::handler) release: tokio::sync::Notify,
}

#[cfg(test)]
static EXPIRED_SESSION_LEASE_REAP_PAUSE: std::sync::Mutex<
    Option<(
        SessionName,
        SessionId,
        std::sync::Arc<ExpiredSessionLeaseReapPause>,
    )>,
> = std::sync::Mutex::new(None);

#[derive(Debug)]
struct SessionLease {
    token: u64,
    session_id: SessionId,
    deadline: Instant,
}

/// Daemon-side owner lease registry for app-owned sessions.
#[derive(Debug, Default)]
pub(crate) struct SessionLeaseStore {
    leases: HashMap<SessionName, SessionLease>,
    next_token: u64,
}

impl SessionLeaseStore {
    fn create(&mut self, session_name: SessionName, session_id: SessionId, ttl: Duration) -> u64 {
        self.next_token = self.next_token.saturating_add(1).max(1);
        let token = self.next_token;
        self.leases.insert(
            session_name,
            SessionLease {
                token,
                session_id,
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

    fn remove_sessions(&mut self, sessions: &[(SessionName, SessionId)]) {
        for (session_name, session_id) in sessions {
            if self
                .leases
                .get(session_name)
                .is_some_and(|lease| lease.session_id == *session_id)
            {
                self.leases.remove(session_name);
            }
        }
    }

    fn rename_session(
        &mut self,
        old_name: &SessionName,
        new_name: SessionName,
        session_id: SessionId,
    ) -> bool {
        if self
            .leases
            .get(old_name)
            .is_none_or(|lease| lease.session_id != session_id)
        {
            return false;
        }
        let lease = self
            .leases
            .remove(old_name)
            .expect("matching session lease must still exist");
        self.leases.insert(new_name, lease);
        true
    }

    fn expired(&mut self, now: Instant) -> Vec<(SessionName, SessionId)> {
        let mut expired = self
            .leases
            .iter()
            .filter(|(_, lease)| lease.deadline <= now)
            .map(|(session_name, lease)| (session_name.clone(), lease.session_id))
            .collect::<Vec<_>>();
        expired.sort_by(|(left, _), (right, _)| left.as_str().cmp(right.as_str()));
        for (session_name, session_id) in &expired {
            if self
                .leases
                .get(session_name)
                .is_some_and(|lease| lease.session_id == *session_id && lease.deadline <= now)
            {
                self.leases.remove(session_name);
            }
        }
        expired
    }
}

impl RequestHandler {
    #[cfg(test)]
    pub(in crate::handler) fn install_expired_session_lease_reap_pause(
        &self,
        session_name: SessionName,
        session_id: SessionId,
    ) -> std::sync::Arc<ExpiredSessionLeaseReapPause> {
        let pause = std::sync::Arc::new(ExpiredSessionLeaseReapPause::default());
        *EXPIRED_SESSION_LEASE_REAP_PAUSE
            .lock()
            .expect("expired session lease reap pause lock") =
            Some((session_name, session_id, pause.clone()));
        pause
    }

    #[cfg(test)]
    async fn pause_after_expired_session_lease_extraction(
        &self,
        expired: &[(SessionName, SessionId)],
    ) {
        let pause = {
            let mut installed = EXPIRED_SESSION_LEASE_REAP_PAUSE
                .lock()
                .expect("expired session lease reap pause lock");
            let matches_expired = installed
                .as_ref()
                .is_some_and(|(paused_name, paused_id, _)| {
                    expired.iter().any(|(session_name, session_id)| {
                        session_name == paused_name && session_id == paused_id
                    })
                });
            matches_expired.then(|| {
                installed
                    .take()
                    .expect("matching lease reap pause remains installed")
                    .2
            })
        };
        let Some(pause) = pause else {
            return;
        };
        pause.reached.notify_one();
        pause.release.notified().await;
    }

    pub(in crate::handler) async fn handle_create_session_lease(
        &self,
        request: rmux_proto::CreateSessionLeaseRequest,
    ) -> Response {
        let ttl = match duration_from_millis(request.ttl_millis) {
            Ok(ttl) => ttl,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        let state = self.state.lock().await;
        let Some(session_id) = state
            .sessions
            .session(&request.session_name)
            .map(rmux_core::Session::id)
        else {
            return Response::Error(ErrorResponse {
                error: RmuxError::SessionNotFound(request.session_name.to_string()),
            });
        };
        self.ensure_session_lease_janitor_started();
        let token = self
            .session_leases
            .lock()
            .expect("session lease mutex must not be poisoned")
            .create(request.session_name, session_id, ttl);
        drop(state);

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

    pub(in crate::handler) fn remove_session_leases(&self, sessions: &[(SessionName, SessionId)]) {
        self.session_leases
            .lock()
            .expect("session lease mutex must not be poisoned")
            .remove_sessions(sessions);
    }

    pub(in crate::handler) fn rename_session_lease(
        &self,
        old_name: &SessionName,
        new_name: &SessionName,
        session_id: SessionId,
    ) {
        self.session_leases
            .lock()
            .expect("session lease mutex must not be poisoned")
            .rename_session(old_name, new_name.clone(), session_id);
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

        #[cfg(test)]
        self.pause_after_expired_session_lease_extraction(&expired)
            .await;

        for (session_name, session_id) in expired {
            let response = self
                .handle_kill_session_identity(
                    KillSessionRequest {
                        target: session_name,
                        kill_all_except_target: false,
                        clear_alerts: false,
                        kill_group: false,
                    },
                    session_id,
                )
                .await;
            if !matches!(
                response,
                Response::KillSession(_) | Response::Error(ErrorResponse { .. })
            ) {
                tracing::debug!(?response, "unexpected lease reaper response");
            }
        }
        self.refresh_hook_identity_aliases().await;
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stale_session_cleanup_preserves_recreated_session_lease() {
        let mut leases = SessionLeaseStore::default();
        let session_name = SessionName::new("reused").expect("valid session name");
        let old_session_id = SessionId::new(41);
        let new_session_id = SessionId::new(42);
        let ttl = Duration::from_secs(30);

        let _old_token = leases.create(session_name.clone(), old_session_id, ttl);
        let new_token = leases.create(session_name.clone(), new_session_id, ttl);
        leases.remove_sessions(&[(session_name.clone(), old_session_id)]);

        assert!(leases.renew(&session_name, new_token, ttl));
        assert_eq!(
            leases
                .leases
                .get(&session_name)
                .map(|lease| lease.session_id),
            Some(new_session_id)
        );
    }

    #[test]
    fn expired_lease_carries_stable_session_identity() {
        let mut leases = SessionLeaseStore::default();
        let session_name = SessionName::new("expired").expect("valid session name");
        let session_id = SessionId::new(73);
        let _token = leases.create(session_name.clone(), session_id, Duration::from_millis(1));

        let expired = leases.expired(Instant::now() + Duration::from_secs(1));

        assert_eq!(expired, vec![(session_name, session_id)]);
    }

    #[test]
    fn rename_moves_only_the_matching_session_lease() {
        let mut leases = SessionLeaseStore::default();
        let old_name = SessionName::new("before").expect("valid session name");
        let new_name = SessionName::new("after").expect("valid session name");
        let session_id = SessionId::new(81);
        let ttl = Duration::from_secs(30);
        let token = leases.create(old_name.clone(), session_id, ttl);

        assert!(leases.rename_session(&old_name, new_name.clone(), session_id));
        assert!(!leases.renew(&old_name, token, ttl));
        assert!(leases.renew(&new_name, token, ttl));
    }

    #[test]
    fn stale_rename_preserves_a_recreated_session_lease() {
        let mut leases = SessionLeaseStore::default();
        let old_name = SessionName::new("reused").expect("valid session name");
        let new_name = SessionName::new("renamed").expect("valid session name");
        let old_session_id = SessionId::new(91);
        let new_session_id = SessionId::new(92);
        let ttl = Duration::from_secs(30);
        let _old_token = leases.create(old_name.clone(), old_session_id, ttl);
        let new_token = leases.create(old_name.clone(), new_session_id, ttl);

        assert!(!leases.rename_session(&old_name, new_name.clone(), old_session_id));
        assert!(leases.renew(&old_name, new_token, ttl));
        assert!(!leases.renew(&new_name, new_token, ttl));
    }
}
