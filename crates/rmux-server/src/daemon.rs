#[cfg(unix)]
use std::fs;
use std::io;
#[cfg(unix)]
use std::os::unix::fs::{DirBuilderExt, FileTypeExt, MetadataExt};
#[cfg(unix)]
use std::os::unix::net::UnixStream as StdUnixStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex as StdMutex};

use tokio::sync::oneshot;
use tokio::task::JoinHandle;
#[cfg(unix)]
use tracing::debug;

use rmux_ipc::{LocalEndpoint, LocalListener};

use crate::listener;
#[cfg(windows)]
use crate::server_access::current_owner_uid;

#[cfg(all(test, unix))]
const FALLBACK_SOCKET_ROOT: &str = "/tmp";
#[cfg(unix)]
const RMUX_SOCK_PERM: u32 = 0o007;
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
}

impl DaemonConfig {
    /// Builds a daemon configuration for the given socket path.
    #[must_use]
    pub fn new(socket_path: PathBuf) -> Self {
        Self {
            socket_path,
            config_load: ConfigLoadOptions::disabled(),
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
            let (shutdown_handle, shutdown_receiver) = ShutdownHandle::new();
            let socket_path = self.config.socket_path().to_path_buf();
            let owner_uid = real_user_id()?;

            let task = tokio::spawn(listener::serve(
                listener,
                socket_path.clone(),
                shutdown_handle.clone(),
                shutdown_receiver,
                self.config.config_load().clone(),
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
            let listener = LocalListener::bind(&endpoint)?;
            let (shutdown_handle, shutdown_receiver) = ShutdownHandle::new();
            let socket_path = self.config.socket_path().to_path_buf();
            let owner_uid = current_owner_uid();

            let task = tokio::spawn(listener::serve(
                listener,
                socket_path.clone(),
                shutdown_handle.clone(),
                shutdown_receiver,
                self.config.config_load().clone(),
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
    if metadata.uid() != user_id || (metadata.mode() & RMUX_SOCK_PERM) != 0 {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            format!("directory {} has unsafe permissions", path.display()),
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

#[cfg(unix)]
#[cfg(test)]
mod tests {
    use super::{
        default_socket_path, remove_stale_socket_if_needed, socket_root_from_env, DaemonConfig,
    };
    use std::ffi::OsStr;
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::os::unix::net::{UnixListener as StdUnixListener, UnixStream as StdUnixStream};
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

    #[test]
    fn default_socket_path_uses_the_spec_layout() {
        let path = default_socket_path().expect("default socket path");
        let path_string = path.to_string_lossy();

        assert!(path_string.ends_with("/default"));
        assert!(path_string.contains("/rmux-"));
    }

    #[test]
    fn unresolved_rmux_tmpdir_falls_back_to_tmp() {
        assert_eq!(
            socket_root_from_env(Some(OsStr::new(
                "relative-rmux-test-path-that-does-not-exist"
            )))
            .expect("socket root"),
            fs::canonicalize("/tmp").expect("canonical /tmp")
        );
    }

    #[test]
    fn real_user_id_matches_process_identity() {
        assert_eq!(
            super::real_user_id().expect("real uid"),
            rmux_os::identity::real_user_id()
        );
    }

    #[test]
    fn daemon_config_returns_the_configured_path() {
        let path = PathBuf::from("/tmp/rmux-test/default");
        let config = DaemonConfig::new(path.clone());

        assert_eq!(config.socket_path(), path.as_path());
    }

    #[test]
    fn stale_socket_probe_removes_unreachable_socket_files() {
        let socket_path = unique_socket_path("stale-socket");
        let parent = socket_path.parent().expect("socket parent");
        let _ = fs::remove_file(&socket_path);
        let _ = fs::remove_dir_all(parent);
        fs::create_dir_all(parent).expect("create socket parent");
        let listener = StdUnixListener::bind(&socket_path).expect("bind stale socket");
        drop(listener);
        wait_until_socket_is_stale(&socket_path).expect("dropped listener becomes unreachable");

        remove_stale_socket_if_needed(&socket_path).expect("remove stale socket");

        assert!(!socket_path.exists());
        let _ = fs::remove_dir_all(parent);
    }

    #[test]
    fn unsafe_managed_rmux_socket_directory_is_rejected() {
        let user_id = super::real_user_id().expect("real uid");
        let root = unique_socket_path("permissions")
            .parent()
            .expect("socket parent")
            .to_path_buf();
        let socket_path = root.join(format!("rmux-{user_id}")).join("default");
        let parent = socket_path.parent().expect("socket parent");
        fs::create_dir_all(parent).expect("create managed socket parent");
        fs::set_permissions(parent, fs::Permissions::from_mode(0o755)).expect("set perms");

        let error = super::ensure_parent_directory(parent).expect_err("unsafe parent should fail");

        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn unsafe_managed_rmux_socket_directory_ancestor_is_rejected() {
        let user_id = super::real_user_id().expect("real uid");
        let root = unique_socket_path("nested-permissions")
            .parent()
            .expect("socket parent")
            .to_path_buf();
        let managed = root.join(format!("rmux-{user_id}"));
        let nested_parent = managed.join("nested");
        fs::create_dir_all(&nested_parent).expect("create nested socket parent");
        fs::set_permissions(&managed, fs::Permissions::from_mode(0o755)).expect("set perms");

        let error = super::ensure_parent_directory(&nested_parent)
            .expect_err("unsafe managed ancestor should fail");

        assert_eq!(error.kind(), std::io::ErrorKind::PermissionDenied);
        let _ = fs::set_permissions(&managed, fs::Permissions::from_mode(0o700));
        let _ = fs::remove_dir_all(root);
    }

    #[test]
    fn stale_socket_removal_leaves_non_socket_paths_untouched() {
        let socket_path = unique_socket_path("not-a-socket");
        let parent = socket_path.parent().expect("socket parent");
        fs::create_dir_all(parent).expect("create socket parent");
        fs::write(&socket_path, "not a socket").expect("write regular file");

        let error = remove_stale_socket_if_needed(&socket_path).expect_err("must fail");

        assert_eq!(error.kind(), std::io::ErrorKind::AlreadyExists);
        assert!(fs::symlink_metadata(&socket_path)
            .expect("metadata")
            .file_type()
            .is_file());
        let _ = fs::remove_file(&socket_path);
        let _ = fs::remove_dir_all(parent);
    }

    fn unique_socket_path(label: &str) -> PathBuf {
        let unique_id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
        let process_id = std::process::id();
        std::env::temp_dir()
            .join(format!("rmux-server-{label}-{process_id}-{unique_id}"))
            .join("default.sock")
    }

    fn wait_until_socket_is_stale(socket_path: &std::path::Path) -> std::io::Result<()> {
        for _ in 0..100 {
            match StdUnixStream::connect(socket_path) {
                Err(error) if super::indicates_stale_socket(&error) => return Ok(()),
                Err(error) => return Err(error),
                Ok(stream) => drop(stream),
            }

            std::thread::sleep(Duration::from_millis(1));
        }

        Err(std::io::Error::new(
            std::io::ErrorKind::TimedOut,
            format!("socket '{}' stayed reachable", socket_path.display()),
        ))
    }
}

#[cfg(windows)]
#[cfg(test)]
mod tests {
    use super::{DaemonConfig, ServerDaemon};
    use rmux_proto::{
        encode_frame, ErrorResponse, FrameDecoder, HasSessionRequest, KillServerRequest,
        KillSessionRequest, ListClientsRequest, ListPanesRequest, ListSessionsRequest,
        ListWindowsRequest, LockServerRequest, NewSessionRequest, Request, Response, RmuxError,
        SessionName, TerminalSize,
    };
    use std::io::{self, Read, Write};
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::time::Duration;

    static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

    #[tokio::test]
    async fn windows_daemon_accepts_ipc_and_dispatches_runtime_requests() -> io::Result<()> {
        let endpoint = unique_endpoint()?;
        let socket_path = endpoint.clone().into_path();
        let handle = ServerDaemon::new(DaemonConfig::new(socket_path.clone()))
            .bind()
            .await?;

        let response = tokio::task::spawn_blocking(move || {
            let mut stream = rmux_ipc::connect_blocking(&endpoint, Duration::from_secs(5))?;
            let request = Request::LockServer(LockServerRequest);
            let frame = encode_frame(&request).map_err(io::Error::other)?;
            stream.write_all(&frame)?;
            read_response(&mut stream)
        })
        .await
        .map_err(io::Error::other)??;

        assert!(matches!(response, Response::LockServer(_)));
        assert_eq!(handle.socket_path(), socket_path.as_path());
        handle.shutdown().await
    }

    #[tokio::test]
    async fn windows_daemon_kill_server_stops_runtime() -> io::Result<()> {
        let endpoint = unique_endpoint()?;
        let socket_path = endpoint.clone().into_path();
        let handle = ServerDaemon::new(DaemonConfig::new(socket_path))
            .bind()
            .await?;

        let response = tokio::task::spawn_blocking(move || {
            let mut stream = rmux_ipc::connect_blocking(&endpoint, Duration::from_secs(5))?;
            let frame =
                encode_frame(&Request::KillServer(KillServerRequest)).map_err(io::Error::other)?;
            stream.write_all(&frame)?;
            read_response(&mut stream)
        })
        .await
        .map_err(io::Error::other)??;

        assert!(matches!(response, Response::KillServer(_)));
        handle.wait().await
    }

    #[tokio::test]
    async fn windows_daemon_empty_session_requests_match_unix_semantics() -> io::Result<()> {
        let endpoint = unique_endpoint()?;
        let socket_path = endpoint.clone().into_path();
        let handle = ServerDaemon::new(DaemonConfig::new(socket_path))
            .bind()
            .await?;
        let target = SessionName::new("missing").expect("valid session");

        let (has_session, kill_session) = tokio::task::spawn_blocking(move || {
            let has_session = roundtrip(
                &endpoint,
                Request::HasSession(HasSessionRequest {
                    target: target.clone(),
                }),
            )?;
            let kill_session = roundtrip(
                &endpoint,
                Request::KillSession(KillSessionRequest {
                    target,
                    kill_all_except_target: false,
                    clear_alerts: false,
                }),
            )?;

            Ok::<_, io::Error>((has_session, kill_session))
        })
        .await
        .map_err(io::Error::other)??;

        assert!(matches!(
            has_session,
            Response::HasSession(rmux_proto::HasSessionResponse { exists: false })
        ));
        assert!(matches!(
            kill_session,
            Response::Error(ErrorResponse {
                error: RmuxError::SessionNotFound(name)
            }) if name == "missing"
        ));

        handle.shutdown().await
    }

    #[tokio::test]
    async fn windows_daemon_creates_detached_session() -> io::Result<()> {
        let endpoint = unique_endpoint()?;
        let socket_path = endpoint.clone().into_path();
        let handle = ServerDaemon::new(DaemonConfig::new(socket_path))
            .bind()
            .await?;

        let response = tokio::task::spawn_blocking(move || {
            roundtrip(
                &endpoint,
                Request::NewSession(NewSessionRequest {
                    session_name: SessionName::new("alpha").expect("valid session"),
                    detached: true,
                    size: Some(TerminalSize { cols: 80, rows: 24 }),
                    environment: None,
                }),
            )
        })
        .await
        .map_err(io::Error::other)??;

        assert!(matches!(response, Response::NewSession(_)));
        handle.shutdown().await
    }

    #[tokio::test]
    async fn windows_daemon_empty_listing_requests_succeed() -> io::Result<()> {
        let endpoint = unique_endpoint()?;
        let socket_path = endpoint.clone().into_path();
        let handle = ServerDaemon::new(DaemonConfig::new(socket_path))
            .bind()
            .await?;

        let responses = tokio::task::spawn_blocking(move || {
            let requests = vec![
                Request::ListSessions(ListSessionsRequest {
                    format: None,
                    filter: None,
                    sort_order: None,
                    reversed: false,
                }),
                Request::ListWindows(ListWindowsRequest {
                    target: SessionName::new("alpha").expect("valid session"),
                    format: None,
                }),
                Request::ListPanes(ListPanesRequest {
                    target: SessionName::new("alpha").expect("valid session"),
                    target_window_index: None,
                    format: None,
                }),
                Request::ListClients(ListClientsRequest {
                    format: None,
                    filter: None,
                    sort_order: None,
                    reversed: false,
                    target_session: None,
                }),
            ];

            requests
                .into_iter()
                .map(|request| roundtrip(&endpoint, request))
                .collect::<io::Result<Vec<_>>>()
        })
        .await
        .map_err(io::Error::other)??;

        for response in responses {
            match response {
                Response::ListSessions(response) => assert!(response.output.stdout().is_empty()),
                Response::ListClients(response) => {
                    assert_eq!(response.match_count, 0);
                    assert!(response.output.stdout().is_empty());
                }
                Response::Error(ErrorResponse {
                    error: RmuxError::SessionNotFound(name),
                }) => assert_eq!(name, "alpha"),
                other => panic!("unexpected response: {other:?}"),
            }
        }

        handle.shutdown().await
    }

    fn unique_endpoint() -> io::Result<rmux_ipc::LocalEndpoint> {
        let unique_id = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
        rmux_ipc::endpoint_for_label(format!("server-windows-{}-{unique_id}", std::process::id()))
    }

    fn roundtrip(endpoint: &rmux_ipc::LocalEndpoint, request: Request) -> io::Result<Response> {
        let mut stream = rmux_ipc::connect_blocking(endpoint, Duration::from_secs(5))?;
        let frame = encode_frame(&request).map_err(io::Error::other)?;
        stream.write_all(&frame)?;
        read_response(&mut stream)
    }

    fn read_response(stream: &mut rmux_ipc::BlockingLocalStream) -> io::Result<Response> {
        let mut decoder = FrameDecoder::new();
        let mut buffer = [0_u8; 8192];

        loop {
            if let Some(response) = decoder.next_frame::<Response>().map_err(io::Error::other)? {
                return Ok(response);
            }

            let bytes_read = stream.read(&mut buffer)?;
            if bytes_read == 0 {
                return Err(io::ErrorKind::UnexpectedEof.into());
            }
            decoder.push_bytes(&buffer[..bytes_read]);
        }
    }
}
