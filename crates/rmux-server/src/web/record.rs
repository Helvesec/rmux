use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rmux_proto::{
    PaneTargetRef, RmuxError, SessionId, SessionName, WebShareScope, WebShareSummary,
    WebShareUrlOptions, WebTerminalPalette,
};
use serde::Serialize;
use tokio::sync::watch;

use super::connection_limit::ConnectionPermit;
use super::leases::{LeaseBook, OperatorLease, SpectatorLease};
use super::origin::origin_allowed;
use super::pairing::WebSharePairingCodes;
use super::secrets::SecretHash;
use super::tunnel::TunnelHandle;

const DEFAULT_LOCAL_WEBSOCKET_ENDPOINT: &str = "ws://127.0.0.1:9777/share";
pub(crate) const OPERATOR_LIMIT_ERROR: &str = "web-share operator limit reached";
pub(crate) const SPECTATOR_LIMIT_ERROR: &str = "web-share spectator limit reached";

#[derive(Debug)]
pub(super) struct WebShareRecord {
    pub(super) allow_loopback_development_origins: bool,
    pub(super) endpoint_origin: String,
    pub(super) expires_at: Option<SystemTime>,
    pub(super) frontend_origin: String,
    pub(super) frontend_url: String,
    pub(super) kill_session_on_expire: bool,
    pub(super) lease_book: Arc<LeaseBook>,
    pub(super) max_operators: Option<u16>,
    pub(super) max_spectators: Option<u16>,
    pub(super) operator_token_hash: Option<SecretHash>,
    pub(super) pairing_codes: WebSharePairingCodes,
    pub(super) revoke_tx: watch::Sender<Option<WebShareRevokeReason>>,
    pub(super) controls: bool,
    pub(super) share_id: String,
    pub(super) target: WebShareTarget,
    pub(super) terminal_palette: Option<WebTerminalPalette>,
    pub(super) url_options: WebShareUrlOptions,
    pub(super) spectator_token_hash: Option<SecretHash>,
    pub(super) _tunnel: Option<TunnelHandle>,
    pub(super) operator: bool,
    pub(super) spectator: bool,
}

impl WebShareRecord {
    pub(super) fn spectator_url(&self, token: Option<&str>) -> Option<String> {
        self.spectator_token_hash
            .is_some()
            .then(|| share_url(self, token))
    }

    pub(super) fn redacted_spectator_url(&self) -> Option<String> {
        self.spectator_url(None)
    }

    pub(super) fn operator_url(&self, token: Option<&str>) -> Option<String> {
        self.operator_token_hash
            .is_some()
            .then(|| share_url(self, token))
    }

    pub(super) fn summary(&self) -> WebShareSummary {
        WebShareSummary {
            share_id: self.share_id.clone(),
            scope: self.target.scope(),
            spectator_url: self.redacted_spectator_url(),
            operator: self.operator,
            spectator: self.spectator,
            controls: self.controls,
            active_spectators: u16::try_from(self.lease_book.spectator_count()).unwrap_or(u16::MAX),
            active_operators: u16::try_from(self.lease_book.operator_count()).unwrap_or(u16::MAX),
            max_spectators: self.max_spectators,
            max_operators: self.max_operators,
            expires_at_unix: self.expires_at.and_then(system_time_to_unix),
            kill_session_on_expire: self.kill_session_on_expire,
        }
    }

    pub(super) fn origin_allowed(&self, received: &str) -> bool {
        origin_allowed(
            received,
            &self.frontend_origin,
            self.allow_loopback_development_origins,
        )
    }

    pub(super) fn connect(
        &self,
        pin: Option<&str>,
        role: WebShareConnectRole,
        connection_permit: ConnectionPermit,
    ) -> Result<WebShareAccess, RmuxError> {
        match role {
            WebShareConnectRole::Spectator => {
                if self.spectator_token_hash.is_none() {
                    return Err(RmuxError::Server(
                        "web-share has no spectator access".to_owned(),
                    ));
                }
                self.pairing_codes.check(pin, role)?;
                let lease = self
                    .lease_book
                    .try_spectator()
                    .ok_or_else(|| RmuxError::Server(SPECTATOR_LIMIT_ERROR.to_owned()))?;
                Ok(self.access(
                    Some(lease),
                    None,
                    connection_permit,
                    WebShareRole::Spectator,
                ))
            }
            WebShareConnectRole::Operator => {
                if self.operator_token_hash.is_none() {
                    return Err(RmuxError::Server(
                        "web-share has no operator access".to_owned(),
                    ));
                };
                self.pairing_codes.check(pin, role)?;
                let lease = self
                    .lease_book
                    .try_operator()
                    .ok_or_else(|| RmuxError::Server(OPERATOR_LIMIT_ERROR.to_owned()))?;
                Ok(self.access(None, Some(lease), connection_permit, WebShareRole::Operator))
            }
        }
    }

    pub(super) fn revoke(self, reason: WebShareRevokeReason) {
        let _ = self.revoke_tx.send(Some(reason));
    }

    fn access(
        &self,
        spectator_lease: Option<SpectatorLease>,
        operator_lease: Option<OperatorLease>,
        connection_permit: ConnectionPermit,
        role: WebShareRole,
    ) -> WebShareAccess {
        WebShareAccess {
            allow_loopback_development_origins: self.allow_loopback_development_origins,
            expected_origin: self.frontend_origin.clone(),
            expires_at: self.expires_at,
            _connection_permit: connection_permit,
            _spectator_lease: spectator_lease,
            _operator_lease: operator_lease,
            lease_book: Arc::clone(&self.lease_book),
            max_operators: self.max_operators,
            max_spectators: self.max_spectators,
            operator: self.operator,
            spectator: self.spectator,
            spectator_pairing_code: self.pairing_codes.spectator().map(str::to_owned),
            role,
            share_id: self.share_id.clone(),
            revoke_rx: self.revoke_tx.subscribe(),
            target: self.target.clone(),
            controls: self.controls,
            terminal_palette: self.terminal_palette.clone(),
            show_viewers: self.url_options.show_viewers,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WebShareTarget {
    Pane(PaneTargetRef),
    Session(WebSessionTarget),
}

impl WebShareTarget {
    pub(crate) fn pane(target: PaneTargetRef) -> Self {
        Self::Pane(target)
    }

    pub(crate) fn session(name: SessionName, id: SessionId) -> Self {
        Self::Session(WebSessionTarget::new(name, id))
    }

    pub(crate) fn scope(&self) -> WebShareScope {
        match self {
            Self::Pane(target) => WebShareScope::Pane(target.clone()),
            Self::Session(target) => WebShareScope::Session(target.name.clone()),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct WebSessionTarget {
    name: SessionName,
    id: SessionId,
}

impl WebSessionTarget {
    pub(crate) fn new(name: SessionName, id: SessionId) -> Self {
        Self { name, id }
    }

    pub(crate) fn name(&self) -> &SessionName {
        &self.name
    }

    pub(crate) const fn id(&self) -> SessionId {
        self.id
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebShareConnectRole {
    Operator,
    Spectator,
}

impl WebShareConnectRole {
    pub(super) const fn backoff_label(self) -> &'static str {
        match self {
            Self::Operator => "operator",
            Self::Spectator => "spectator",
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebShareRevokeReason {
    PaneGone,
    SessionGone,
    StoppedByOwner,
    TtlExpired,
}

impl WebShareRevokeReason {
    pub(crate) const fn as_str(self) -> &'static str {
        match self {
            Self::PaneGone => "pane_gone",
            Self::SessionGone => "session_gone",
            Self::StoppedByOwner => "stopped_by_owner",
            Self::TtlExpired => "ttl_expired",
        }
    }
}

#[derive(Debug)]
pub(crate) struct WebShareAccess {
    allow_loopback_development_origins: bool,
    expected_origin: String,
    expires_at: Option<SystemTime>,
    _connection_permit: ConnectionPermit,
    _spectator_lease: Option<SpectatorLease>,
    _operator_lease: Option<OperatorLease>,
    lease_book: Arc<LeaseBook>,
    max_operators: Option<u16>,
    max_spectators: Option<u16>,
    operator: bool,
    spectator: bool,
    spectator_pairing_code: Option<String>,
    revoke_rx: watch::Receiver<Option<WebShareRevokeReason>>,
    role: WebShareRole,
    share_id: String,
    target: WebShareTarget,
    controls: bool,
    terminal_palette: Option<WebTerminalPalette>,
    show_viewers: bool,
}

impl WebShareAccess {
    pub(crate) fn origin_allowed(&self, received: &str) -> bool {
        origin_allowed(
            received,
            &self.expected_origin,
            self.allow_loopback_development_origins,
        )
    }

    pub(crate) fn is_operator(&self) -> bool {
        matches!(self.role, WebShareRole::Operator)
    }

    pub(crate) const fn has_operator_access(&self) -> bool {
        self.operator
    }

    pub(crate) const fn has_spectator_access(&self) -> bool {
        self.spectator
    }

    pub(crate) fn operator_visible_spectator_pairing_code(&self) -> Option<&str> {
        self.is_operator()
            .then_some(self.spectator_pairing_code.as_deref())
            .flatten()
    }

    pub(crate) fn connect_role(&self) -> WebShareConnectRole {
        match self.role {
            WebShareRole::Operator => WebShareConnectRole::Operator,
            WebShareRole::Spectator => WebShareConnectRole::Spectator,
        }
    }

    pub(crate) fn controls(&self) -> bool {
        self.controls && self.is_operator()
    }

    pub(crate) fn is_resize_authority(&self) -> bool {
        self._operator_lease
            .as_ref()
            .is_some_and(OperatorLease::is_resize_authority)
    }

    pub(crate) fn share_id(&self) -> &str {
        &self.share_id
    }

    pub(crate) fn expires_at(&self) -> Option<SystemTime> {
        self.expires_at
    }

    pub(crate) fn connection_counts(&self) -> WebShareConnectionCounts {
        WebShareConnectionCounts::new(
            u16::try_from(self.lease_book.spectator_count()).unwrap_or(u16::MAX),
            self.max_spectators,
            u16::try_from(self.lease_book.operator_count()).unwrap_or(u16::MAX),
            self.max_operators,
        )
    }

    pub(crate) fn target(&self) -> &WebShareTarget {
        &self.target
    }

    pub(crate) fn terminal_palette(&self) -> Option<&WebTerminalPalette> {
        self.terminal_palette.as_ref()
    }

    pub(crate) const fn show_viewers(&self) -> bool {
        self.show_viewers
    }

    pub(crate) fn revoke_receiver(&self) -> watch::Receiver<Option<WebShareRevokeReason>> {
        self.revoke_rx.clone()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
pub(crate) struct WebShareConnectionCounts {
    pub(crate) spectators_active: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) spectators_max: Option<u16>,
    pub(crate) operators_active: u16,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub(crate) operators_max: Option<u16>,
    pub(crate) viewers_connected: u16,
}

impl WebShareConnectionCounts {
    pub(crate) fn new(
        spectators_active: u16,
        spectators_max: Option<u16>,
        operators_active: u16,
        operators_max: Option<u16>,
    ) -> Self {
        Self {
            spectators_active,
            spectators_max,
            operators_active,
            operators_max,
            viewers_connected: spectators_active.saturating_add(operators_active),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebShareRole {
    Operator,
    Spectator,
}

pub(super) fn websocket_endpoint(base_url: &str) -> String {
    let (scheme, authority) = base_url
        .split_once("://")
        .expect("validated web-share base URL must include scheme");
    let ws_scheme = if scheme.eq_ignore_ascii_case("https") {
        "wss"
    } else {
        "ws"
    };
    format!("{ws_scheme}://{authority}/share")
}

pub(super) fn system_time_to_unix(value: SystemTime) -> Option<u64> {
    value
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}

fn share_url(record: &WebShareRecord, token: Option<&str>) -> String {
    let endpoint = websocket_endpoint(&record.endpoint_origin);
    let token = token.unwrap_or("[REDACTED]");
    debug_assert!(
        record.frontend_url.starts_with(&record.frontend_origin),
        "frontend URL must belong to its expected origin"
    );
    let mut params = Vec::with_capacity(7);
    if endpoint != DEFAULT_LOCAL_WEBSOCKET_ENDPOINT {
        params.push(format!("e={endpoint}"));
    }
    params.push(format!("t={token}"));
    if record.url_options.no_navbar {
        params.push("navbar=off".to_owned());
    }
    if record.url_options.no_disclaimer {
        params.push("disclaimer=off".to_owned());
    }
    if let Some(theme) = record.url_options.terminal_theme {
        params.push(format!("theme={}", theme.as_url_value()));
    }
    format!("{}/#{}", record.frontend_url, params.join("&"))
}
