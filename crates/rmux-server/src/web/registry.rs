use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;
use std::time::{Duration, SystemTime};

use rmux_proto::RmuxError;
use rmux_proto::{
    CommandOutput, CreateWebShareRequest, ListWebSharesRequest, LookupWebShareRequest,
    StopAllWebSharesRequest, StopWebShareRequest, WebShareConfigRequest, WebShareConfigResponse,
    WebShareCreatedResponse, WebShareListResponse, WebShareListener, WebShareLookupResponse,
    WebShareResponse, WebShareStoppedAllResponse, WebShareStoppedResponse, WebShareSummary,
};
use tokio::sync::watch;
use tokio::time::sleep;
use tracing::info;

use super::backoff::AuthBackoff;
use super::leases::LeaseBook;
use super::origin::{validate_frontend_url, validate_public_base_url, FrontendUrl};
use super::record::{
    system_time_to_unix, WebShareAccess, WebShareConnectRole, WebShareRecord, WebShareRevokeReason,
};
use super::secrets::{
    random_pairing_code, random_share_id, random_token, valid_token_shape, SecretHash,
};
use super::settings::WebShareSettings;

const DEFAULT_MAX_READERS: u16 = 5;
const DEFAULT_TTL_SECONDS: u64 = 60 * 60;
const MAX_TTL_SECONDS: u64 = 7 * 24 * 60 * 60;

#[derive(Debug)]
pub(crate) struct WebShareRegistry {
    backoff: AuthBackoff,
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
            backoff: AuthBackoff::new(),
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
                self.config(request).map(WebShareResponse::Config)
            }
        }
    }

    pub(crate) fn create(
        &self,
        request: CreateWebShareRequest,
    ) -> Result<WebShareCreatedResponse, RmuxError> {
        self.require_listener_available()?;
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
        let read_token_hash = SecretHash::from_secret(&read_token);
        let operator_token_hash = operator_token.as_deref().map(SecretHash::from_secret);
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
            operator_token_hash,
            pairing_code: pairing_code.clone(),
            revoke_tx,
            controls: request.controls,
            share_id: share_id.clone(),
            scope: request.scope.clone(),
            terminal_palette,
            url_options: request.url_options,
            read_token_hash,
            writable: request.writable,
        };

        let read_url = record.read_url(&read_token);
        let operator_url = record.operator_url(operator_token.as_deref());
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

    pub(crate) async fn connect(
        &self,
        token: &str,
        pin: Option<&str>,
    ) -> Result<WebShareAccess, RmuxError> {
        if !valid_token_shape(token) {
            return Err(RmuxError::Server("invalid web-share token".to_owned()));
        }
        let token_hash = SecretHash::from_secret(token);
        let lookup = {
            let mut inner = self
                .inner
                .lock()
                .expect("web-share registry mutex must not be poisoned");
            inner.prune_expired();
            inner.capability(&token_hash)
        };
        let backoff_key = lookup
            .as_ref()
            .map(|capability| capability.share_id.clone())
            .unwrap_or_else(|| token_hash.backoff_key());
        let delay = self.backoff.delay_before_next_attempt(&backoff_key);
        if !delay.is_zero() {
            sleep(delay).await;
        }

        let result = {
            let mut inner = self
                .inner
                .lock()
                .expect("web-share registry mutex must not be poisoned");
            inner.prune_expired();
            match inner.capability(&token_hash) {
                Some(capability) => match inner.records.get(&capability.share_id) {
                    Some(record) => record.connect(pin, capability.role),
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
                self.backoff.record_success(&backoff_key);
                info!(share_id = %access.share_id(), role = ?access.connect_role(), "web_share_access_granted");
                Ok(access)
            }
            Err(error) => {
                if is_auth_failure_for_backoff(&error) {
                    let failure = self.backoff.record_failure(&backoff_key);
                    info!(
                        share_id = %backoff_key,
                        fails = failure.fails,
                        next_delay_ms = failure.next_delay.as_millis(),
                        "web_share_auth_backoff"
                    );
                }
                Err(error)
            }
        }
    }

    pub(crate) fn known_token_origin_allowed(&self, token: &str, origin: &str) -> Option<bool> {
        if !valid_token_shape(token) {
            return None;
        }
        let token_hash = SecretHash::from_secret(token);
        let mut inner = self
            .inner
            .lock()
            .expect("web-share registry mutex must not be poisoned");
        inner.prune_expired();
        let capability = inner.capability(&token_hash)?;
        inner
            .records
            .get(&capability.share_id)
            .map(|record| record.origin_allowed(origin))
    }

    pub(crate) fn listener(&self) -> WebShareListener {
        self.settings.listener()
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
    message.contains("invalid web-share key")
        || message.contains("invalid web-share pairing code")
        || message.contains("does not exist or has expired")
}

#[derive(Debug)]
struct WebShareState {
    records: HashMap<String, WebShareRecord>,
    tokens: HashMap<SecretHash, WebCapability>,
    listener: WebListenerState,
}

impl Default for WebShareState {
    fn default() -> Self {
        Self {
            records: HashMap::new(),
            tokens: HashMap::new(),
            listener: WebListenerState::Available,
        }
    }
}

#[derive(Debug)]
enum WebListenerState {
    Available,
    Unavailable(String),
}

#[derive(Debug, Clone)]
struct WebCapability {
    share_id: String,
    role: WebShareConnectRole,
}

impl WebShareState {
    fn insert(&mut self, record: WebShareRecord) {
        self.tokens.insert(
            record.read_token_hash,
            WebCapability {
                share_id: record.share_id.clone(),
                role: WebShareConnectRole::Read,
            },
        );
        if let Some(hash) = record.operator_token_hash {
            self.tokens.insert(
                hash,
                WebCapability {
                    share_id: record.share_id.clone(),
                    role: WebShareConnectRole::Operator,
                },
            );
        }
        self.records.insert(record.share_id.clone(), record);
    }

    fn remove(&mut self, share_id: &str, reason: WebShareRevokeReason) -> bool {
        self.records
            .remove(share_id)
            .map(|record| {
                self.remove_tokens(&record);
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
        self.tokens.clear();
        self.records.clear();
        stopped
    }

    fn capability(&self, token_hash: &SecretHash) -> Option<WebCapability> {
        self.tokens.get(token_hash).cloned()
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
                self.remove_tokens(&record);
                record.revoke(WebShareRevokeReason::TtlExpired);
            }
        }
    }

    fn remove_tokens(&mut self, record: &WebShareRecord) {
        self.tokens.remove(&record.read_token_hash);
        if let Some(hash) = record.operator_token_hash {
            self.tokens.remove(&hash);
        }
    }
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
