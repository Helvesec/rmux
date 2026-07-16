use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rmux_core::PaneId;
use rmux_proto::{
    CommandOutput, CreateWebShareRequest, ListWebSharesRequest, LookupWebShareRequest,
    StopAllWebSharesRequest, StopWebShareRequest, WebShareConfigRequest, WebShareConfigResponse,
    WebShareCreatedResponse, WebShareListResponse, WebShareListener, WebShareLookupResponse,
    WebShareResponse, WebShareStoppedAllResponse, WebShareStoppedResponse,
};
use rmux_proto::{RmuxError, SessionId, SessionName};
use tokio::sync::watch;
use tokio::time::sleep;
use tracing::info;

#[path = "registry_output.rs"]
mod output;
#[path = "registry_state.rs"]
mod state;

use super::backoff::{AttemptOutcome, AttemptReservation, AuthBackoff};
use super::connection_limit::{ConnectionLimit, DEFAULT_AUTHENTICATED_CONNECTION_LIMIT};
use super::leases::LeaseBook;
use super::origin::{validate_frontend_url, validate_public_base_url, FrontendUrl};
use super::pairing::WebSharePairingCodes;
use super::record::{
    system_time_to_unix, WebSessionTarget, WebShareAccess, WebShareRecord, WebShareRevokeReason,
    WebShareTarget,
};
use super::secrets::{
    derive_spectator_token, random_share_id, random_token, valid_token_id_shape, SecretHash,
};
use super::settings::WebShareSettings;
use super::tunnel::TunnelInfo;
use output::{created_output, list_output, lookup_output, stopped_output, CreatedOutput};
pub(crate) use state::{ExpiredWebShare, WebShareExpiryPoll};
use state::{WebListenerState, WebShareState};

const MAX_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;
const DEFAULT_MAX_OPERATORS: u16 = 1;
const DEFAULT_MAX_SPECTATORS: u16 = 12;
const UNKNOWN_TOKEN_BACKOFF_KEY: &str = "token_id:<unknown>";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct WebShareRoleLimits {
    pub(crate) max_spectators: Option<u16>,
    pub(crate) max_operators: Option<u16>,
}

#[derive(Debug)]
pub(crate) struct ResolvedCreateWebShareRequest {
    request: CreateWebShareRequest,
    target: WebShareTarget,
    tunnel: Option<TunnelInfo>,
}

impl ResolvedCreateWebShareRequest {
    pub(crate) fn new(request: CreateWebShareRequest, target: WebShareTarget) -> Self {
        Self {
            request,
            target,
            tunnel: None,
        }
    }

    pub(crate) fn public_base_url(&self) -> Option<&str> {
        self.request.public_base_url.as_deref()
    }

    pub(crate) const fn request(&self) -> &CreateWebShareRequest {
        &self.request
    }

    pub(crate) fn tunnel_provider(&self) -> Option<&str> {
        self.request.tunnel_provider.as_deref()
    }

    pub(crate) fn with_tunnel(mut self, tunnel: TunnelInfo) -> Self {
        self.request.public_base_url = Some(tunnel.public_url.clone());
        self.request.tunnel_provider = None;
        self.tunnel = Some(tunnel);
        self
    }

    pub(crate) fn expiry_kill_target(&self) -> Option<WebSessionTarget> {
        if !self.request.kill_session_on_expire {
            return None;
        }
        match &self.target {
            WebShareTarget::Session(target) => Some(target.clone()),
            WebShareTarget::Pane(_) => None,
        }
    }

    pub(crate) const fn target(&self) -> &WebShareTarget {
        &self.target
    }
}

#[cfg(test)]
impl From<CreateWebShareRequest> for ResolvedCreateWebShareRequest {
    fn from(request: CreateWebShareRequest) -> Self {
        let target = match &request.scope {
            rmux_proto::WebShareScope::Pane(target) => WebShareTarget::pane(target.clone()),
            rmux_proto::WebShareScope::Session(name) => {
                WebShareTarget::session(name.clone(), rmux_proto::SessionId::new(0))
            }
        };
        Self {
            request,
            target,
            tunnel: None,
        }
    }
}

#[derive(Debug)]
pub(crate) struct WebShareRegistry {
    backoff: AuthBackoff,
    connection_limit: std::sync::Arc<ConnectionLimit>,
    inner: Mutex<WebShareState>,
    next_id: AtomicU64,
    settings: Mutex<WebShareSettings>,
}

impl Default for WebShareRegistry {
    fn default() -> Self {
        Self::new(WebShareSettings::default())
    }
}

impl WebShareRegistry {
    #[cfg(test)]
    pub(crate) async fn connect(
        &self,
        token: &str,
        pin: Option<&str>,
    ) -> Result<WebShareAccess, RmuxError> {
        let token_id = SecretHash::from_secret(token).token_id();
        self.connect_token_id(&token_id, pin).await
    }

    #[cfg(test)]
    pub(crate) fn known_token_origin_allowed(&self, token: &str, origin: &str) -> Option<bool> {
        let token_id = SecretHash::from_secret(token).token_id();
        self.pre_auth_token(&token_id, origin)
            .map(|(_, allowed)| allowed)
    }

    #[cfg(test)]
    pub(crate) fn new_with_authenticated_connection_limit(max_connections: usize) -> Self {
        Self {
            backoff: AuthBackoff::new(),
            connection_limit: ConnectionLimit::new(max_connections),
            inner: Mutex::new(WebShareState::default()),
            next_id: AtomicU64::new(1),
            settings: Mutex::new(WebShareSettings::default()),
        }
    }

    pub(crate) fn new(settings: WebShareSettings) -> Self {
        Self {
            backoff: AuthBackoff::new(),
            connection_limit: ConnectionLimit::new(DEFAULT_AUTHENTICATED_CONNECTION_LIMIT),
            inner: Mutex::new(WebShareState::default()),
            next_id: AtomicU64::new(1),
            settings: Mutex::new(settings),
        }
    }

    pub(crate) fn handle(
        &self,
        request: rmux_proto::WebShareRequest,
    ) -> Result<WebShareResponse, RmuxError> {
        match request {
            rmux_proto::WebShareRequest::Create(_) => Err(RmuxError::Server(
                "web-share create requires a resolved server target".to_owned(),
            )),
            rmux_proto::WebShareRequest::List(request) => {
                Ok(WebShareResponse::List(self.list(request)))
            }
            rmux_proto::WebShareRequest::Stop(request) => {
                Ok(WebShareResponse::Stopped(self.stop(request)))
            }
            rmux_proto::WebShareRequest::StopAll(request) => {
                Ok(WebShareResponse::StoppedAll(self.stop_all(request)))
            }
            rmux_proto::WebShareRequest::Lookup(request) => {
                Ok(WebShareResponse::Lookup(self.lookup(request)))
            }
            rmux_proto::WebShareRequest::Config(request) => {
                self.config(request).map(WebShareResponse::Config)
            }
        }
    }

    pub(crate) fn create(
        &self,
        resolved: impl Into<ResolvedCreateWebShareRequest>,
    ) -> Result<WebShareCreatedResponse, RmuxError> {
        let resolved = resolved.into();
        let ResolvedCreateWebShareRequest {
            request,
            target,
            tunnel,
        } = resolved;
        let limits = self.validate_create_options(&request)?;
        let settings = self.settings();
        let endpoint_origin =
            self.endpoint_origin(&settings, request.public_base_url.as_deref())?;
        let frontend = self.frontend(&settings, request.frontend_url.as_deref())?;
        let share_id = self.next_share_id()?;
        let operator_token = request.operator.then(random_token).transpose()?;
        let spectator_token = if request.spectator {
            match operator_token.as_deref() {
                Some(token) => Some(derive_spectator_token(token)?),
                None => Some(random_token()?),
            }
        } else {
            None
        };
        let spectator_token_hash = spectator_token.as_deref().map(SecretHash::from_secret);
        let operator_token_hash = operator_token.as_deref().map(SecretHash::from_secret);
        let pairing_codes = WebSharePairingCodes::for_request(&request)?;
        if request.kill_session_on_expire && !matches!(target, WebShareTarget::Session(_)) {
            return Err(RmuxError::Server(
                "web-share --kill-session-on-expire requires a session target".to_owned(),
            ));
        }
        let expires_at = resolve_expiration(&request)?;
        let ttl_seconds = expires_at
            .and_then(|deadline| deadline.duration_since(SystemTime::now()).ok())
            .map(|duration| duration.as_secs())
            .unwrap_or(0);
        let controls = request.operator && matches!(target, WebShareTarget::Session(_));
        let lease_book = LeaseBook::new(
            limits.max_spectators.map(usize::from),
            limits.max_operators.map(usize::from),
        );
        let (revoke_tx, _) = watch::channel(None);
        let terminal_palette = request.terminal_palette.as_deref().cloned();

        let tunnel_provider = tunnel.as_ref().map(|tunnel| tunnel.provider.clone());
        let tunnel_public_url = tunnel.as_ref().map(|tunnel| tunnel.public_url.clone());
        let record = WebShareRecord {
            allow_loopback_development_origins: request.public_base_url.is_none(),
            endpoint_origin,
            expires_at,
            frontend_origin: frontend.origin,
            frontend_url: frontend.url,
            kill_session_on_expire: request.kill_session_on_expire,
            lease_book,
            max_operators: limits.max_operators,
            max_spectators: limits.max_spectators,
            operator_token_hash,
            pairing_codes: pairing_codes.clone(),
            revoke_tx,
            controls,
            share_id: share_id.clone(),
            target: target.clone(),
            terminal_palette,
            url_options: request.url_options,
            spectator_token_hash,
            _tunnel: tunnel.map(|tunnel| tunnel.handle),
            operator: request.operator,
            spectator: request.spectator,
        };

        let spectator_url = record.spectator_url(spectator_token.as_deref());
        let operator_url = record.operator_url(operator_token.as_deref());
        let summary_scope = record.target.scope();
        let expires_at_unix = expires_at.and_then(system_time_to_unix);
        self.inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .insert(record);
        info!(
            share_id = %share_id,
            scope = %summary_scope,
            operator = request.operator,
            spectator = request.spectator,
            controls,
            ttl_seconds,
            max_spectators = ?limits.max_spectators,
            max_operators = ?limits.max_operators,
            public = request.public_base_url.is_some(),
            tunnel_provider = tunnel_provider.as_deref().unwrap_or(""),
            pin_required = request.require_pin,
            listener_port = settings.port,
            "web_share_created"
        );

        let output = created_output(CreatedOutput {
            spectator_url: spectator_url.as_deref(),
            operator_url_emitted: operator_url.is_some(),
            tunnel_provider: tunnel_provider.as_deref(),
            tunnel_public_url: tunnel_public_url.as_deref(),
            operator_pin: pairing_codes.operator(),
            spectator_pin: pairing_codes.spectator(),
            expires_at_unix,
            kill_session_on_expire: request.kill_session_on_expire,
        });
        Ok(WebShareCreatedResponse {
            share_id,
            scope: summary_scope,
            spectator_url,
            operator_url,
            tunnel_provider,
            tunnel_public_url,
            expires_at_unix,
            operator_pairing_code: pairing_codes.operator().map(str::to_owned),
            spectator_pairing_code: pairing_codes.spectator().map(str::to_owned),
            max_spectators: limits.max_spectators,
            max_operators: limits.max_operators,
            operator: request.operator,
            spectator: request.spectator,
            controls,
            kill_session_on_expire: request.kill_session_on_expire,
            output,
        })
    }

    pub(crate) fn poll_expiry(&self, share_id: &str) -> WebShareExpiryPoll {
        self.inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .poll_expiry(share_id)
    }

    pub(crate) fn expiry_revoke_receiver(
        &self,
        share_id: &str,
    ) -> Option<watch::Receiver<Option<WebShareRevokeReason>>> {
        self.inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .records
            .get(share_id)
            .map(|record| record.revoke_tx.subscribe())
    }

    pub(crate) fn list(&self, _request: ListWebSharesRequest) -> WebShareListResponse {
        let mut inner = self
            .inner
            .lock()
            .expect("web-share registry mutex must not be poisoned");
        inner.prune_expired();
        let shares = inner.summaries();
        WebShareListResponse {
            output: list_output(&shares),
            shares,
        }
    }

    pub(crate) fn stop(&self, request: StopWebShareRequest) -> WebShareStoppedResponse {
        let stopped = self
            .inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .remove(&request.share_id, WebShareRevokeReason::StoppedByOwner);
        if stopped {
            info!(share_id = %request.share_id, reason = "cli_stop", "web_share_stopped");
        }
        WebShareStoppedResponse {
            output: stopped_output(&request.share_id, stopped),
            share_id: request.share_id,
            stopped,
        }
    }

    pub(crate) fn stop_all(&self, _request: StopAllWebSharesRequest) -> WebShareStoppedAllResponse {
        let stopped = self
            .inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .clear(WebShareRevokeReason::StoppedByOwner);
        if stopped > 0 {
            info!(stopped, reason = "cli_stop_all", "web_share_stop_all");
        }
        WebShareStoppedAllResponse {
            output: CommandOutput::from_stdout(format!("stopped {stopped}\n")),
            stopped,
        }
    }

    pub(crate) fn remove_targets_for_sessions(&self, sessions: &[(SessionName, SessionId)]) -> u32 {
        if sessions.is_empty() {
            return 0;
        }
        let removed = self
            .inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .remove_targets_for_sessions(sessions, WebShareRevokeReason::SessionGone);
        if removed > 0 {
            info!(removed, reason = "session_removed", "web_share_pruned");
        }
        removed
    }

    pub(crate) fn remove_targets_for_panes(&self, pane_ids: &[PaneId]) -> u32 {
        if pane_ids.is_empty() {
            return 0;
        }
        let removed = self
            .inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .remove_targets_for_panes(pane_ids, WebShareRevokeReason::PaneGone);
        if removed > 0 {
            info!(removed, reason = "pane_removed", "web_share_pruned");
        }
        removed
    }

    pub(crate) fn rename_session_targets(
        &self,
        old_name: &SessionName,
        new_name: &SessionName,
        session_id: SessionId,
    ) -> u32 {
        let renamed = self
            .inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .rename_session_targets(old_name, new_name, session_id);
        if renamed > 0 {
            info!(renamed, reason = "session_renamed", "web_share_retargeted");
        }
        renamed
    }

    pub(crate) fn lookup(&self, request: LookupWebShareRequest) -> WebShareLookupResponse {
        let mut inner = self
            .inner
            .lock()
            .expect("web-share registry mutex must not be poisoned");
        inner.prune_expired();
        let share = inner.summary(&request.share_id);
        WebShareLookupResponse {
            output: lookup_output(share.as_ref()),
            share,
        }
    }

    pub(crate) fn config(
        &self,
        _request: WebShareConfigRequest,
    ) -> Result<WebShareConfigResponse, RmuxError> {
        self.require_listener_available()?;
        let listener = self.listener();
        Ok(WebShareConfigResponse {
            output: CommandOutput::from_stdout(format!(
                "{}:{} {}\n",
                listener.host, listener.port, listener.frontend_origin
            )),
            listener,
        })
    }

    pub(crate) async fn connect_token_id(
        &self,
        token_id: &str,
        pin: Option<&str>,
    ) -> Result<WebShareAccess, RmuxError> {
        if !valid_token_id_shape(token_id) {
            return Err(RmuxError::Server("invalid web-share token id".to_owned()));
        }
        let lookup = {
            let mut inner = self
                .inner
                .lock()
                .expect("web-share registry mutex must not be poisoned");
            inner.prune_expired();
            inner.capability_by_token_id(token_id)
        };
        let backoff_key = lookup
            .as_ref()
            .map(|capability| {
                format!(
                    "{}:{}",
                    capability.share_id,
                    capability.role.backoff_label()
                )
            })
            .unwrap_or_else(|| UNKNOWN_TOKEN_BACKOFF_KEY.to_owned());
        // Reserve the attempt up front so concurrent guesses are throttled by the
        // in-flight count, not just by past recorded failures. The guard makes
        // this cancellation-safe: if an async timeout or task abort drops the
        // future before an explicit result, the attempt settles as `Other`.
        let (wait, attempt) = match self.backoff.reserve_attempt(&backoff_key) {
            AttemptReservation::Locked => {
                // Failure budget exhausted: fail closed while the temporary
                // lock is active. Return the SAME uniform error as a missing
                // share so no new oracle is exposed, and do not settle (the
                // locked path reserves no attempt).
                info!(share_id = %backoff_key, "web_share_auth_locked");
                return Err(RmuxError::Server(
                    "web-share does not exist or has expired".to_owned(),
                ));
            }
            AttemptReservation::Wait { delay, guard } => (delay, guard),
        };
        if !wait.is_zero() {
            sleep(wait).await;
        }

        let result = {
            let mut inner = self
                .inner
                .lock()
                .expect("web-share registry mutex must not be poisoned");
            inner.prune_expired();
            match inner.capability_by_token_id(token_id) {
                Some(capability) => match inner.records.get(&capability.share_id) {
                    Some(record) => match self.connection_limit.try_acquire() {
                        Some(permit) => record.connect(pin, capability.role, permit),
                        None => Err(RmuxError::Server(
                            "web-share connection limit reached".to_owned(),
                        )),
                    },
                    None => Err(RmuxError::Server(
                        "web-share does not exist or has expired".to_owned(),
                    )),
                },
                None => Err(RmuxError::Server(
                    "web-share does not exist or has expired".to_owned(),
                )),
            }
        };

        match result {
            Ok(access) => {
                attempt.settle(AttemptOutcome::Success);
                info!(share_id = %access.share_id(), role = ?access.connect_role(), "web_share_access_granted");
                Ok(access)
            }
            Err(error) => {
                let outcome = if is_auth_failure_for_backoff(&error) {
                    AttemptOutcome::AuthFailure
                } else {
                    AttemptOutcome::Other
                };
                attempt.settle(outcome);
                if matches!(outcome, AttemptOutcome::AuthFailure) {
                    info!(share_id = %backoff_key, "web_share_auth_backoff");
                }
                Err(error)
            }
        }
    }

    pub(crate) fn pre_auth_token(
        &self,
        token_id: &str,
        origin: &str,
    ) -> Option<(SecretHash, bool)> {
        if !valid_token_id_shape(token_id) {
            return None;
        }
        let mut inner = self
            .inner
            .lock()
            .expect("web-share registry mutex must not be poisoned");
        inner.prune_expired();
        let capability = inner.capability_by_token_id(token_id)?;
        let record = inner.records.get(&capability.share_id)?;
        Some((capability.secret_hash, record.origin_allowed(origin)))
    }

    pub(crate) fn listener(&self) -> WebShareListener {
        self.settings().listener()
    }

    pub(crate) fn listener_available(&self) -> bool {
        self.inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .listener
            .is_available()
    }

    pub(crate) fn settings(&self) -> WebShareSettings {
        self.settings
            .lock()
            .expect("web-share settings mutex must not be poisoned")
            .clone()
    }

    pub(crate) fn update_listener_port(&self, port: u16) {
        self.settings
            .lock()
            .expect("web-share settings mutex must not be poisoned")
            .set_port(port);
    }

    pub(crate) fn validate_create_options(
        &self,
        request: &CreateWebShareRequest,
    ) -> Result<WebShareRoleLimits, RmuxError> {
        self.require_listener_available()?;
        if request.public_base_url.is_some() && request.tunnel_provider.is_some() {
            return Err(RmuxError::Server(
                "web-share --tunnel-url and --tunnel-provider are mutually exclusive".to_owned(),
            ));
        }
        if request.kill_session_on_expire && request.scope.is_pane() {
            return Err(RmuxError::Server(
                "web-share --kill-session-on-expire requires a session target".to_owned(),
            ));
        }
        if request.kill_session_on_expire
            && request.ttl_seconds.is_none()
            && request.expires_at_unix.is_none()
        {
            return Err(RmuxError::Server(
                "web-share --kill-session-on-expire requires --ttl or --expires-at".to_owned(),
            ));
        }
        if !request.operator && !request.spectator {
            return Err(RmuxError::Server(
                "web-share requires at least one access role".to_owned(),
            ));
        }
        if request.max_spectators.is_some() && !request.spectator {
            return Err(RmuxError::Server(
                "web-share --max-spectators cannot be used without a spectator URL".to_owned(),
            ));
        }
        if request.max_spectators == Some(0) {
            return Err(RmuxError::Server(
                "web-share --max-spectators must be at least 1".to_owned(),
            ));
        }
        if request.max_operators.is_some() && !request.operator {
            return Err(RmuxError::Server(
                "web-share --max-operators cannot be used without an operator URL".to_owned(),
            ));
        }
        if request.max_operators == Some(0) {
            return Err(RmuxError::Server(
                "web-share --max-operators must be at least 1".to_owned(),
            ));
        }
        let _ = resolve_expiration(request)?;
        Ok(WebShareRoleLimits {
            max_spectators: request
                .spectator
                .then_some(request.max_spectators.unwrap_or(DEFAULT_MAX_SPECTATORS)),
            max_operators: request
                .operator
                .then_some(request.max_operators.unwrap_or(DEFAULT_MAX_OPERATORS)),
        })
    }

    pub(crate) fn mark_listener_available(&self) {
        self.inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .listener = WebListenerState::Available;
    }

    pub(crate) fn mark_listener_unavailable(&self, reason: impl Into<String>) {
        self.inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .listener = WebListenerState::Unavailable(reason.into());
    }

    fn next_share_id(&self) -> Result<String, RmuxError> {
        for _ in 0..32 {
            let share_id = random_share_id()?;
            if !self
                .inner
                .lock()
                .expect("web-share registry mutex must not be poisoned")
                .records
                .contains_key(&share_id)
            {
                return Ok(share_id);
            }
        }
        let sequence = self.next_id.fetch_add(1, Ordering::Relaxed);
        Err(RmuxError::Server(format!(
            "failed to create unique web-share id after {sequence} attempts"
        )))
    }

    fn endpoint_origin(
        &self,
        settings: &WebShareSettings,
        requested: Option<&str>,
    ) -> Result<String, RmuxError> {
        match requested {
            Some(value) => validate_public_base_url(value),
            None => Ok(settings.local_endpoint_origin()),
        }
    }

    fn frontend(
        &self,
        settings: &WebShareSettings,
        requested: Option<&str>,
    ) -> Result<FrontendUrl, RmuxError> {
        match requested {
            Some(value) => validate_frontend_url(value),
            None => Ok(FrontendUrl {
                origin: settings.frontend_origin.clone(),
                url: settings.frontend_url.clone(),
            }),
        }
    }

    fn require_listener_available(&self) -> Result<(), RmuxError> {
        let inner = self
            .inner
            .lock()
            .expect("web-share registry mutex must not be poisoned");
        match &inner.listener {
            WebListenerState::Available => Ok(()),
            WebListenerState::Unavailable(reason) => Err(RmuxError::Server(format!(
                "web-share listener unavailable: {reason}"
            ))),
        }
    }
}

fn is_auth_failure_for_backoff(error: &RmuxError) -> bool {
    let message = error.to_string();
    message.contains("invalid web-share pairing code")
        || message.contains("does not exist or has expired")
}

fn resolve_expiration(request: &CreateWebShareRequest) -> Result<Option<SystemTime>, RmuxError> {
    if request.ttl_seconds.is_some() && request.expires_at_unix.is_some() {
        return Err(RmuxError::Server(
            "web-share --ttl and --expires-at are mutually exclusive".to_owned(),
        ));
    }
    let now = SystemTime::now();
    let Some(deadline) = (if let Some(expires_at_unix) = request.expires_at_unix {
        Some(
            UNIX_EPOCH
                .checked_add(Duration::from_secs(expires_at_unix))
                .ok_or_else(|| {
                    RmuxError::Server("web-share --expires-at is out of range".to_owned())
                })?,
        )
    } else if let Some(ttl_seconds) = request.ttl_seconds {
        if ttl_seconds == 0 || ttl_seconds > MAX_TTL_SECONDS {
            return Err(RmuxError::Server(
                "web-share TTL must be between 1 second and 7 days".to_owned(),
            ));
        }
        Some(
            now.checked_add(Duration::from_secs(ttl_seconds))
                .ok_or_else(|| RmuxError::Server("web-share TTL is out of range".to_owned()))?,
        )
    } else {
        None
    }) else {
        return Ok(None);
    };
    if deadline <= now {
        return Err(RmuxError::Server(
            "web-share --expires-at must be in the future".to_owned(),
        ));
    }
    if deadline.duration_since(now).unwrap_or(Duration::ZERO) > Duration::from_secs(MAX_TTL_SECONDS)
    {
        return Err(RmuxError::Server(
            "web-share expiration must be within 7 days".to_owned(),
        ));
    }
    Ok(Some(deadline))
}
