#[cfg(unix)]
use std::fs;
use std::io;
#[cfg(windows)]
use std::io::{Read, Write};
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt, PermissionsExt};
#[cfg(unix)]
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};
#[cfg(windows)]
use std::time::Duration;

use tokio::sync::oneshot;
use tokio::task::JoinHandle;
#[cfg(unix)]
use tracing::debug;

use rmux_core::events::SubscriptionLimits;
#[cfg(windows)]
use rmux_ipc::connect_blocking;
use rmux_ipc::{LocalEndpoint, LocalListener};
#[cfg(windows)]
use rmux_proto::{
    encode_frame, FrameDecoder, HasSessionRequest, Request, Response, RmuxError, SessionName,
};

use crate::listener;
#[cfg(windows)]
use crate::server_access::current_owner_uid;

#[cfg(all(test, unix))]
const FALLBACK_SOCKET_ROOT: &str = "/tmp";
#[cfg(unix)]
const BOUND_SOCKET_MODE: u32 = 0o600;
#[cfg(unix)]
const UNSAFE_PERMISSION_MASK: u32 = 0o077;
#[cfg(unix)]
const SOCKET_DIR_PREFIX: &str = "rmux";

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
}

impl DaemonConfig {
    /// Builds a daemon configuration for the given socket path.
    #[must_use]
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            config_load: ConfigLoadOptions::disabled(),
            subscription_limits: SubscriptionLimits::default(),
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
    /// Load tmux's default config search path.
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
            prepare_socket_path(self.config.socket_path())?;
            let endpoint = LocalEndpoint::from_path(self.config.socket_path().to_path_buf());
            let listener = LocalListener::bind(&endpoint)?;
            enforce_bound_socket_permissions(self.config.socket_path())?;
            let (shutdown_handle, shutdown_receiver) = ShutdownHandle::new();
            let socket_path = self.config.socket_path().to_path_buf();
            let owner_uid = real_user_id()?;

            let task = tokio::spawn(listener::serve(
                listener,
                socket_path.clone(),
                shutdown_handle.clone(),
                shutdown_receiver,
                self.config.config_load().clone(),
                self.config.subscription_limits(),
                owner_uid,
            ));

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

            let task = tokio::spawn(listener::serve(
                listener,
                socket_path.clone(),
                shutdown_handle.clone(),
                shutdown_receiver,
                self.config.config_load().clone(),
                self.config.subscription_limits(),
                owner_uid,
            ));

            Ok(ServerHandle {
                socket_path,
                shutdown_handle,
                task: Some(task),
            })
        }
    }
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

#[cfg(unix)]
fn prepare_socket_path(socket_path: &Path) -> io::Result<()> {
    let parent = socket_path.parent().ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "socket path '{}' has no parent directory",
                socket_path.display()
            ),
        )
    })?;

    ensure_parent_directory(parent)?;
    remove_stale_socket_if_needed(socket_path)
}

#[cfg(unix)]
fn ensure_parent_directory(parent: &Path) -> io::Result<()> {
    let mut builder = fs::DirBuilder::new();
    builder.recursive(true);
    builder.mode(0o700);
    match builder.create(parent) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            if !fs::metadata(parent)?.is_dir() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    format!("'{}' exists and is not a directory", parent.display()),
                ));
            }
        }
        Err(error) => return Err(error),
    }

    ensure_directory(parent)?;
    if let Some(managed_parent) = managed_rmux_socket_directory(parent)? {
        ensure_safe_rmux_socket_directory(&managed_parent)?;
    }

    Ok(())
}

#[cfg(unix)]
fn ensure_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if metadata.is_dir() {
        return Ok(());
    }

    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        format!("'{}' exists and is not a directory", path.display()),
    ))
}

#[cfg(unix)]
fn managed_rmux_socket_directory(path: &Path) -> io::Result<Option<PathBuf>> {
    let expected = format!("{SOCKET_DIR_PREFIX}-{}", real_user_id()?);
    Ok(path.ancestors().find_map(|ancestor| {
        ancestor
            .file_name()
            .and_then(|name| name.to_str())
            .filter(|name| *name == expected)
            .map(|_| ancestor.to_path_buf())
    }))
}

#[cfg(unix)]
fn ensure_safe_rmux_socket_directory(path: &Path) -> io::Result<()> {
    let metadata = fs::symlink_metadata(path)?;
    if !metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} is not a directory", path.display()),
        ));
    }

    let user_id = real_user_id()?;
    if metadata.uid() != user_id || (metadata.mode() & UNSAFE_PERMISSION_MASK) != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("directory {} has unsafe permissions", path.display()),
        ));
    }

    Ok(())
}

#[cfg(unix)]
fn enforce_bound_socket_permissions(socket_path: &Path) -> io::Result<()> {
    validate_bound_socket(socket_path, false)?;
    fs::set_permissions(socket_path, fs::Permissions::from_mode(BOUND_SOCKET_MODE))?;
    validate_bound_socket(socket_path, true)
}

#[cfg(unix)]
fn validate_bound_socket(socket_path: &Path, require_owner_only: bool) -> io::Result<()> {
    let metadata = fs::symlink_metadata(socket_path)?;
    if metadata.file_type().is_symlink() || !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "socket path '{}' is not a Unix socket",
                socket_path.display()
            ),
        ));
    }

    let user_id = real_user_id()?;
    if metadata.uid() != user_id {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("socket {} has unsafe ownership", socket_path.display()),
        ));
    }

    if require_owner_only && (metadata.mode() & UNSAFE_PERMISSION_MASK) != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("socket {} has unsafe permissions", socket_path.display()),
        ));
    }

    Ok(())
}

#[cfg(unix)]
fn remove_stale_socket_if_needed(socket_path: &Path) -> io::Result<()> {
    let metadata = match fs::symlink_metadata(socket_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };

    if !metadata.file_type().is_socket() {
        return Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!(
                "socket path '{}' exists but is not a Unix socket",
                socket_path.display()
            ),
        ));
    }

    match StdUnixStream::connect(socket_path) {
        Ok(_stream) => Err(io::Error::new(
            io::ErrorKind::AddrInUse,
            format!("socket '{}' is already in use", socket_path.display()),
        )),
        Err(error) if indicates_stale_socket(&error) => {
            debug!(
                "removing stale socket '{}' after failed connect probe: {error}",
                socket_path.display()
            );
            match fs::remove_file(socket_path) {
                Ok(()) => Ok(()),
                Err(remove_error) if remove_error.kind() == io::ErrorKind::NotFound => Ok(()),
                Err(remove_error) => Err(remove_error),
            }
        }
        Err(error) => Err(error),
    }
}

#[cfg(unix)]
fn indicates_stale_socket(error: &io::Error) -> bool {
    matches!(
        error.kind(),
        io::ErrorKind::ConnectionRefused | io::ErrorKind::NotFound
    )
}

#[cfg(unix)]
pub(crate) fn real_user_id() -> io::Result<u32> {
    Ok(rmux_os::identity::real_user_id())
}

#[cfg(all(test, unix))]
#[path = "daemon_tests/unix.rs"]
mod tests;

#[cfg(all(test, windows))]
#[path = "daemon_tests/windows.rs"]
mod tests;
