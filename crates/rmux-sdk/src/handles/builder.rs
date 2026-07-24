//! Builder for the opaque RMUX SDK facade.

use std::fmt;
use std::path::PathBuf;
use std::time::Duration;

use super::rmux::{connect_or_start_transport, connect_transport_to_endpoint, Rmux};
use crate::bootstrap::discovery;
use crate::{Result, RmuxEndpoint};

/// Builder for an inert [`Rmux`] facade handle.
///
/// The builder only stores configuration. It does not resolve default
/// endpoints, touch the filesystem, open IPC handles, or require a running
/// daemon.
pub struct RmuxBuilder {
    endpoint: RmuxEndpoint,
    default_timeout: Option<Duration>,
}

impl RmuxBuilder {
    /// Creates a builder configured to use default endpoint discovery.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Sets the daemon endpoint selection explicitly.
    ///
    /// Passing [`RmuxEndpoint::Default`] restores deferred SDK endpoint
    /// discovery.
    #[must_use]
    pub fn endpoint(mut self, endpoint: RmuxEndpoint) -> Self {
        self.endpoint = endpoint;
        self
    }

    /// Sets an explicit Unix-domain socket path.
    ///
    /// This method is available on every compile target and only records the
    /// path; it does not require Unix at runtime.
    #[must_use]
    pub fn unix_socket(self, path: impl Into<PathBuf>) -> Self {
        self.endpoint(RmuxEndpoint::UnixSocket(path.into()))
    }

    /// Sets an explicit Windows named-pipe identifier.
    ///
    /// This method is available on every compile target and only records the
    /// pipe name; it does not require Windows at runtime.
    #[must_use]
    pub fn windows_pipe(self, pipe: impl Into<String>) -> Self {
        self.endpoint(RmuxEndpoint::WindowsPipe(pipe.into()))
    }

    /// Restores deferred SDK endpoint discovery.
    #[must_use]
    pub fn default_endpoint(self) -> Self {
        self.endpoint(RmuxEndpoint::Default)
    }

    /// Sets the default timeout used by SDK operations built from this handle.
    ///
    /// Passing `Duration::MAX` records an explicit no-timeout default.
    #[must_use]
    pub fn default_timeout(mut self, timeout: Duration) -> Self {
        self.default_timeout = Some(timeout);
        self
    }

    /// Returns the endpoint selection currently recorded by this builder.
    #[must_use]
    pub fn configured_endpoint(&self) -> &RmuxEndpoint {
        &self.endpoint
    }

    /// Returns the operation timeout default currently recorded by this
    /// builder.
    #[must_use]
    pub const fn configured_default_timeout(&self) -> Option<Duration> {
        self.default_timeout
    }

    /// Resolves the endpoint that would be used by runtime SDK operations.
    ///
    /// This consults SDK discovery only when the configured endpoint is
    /// [`RmuxEndpoint::Default`].
    pub fn resolved_endpoint(&self) -> Result<RmuxEndpoint> {
        discovery::resolve_endpoint(&self.endpoint)
    }

    /// Resolves the timeout that would be used by one runtime SDK operation.
    ///
    /// `per_operation_timeout` has precedence over this builder's configured
    /// default and can use `Duration::MAX` to request no timeout.
    #[must_use]
    pub fn resolved_timeout(&self, per_operation_timeout: Option<Duration>) -> Option<Duration> {
        discovery::resolve_timeout(per_operation_timeout, self.default_timeout)
    }

    /// Builds an inert facade handle from the recorded configuration.
    ///
    /// Building does not contact the daemon or perform endpoint resolution.
    #[must_use]
    pub fn build(self) -> Rmux {
        Rmux::from_config(self.endpoint, self.default_timeout)
    }

    /// Connects to the configured daemon endpoint and returns a live facade.
    ///
    /// Unlike [`Self::build`], this resolves the endpoint immediately and
    /// opens the local IPC transport. It never starts a daemon.
    pub async fn connect(self) -> Result<Rmux> {
        let endpoint = discovery::resolve_endpoint(&self.endpoint)?;
        let timeout = discovery::resolve_timeout(None, self.default_timeout);
        let transport = connect_transport_to_endpoint(&endpoint, timeout).await?;
        Ok(Rmux::from_connected_transport(
            endpoint,
            self.default_timeout,
            transport,
        ))
    }

    /// Connects to a daemon, starting the platform hidden daemon if needed.
    ///
    /// Startup is serialized by the OS-specific bootstrap primitive. Dropping
    /// the returned facade never kills the daemon; callers use
    /// [`Rmux::shutdown`] for an explicit shutdown request.
    pub async fn connect_or_start(self) -> Result<Rmux> {
        let endpoint = discovery::resolve_endpoint(&self.endpoint)?;
        let connected = connect_or_start_transport(&endpoint, self.default_timeout).await?;
        Ok(Rmux::from_connected_transport(
            connected.endpoint,
            self.default_timeout,
            connected.transport,
        ))
    }
}

impl Default for RmuxBuilder {
    fn default() -> Self {
        Self {
            endpoint: RmuxEndpoint::Default,
            default_timeout: None,
        }
    }
}

impl fmt::Debug for RmuxBuilder {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter
            .debug_struct("RmuxBuilder")
            .finish_non_exhaustive()
    }
}
