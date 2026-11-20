//! Web-share creation: option validation, expiration, identifiers and the
//! record inserted for a new share.

use std::sync::atomic::Ordering;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rmux_proto::{CreateWebShareRequest, RmuxError, WebShareCreatedResponse};
use tokio::sync::watch;
use tracing::info;

use super::super::leases::LeaseBook;
use super::super::origin::{validate_frontend_url, validate_public_base_url, FrontendUrl};
use super::super::pairing::WebSharePairingCodes;
use super::super::record::{system_time_to_unix, WebShareRecord, WebShareTarget};
use super::super::secrets::{derive_spectator_token, random_share_id, random_token, SecretHash};
use super::super::settings::WebShareSettings;
use super::output::{created_output, CreatedOutput};
use super::{ResolvedCreateWebShareRequest, WebShareRegistry, WebShareRoleLimits};

const MAX_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;
const DEFAULT_MAX_OPERATORS: u16 = 1;
const DEFAULT_MAX_SPECTATORS: u16 = 12;

impl WebShareRegistry {
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
