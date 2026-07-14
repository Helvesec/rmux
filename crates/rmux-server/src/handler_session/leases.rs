use std::collections::HashMap;
use std::future::Future;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use rmux_proto::{
    CreateSessionLeaseResponse, ErrorResponse, KillSessionRequest, ReleaseSessionLeaseResponse,
    RenewSessionLeaseResponse, Response, RmuxError, SessionId, SessionName,
};

use super::RequestHandler;

const LEASE_REAPER_INTERVAL: Duration = Duration::from_millis(100);

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
    deadline: Instant,
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
                deadline: Instant::now() + ttl,
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
    ) -> bool {
        let Some(session_id) = self.live_session_id(sessions, wire_session_name, token) else {
            return false;
        };
        let ownership = self
            .ownerships
            .get_mut(&session_id)
            .expect("validated lease ownership must remain present");
        ownership.deadline = Instant::now() + ttl;
        true
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

    fn expired(&mut self, now: Instant) -> Vec<(SessionName, SessionId)> {
        let mut expired = self
            .ownerships
            .iter()
            .filter(|(_, ownership)| ownership.deadline <= now)
            .map(|(session_id, ownership)| (ownership.current_session_name.clone(), *session_id))
            .collect::<Vec<_>>();
        expired.sort_by(|(left, _), (right, _)| left.as_str().cmp(right.as_str()));
        for (session_name, session_id) in &expired {
            if self.ownerships.get(session_id).is_some_and(|ownership| {
                ownership.current_session_name == *session_name && ownership.deadline <= now
            }) {
                self.ownerships.remove(session_id);
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
        let renewed = self
            .session_leases
            .lock()
            .expect("session lease mutex must not be poisoned")
            .renew(&state.sessions, &request.session_name, request.token, ttl);
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

        assert!(!leases.renew(&sessions, &session_name, old_token, ttl));
        assert!(leases.renew(&sessions, &session_name, new_token, ttl));
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

        let expired = leases.expired(Instant::now() + Duration::from_secs(1));

        assert_eq!(expired, vec![(session_name, session_id)]);
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
        assert!(leases.renew(&sessions, &old_name, token, ttl));
        assert!(!leases.renew(&sessions, &new_name, token, ttl));
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

        assert!(leases.renew(&sessions, &wire_name, original_token, ttl));
        assert!(!leases.release(&sessions, &wire_name, u64::MAX));
        assert!(leases.release(&sessions, &wire_name, original_token));
        assert!(leases.renew(&sessions, &wire_name, homonym_token, ttl));
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

        assert!(!leases.renew(&sessions, &session_name, old_token, ttl));
        assert!(!leases.release(&sessions, &session_name, old_token));
        assert!(leases.renew(&sessions, &session_name, new_token, ttl));
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
