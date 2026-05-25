use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use base64::Engine;
use rmux_proto::RmuxError;
use rmux_proto::{
    CommandOutput, CreateWebShareRequest, ListWebSharesRequest, LookupWebShareRequest,
    StopAllWebSharesRequest, StopWebShareRequest, WebShareConfigRequest, WebShareConfigResponse,
    WebShareCreatedResponse, WebShareListResponse, WebShareListener, WebShareLookupResponse,
    WebShareResponse, WebShareScope, WebShareStoppedAllResponse, WebShareStoppedResponse,
    WebShareSummary, WebShareUrlOptions, WebTerminalPalette,
};
use tokio::sync::watch;
use tracing::info;

use super::leases::{ConnectionLease, LeaseBook};
use super::origin::{origin_allowed, validate_frontend_url, validate_public_base_url, FrontendUrl};

const DEFAULT_FRONTEND_ORIGIN: &str = "https://share.rmux.io";
const DEFAULT_FRONTEND_URL: &str = "https://share.rmux.io";
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_MAX_READERS: u16 = 5;
const DEFAULT_PORT: u16 = 9777;
const DEFAULT_TTL_SECONDS: u64 = 60 * 60;
const MAX_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;

#[derive(Debug)]
pub(crate) struct WebShareRegistry {
    inner: Mutex<WebShareState>,
    next_id: AtomicU64,
    settings: WebShareSettings,
}

impl Default for WebShareRegistry {
    fn default() -> Self {
        Self::new(WebShareSettings::default())
    }
}

impl WebShareRegistry {
    pub(crate) fn new(settings: WebShareSettings) -> Self {
        Self {
            inner: Mutex::new(WebShareState::default()),
            next_id: AtomicU64::new(1),
            settings,
        }
    }

    pub(crate) fn handle(
        &self,
        request: rmux_proto::WebShareRequest,
    ) -> Result<WebShareResponse, RmuxError> {
        match request {
            rmux_proto::WebShareRequest::Create(request) => {
                self.create(request).map(WebShareResponse::Created)
            }
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
                Ok(WebShareResponse::Config(self.config(request)))
            }
        }
    }

    pub(crate) fn create(
        &self,
        request: CreateWebShareRequest,
    ) -> Result<WebShareCreatedResponse, RmuxError> {
        if request.controls && !request.writable {
            return Err(RmuxError::Server(
                "web-share controls require --writable".to_owned(),
            ));
        }
        if request.controls && request.scope.is_pane() {
            return Err(RmuxError::Server(
                "web-share controls require a session target".to_owned(),
            ));
        }
        let max_readers = request.max_readers.unwrap_or(DEFAULT_MAX_READERS);
        if max_readers == 0 {
            return Err(RmuxError::Server(
                "web-share requires at least one read slot".to_owned(),
            ));
        }
        let endpoint_origin = self.endpoint_origin(request.public_base_url.as_deref())?;
        let frontend = self.frontend(request.frontend_url.as_deref())?;
        let share_id = self.next_share_id()?;
        let read_token = random_token()?;
        let operator_token = request.writable.then(random_token).transpose()?;
        let pairing_code = request.require_pin.then(random_pairing_code).transpose()?;
        let ttl_seconds = request.ttl_seconds.unwrap_or(DEFAULT_TTL_SECONDS);
        if ttl_seconds == 0 || ttl_seconds > MAX_TTL_SECONDS {
            return Err(RmuxError::Server(
                "web-share TTL must be between 1 second and 7 days".to_owned(),
            ));
        }
        let expires_at = Some(SystemTime::now() + Duration::from_secs(ttl_seconds));
        let lease_book = LeaseBook::new(usize::from(max_readers));
        let (revoke_tx, _) = watch::channel(None);
        let terminal_palette = request.terminal_palette.as_deref().cloned();

        let record = WebShareRecord {
            allow_loopback_development_origins: request.public_base_url.is_none(),
            endpoint_origin,
            expires_at,
            frontend_origin: frontend.origin,
            frontend_url: frontend.url,
            lease_book,
            max_readers,
            operator_token: operator_token.clone(),
            pairing_code: pairing_code.clone(),
            revoke_tx,
            controls: request.controls,
            share_id: share_id.clone(),
            scope: request.scope.clone(),
            terminal_palette,
            url_options: request.url_options,
            read_token: read_token.clone(),
            writable: request.writable,
        };

        let read_url = record.read_url();
        let operator_url = record.operator_url();
        let summary_scope = record.scope.clone();
        let expires_at_unix = expires_at.and_then(system_time_to_unix);
        self.inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .insert(record);
        info!(
            share_id = %share_id,
            scope = %summary_scope,
            writable = request.writable,
            controls = request.controls,
            ttl_seconds,
            max_readers,
            public = request.public_base_url.is_some(),
            pin_required = request.require_pin,
            listener_port = self.settings.port,
            "web_share_created"
        );

        let output = created_output(&read_url, pairing_code.as_deref());
        Ok(WebShareCreatedResponse {
            share_id,
            scope: summary_scope,
            read_url,
            operator_url,
            expires_at_unix,
            pairing_code,
            max_readers,
            writable: request.writable,
            controls: request.controls,
            output,
        })
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

    pub(crate) fn config(&self, _request: WebShareConfigRequest) -> WebShareConfigResponse {
        let listener = self.listener();
        WebShareConfigResponse {
            output: CommandOutput::from_stdout(format!(
                "{}:{} {}\n",
                listener.host, listener.port, listener.frontend_origin
            )),
            listener,
        }
    }

    pub(crate) fn connect(
        &self,
        share_id: &str,
        key: &str,
        pin: Option<&str>,
        role: WebShareConnectRole,
    ) -> Result<WebShareAccess, RmuxError> {
        let mut inner = self
            .inner
            .lock()
            .expect("web-share registry mutex must not be poisoned");
        inner.prune_expired();
        let record = inner.records.get(share_id).ok_or_else(|| {
            RmuxError::Server("web-share does not exist or has expired".to_owned())
        })?;
        let access = record.connect(key, pin, role)?;
        info!(share_id, role = ?role, "web_share_access_granted");
        Ok(access)
    }

    pub(crate) fn listener(&self) -> WebShareListener {
        self.settings.listener()
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

    fn endpoint_origin(&self, requested: Option<&str>) -> Result<String, RmuxError> {
        match requested {
            Some(value) => validate_public_base_url(value),
            None => Ok(self.settings.local_endpoint_origin()),
        }
    }

    fn frontend(&self, requested: Option<&str>) -> Result<FrontendUrl, RmuxError> {
        match requested {
            Some(value) => validate_frontend_url(value),
            None => Ok(FrontendUrl {
                origin: self.settings.frontend_origin.clone(),
                url: self.settings.frontend_url.clone(),
            }),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct WebShareSettings {
    host: String,
    port: u16,
    frontend_origin: String,
    frontend_url: String,
}

impl Default for WebShareSettings {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_owned(),
            port: DEFAULT_PORT,
            frontend_origin: DEFAULT_FRONTEND_ORIGIN.to_owned(),
            frontend_url: DEFAULT_FRONTEND_URL.to_owned(),
        }
    }
}

impl WebShareSettings {
    pub(crate) fn from_options(
        port: u16,
        frontend_origin: Option<String>,
    ) -> Result<Self, RmuxError> {
        let frontend = match frontend_origin {
            Some(value) => validate_frontend_url(&value)?,
            None => FrontendUrl {
                origin: DEFAULT_FRONTEND_ORIGIN.to_owned(),
                url: DEFAULT_FRONTEND_URL.to_owned(),
            },
        };
        Ok(Self {
            host: DEFAULT_HOST.to_owned(),
            port,
            frontend_origin: frontend.origin,
            frontend_url: frontend.url,
        })
    }

    fn listener(&self) -> WebShareListener {
        WebShareListener {
            host: self.host.clone(),
            port: self.port,
            frontend_origin: self.frontend_origin.clone(),
        }
    }

    fn local_endpoint_origin(&self) -> String {
        format!("http://{}:{}", self.host, self.port)
    }
}

#[derive(Debug, Default)]
struct WebShareState {
    records: HashMap<String, WebShareRecord>,
}

impl WebShareState {
    fn insert(&mut self, record: WebShareRecord) {
        self.records.insert(record.share_id.clone(), record);
    }

    fn remove(&mut self, share_id: &str, reason: WebShareRevokeReason) -> bool {
        self.records
            .remove(share_id)
            .map(|record| {
                record.revoke(reason);
                true
            })
            .unwrap_or(false)
    }

    fn clear(&mut self, reason: WebShareRevokeReason) -> u32 {
        let stopped = u32::try_from(self.records.len()).unwrap_or(u32::MAX);
        for (_, record) in self.records.drain() {
            record.revoke(reason);
        }
        self.records.clear();
        stopped
    }

    fn summaries(&self) -> Vec<WebShareSummary> {
        let mut summaries = self
            .records
            .values()
            .map(WebShareRecord::summary)
            .collect::<Vec<_>>();
        summaries.sort_by(|left, right| left.share_id.cmp(&right.share_id));
        summaries
    }

    fn summary(&self, share_id: &str) -> Option<WebShareSummary> {
        self.records.get(share_id).map(WebShareRecord::summary)
    }

    fn prune_expired(&mut self) {
        let now = SystemTime::now();
        let expired = self
            .records
            .iter()
            .filter(|(_, record)| record.expires_at.is_some_and(|expires| expires <= now))
            .map(|(id, _)| id.clone())
            .collect::<Vec<_>>();
        for id in expired {
            if let Some(record) = self.records.remove(&id) {
                record.revoke(WebShareRevokeReason::TtlExpired);
            }
        }
    }
}

#[derive(Debug)]
struct WebShareRecord {
    allow_loopback_development_origins: bool,
    endpoint_origin: String,
    expires_at: Option<SystemTime>,
    frontend_origin: String,
    frontend_url: String,
    lease_book: Arc<LeaseBook>,
    max_readers: u16,
    operator_token: Option<String>,
    pairing_code: Option<String>,
    revoke_tx: watch::Sender<Option<WebShareRevokeReason>>,
    controls: bool,
    share_id: String,
    scope: WebShareScope,
    terminal_palette: Option<WebTerminalPalette>,
    url_options: WebShareUrlOptions,
    read_token: String,
    writable: bool,
}

impl WebShareRecord {
    fn read_url(&self) -> String {
        share_url(self, Some(&self.read_token), "read")
    }

    fn redacted_read_url(&self) -> String {
        share_url(self, None, "read")
    }

    fn operator_url(&self) -> Option<String> {
        self.operator_token
            .as_deref()
            .map(|token| share_url(self, Some(token), "operator"))
    }

    fn summary(&self) -> WebShareSummary {
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

    fn connect(
        &self,
        key: &str,
        pin: Option<&str>,
        role: WebShareConnectRole,
    ) -> Result<WebShareAccess, RmuxError> {
        match role {
            WebShareConnectRole::Read => {
                if !secret_eq(key, &self.read_token) {
                    return Err(RmuxError::Server("invalid web-share key".to_owned()));
                }
                self.check_pairing_code(pin)?;
                let lease = self
                    .lease_book
                    .try_read()
                    .map(ConnectionLease::Read)
                    .ok_or_else(|| RmuxError::Server("web-share read limit reached".to_owned()))?;
                Ok(self.access(lease, WebShareRole::Read))
            }
            WebShareConnectRole::Operator => {
                let Some(operator_key) = self.operator_token.as_deref() else {
                    return Err(RmuxError::Server(
                        "web-share is not writable for operator role".to_owned(),
                    ));
                };
                if !secret_eq(key, operator_key) {
                    return Err(RmuxError::Server("invalid web-share key".to_owned()));
                }
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
            _lease: Some(lease),
            role,
            revoke_rx: self.revoke_tx.subscribe(),
            scope: self.scope.clone(),
            controls: self.controls,
            terminal_palette: self.terminal_palette.clone(),
        }
    }

    fn revoke(self, reason: WebShareRevokeReason) {
        let _ = self.revoke_tx.send(Some(reason));
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
    _lease: Option<ConnectionLease>,
    revoke_rx: watch::Receiver<Option<WebShareRevokeReason>>,
    role: WebShareRole,
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

    pub(crate) fn controls(&self) -> bool {
        self.controls && self.is_operator()
    }

    pub(crate) fn expires_at(&self) -> Option<SystemTime> {
        self.expires_at
    }

    pub(crate) fn release_operator(&mut self) -> bool {
        let Some(lease) = self._lease.take() else {
            return false;
        };
        match lease.release_operator() {
            Ok(read) => {
                self._lease = Some(read);
                self.role = WebShareRole::Read;
                true
            }
            Err(read) => {
                self._lease = Some(read);
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

fn created_output(read_url: &str, pairing_code: Option<&str>) -> CommandOutput {
    let mut output = String::new();
    output.push_str("read ");
    output.push_str(read_url);
    output.push('\n');
    if let Some(pairing_code) = pairing_code {
        output.push_str("pin ");
        output.push_str(pairing_code);
        output.push('\n');
    }
    CommandOutput::from_stdout(output)
}

fn list_output(shares: &[WebShareSummary]) -> CommandOutput {
    let mut output = String::new();
    for share in shares {
        output.push_str(&share.share_id);
        output.push(' ');
        output.push_str(&share.scope.to_string());
        output.push(' ');
        output.push_str(share.read_url.as_deref().unwrap_or("-"));
        output.push('\n');
    }
    CommandOutput::from_stdout(output)
}

fn lookup_output(share: Option<&WebShareSummary>) -> CommandOutput {
    match share {
        Some(share) => list_output(std::slice::from_ref(share)),
        None => CommandOutput::from_stdout(Vec::new()),
    }
}

fn stopped_output(share_id: &str, stopped: bool) -> CommandOutput {
    let status = if stopped { "stopped" } else { "missing" };
    CommandOutput::from_stdout(format!("{status} {share_id}\n"))
}

fn share_url(record: &WebShareRecord, token: Option<&str>, role: &str) -> String {
    let endpoint = websocket_endpoint(&record.endpoint_origin);
    let key = token.unwrap_or("[REDACTED]");
    debug_assert!(
        record.frontend_url.starts_with(&record.frontend_origin),
        "frontend URL must belong to its expected origin"
    );
    let mut url = format!(
        "{}/#endpoint={endpoint}&id={}&key={key}",
        record.frontend_url, record.share_id
    );
    if role != "read" {
        url.push_str("&role=");
        url.push_str(role);
    }
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

fn websocket_endpoint(base_url: &str) -> String {
    let (scheme, authority) = base_url
        .split_once("://")
        .expect("validated web-share base URL must include scheme");
    let ws_scheme = if scheme == "https" { "wss" } else { "ws" };
    format!("{ws_scheme}://{authority}/share")
}

fn random_share_id() -> Result<String, RmuxError> {
    const ALPHABET: &[u8; 32] = b"abcdefghijklmnopqrstuvwxyz234567";
    let mut bytes = [0u8; 5];
    getrandom::fill(&mut bytes).map_err(random_error)?;
    let value = u64::from_be_bytes([0, 0, 0, bytes[0], bytes[1], bytes[2], bytes[3], bytes[4]]);
    let mut out = String::with_capacity(8);
    for shift in (0..40).step_by(5).rev() {
        let index = ((value >> shift) & 0x1f) as usize;
        out.push(ALPHABET[index] as char);
    }
    Ok(out)
}

fn random_pairing_code() -> Result<String, RmuxError> {
    loop {
        let mut bytes = [0u8; 3];
        getrandom::fill(&mut bytes).map_err(random_error)?;
        let value = (u32::from(bytes[0]) << 16) | (u32::from(bytes[1]) << 8) | u32::from(bytes[2]);
        if value < 16_000_000 {
            return Ok(format!("{:06}", value % 1_000_000));
        }
    }
}

fn random_token() -> Result<String, RmuxError> {
    let mut bytes = [0u8; 32];
    getrandom::fill(&mut bytes).map_err(random_error)?;
    Ok(base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(bytes))
}

fn random_error(error: getrandom::Error) -> RmuxError {
    RmuxError::Server(format!("failed to create web-share secret: {error}"))
}

fn secret_eq(left: &str, right: &str) -> bool {
    let left = left.as_bytes();
    let right = right.as_bytes();
    let max = left.len().max(right.len());
    let mut diff = left.len() ^ right.len();
    for index in 0..max {
        let a = left.get(index).copied().unwrap_or(0);
        let b = right.get(index).copied().unwrap_or(0);
        diff |= usize::from(a ^ b);
    }
    diff == 0
}

fn system_time_to_unix(value: SystemTime) -> Option<u64> {
    value
        .duration_since(UNIX_EPOCH)
        .ok()
        .map(|duration| duration.as_secs())
}
