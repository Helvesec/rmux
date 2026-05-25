use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rmux_proto::{
    RmuxError, WebShareScope, WebShareSummary, WebShareUrlOptions, WebTerminalPalette,
};
use tokio::sync::watch;

use super::leases::{ConnectionLease, LeaseBook};
use super::origin::origin_allowed;
use super::secrets::{secret_eq, SecretHash};

#[derive(Debug)]
pub(super) struct WebShareRecord {
    pub(super) allow_loopback_development_origins: bool,
    pub(super) endpoint_origin: String,
    pub(super) expires_at: Option<SystemTime>,
    pub(super) frontend_origin: String,
    pub(super) frontend_url: String,
    pub(super) lease_book: Arc<LeaseBook>,
    pub(super) max_readers: u16,
    pub(super) operator_token_hash: Option<SecretHash>,
    pub(super) pairing_code: Option<String>,
    pub(super) revoke_tx: watch::Sender<Option<WebShareRevokeReason>>,
    pub(super) controls: bool,
    pub(super) share_id: String,
    pub(super) scope: WebShareScope,
    pub(super) terminal_palette: Option<WebTerminalPalette>,
    pub(super) url_options: WebShareUrlOptions,
    pub(super) read_token_hash: SecretHash,
    pub(super) writable: bool,
}

impl WebShareRecord {
    pub(super) fn read_url(&self, token: &str) -> String {
        share_url(self, Some(token))
    }

    pub(super) fn redacted_read_url(&self) -> String {
        share_url(self, None)
    }

    pub(super) fn operator_url(&self, token: Option<&str>) -> Option<String> {
        self.operator_token_hash
            .is_some()
            .then(|| share_url(self, token))
    }

    pub(super) fn summary(&self) -> WebShareSummary {
        WebShareSummary {
            share_id: self.share_id.clone(),
            scope: self.scope.clone(),
            read_url: Some(self.redacted_read_url()),
            writable: self.writable,
            controls: self.controls,
            active_readers: u16::try_from(self.lease_book.reader_count()).unwrap_or(u16::MAX),
            max_readers: self.max_readers,
            operator_connected: self.lease_book.operator_connected(),
            expires_at_unix: self.expires_at.and_then(system_time_to_unix),
        }
    }

    pub(super) fn connect(
        &self,
        pin: Option<&str>,
        role: WebShareConnectRole,
    ) -> Result<WebShareAccess, RmuxError> {
        match role {
            WebShareConnectRole::Read => {
                self.check_pairing_code(pin)?;
                let lease = self
                    .lease_book
                    .try_read()
                    .map(ConnectionLease::Read)
                    .ok_or_else(|| RmuxError::Server("web-share read limit reached".to_owned()))?;
                Ok(self.access(lease, WebShareRole::Read))
            }
            WebShareConnectRole::Operator => {
                if self.operator_token_hash.is_none() {
                    return Err(RmuxError::Server(
                        "web-share is not writable for operator role".to_owned(),
                    ));
                };
                self.check_pairing_code(pin)?;
                let lease = self
                    .lease_book
                    .try_operator()
                    .map(ConnectionLease::Operator)
                    .ok_or_else(|| {
                        RmuxError::Server("web-share operator is already connected".to_owned())
                    })?;
                Ok(self.access(lease, WebShareRole::Operator))
            }
        }
    }

    pub(super) fn revoke(self, reason: WebShareRevokeReason) {
        let _ = self.revoke_tx.send(Some(reason));
    }

    fn check_pairing_code(&self, pin: Option<&str>) -> Result<(), RmuxError> {
        let Some(expected) = self.pairing_code.as_deref() else {
            return Ok(());
        };
        if pin.is_some_and(|provided| secret_eq(provided, expected)) {
            return Ok(());
        }
        Err(RmuxError::Server(
            "invalid web-share pairing code".to_owned(),
        ))
    }

    fn access(&self, lease: ConnectionLease, role: WebShareRole) -> WebShareAccess {
        WebShareAccess {
            allow_loopback_development_origins: self.allow_loopback_development_origins,
            expected_origin: self.frontend_origin.clone(),
            expires_at: self.expires_at,
            lease: Some(lease),
            role,
            share_id: self.share_id.clone(),
            revoke_rx: self.revoke_tx.subscribe(),
            scope: self.scope.clone(),
            controls: self.controls,
            terminal_palette: self.terminal_palette.clone(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum WebShareConnectRole {
    Operator,
    Read,
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
    lease: Option<ConnectionLease>,
    revoke_rx: watch::Receiver<Option<WebShareRevokeReason>>,
    role: WebShareRole,
    share_id: String,
    scope: WebShareScope,
    controls: bool,
    terminal_palette: Option<WebTerminalPalette>,
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

    pub(crate) fn connect_role(&self) -> WebShareConnectRole {
        match self.role {
            WebShareRole::Operator => WebShareConnectRole::Operator,
            WebShareRole::Read => WebShareConnectRole::Read,
        }
    }

    pub(crate) fn controls(&self) -> bool {
        self.controls && self.is_operator()
    }

    pub(crate) fn share_id(&self) -> &str {
        &self.share_id
    }

    pub(crate) fn expires_at(&self) -> Option<SystemTime> {
        self.expires_at
    }

    pub(crate) fn release_operator(&mut self) -> bool {
        let Some(lease) = self.lease.take() else {
            return false;
        };
        match lease.release_operator() {
            Ok(read) => {
                self.lease = Some(read);
                self.role = WebShareRole::Read;
                true
            }
            Err(read) => {
                self.lease = Some(read);
                false
            }
        }
    }

    pub(crate) fn scope(&self) -> &WebShareScope {
        &self.scope
    }

    pub(crate) fn terminal_palette(&self) -> Option<&WebTerminalPalette> {
        self.terminal_palette.as_ref()
    }

    pub(crate) fn revoke_receiver(&self) -> watch::Receiver<Option<WebShareRevokeReason>> {
        self.revoke_rx.clone()
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebShareRole {
    Operator,
    Read,
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
    let mut url = if record.allow_loopback_development_origins {
        format!("{}/#t={token}", record.frontend_url)
    } else {
        format!("{}/#e={endpoint}&t={token}", record.frontend_url)
    };
    if record.pairing_code.is_some() {
        url.push_str("&pin=required");
    }
    if record.url_options.no_navbar {
        url.push_str("&navbar=off");
    }
    if record.url_options.no_disclaimer {
        url.push_str("&disclaimer=off");
    }
    if let Some(theme) = record.url_options.terminal_theme {
        url.push_str("&theme=");
        url.push_str(theme.as_url_value());
    }
    url
}
