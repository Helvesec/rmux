use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rmux_proto::{
    CommandOutput, CreateWebShareRequest, ListWebSharesRequest, LookupWebShareRequest,
    StopAllWebSharesRequest, StopWebShareRequest, WebShareConfigRequest, WebShareConfigResponse,
    WebShareCreatedResponse, WebShareListResponse, WebShareListener, WebShareLookupResponse,
    WebShareResponse, WebShareStoppedAllResponse, WebShareStoppedResponse, WebShareSummary,
};
use rmux_proto::{PaneTargetRef, RmuxError};

use super::leases::{ConnectionLease, LeaseBook};
use super::origin::validate_public_base_url;

const DEFAULT_FRONTEND_ORIGIN: &str = "http://127.0.0.1:9777";
const DEFAULT_HOST: &str = "127.0.0.1";
const DEFAULT_MAX_VIEWERS: u16 = 5;
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
        let max_viewers = request.max_viewers.unwrap_or(DEFAULT_MAX_VIEWERS);
        if max_viewers == 0 {
            return Err(RmuxError::Server(
                "web-share requires at least one viewer slot".to_owned(),
            ));
        }
        let base_url = self.public_base_url(request.public_base_url.as_deref())?;
        let share_id = self.next_share_id()?;
        let viewer_token = random_token()?;
        let operator_token = request.writable.then(random_token).transpose()?;
        let ttl_seconds = request.ttl_seconds.unwrap_or(DEFAULT_TTL_SECONDS);
        if ttl_seconds == 0 || ttl_seconds > MAX_TTL_SECONDS {
            return Err(RmuxError::Server(
                "web-share TTL must be between 1 second and 7 days".to_owned(),
            ));
        }
        let expires_at = Some(SystemTime::now() + Duration::from_secs(ttl_seconds));
        let lease_book = LeaseBook::new(usize::from(max_viewers));

        let record = WebShareRecord {
            base_url,
            expires_at,
            lease_book,
            max_viewers,
            operator_token: operator_token.clone(),
            share_id: share_id.clone(),
            target: request.target.clone(),
            viewer_token: viewer_token.clone(),
            writable: request.writable,
        };

        let viewer_url = record.viewer_url();
        let operator_url = record.operator_url();
        let summary_target = record.target.clone();
        let expires_at_unix = expires_at.and_then(system_time_to_unix);
        self.inner
            .lock()
            .expect("web-share registry mutex must not be poisoned")
            .insert(record);

        let output = created_output(&viewer_url);
        Ok(WebShareCreatedResponse {
            share_id,
            target: summary_target,
            viewer_url,
            operator_url,
            expires_at_unix,
            max_viewers,
            writable: request.writable,
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
            .remove(&request.share_id);
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
            .clear();
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

    pub(crate) fn connect(&self, share_id: &str, key: &str) -> Result<WebShareAccess, RmuxError> {
        let mut inner = self
            .inner
            .lock()
            .expect("web-share registry mutex must not be poisoned");
        inner.prune_expired();
        let record = inner.records.get(share_id).ok_or_else(|| {
            RmuxError::Server("web-share does not exist or has expired".to_owned())
        })?;
        record.connect(key)
    }

    pub(crate) fn listener(&self) -> WebShareListener {
        self.settings.listener()
    }

    fn next_share_id(&self) -> Result<String, RmuxError> {
        let sequence = self.next_id.fetch_add(1, Ordering::Relaxed);
        Ok(format!("{sequence:x}-{}", random_token()?))
    }

    fn public_base_url(&self, requested: Option<&str>) -> Result<String, RmuxError> {
        match requested {
            Some(value) => validate_public_base_url(value),
            None => Ok(self.settings.frontend_origin.clone()),
        }
    }
}

#[derive(Debug, Clone)]
pub(crate) struct WebShareSettings {
    host: String,
    port: u16,
    frontend_origin: String,
}

impl Default for WebShareSettings {
    fn default() -> Self {
        Self {
            host: DEFAULT_HOST.to_owned(),
            port: DEFAULT_PORT,
            frontend_origin: DEFAULT_FRONTEND_ORIGIN.to_owned(),
        }
    }
}

impl WebShareSettings {
    fn listener(&self) -> WebShareListener {
        WebShareListener {
            host: self.host.clone(),
            port: self.port,
            frontend_origin: self.frontend_origin.clone(),
        }
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

    fn remove(&mut self, share_id: &str) -> bool {
        self.records.remove(share_id).is_some()
    }

    fn clear(&mut self) -> u32 {
        let stopped = u32::try_from(self.records.len()).unwrap_or(u32::MAX);
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
        self.records
            .retain(|_, record| record.expires_at.is_none_or(|expires| expires > now));
    }
}

#[derive(Debug)]
struct WebShareRecord {
    base_url: String,
    expires_at: Option<SystemTime>,
    lease_book: Arc<LeaseBook>,
    max_viewers: u16,
    operator_token: Option<String>,
    share_id: String,
    target: PaneTargetRef,
    viewer_token: String,
    writable: bool,
}

impl WebShareRecord {
    fn viewer_url(&self) -> String {
        share_url(&self.base_url, &self.share_id, Some(&self.viewer_token))
    }

    fn redacted_viewer_url(&self) -> String {
        share_url(&self.base_url, &self.share_id, None)
    }

    fn operator_url(&self) -> Option<String> {
        self.operator_token
            .as_deref()
            .map(|token| share_url(&self.base_url, &self.share_id, Some(token)))
    }

    fn summary(&self) -> WebShareSummary {
        WebShareSummary {
            share_id: self.share_id.clone(),
            target: self.target.clone(),
            viewer_url: Some(self.redacted_viewer_url()),
            writable: self.writable,
            active_viewers: u16::try_from(self.lease_book.viewer_count()).unwrap_or(u16::MAX),
            max_viewers: self.max_viewers,
            operator_connected: self.lease_book.operator_connected(),
            expires_at_unix: self.expires_at.and_then(system_time_to_unix),
        }
    }

    fn connect(&self, key: &str) -> Result<WebShareAccess, RmuxError> {
        if secret_eq(key, &self.viewer_token) {
            let lease = self
                .lease_book
                .try_viewer()
                .map(ConnectionLease::Viewer)
                .ok_or_else(|| RmuxError::Server("web-share viewer limit reached".to_owned()))?;
            return Ok(WebShareAccess {
                expected_origin: self.base_url.clone(),
                _lease: lease,
                role: WebShareRole::Viewer,
                target: self.target.clone(),
            });
        }
        if self
            .operator_token
            .as_deref()
            .is_some_and(|operator_key| secret_eq(key, operator_key))
        {
            let lease = self
                .lease_book
                .try_operator()
                .map(ConnectionLease::Operator)
                .ok_or_else(|| {
                    RmuxError::Server("web-share operator is already connected".to_owned())
                })?;
            return Ok(WebShareAccess {
                expected_origin: self.base_url.clone(),
                _lease: lease,
                role: WebShareRole::Operator,
                target: self.target.clone(),
            });
        }
        Err(RmuxError::Server("invalid web-share key".to_owned()))
    }
}

#[derive(Debug)]
pub(crate) struct WebShareAccess {
    expected_origin: String,
    _lease: ConnectionLease,
    role: WebShareRole,
    target: PaneTargetRef,
}

impl WebShareAccess {
    pub(crate) fn expected_origin(&self) -> &str {
        &self.expected_origin
    }

    pub(crate) fn is_operator(&self) -> bool {
        matches!(self.role, WebShareRole::Operator)
    }

    pub(crate) fn target(&self) -> &PaneTargetRef {
        &self.target
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum WebShareRole {
    Operator,
    Viewer,
}

fn created_output(viewer_url: &str) -> CommandOutput {
    let mut output = String::new();
    output.push_str("viewer ");
    output.push_str(viewer_url);
    output.push('\n');
    CommandOutput::from_stdout(output)
}

fn list_output(shares: &[WebShareSummary]) -> CommandOutput {
    let mut output = String::new();
    for share in shares {
        output.push_str(&share.share_id);
        output.push(' ');
        output.push_str(&share.target.to_string());
        output.push(' ');
        output.push_str(share.viewer_url.as_deref().unwrap_or("-"));
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

fn share_url(base_url: &str, share_id: &str, token: Option<&str>) -> String {
    match token {
        Some(token) => format!("{base_url}/s/{share_id}?key={token}"),
        None => format!("{base_url}/s/{share_id}"),
    }
}

fn random_token() -> Result<String, RmuxError> {
    let mut bytes = [0u8; 16];
    getrandom::fill(&mut bytes)
        .map_err(|error| RmuxError::Server(format!("failed to create web-share token: {error}")))?;
    Ok(hex_encode(&bytes))
}

fn hex_encode(bytes: &[u8]) -> String {
    const TABLE: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(TABLE[(byte >> 4) as usize] as char);
        out.push(TABLE[(byte & 0x0f) as usize] as char);
    }
    out
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

#[cfg(test)]
mod tests {
    use super::*;
    use rmux_proto::{PaneId, SessionName};

    fn target() -> PaneTargetRef {
        PaneTargetRef::by_id(
            SessionName::new("alpha").expect("valid session"),
            PaneId::new(7),
        )
    }

    #[test]
    fn create_returns_secret_urls_but_list_is_redacted() {
        let registry = WebShareRegistry::default();
        let created = registry
            .create(CreateWebShareRequest {
                target: target(),
                public_base_url: Some("https://share.example".to_owned()),
                ttl_seconds: Some(60),
                max_viewers: Some(2),
                writable: true,
            })
            .expect("share creates");

        assert!(created.viewer_url.contains("?key="));
        assert!(created
            .operator_url
            .as_deref()
            .is_some_and(|url| url.contains("?key=")));
        let stdout = String::from_utf8_lossy(created.output.stdout());
        assert!(stdout.contains("viewer "));
        assert!(!stdout.contains("operator "));

        let listed = registry.list(ListWebSharesRequest);
        assert_eq!(listed.shares.len(), 1);
        let redacted = listed.shares[0].viewer_url.as_deref().expect("url");
        assert_eq!(
            redacted,
            format!("https://share.example/s/{}", created.share_id)
        );
    }

    #[test]
    fn public_base_url_rejects_query_and_fragment() {
        assert!(validate_public_base_url("https://x.test?a=1").is_err());
        assert!(validate_public_base_url("https://x.test#frag").is_err());
        assert!(validate_public_base_url("ssh://x.test").is_err());
    }

    #[test]
    fn stop_all_reports_removed_share_count() {
        let registry = WebShareRegistry::default();
        for _ in 0..2 {
            registry
                .create(CreateWebShareRequest {
                    target: target(),
                    public_base_url: None,
                    ttl_seconds: None,
                    max_viewers: None,
                    writable: false,
                })
                .expect("share creates");
        }
        assert_eq!(registry.stop_all(StopAllWebSharesRequest).stopped, 2);
        assert!(registry.list(ListWebSharesRequest).shares.is_empty());
    }

    #[test]
    fn connect_enforces_viewer_cap_and_single_operator() {
        let registry = WebShareRegistry::default();
        let created = registry
            .create(CreateWebShareRequest {
                target: target(),
                public_base_url: None,
                ttl_seconds: None,
                max_viewers: Some(1),
                writable: true,
            })
            .expect("share creates");
        let viewer_key = key_from_url(&created.viewer_url);
        let operator_key = key_from_url(created.operator_url.as_deref().expect("operator url"));

        let viewer = registry
            .connect(&created.share_id, &viewer_key)
            .expect("viewer connects");
        assert!(!viewer.is_operator());
        assert!(registry.connect(&created.share_id, &viewer_key).is_err());

        let operator = registry
            .connect(&created.share_id, &operator_key)
            .expect("operator connects");
        assert!(operator.is_operator());
        assert!(registry.connect(&created.share_id, &operator_key).is_err());

        drop(viewer);
        assert!(registry.connect(&created.share_id, &viewer_key).is_ok());
    }

    fn key_from_url(url: &str) -> String {
        url.split_once("?key=")
            .map(|(_, key)| key.to_owned())
            .expect("key query")
    }
}
