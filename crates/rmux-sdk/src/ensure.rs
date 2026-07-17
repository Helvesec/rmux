//! Daemon-backed session creation and reuse builders.

#[cfg(windows)]
use std::ffi::OsString;
use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

mod redaction;

use crate::handles::{session, Rmux, Session};
use crate::transport::TransportClient;
use crate::{
    ProcessCommandSpec, ProcessSpec, Result, RmuxEndpoint, RmuxError, SessionId, SessionName,
    TerminalSizeSpec,
};
use redaction::redact_environment_error;
use rmux_proto::{NewSessionExtRequest, Request, Response};

#[cfg(windows)]
const RMUX_CLIENT_SHELL_ENV: &str = "RMUX_CLIENT_SHELL";
#[cfg(windows)]
const INTERNAL_TMUX_COMPAT_ENV: &str = "RMUX_INTERNAL_INVOKED_AS_TMUX";

/// Session ensure policy.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[non_exhaustive]
pub enum EnsureSessionPolicy {
    /// Create a new session and report duplicate names as daemon errors.
    CreateOnly,
    /// Create a new session, or reuse an existing named session through
    /// `new-session -A` semantics.
    #[default]
    CreateOrReuse,
    /// Reuse an existing named session without creating a new one. This backs
    /// [`Rmux::session`] when callers want a handle to a known live session.
    ReuseOnly,
}

/// Builder for daemon-backed session creation or reuse.
///
/// The builder records caller intent and becomes active only when passed to
/// [`Rmux::ensure_session`] or [`EnsureSession::ensure`]. Process environment
/// overrides are sent only to the daemon request and are omitted from debug
/// output and returned handles.
#[derive(Clone, PartialEq, Eq)]
pub struct EnsureSession {
    session_name: Option<SessionName>,
    working_directory: Option<String>,
    detached: bool,
    size: Option<TerminalSizeSpec>,
    process: ProcessSpec,
    group_target: Option<SessionName>,
    policy: EnsureSessionPolicy,
    window_name: Option<String>,
    creation_tags: Option<Vec<String>>,
    timeout: Option<Duration>,
}

impl EnsureSession {
    /// Creates a builder that addresses an exact session name.
    #[must_use]
    pub fn named(session_name: SessionName) -> Self {
        Self {
            session_name: Some(session_name),
            ..Self::default()
        }
    }

    /// Creates a builder after validating a session-name string.
    pub fn try_named(session_name: impl AsRef<str>) -> Result<Self> {
        SessionName::new(session_name.as_ref())
            .map(Self::named)
            .map_err(RmuxError::protocol)
    }

    /// Creates a builder that asks the daemon to assign an automatic name.
    #[must_use]
    pub fn auto_named() -> Self {
        Self::default()
    }

    /// Sets the exact session name.
    #[must_use]
    pub fn session_name(mut self, session_name: SessionName) -> Self {
        self.session_name = Some(session_name);
        self
    }

    /// Clears the exact session name so the daemon assigns one.
    #[must_use]
    pub fn automatic_name(mut self) -> Self {
        self.session_name = None;
        self
    }

    /// Returns the exact requested session name, when one is configured.
    #[must_use]
    pub const fn configured_session_name(&self) -> Option<&SessionName> {
        self.session_name.as_ref()
    }

    /// Sets the ensure policy.
    #[must_use]
    pub fn policy(mut self, policy: EnsureSessionPolicy) -> Self {
        self.policy = policy;
        self
    }

    /// Uses create-only semantics.
    #[must_use]
    pub fn create_only(self) -> Self {
        self.policy(EnsureSessionPolicy::CreateOnly)
    }

    /// Uses create-or-reuse semantics.
    #[must_use]
    pub fn create_or_reuse(self) -> Self {
        self.policy(EnsureSessionPolicy::CreateOrReuse)
    }

    /// Uses reuse-only semantics.
    #[must_use]
    pub fn reuse_only(self) -> Self {
        self.policy(EnsureSessionPolicy::ReuseOnly)
    }

    /// Returns the configured ensure policy.
    #[must_use]
    pub const fn configured_policy(&self) -> EnsureSessionPolicy {
        self.policy
    }

    /// Sets the session start directory template.
    #[must_use]
    pub fn working_directory(mut self, working_directory: impl Into<String>) -> Self {
        self.working_directory = Some(working_directory.into());
        self
    }

    /// Sets whether the daemon should leave the session detached.
    #[must_use]
    pub fn detached(mut self, detached: bool) -> Self {
        self.detached = detached;
        self
    }

    /// Records initial terminal size for new sessions.
    #[must_use]
    pub fn size(mut self, size: TerminalSizeSpec) -> Self {
        self.size = Some(size);
        self
    }

    /// Records the legacy initial command vector for the initial pane.
    ///
    /// This preserves the legacy behavior: a one-element command is
    /// interpreted by the daemon as shell text, while multiple elements are
    /// treated as direct argv. New code that wants explicit launch semantics
    /// should prefer [`Self::argv`] or [`Self::shell`].
    #[must_use]
    pub fn command<I, S>(mut self, command: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.process.command = Some(command.into_iter().map(Into::into).collect());
        self.process.process_command = None;
        self
    }

    /// Records direct process argv for the initial pane.
    #[must_use]
    pub fn argv<I, S>(mut self, command: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.process.command = None;
        self.process.process_command = Some(ProcessCommandSpec::Argv(
            command.into_iter().map(Into::into).collect(),
        ));
        self
    }

    /// Records shell command text for the initial pane.
    #[must_use]
    pub fn shell(mut self, command: impl Into<String>) -> Self {
        self.process.command = None;
        self.process.process_command = Some(ProcessCommandSpec::Shell(command.into()));
        self
    }

    /// Records process environment overrides for the initial pane.
    ///
    /// Explicit empty iterators are preserved as `Some([])`.
    #[must_use]
    pub fn environment<I, S>(mut self, environment: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.process.environment = Some(environment.into_iter().map(Into::into).collect());
        self
    }

    /// Sets all process-spawn fields at once.
    #[must_use]
    pub fn process(mut self, process: ProcessSpec) -> Self {
        self.process = process;
        self
    }

    /// Records the group target used for grouped-session creation.
    #[must_use]
    pub fn group_target(mut self, group_target: SessionName) -> Self {
        self.group_target = Some(group_target);
        self
    }

    /// Records the initial active-window name for newly created sessions.
    #[must_use]
    pub fn window_name(mut self, window_name: impl Into<String>) -> Self {
        self.window_name = Some(window_name.into());
        self
    }

    /// Records caller-supplied session tag labels.
    ///
    /// Explicit empty iterators are preserved as `Some([])` and can be
    /// observed on the returned [`Session`] handle.
    #[must_use]
    pub fn tags<I, S>(mut self, tags: I) -> Self
    where
        I: IntoIterator<Item = S>,
        S: Into<String>,
    {
        self.creation_tags = Some(tags.into_iter().map(Into::into).collect());
        self
    }

    /// Records an explicit empty tag label set.
    #[must_use]
    pub fn empty_tags(self) -> Self {
        self.tags(Vec::<String>::new())
    }

    /// Appends one caller-supplied session tag label.
    #[must_use]
    pub fn tag(mut self, tag: impl Into<String>) -> Self {
        self.creation_tags
            .get_or_insert_with(Vec::new)
            .push(tag.into());
        self
    }

    /// Returns caller-supplied tag labels, preserving explicit empty sets.
    #[must_use]
    pub fn configured_tags(&self) -> Option<&[String]> {
        self.creation_tags.as_deref()
    }

    /// Sets a per-operation timeout override.
    ///
    /// `Duration::MAX` requests no timeout, matching [`RmuxBuilder`](crate::RmuxBuilder)
    /// timeout semantics.
    #[must_use]
    pub fn timeout(mut self, timeout: Duration) -> Self {
        self.timeout = Some(timeout);
        self
    }

    /// Returns the configured per-operation timeout override.
    #[must_use]
    pub const fn configured_timeout(&self) -> Option<Duration> {
        self.timeout
    }

    /// Resolves the timeout that this ensure operation would use.
    #[must_use]
    pub fn resolved_timeout(&self, rmux: &Rmux) -> Option<Duration> {
        rmux.resolved_timeout(self.timeout)
    }

    /// Ensures this session through a daemon-backed [`Rmux`] facade.
    pub async fn ensure(self, rmux: &Rmux) -> Result<Session> {
        ensure_session(rmux, self).await
    }

    pub(crate) fn to_new_session_request(&self, attach_if_exists: bool) -> NewSessionExtRequest {
        NewSessionExtRequest {
            session_name: self.session_name.clone(),
            working_directory: self.working_directory.clone(),
            detached: self.detached,
            size: self.size.map(Into::into),
            environment: self.process.environment.clone(),
            group_target: self.group_target.clone(),
            attach_if_exists,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: self.window_name.clone(),
            print_session_info: false,
            print_format: None,
            command: self.process.command.clone(),
            process_command: self
                .process
                .process_command
                .clone()
                .map(rmux_proto::ProcessCommand::from),
            client_environment: sdk_client_environment(),
            skip_environment_update: false,
        }
    }
}

#[cfg(windows)]
fn sdk_client_environment() -> Option<Vec<String>> {
    let mut environment = std::env::vars_os()
        .map(|(name, value)| {
            (
                name.to_string_lossy().into_owned(),
                value.to_string_lossy().into_owned(),
            )
        })
        .filter(|(name, _)| !name.starts_with('='))
        .filter(|(name, _)| !name.eq_ignore_ascii_case(RMUX_CLIENT_SHELL_ENV))
        .filter(|(name, _)| !name.eq_ignore_ascii_case(INTERNAL_TMUX_COMPAT_ENV))
        .map(|(name, value)| format!("{name}={value}"))
        .collect::<Vec<_>>();

    let shell = std::env::var_os("COMSPEC")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OsString::from("cmd.exe"))
        .to_string_lossy()
        .into_owned();
    environment.push(format!("{RMUX_CLIENT_SHELL_ENV}={shell}"));

    Some(environment)
}

#[cfg(not(windows))]
fn sdk_client_environment() -> Option<Vec<String>> {
    None
}

impl Default for EnsureSession {
    fn default() -> Self {
        Self {
            session_name: None,
            working_directory: None,
            detached: true,
            size: None,
            process: ProcessSpec::default(),
            group_target: None,
            policy: EnsureSessionPolicy::default(),
            window_name: None,
            creation_tags: None,
            timeout: None,
        }
    }
}

impl fmt::Debug for EnsureSession {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("EnsureSession")
            .field("session_name", &self.session_name)
            .field("working_directory", &self.working_directory)
            .field("detached", &self.detached)
            .field("size", &self.size)
            .field("process", &self.process)
            .field("group_target", &self.group_target)
            .field("policy", &self.policy)
            .field("window_name", &self.window_name)
            .field("creation_tags", &self.creation_tags)
            .field("timeout", &self.timeout)
            .finish_non_exhaustive()
    }
}

async fn ensure_session(rmux: &Rmux, builder: EnsureSession) -> Result<Session> {
    let endpoint = rmux.resolved_endpoint()?;
    let timeout = builder.resolved_timeout(rmux);
    let transport = rmux
        .connect_resolved_transport_for_operation(&endpoint, timeout)
        .await?;

    let (session_name, created) = match builder.policy {
        EnsureSessionPolicy::CreateOnly => {
            (create_session(&transport, &builder, false).await?, true)
        }
        EnsureSessionPolicy::CreateOrReuse => create_or_reuse_session(&transport, &builder).await?,
        EnsureSessionPolicy::ReuseOnly => reuse_session(&transport, &builder).await?,
    };

    Ok(Session::new(
        session_name,
        endpoint,
        rmux.configured_default_timeout(),
        transport,
        created,
        builder.creation_tags,
    ))
}

pub(crate) async fn create_owned_session(
    builder: EnsureSession,
    required_capabilities: &[&str],
    endpoint: RmuxEndpoint,
    default_timeout: Option<Duration>,
    transport: TransportClient,
) -> Result<(Session, SessionId)> {
    debug_assert_eq!(builder.policy, EnsureSessionPolicy::CreateOnly);
    if !required_capabilities.is_empty() {
        crate::capabilities::require(&transport, required_capabilities).await?;
    }
    let mut request = builder.to_new_session_request(false);
    request.print_session_info = true;
    request.print_format = Some("#{session_id}".to_owned());
    crate::capabilities::require_process_command_if_present(
        &transport,
        request.process_command.as_ref(),
    )
    .await
    .map_err(|error| redact_builder_environment_error(error, &builder))?;
    let response = transport
        .request(Request::NewSessionExt(Box::new(request)))
        .await
        .map_err(|error| redact_builder_environment_error(error, &builder))?;
    let Response::NewSession(response) = response else {
        return Err(session::unexpected_response("new-session", response));
    };
    let session_id = parse_owned_session_id(
        response
            .command_output()
            .ok_or_else(|| owned_session_identity_error("missing print output"))?
            .stdout(),
    )?;
    let session = Session::new(
        response.session_name,
        endpoint,
        default_timeout,
        transport,
        true,
        builder.creation_tags,
    );
    Ok((session, session_id))
}

pub(crate) async fn preflight_owned_session_capabilities(
    transport: &TransportClient,
    required_capabilities: &[&str],
) -> Result<()> {
    crate::capabilities::require(transport, required_capabilities).await
}

fn parse_owned_session_id(stdout: &[u8]) -> Result<SessionId> {
    let rendered = std::str::from_utf8(stdout)
        .map_err(|_| owned_session_identity_error("print output was not UTF-8"))?;
    let rendered = rendered
        .strip_suffix('\n')
        .ok_or_else(|| owned_session_identity_error("print output was not one line"))?;
    if rendered.contains(['\n', '\r']) {
        return Err(owned_session_identity_error(
            "print output contained multiple lines",
        ));
    }
    let raw_id = rendered
        .strip_prefix('$')
        .ok_or_else(|| owned_session_identity_error("print output did not contain a session id"))?;
    let raw_id = raw_id.parse::<u32>().map_err(|_| {
        owned_session_identity_error("print output contained an invalid session id")
    })?;
    Ok(SessionId::new(raw_id))
}

fn owned_session_identity_error(reason: &str) -> RmuxError {
    RmuxError::protocol(rmux_proto::RmuxError::Server(format!(
        "daemon returned invalid owned-session identity: {reason}"
    )))
}

async fn create_or_reuse_session(
    transport: &TransportClient,
    builder: &EnsureSession,
) -> Result<(SessionName, bool)> {
    let Some(configured_name) = builder.session_name.as_ref() else {
        let session_name = create_session(transport, builder, true).await?;
        return Ok((session_name, true));
    };

    let existed_before = session::has_session(transport, configured_name.clone())
        .await
        .map_err(|error| redact_builder_environment_error(error, builder))?;
    if existed_before {
        let session_name = create_session(transport, builder, true).await?;
        return Ok((session_name, false));
    }

    match create_session(transport, builder, false).await {
        Ok(session_name) => Ok((session_name, true)),
        Err(error) if builder.group_target.is_none() && is_duplicate_session_error(&error) => {
            let session_name = create_session(transport, builder, true).await?;
            Ok((session_name, false))
        }
        Err(error) => Err(error),
    }
}

async fn create_session(
    transport: &TransportClient,
    builder: &EnsureSession,
    attach_if_exists: bool,
) -> Result<SessionName> {
    let request = builder.to_new_session_request(attach_if_exists);
    crate::capabilities::require_process_command_if_present(
        transport,
        request.process_command.as_ref(),
    )
    .await
    .map_err(|error| redact_builder_environment_error(error, builder))?;
    match transport
        .request(Request::NewSessionExt(Box::new(request)))
        .await
        .map_err(|error| redact_builder_environment_error(error, builder))?
    {
        Response::NewSession(response) => Ok(response.session_name),
        response => Err(session::unexpected_response("new-session", response)),
    }
}

async fn reuse_session(
    transport: &TransportClient,
    builder: &EnsureSession,
) -> Result<(SessionName, bool)> {
    let Some(session_name) = builder.session_name.clone() else {
        return Err(RmuxError::protocol(rmux_proto::RmuxError::Server(
            "reuse-only ensure-session requires an explicit session name".to_owned(),
        )));
    };

    if session::has_session(transport, session_name.clone())
        .await
        .map_err(|error| redact_builder_environment_error(error, builder))?
    {
        Ok((session_name, false))
    } else {
        Err(RmuxError::protocol(rmux_proto::RmuxError::SessionNotFound(
            session_name.to_string(),
        )))
    }
}

fn redact_builder_environment_error(error: RmuxError, builder: &EnsureSession) -> RmuxError {
    redact_environment_error(error, builder.process.environment.as_deref())
}

fn is_duplicate_session_error(error: &RmuxError) -> bool {
    matches!(
        error,
        RmuxError::Protocol {
            source: rmux_proto::RmuxError::DuplicateSession(_),
        }
    )
}

#[cfg(test)]
#[path = "ensure_tests.rs"]
mod tests;
