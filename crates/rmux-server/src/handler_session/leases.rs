use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use rmux_proto::{
    CreateSessionLeaseResponse, ErrorResponse, KillSessionRequest, ReleaseSessionLeaseResponse,
    RenewSessionLeaseResponse, Response, RmuxError, SessionId, SessionName,
};

use super::RequestHandler;

#[path = "leases/clock.rs"]
mod clock;

use clock::{LeaseDeadline, ReaperSchedule, ReaperWake, REAPER_INTERVAL};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum SessionLeaseCreateAddressing {
    Nominal,
    StableId,
}

tokio::task_local! {
    static SESSION_LEASE_CREATE_ADDRESSING: SessionLeaseCreateAddressing;
}

pub(crate) async fn with_session_lease_create_addressing<T, F>(
    addressing: SessionLeaseCreateAddressing,
    future: F,
) -> T
where
    F: Future<Output = T>,
{
    SESSION_LEASE_CREATE_ADDRESSING
        .scope(addressing, future)
        .await
}

fn current_session_lease_create_addressing() -> SessionLeaseCreateAddressing {
    SESSION_LEASE_CREATE_ADDRESSING
        .try_with(|addressing| *addressing)
        .unwrap_or(SessionLeaseCreateAddressing::Nominal)
}

#[cfg(test)]
#[derive(Debug, Default)]
pub(in crate::handler) struct ExpiredSessionLeaseReapPause {
    pub(in crate::handler) reached: tokio::sync::Notify,
    pub(in crate::handler) release: tokio::sync::Notify,
    pub(in crate::handler) completed: tokio::sync::Notify,
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
struct SessionOwnership {
    token: u64,
    wire_session_name: SessionName,
    current_session_name: SessionName,
    deadline: LeaseDeadline,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ExpiredSessionLease {
    session_name: SessionName,
    session_id: SessionId,
    token: u64,
}

/// Daemon-side owner lease registry for app-owned sessions.
#[derive(Debug, Default)]
pub(crate) struct SessionLeaseStore {
    ownerships: HashMap<SessionId, SessionOwnership>,
    next_token: u64,
}

impl SessionLeaseStore {
    fn create_lease(
        &mut self,
        wire_session_name: SessionName,
        current_session_name: SessionName,
        session_id: SessionId,
        ttl: Duration,
    ) -> Result<u64, RmuxError> {
        let deadline = LeaseDeadline::from_now(ttl).ok_or_else(lease_ttl_range_error)?;
        let token = self
            .next_token
            .checked_add(1)
            .ok_or_else(|| RmuxError::Server("session lease token space exhausted".to_owned()))?;
        self.next_token = token;
        self.ownerships.insert(
            session_id,
            SessionOwnership {
                token,
                wire_session_name,
                current_session_name,
                deadline,
            },
        );
        Ok(token)
    }

    fn renew(
        &mut self,
        sessions: &rmux_core::SessionStore,
        wire_session_name: &SessionName,
        token: u64,
        ttl: Duration,
    ) -> Result<bool, RmuxError> {
        let Some(session_id) = self.live_session_id(sessions, wire_session_name, token) else {
            return Ok(false);
        };
        let ownership = self
            .ownerships
            .get_mut(&session_id)
            .expect("validated lease ownership must remain present");
        if !ownership.deadline.renew_from_now(ttl) {
            return Err(lease_ttl_range_error());
        }
        Ok(true)
    }

    fn release(
        &mut self,
        sessions: &rmux_core::SessionStore,
        wire_session_name: &SessionName,
        token: u64,
    ) -> bool {
        let Some(session_id) = self.live_session_id(sessions, wire_session_name, token) else {
            return false;
        };
        self.ownerships.remove(&session_id);
        true
    }

    fn live_session_id(
        &self,
        sessions: &rmux_core::SessionStore,
        wire_session_name: &SessionName,
        token: u64,
    ) -> Option<SessionId> {
        let (session_id, ownership) = self.ownerships.iter().find(|(_, ownership)| {
            ownership.token == token && ownership.wire_session_name == *wire_session_name
        })?;
        sessions
            .session_by_id(*session_id)
            .is_some_and(|session| session.name() == &ownership.current_session_name)
            .then_some(*session_id)
    }

    fn remove_sessions(&mut self, sessions: &[(SessionName, SessionId)]) {
        for (session_name, session_id) in sessions {
            if self
                .ownerships
                .get(session_id)
                .is_some_and(|ownership| ownership.current_session_name == *session_name)
            {
                self.ownerships.remove(session_id);
            }
        }
    }

    fn rename_session(
        &mut self,
        old_name: &SessionName,
        new_name: SessionName,
        session_id: SessionId,
    ) -> bool {
        let Some(ownership) = self.ownerships.get_mut(&session_id) else {
            return false;
        };
        if ownership.current_session_name != *old_name {
            return false;
        }
        ownership.current_session_name = new_name;
        true
    }

    fn expired(
        &mut self,
        now: Instant,
        reaper_wake: Option<ReaperWake>,
    ) -> Vec<ExpiredSessionLease> {
        if let Some(wake) = reaper_wake {
            for ownership in self.ownerships.values_mut() {
                ownership.deadline.preserve_budget_across_reaper_pause(wake);
            }
        }
        let mut expired = self
            .ownerships
            .iter()
            .filter(|(_, ownership)| ownership.deadline.is_expired_at(now))
            .map(|(session_id, ownership)| ExpiredSessionLease {
                session_name: ownership.current_session_name.clone(),
                session_id: *session_id,
                token: ownership.token,
            })
            .collect::<Vec<_>>();
        expired.sort_by(|left, right| left.session_name.as_str().cmp(right.session_name.as_str()));
        expired
    }

    fn claim_expired(&mut self, session_id: SessionId, token: u64, now: Instant) -> bool {
        if self.ownerships.get(&session_id).is_none_or(|ownership| {
            ownership.token != token || !ownership.deadline.is_expired_at(now)
        }) {
            return false;
        }
        self.ownerships.remove(&session_id);
        true
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
        expired: &[ExpiredSessionLease],
    ) -> Option<std::sync::Arc<ExpiredSessionLeaseReapPause>> {
        let pause = {
            let mut installed = EXPIRED_SESSION_LEASE_REAP_PAUSE
                .lock()
                .expect("expired session lease reap pause lock");
            let matches_expired = installed
                .as_ref()
                .is_some_and(|(paused_name, paused_id, _)| {
                    expired.iter().any(|candidate| {
                        candidate.session_name == *paused_name && candidate.session_id == *paused_id
                    })
                });
            matches_expired.then(|| {
                installed
                    .take()
                    .expect("matching lease reap pause remains installed")
                    .2
            })
        };
        let pause = pause?;
        pause.reached.notify_one();
        pause.release.notified().await;
        Some(pause)
    }

    pub(super) fn claim_expired_session_lease(&self, session_id: SessionId, token: u64) -> bool {
        self.session_leases
            .lock()
            .expect("session lease mutex must not be poisoned")
            .claim_expired(session_id, token, Instant::now())
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
        let Some((current_session_name, session_id)) = resolve_session_lease_create_target(
            &state.sessions,
            &request.session_name,
            current_session_lease_create_addressing(),
        ) else {
            return Response::Error(ErrorResponse {
                error: RmuxError::SessionNotFound(request.session_name.to_string()),
            });
        };
        let token = match self
            .session_leases
            .lock()
            .expect("session lease mutex must not be poisoned")
            .create_lease(request.session_name, current_session_name, session_id, ttl)
        {
            Ok(token) => token,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        drop(state);
        self.ensure_session_lease_janitor_started();

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
        let state = self.state.lock().await;
        let renewed = match self
            .session_leases
            .lock()
            .expect("session lease mutex must not be poisoned")
            .renew(&state.sessions, &request.session_name, request.token, ttl)
        {
            Ok(renewed) => renewed,
            Err(error) => return Response::Error(ErrorResponse { error }),
        };
        drop(state);
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
        let state = self.state.lock().await;
        let released = self
            .session_leases
            .lock()
            .expect("session lease mutex must not be poisoned")
            .release(&state.sessions, &request.session_name, request.token);
        drop(state);
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
        let mut reaper_schedule = ReaperSchedule::new(Instant::now());
        tokio::spawn(async move {
            loop {
                tokio::time::sleep(REAPER_INTERVAL).await;
                let Some(handler) = weak.upgrade() else {
                    break;
                };
                let wake = reaper_schedule.observe_wake(Instant::now());
                handler.reap_expired_session_leases(wake).await;
            }
        });
    }

    async fn reap_expired_session_leases(&self, wake: ReaperWake) {
        let observed_at = wake.observed_at();
        let expired = self
            .session_leases
            .lock()
            .expect("session lease mutex must not be poisoned")
            .expired(observed_at, Some(wake));

        #[cfg(test)]
        let reap_pause = self
            .pause_after_expired_session_lease_extraction(&expired)
            .await;

        for candidate in expired {
            let response = self
                .handle_kill_expired_session_lease_identity(
                    KillSessionRequest {
                        target: candidate.session_name,
                        kill_all_except_target: false,
                        clear_alerts: false,
                        kill_group: false,
                    },
                    candidate.session_id,
                    candidate.token,
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
        #[cfg(test)]
        if let Some(pause) = reap_pause {
            pause.completed.notify_one();
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
    let ttl = Duration::from_millis(ttl_millis);
    if Instant::now().checked_add(ttl).is_none() {
        return Err(lease_ttl_range_error());
    }
    Ok(ttl)
}

fn lease_ttl_range_error() -> RmuxError {
    RmuxError::Server("session lease ttl exceeds the platform deadline range".to_owned())
}

fn resolve_session_lease_create_target(
    sessions: &rmux_core::SessionStore,
    target: &SessionName,
    addressing: SessionLeaseCreateAddressing,
) -> Option<(SessionName, SessionId)> {
    match addressing {
        SessionLeaseCreateAddressing::Nominal => sessions
            .session(target)
            .map(|session| (session.name().clone(), session.id())),
        SessionLeaseCreateAddressing::StableId => {
            let raw_id = target.as_str().strip_prefix('$')?;
            let session_id = raw_id.parse::<u32>().ok().map(SessionId::new)?;
            sessions
                .session_by_id(session_id)
                .map(|session| (session.name().clone(), session_id))
        }
    }
}

fn lease_lost_error(session_name: &SessionName) -> RmuxError {
    RmuxError::owned_session_lease_lost(session_name.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lease_ttl_range_validation_matches_the_platform_deadline() {
        let oversized = Duration::from_millis(u64::MAX);
        let platform_accepts = Instant::now().checked_add(oversized).is_some();
        let validated = duration_from_millis(u64::MAX);

        assert_eq!(
            validated.is_ok(),
            platform_accepts,
            "wire ttl validation must match the platform Instant range"
        );
    }

    #[test]
    fn lease_store_rejects_overflow_without_poisoning_create_or_renew() {
        let mut leases = SessionLeaseStore::default();
        let mut sessions = rmux_core::SessionStore::new();
        let (session_name, session_id) = create_session(&mut sessions, "bounded");
        let oversized = Duration::MAX;

        let create_error = leases
            .create_lease(
                session_name.clone(),
                session_name.clone(),
                session_id,
                oversized,
            )
            .expect_err("overflowing create must fail");
        assert!(create_error
            .to_string()
            .contains("exceeds the platform deadline range"));

        let valid_ttl = Duration::from_secs(30);
        let token = leases
            .create_lease(
                session_name.clone(),
                session_name.clone(),
                session_id,
                valid_ttl,
            )
            .expect("valid create still succeeds");
        let renew_error = leases
            .renew(&sessions, &session_name, token, oversized)
            .expect_err("overflowing renewal must fail");
        assert!(renew_error
            .to_string()
            .contains("exceeds the platform deadline range"));
        assert!(leases
            .renew(&sessions, &session_name, token, valid_ttl)
            .expect("valid renewal still succeeds"));
    }

    fn create_session(
        sessions: &mut rmux_core::SessionStore,
        raw_name: &str,
    ) -> (SessionName, SessionId) {
        let session_name = SessionName::new(raw_name).expect("valid session name");
        sessions
            .create_session(session_name.clone(), rmux_proto::TerminalSize::new(80, 24))
            .expect("session creation succeeds");
        let session_id = sessions
            .session(&session_name)
            .expect("created session exists")
            .id();
        (session_name, session_id)
    }

    #[test]
    fn stale_session_cleanup_preserves_recreated_session_lease() {
        let mut leases = SessionLeaseStore::default();
        let mut sessions = rmux_core::SessionStore::new();
        let (session_name, old_session_id) = create_session(&mut sessions, "reused");
        let ttl = Duration::from_secs(30);

        let old_token = leases
            .create_lease(
                session_name.clone(),
                session_name.clone(),
                old_session_id,
                ttl,
            )
            .expect("old lease token");
        leases.remove_sessions(&[(session_name.clone(), old_session_id)]);
        sessions
            .remove_session(&session_name)
            .expect("old session removal succeeds");
        let (_, new_session_id) = create_session(&mut sessions, "reused");
        let new_token = leases
            .create_lease(
                session_name.clone(),
                session_name.clone(),
                new_session_id,
                ttl,
            )
            .expect("new lease token");

        assert!(!leases
            .renew(&sessions, &session_name, old_token, ttl)
            .expect("valid ttl"));
        assert!(leases
            .renew(&sessions, &session_name, new_token, ttl)
            .expect("valid ttl"));
        assert_eq!(
            leases
                .ownerships
                .get(&new_session_id)
                .map(|ownership| ownership.current_session_name.clone()),
            Some(session_name)
        );
    }

    #[test]
    fn expired_lease_carries_stable_session_identity() {
        let mut leases = SessionLeaseStore::default();
        let session_name = SessionName::new("expired").expect("valid session name");
        let session_id = SessionId::new(73);
        let _token = leases
            .create_lease(
                session_name.clone(),
                session_name.clone(),
                session_id,
                Duration::from_millis(1),
            )
            .expect("lease token");

        let expired = leases.expired(Instant::now() + Duration::from_secs(1), None);

        assert_eq!(
            expired,
            vec![ExpiredSessionLease {
                session_name,
                session_id,
                token: 1,
            }]
        );
    }

    #[test]
    fn rename_moves_only_the_matching_session_lease() {
        let mut leases = SessionLeaseStore::default();
        let mut sessions = rmux_core::SessionStore::new();
        let (old_name, session_id) = create_session(&mut sessions, "before");
        let new_name = SessionName::new("after").expect("valid session name");
        let ttl = Duration::from_secs(30);
        let token = leases
            .create_lease(old_name.clone(), old_name.clone(), session_id, ttl)
            .expect("lease token");

        sessions
            .rename_session(&old_name, new_name.clone())
            .expect("session rename succeeds");
        assert!(leases.rename_session(&old_name, new_name.clone(), session_id));
        assert!(leases
            .renew(&sessions, &old_name, token, ttl)
            .expect("valid ttl"));
        assert!(!leases
            .renew(&sessions, &new_name, token, ttl)
            .expect("valid ttl"));
        assert_eq!(
            leases.ownerships.get(&session_id).map(|ownership| {
                (
                    &ownership.wire_session_name,
                    &ownership.current_session_name,
                )
            }),
            Some((&old_name, &new_name))
        );
    }

    #[test]
    fn renamed_lease_token_selects_original_identity_over_new_homonym() {
        let mut leases = SessionLeaseStore::default();
        let mut sessions = rmux_core::SessionStore::new();
        let (wire_name, original_id) = create_session(&mut sessions, "reused");
        let renamed = SessionName::new("renamed").expect("valid session name");
        let ttl = Duration::from_secs(30);
        let original_token = leases
            .create_lease(wire_name.clone(), wire_name.clone(), original_id, ttl)
            .expect("original lease token");

        sessions
            .rename_session(&wire_name, renamed.clone())
            .expect("session rename succeeds");
        assert!(leases.rename_session(&wire_name, renamed, original_id));
        let (_, homonym_id) = create_session(&mut sessions, "reused");
        let homonym_token = leases
            .create_lease(wire_name.clone(), wire_name.clone(), homonym_id, ttl)
            .expect("homonym lease token");

        assert!(leases
            .renew(&sessions, &wire_name, original_token, ttl)
            .expect("valid ttl"));
        assert!(!leases.release(&sessions, &wire_name, u64::MAX));
        assert!(leases.release(&sessions, &wire_name, original_token));
        assert!(leases
            .renew(&sessions, &wire_name, homonym_token, ttl)
            .expect("valid ttl"));
        assert!(leases.ownerships.contains_key(&homonym_id));
        assert!(!leases.ownerships.contains_key(&original_id));
    }

    #[test]
    fn revoked_lease_cannot_attach_to_recreated_same_name() {
        let mut leases = SessionLeaseStore::default();
        let mut sessions = rmux_core::SessionStore::new();
        let (session_name, old_session_id) = create_session(&mut sessions, "recreated");
        let ttl = Duration::from_secs(30);
        let old_token = leases
            .create_lease(
                session_name.clone(),
                session_name.clone(),
                old_session_id,
                ttl,
            )
            .expect("old lease token");

        leases.remove_sessions(&[(session_name.clone(), old_session_id)]);
        sessions
            .remove_session(&session_name)
            .expect("old session removal succeeds");
        let (_, new_session_id) = create_session(&mut sessions, "recreated");
        let new_token = leases
            .create_lease(
                session_name.clone(),
                session_name.clone(),
                new_session_id,
                ttl,
            )
            .expect("new lease token");

        assert!(!leases
            .renew(&sessions, &session_name, old_token, ttl)
            .expect("valid ttl"));
        assert!(!leases.release(&sessions, &session_name, old_token));
        assert!(leases
            .renew(&sessions, &session_name, new_token, ttl)
            .expect("valid ttl"));
        assert!(leases.ownerships.contains_key(&new_session_id));
    }

    #[test]
    fn exhausted_token_space_fails_closed_without_reusing_a_token() {
        let mut leases = SessionLeaseStore {
            ownerships: HashMap::new(),
            next_token: u64::MAX,
        };
        let session_name = SessionName::new("exhausted").expect("valid session name");

        let error = leases
            .create_lease(
                session_name.clone(),
                session_name,
                SessionId::new(1),
                Duration::from_secs(30),
            )
            .expect_err("token exhaustion must reject lease creation");

        assert!(error.to_string().contains("token space exhausted"));
        assert!(leases.ownerships.is_empty());
    }
}
