#[cfg(windows)]
use std::ffi::{OsStr, OsString};
#[cfg(all(test, unix))]
use std::fs;
use std::io;
#[cfg(windows)]
use std::io::Read;
#[cfg(windows)]
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
#[cfg(windows)]
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::task::JoinHandle;

use rmux_core::events::SubscriptionLimits;
#[cfg(windows)]
use rmux_ipc::connect_blocking;
use rmux_ipc::LocalEndpoint;
#[cfg(windows)]
use rmux_ipc::LocalListener;
#[cfg(windows)]
use rmux_proto::{
    encode_frame, FrameDecoder, HasSessionRequest, Request, Response, RmuxError, SessionName,
};

use crate::listener;
use crate::listener_options::ServeOptions;
#[cfg(windows)]
use crate::server_access::current_owner_uid;
#[cfg(unix)]
use crate::unix_socket::bind_unix_listener_at;
#[cfg(unix)]
use crate::unix_socket::real_user_id;
#[cfg(all(test, unix))]
use crate::unix_socket::{
    ensure_parent_directory, indicates_stale_socket, remove_stale_socket_if_needed,
};
#[cfg(unix)]
use crate::unix_socket_access::UnixSocketAccessController;

#[cfg(all(test, unix))]
const FALLBACK_SOCKET_ROOT: &str = "/tmp";
const DEFAULT_WEB_PORT: u16 = 9777;

/// Computes the default RMUX daemon socket path.
///
/// The path uses an rmux-specific per-user directory so it cannot collide with
/// a real tmux server socket.
pub fn default_socket_path() -> io::Result<PathBuf> {
    rmux_ipc::default_endpoint().map(LocalEndpoint::into_path)
}

#[cfg(all(test, unix))]
fn socket_root_from_env(tmpdir: Option<&std::ffi::OsStr>) -> io::Result<PathBuf> {
    let tmpdir = tmpdir
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .into_iter();
    let candidates = tmpdir.chain(std::iter::once(PathBuf::from(FALLBACK_SOCKET_ROOT)));

    for candidate in candidates {
        if let Ok(resolved) = fs::canonicalize(&candidate) {
            return Ok(resolved);
        }
    }

    Err(io::Error::new(
        io::ErrorKind::NotFound,
        "no suitable rmux socket directory",
    ))
}

/// Daemon configuration for a single RMUX server instance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DaemonConfig {
    socket_path: PathBuf,
    config_load: ConfigLoadOptions,
    subscription_limits: SubscriptionLimits,
    web_frontend: Option<String>,
    web_port: u16,
    web_port_explicit: bool,
    web_required: bool,
    startup_ready_fd: Option<i32>,
    #[cfg(windows)]
    startup_ready_event: Option<OsString>,
}

impl DaemonConfig {
    /// Builds a daemon configuration for the given socket path.
    #[must_use]
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            config_load: ConfigLoadOptions::disabled(),
            subscription_limits: SubscriptionLimits::default(),
            web_frontend: None,
            web_port: DEFAULT_WEB_PORT,
            web_port_explicit: false,
            web_required: false,
            startup_ready_fd: None,
            #[cfg(windows)]
            startup_ready_event: None,
        }
    }

    /// Builds a daemon configuration using the default spec socket path.
    pub fn with_default_socket_path() -> io::Result<Self> {
        Ok(Self::new(default_socket_path()?))
    }

    /// Returns the configured local IPC endpoint path.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Returns the startup config loading policy.
    #[must_use]
    pub const fn config_load(&self) -> &ConfigLoadOptions {
        &self.config_load
    }

    /// Returns the pane-output subscription limits.
    #[must_use]
    pub fn subscription_limits(&self) -> SubscriptionLimits {
        self.subscription_limits
    }

    /// Returns the configured web-share listener port.
    #[must_use]
    pub const fn web_port(&self) -> u16 {
        self.web_port
    }

    /// Returns whether the web-share listener port was explicitly configured.
    #[must_use]
    pub const fn web_port_explicit(&self) -> bool {
        self.web_port_explicit
    }

    /// Returns whether this daemon startup requires the web listener to bind.
    #[must_use]
    pub const fn web_required(&self) -> bool {
        self.web_required
    }

    /// Returns the optional external web-share frontend origin.
    #[must_use]
    pub fn web_frontend(&self) -> Option<&str> {
        self.web_frontend.as_deref()
    }

    /// Enables RMUX default startup config loading.
    #[must_use]
    pub fn with_default_config_load(mut self, quiet: bool, cwd: Option<PathBuf>) -> Self {
        self.config_load = ConfigLoadOptions {
            selection: ConfigFileSelection::Default,
            quiet,
            cwd,
        };
        self
    }

    /// Overrides pane-output subscription limits for this daemon.
    #[must_use]
    pub fn with_subscription_limits(mut self, subscription_limits: SubscriptionLimits) -> Self {
        self.subscription_limits = subscription_limits;
        self
    }

    /// Overrides the web-share listener port.
    #[must_use]
    pub const fn with_web_port(mut self, port: u16) -> Self {
        self.web_port = port;
        self.web_port_explicit = true;
        self.web_required = true;
        self
    }

    /// Overrides the frontend origin used in generated web-share URLs.
    #[must_use]
    pub fn with_web_frontend(mut self, frontend: String) -> Self {
        self.web_frontend = Some(frontend);
        self.web_required = true;
        self
    }

    /// Enables explicit `-f` startup config loading.
    #[must_use]
    pub fn with_config_files(
        mut self,
        files: Vec<PathBuf>,
        quiet: bool,
        cwd: Option<PathBuf>,
    ) -> Self {
        self.config_load = ConfigLoadOptions {
            selection: ConfigFileSelection::Files(files),
            quiet,
            cwd,
        };
        self
    }

    /// Signals this inherited Linux eventfd after the daemon listener is bound.
    #[cfg(target_os = "linux")]
    #[must_use]
    pub const fn with_startup_ready_fd(mut self, ready_fd: i32) -> Self {
        self.startup_ready_fd = Some(ready_fd);
        self
    }

    #[cfg(unix)]
    const fn startup_ready_fd(&self) -> Option<i32> {
        self.startup_ready_fd
    }

    /// Signals this named Win32 event after the daemon listener is bound.
    #[cfg(windows)]
    #[must_use]
    pub fn with_startup_ready_event(mut self, ready_event: OsString) -> Self {
        self.startup_ready_event = Some(ready_event);
        self
    }

    #[cfg(windows)]
    fn startup_ready_event(&self) -> Option<&OsStr> {
        self.startup_ready_event.as_deref()
    }
}

/// Startup config loading policy for a daemon.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfigLoadOptions {
    selection: ConfigFileSelection,
    quiet: bool,
    cwd: Option<PathBuf>,
}

impl ConfigLoadOptions {
    /// Builds a config policy that performs no startup config loading.
    #[must_use]
    pub const fn disabled() -> Self {
        Self {
            selection: ConfigFileSelection::Disabled,
            quiet: true,
            cwd: None,
        }
    }

    /// Returns the selected config files mode.
    #[must_use]
    pub const fn selection(&self) -> &ConfigFileSelection {
        &self.selection
    }

    /// Returns whether missing files should be suppressed.
    #[must_use]
    pub const fn quiet(&self) -> bool {
        self.quiet
    }

    /// Returns the startup client's current working directory.
    #[must_use]
    pub fn cwd(&self) -> Option<&Path> {
        self.cwd.as_deref()
    }
}

/// Config file selection mode for daemon startup.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ConfigFileSelection {
    /// Do not load config files.
    Disabled,
    /// Load RMUX default config files, with a filtered tmux config fallback.
    Default,
    /// Load the explicit `-f` files in order.
    Files(Vec<PathBuf>),
}

/// RMUX daemon launcher — call [`bind`](Self::bind) to start listening.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ServerDaemon {
    config: DaemonConfig,
}

#[derive(Debug, Clone)]
pub(crate) struct ShutdownHandle {
    sender: Arc<StdMutex<Option<oneshot::Sender<()>>>>,
}

impl ShutdownHandle {
    pub(crate) fn new() -> (Self, oneshot::Receiver<()>) {
        let (sender, receiver) = oneshot::channel();
        (
            Self {
                sender: Arc::new(StdMutex::new(Some(sender))),
            },
            receiver,
        )
    }

    pub(crate) fn request_shutdown(&self) {
        if let Some(sender) = self.sender.lock().expect("shutdown sender").take() {
            let _ = sender.send(());
        }
    }
}

impl ServerDaemon {
    /// Creates a daemon launcher for the given configuration.
    #[must_use]
    pub fn new(config: DaemonConfig) -> Self {
        Self { config }
    }

    /// Binds the local IPC endpoint, starts accepting requests, and returns a handle.
    pub async fn bind(self) -> io::Result<ServerHandle> {
        #[cfg(unix)]
        {
            let bound_listener = bind_unix_listener_at(self.config.socket_path())?;
            let socket_access = UnixSocketAccessController::new(
                self.config.socket_path(),
                bound_listener.identity,
            )?;
            let (shutdown_handle, shutdown_receiver) = ShutdownHandle::new();
            let signal_watcher = crate::signals::SignalWatcher::install()?;
            let socket_path = self.config.socket_path().to_path_buf();
            let owner_uid = real_user_id()?;
            let serve_options = ServeOptions::new(
                self.config.config_load().clone(),
                self.config.subscription_limits(),
                owner_uid,
            )
            .with_web_options(
                self.config.web_port(),
                self.config.web_frontend().map(str::to_owned),
                self.config.web_required(),
                self.config.web_port_explicit(),
            )
            .with_socket_identity(bound_listener.identity)
            .with_socket_access(socket_access)
            .with_server_signals(signal_watcher);

            let task = tokio::spawn(listener::serve(
                bound_listener.listener,
                socket_path.clone(),
                shutdown_handle.clone(),
                shutdown_receiver,
                serve_options,
            ));
            if let Some(ready_fd) = self.config.startup_ready_fd() {
                signal_startup_ready_fd(ready_fd);
            }

            Ok(ServerHandle {
                socket_path,
                shutdown_handle,
                task: Some(task),
            })
        }

        #[cfg(windows)]
        {
            let endpoint = LocalEndpoint::from_path(self.config.socket_path().to_path_buf());
            let listener = bind_windows_listener(&endpoint)?;
            let (shutdown_handle, shutdown_receiver) = ShutdownHandle::new();
            let socket_path = self.config.socket_path().to_path_buf();
            let owner_uid = current_owner_uid();
            let serve_options = ServeOptions::new(
                self.config.config_load().clone(),
                self.config.subscription_limits(),
                owner_uid,
            )
            .with_web_options(
                self.config.web_port(),
                self.config.web_frontend().map(str::to_owned),
                self.config.web_required(),
                self.config.web_port_explicit(),
            );

            let task = tokio::spawn(listener::serve(
                listener,
                socket_path.clone(),
                shutdown_handle.clone(),
                shutdown_receiver,
                serve_options,
            ));
            if let Some(ready_event) = self.config.startup_ready_event() {
                signal_startup_ready_event(ready_event);
            }

            Ok(ServerHandle {
                socket_path,
                shutdown_handle,
                task: Some(task),
            })
        }
    }
}

#[cfg(unix)]
fn signal_startup_ready_fd(_ready_fd: i32) {
    #[cfg(target_os = "linux")]
    {
        let _ = rmux_os::daemon::signal_startup_ready_fd(_ready_fd);
    }
}

#[cfg(windows)]
fn signal_startup_ready_event(ready_event: &OsStr) {
    let _ = rmux_os::daemon::signal_startup_ready_event(ready_event);
}

#[cfg(windows)]
fn bind_windows_listener(endpoint: &LocalEndpoint) -> io::Result<LocalListener> {
    match LocalListener::bind(endpoint) {
        Ok(listener) => Ok(listener),
        Err(bind_error) => Err(windows_bind_error(endpoint, bind_error)),
    }
}

#[cfg(windows)]
fn windows_bind_error(endpoint: &LocalEndpoint, bind_error: io::Error) -> io::Error {
    if windows_pipe_responds(endpoint) {
        return io::Error::new(
            io::ErrorKind::AddrInUse,
            format!(
                "Windows named pipe '{}' is already held by a responsive rmux-compatible server",
                endpoint.as_path().display()
            ),
        );
    }

    io::Error::new(
        bind_error.kind(),
        format!(
            "failed to bind Windows named pipe '{}': {bind_error}. Another process may still be holding this endpoint",
            endpoint.as_path().display()
        ),
    )
}

#[cfg(windows)]
fn windows_pipe_responds(endpoint: &LocalEndpoint) -> bool {
    let endpoint = endpoint.clone();
    std::thread::spawn(move || windows_protocol_probe(&endpoint).unwrap_or(false))
        .join()
        .unwrap_or(false)
}

#[cfg(windows)]
fn windows_protocol_probe(endpoint: &LocalEndpoint) -> io::Result<bool> {
    let mut stream = connect_blocking(endpoint, Duration::from_millis(100))?;
    stream.set_write_timeout(Some(Duration::from_millis(100)))?;
    stream.set_read_timeout(Some(Duration::from_millis(100)))?;

    let request = Request::HasSession(HasSessionRequest {
        target: SessionName::new("__rmux_probe__").map_err(io::Error::other)?,
    });
    let frame = encode_frame(&request).map_err(io::Error::other)?;
    stream.write_all(&frame)?;
    stream.flush()?;

    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 512];
    loop {
        let bytes_read = match stream.read(&mut buffer) {
            Ok(0) => return Ok(false),
            Ok(bytes_read) => bytes_read,
            Err(error) if error.kind() == io::ErrorKind::TimedOut => return Ok(false),
            Err(error) => return Err(error),
        };
        decoder.push_bytes(&buffer[..bytes_read]);
        match decoder.next_frame::<Response>() {
            Ok(Some(Response::HasSession(_))) => return Ok(true),
            Ok(Some(_response)) => return Ok(false),
            Ok(None) => continue,
            Err(RmuxError::IncompleteFrame { .. }) => continue,
            Err(_error) => return Ok(false),
        }
    }
}

/// Handle to a running RMUX daemon; dropping it triggers shutdown.
#[derive(Debug)]
pub struct ServerHandle {
    socket_path: PathBuf,
    shutdown_handle: ShutdownHandle,
    task: Option<JoinHandle<io::Result<()>>>,
}

impl ServerHandle {
    /// Returns the bound local IPC endpoint path for the running daemon.
    #[must_use]
    pub fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    /// Waits for the daemon task to exit after an external shutdown request.
    pub async fn wait(mut self) -> io::Result<()> {
        if let Some(task) = self.task.take() {
            return task.await.map_err(io::Error::other)?;
        }

        Ok(())
    }

    /// Requests shutdown and waits for socket cleanup to complete.
    pub async fn shutdown(mut self) -> io::Result<()> {
        self.request_shutdown();

        if let Some(task) = self.task.take() {
            return task.await.map_err(io::Error::other)?;
        }

        Ok(())
    }

    fn request_shutdown(&mut self) {
        self.shutdown_handle.request_shutdown();
    }
}

impl Drop for ServerHandle {
    fn drop(&mut self) {
        self.request_shutdown();
    }
}

#[cfg(all(test, unix))]
#[path = "daemon_tests/unix.rs"]
mod tests;

#[cfg(all(test, windows))]
#[path = "daemon_tests/windows.rs"]
mod tests;
