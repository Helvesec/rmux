use std::env;
use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::path::Path;
use std::path::PathBuf;
#[cfg(unix)]
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::time::Duration;

use rmux_sdk::bootstrap::discovery::{
    resolve_timeout, SDK_ENDPOINT_ENV, SDK_TIMEOUT_MS_ENV, V1_DEFAULT_TIMEOUT,
};
use rmux_sdk::{RmuxBuilder, RmuxEndpoint};

static ENV_LOCK: Mutex<()> = Mutex::new(());
#[cfg(unix)]
static UNIQUE_ID: AtomicUsize = AtomicUsize::new(0);

#[test]
fn timeout_resolution_uses_per_operation_builder_env_then_default() {
    let _lock = lock_env();
    let _env = EnvGuard::remove(SDK_TIMEOUT_MS_ENV);

    assert_eq!(resolve_timeout(None, None), Some(V1_DEFAULT_TIMEOUT));
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { env::set_var(SDK_TIMEOUT_MS_ENV, "1500") };
    assert_eq!(
        resolve_timeout(None, None),
        Some(Duration::from_millis(1500))
    );
    assert_eq!(
        RmuxBuilder::new().resolved_timeout(None),
        Some(Duration::from_millis(1500))
    );
    assert_eq!(
        RmuxBuilder::new()
            .default_timeout(Duration::from_millis(2500))
            .resolved_timeout(None),
        Some(Duration::from_millis(2500))
    );
    assert_eq!(
        RmuxBuilder::new()
            .default_timeout(Duration::from_millis(2500))
            .resolved_timeout(Some(Duration::ZERO)),
        Some(Duration::ZERO)
    );
    assert_eq!(
        RmuxBuilder::new()
            .default_timeout(Duration::ZERO)
            .resolved_timeout(None),
        Some(Duration::ZERO)
    );
    assert_eq!(
        RmuxBuilder::new()
            .default_timeout(Duration::from_millis(2500))
            .resolved_timeout(Some(Duration::from_millis(25))),
        Some(Duration::from_millis(25))
    );
}

#[test]
fn duration_max_resolves_to_no_timeout_at_explicit_layers() {
    let _lock = lock_env();
    let _env = EnvGuard::set(SDK_TIMEOUT_MS_ENV, "1500");

    assert_eq!(
        RmuxBuilder::new()
            .default_timeout(Duration::from_millis(2500))
            .resolved_timeout(Some(Duration::MAX)),
        None
    );
    assert_eq!(
        RmuxBuilder::new()
            .default_timeout(Duration::MAX)
            .resolved_timeout(None),
        None
    );
}

#[test]
fn malformed_timeout_environment_falls_back_to_v1_default() {
    let _lock = lock_env();
    let _env = EnvGuard::set(SDK_TIMEOUT_MS_ENV, "not-a-number");

    assert_eq!(resolve_timeout(None, None), Some(V1_DEFAULT_TIMEOUT));
}

#[test]
fn explicit_endpoint_variants_bypass_sdk_environment() {
    let _lock = lock_env();
    let _endpoint = EnvGuard::set(SDK_ENDPOINT_ENV, "definitely-not-a-discovered-endpoint");

    let unix_endpoint = RmuxEndpoint::UnixSocket(PathBuf::from("relative-explicit.sock"));
    assert_eq!(
        RmuxBuilder::new()
            .endpoint(unix_endpoint.clone())
            .resolved_endpoint()
            .expect("resolve explicit Unix endpoint"),
        unix_endpoint
    );

    let windows_endpoint = RmuxEndpoint::WindowsPipe("explicit-pipe".to_owned());
    assert_eq!(
        RmuxBuilder::new()
            .endpoint(windows_endpoint.clone())
            .resolved_endpoint()
            .expect("resolve explicit Windows endpoint"),
        windows_endpoint
    );
}

#[cfg(unix)]
#[test]
fn unix_endpoint_precedence_uses_explicit_then_sdk_env_then_default() {
    let _lock = lock_env();
    let root = TestRoot::new("endpoint-precedence");
    let _tmpdir = EnvGuard::set_os("RMUX_TMPDIR", root.path().as_os_str());
    let _endpoint = EnvGuard::remove(SDK_ENDPOINT_ENV);

    let default_path = expect_unix_endpoint(RmuxBuilder::new().resolved_endpoint());
    assert!(default_path.starts_with(root.path()));
    assert_eq!(default_path.file_name(), Some(OsStr::new("default")));

    let env_path = default_path
        .parent()
        .expect("owned socket root")
        .join("sdk-env.sock");
    let _listener = bind_unix_socket(&env_path);
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { env::set_var(SDK_ENDPOINT_ENV, &env_path) };

    assert_eq!(
        expect_unix_endpoint(RmuxBuilder::new().resolved_endpoint()),
        env_path
    );

    let explicit_path = root.path().join("explicit-outside-owned-root.sock");
    assert_eq!(
        expect_unix_endpoint(
            RmuxBuilder::new()
                .unix_socket(&explicit_path)
                .resolved_endpoint()
        ),
        explicit_path
    );
    assert_eq!(
        expect_unix_endpoint(
            RmuxBuilder::new()
                .unix_socket(root.path().join("discarded-explicit.sock"))
                .default_endpoint()
                .resolved_endpoint()
        ),
        env_path
    );
}

#[cfg(unix)]
#[test]
fn unix_rmux_tmpdir_canonicalization_feeds_default_and_sdk_allowlist() {
    let _lock = lock_env();
    let root = TestRoot::new("tmpdir-canonical");
    let real_tmpdir = root.path().join("real-tmp");
    let linked_tmpdir = root.path().join("linked-tmp");
    std::fs::create_dir_all(&real_tmpdir).expect("create real tmpdir");
    std::os::unix::fs::symlink(&real_tmpdir, &linked_tmpdir).expect("create tmpdir symlink");

    let _tmpdir = EnvGuard::set_os("RMUX_TMPDIR", linked_tmpdir.as_os_str());
    let _endpoint = EnvGuard::remove(SDK_ENDPOINT_ENV);

    let default_path = expect_unix_endpoint(RmuxBuilder::new().resolved_endpoint());
    let real_tmpdir = std::fs::canonicalize(real_tmpdir).expect("canonical real tmpdir");
    assert!(default_path.starts_with(&real_tmpdir));
    assert!(!default_path.starts_with(&linked_tmpdir));
    assert_eq!(default_path.file_name(), Some(OsStr::new("default")));

    let owned_root = default_path.parent().expect("owned socket root");
    let env_path = owned_root.join("sdk-env.sock");
    let _listener = bind_unix_socket(&env_path);
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { env::set_var(SDK_ENDPOINT_ENV, &env_path) };

    assert_eq!(
        expect_unix_endpoint(RmuxBuilder::new().resolved_endpoint()),
        env_path
    );
}

#[cfg(unix)]
#[test]
fn unix_malformed_env_endpoint_falls_back_to_platform_default() {
    let _lock = lock_env();
    let root = TestRoot::new("malformed-env");
    let _tmpdir = EnvGuard::set_os("RMUX_TMPDIR", root.path().as_os_str());
    let _endpoint = EnvGuard::remove(SDK_ENDPOINT_ENV);
    let default_path = expect_unix_endpoint(RmuxBuilder::new().resolved_endpoint());

    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { env::set_var(SDK_ENDPOINT_ENV, "relative.sock") };

    assert_eq!(
        expect_unix_endpoint(RmuxBuilder::new().resolved_endpoint()),
        default_path
    );
}

#[cfg(unix)]
#[test]
fn unix_env_endpoint_accepts_missing_target_inside_owned_root() {
    let _lock = lock_env();
    let root = TestRoot::new("missing-env-target");
    let _tmpdir = EnvGuard::set_os("RMUX_TMPDIR", root.path().as_os_str());
    let _endpoint = EnvGuard::remove(SDK_ENDPOINT_ENV);
    let default_path = expect_unix_endpoint(RmuxBuilder::new().resolved_endpoint());
    let owned_root = default_path.parent().expect("owned socket root");
    std::fs::create_dir_all(owned_root).expect("create owned root");

    let env_path = owned_root.join("not-yet-bound.sock");
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { env::set_var(SDK_ENDPOINT_ENV, &env_path) };

    assert_eq!(
        expect_unix_endpoint(RmuxBuilder::new().resolved_endpoint()),
        env_path
    );
}

#[cfg(unix)]
#[test]
fn unix_disallowed_env_endpoint_falls_back_to_platform_default() {
    let _lock = lock_env();
    let root = TestRoot::new("disallowed-env");
    let _tmpdir = EnvGuard::set_os("RMUX_TMPDIR", root.path().as_os_str());
    let _endpoint = EnvGuard::remove(SDK_ENDPOINT_ENV);
    let default_path = expect_unix_endpoint(RmuxBuilder::new().resolved_endpoint());

    let disallowed = root.path().join("not-rmux-owned").join("sdk.sock");
    let _listener = bind_unix_socket(&disallowed);
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { env::set_var(SDK_ENDPOINT_ENV, &disallowed) };

    assert_eq!(
        expect_unix_endpoint(RmuxBuilder::new().resolved_endpoint()),
        default_path
    );
}

#[cfg(unix)]
#[test]
fn unix_env_endpoint_rejects_parent_escape_and_symlinked_parent() {
    let _lock = lock_env();
    let root = TestRoot::new("parent-escape");
    let _tmpdir = EnvGuard::set_os("RMUX_TMPDIR", root.path().as_os_str());
    let _endpoint = EnvGuard::remove(SDK_ENDPOINT_ENV);
    let default_path = expect_unix_endpoint(RmuxBuilder::new().resolved_endpoint());
    let owned_root = default_path.parent().expect("owned socket root");
    std::fs::create_dir_all(owned_root).expect("create owned root");

    let escaped_socket = root.path().join("escaped").join("sdk.sock");
    let _escaped_listener = bind_unix_socket(&escaped_socket);
    let traversal_path = owned_root.join("..").join("escaped").join("sdk.sock");
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { env::set_var(SDK_ENDPOINT_ENV, &traversal_path) };
    assert_eq!(
        expect_unix_endpoint(RmuxBuilder::new().resolved_endpoint()),
        default_path
    );

    let symlink_target = root.path().join("symlink-target");
    std::fs::create_dir_all(&symlink_target).expect("create symlink target");
    let symlinked_parent = owned_root.join("linked-parent");
    std::os::unix::fs::symlink(&symlink_target, &symlinked_parent).expect("create parent symlink");
    let linked_socket = symlink_target.join("sdk.sock");
    let _linked_listener = bind_unix_socket(&linked_socket);

    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { env::set_var(SDK_ENDPOINT_ENV, symlinked_parent.join("sdk.sock")) };
    assert_eq!(
        expect_unix_endpoint(RmuxBuilder::new().resolved_endpoint()),
        default_path
    );
}

#[cfg(unix)]
#[test]
fn unix_env_endpoint_rejects_symlink_and_regular_file_targets() {
    let _lock = lock_env();
    let root = TestRoot::new("unsafe-targets");
    let _tmpdir = EnvGuard::set_os("RMUX_TMPDIR", root.path().as_os_str());
    let _endpoint = EnvGuard::remove(SDK_ENDPOINT_ENV);
    let default_path = expect_unix_endpoint(RmuxBuilder::new().resolved_endpoint());
    let owned_root = default_path.parent().expect("owned socket root");
    std::fs::create_dir_all(owned_root).expect("create owned root");

    let regular = owned_root.join("regular-file.sock");
    std::fs::write(&regular, b"not a socket").expect("write regular file");
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { env::set_var(SDK_ENDPOINT_ENV, &regular) };
    assert_eq!(
        expect_unix_endpoint(RmuxBuilder::new().resolved_endpoint()),
        default_path
    );

    let real_socket = owned_root.join("real.sock");
    let _listener = bind_unix_socket(&real_socket);
    let symlink = owned_root.join("link.sock");
    std::os::unix::fs::symlink(&real_socket, &symlink).expect("create socket symlink");
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { env::set_var(SDK_ENDPOINT_ENV, &symlink) };

    assert_eq!(
        expect_unix_endpoint(RmuxBuilder::new().resolved_endpoint()),
        default_path
    );
}

#[cfg(unix)]
#[test]
fn unix_explicit_paths_bypass_auto_discovery_safety_checks() {
    let _lock = lock_env();
    let root = TestRoot::new("explicit-exception");
    let _tmpdir = EnvGuard::set_os("RMUX_TMPDIR", root.path().as_os_str());
    let _endpoint = EnvGuard::remove(SDK_ENDPOINT_ENV);

    let explicit = root.path().join("not-rmux-owned").join("regular.sock");
    std::fs::create_dir_all(explicit.parent().expect("explicit parent"))
        .expect("create explicit parent");
    std::fs::write(&explicit, b"not a socket").expect("write explicit regular file");

    assert_eq!(
        expect_unix_endpoint(
            RmuxBuilder::new()
                .unix_socket(&explicit)
                .resolved_endpoint()
        ),
        explicit
    );
}

#[cfg(unix)]
#[test]
fn unix_unresolved_rmux_tmpdir_still_falls_back_to_tmp() {
    let _lock = lock_env();
    let root = TestRoot::new("tmpdir-fallback");
    let missing = root.path().join("does-not-exist");
    let _tmpdir = EnvGuard::set_os("RMUX_TMPDIR", missing.as_os_str());
    let _endpoint = EnvGuard::remove(SDK_ENDPOINT_ENV);

    let default_path = expect_unix_endpoint(RmuxBuilder::new().resolved_endpoint());
    let tmp = std::fs::canonicalize("/tmp").expect("canonical /tmp");

    assert!(default_path.starts_with(tmp));
    assert_eq!(default_path.file_name(), Some(OsStr::new("default")));
}

#[cfg(windows)]
#[test]
fn windows_endpoint_precedence_uses_explicit_then_sdk_env_then_default() {
    let _lock = lock_env();
    let _endpoint = EnvGuard::set(SDK_ENDPOINT_ENV, r"\\.\pipe\rmux-S-1-5-21-sdk-default");

    assert_eq!(
        expect_windows_endpoint(RmuxBuilder::new().resolved_endpoint()),
        r"\\.\pipe\rmux-S-1-5-21-sdk-default"
    );
    assert_eq!(
        expect_windows_endpoint(
            RmuxBuilder::new()
                .windows_pipe("explicit-pipe")
                .resolved_endpoint()
        ),
        "explicit-pipe"
    );

    env::set_var(SDK_ENDPOINT_ENV, r"\\.\pipe\not-rmux");
    let fallback = expect_windows_endpoint(RmuxBuilder::new().resolved_endpoint());
    assert!(fallback.starts_with(r"\\.\pipe\rmux-"));
}

fn lock_env() -> MutexGuard<'static, ()> {
    ENV_LOCK
        .lock()
        .unwrap_or_else(|poisoned| poisoned.into_inner())
}

struct EnvGuard {
    key: &'static str,
    previous: Option<OsString>,
}

impl EnvGuard {
    fn set(key: &'static str, value: &str) -> Self {
        Self::set_os(key, OsStr::new(value))
    }

    fn set_os(key: &'static str, value: &OsStr) -> Self {
        let previous = env::var_os(key);
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { env::set_var(key, value) };
        Self { key, previous }
    }

    fn remove(key: &'static str) -> Self {
        let previous = env::var_os(key);
        // TODO: Audit that the environment access only happens in single-threaded code.
        unsafe { env::remove_var(key) };
        Self { key, previous }
    }
}

impl Drop for EnvGuard {
    fn drop(&mut self) {
        match &self.previous {
            // TODO: Audit that the environment access only happens in single-threaded code.
            Some(value) => unsafe { env::set_var(self.key, value) },
            // TODO: Audit that the environment access only happens in single-threaded code.
            None => unsafe { env::remove_var(self.key) },
        }
    }
}

#[cfg(unix)]
struct TestRoot {
    path: PathBuf,
}

#[cfg(unix)]
impl TestRoot {
    fn new(label: &str) -> Self {
        let unique = UNIQUE_ID.fetch_add(1, Ordering::Relaxed);
        // Keep Unix socket fixtures below macOS' short sockaddr_un budget.
        let path = PathBuf::from("/tmp").join(format!(
            "rmux-sd-{}-{}-{unique}",
            compact_label(label),
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("create test root");
        let path = std::fs::canonicalize(path).expect("canonical test root");

        Self { path }
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

#[cfg(unix)]
impl Drop for TestRoot {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.path);
    }
}

#[cfg(unix)]
fn expect_unix_endpoint(endpoint: rmux_sdk::Result<RmuxEndpoint>) -> PathBuf {
    match endpoint.expect("resolve endpoint") {
        RmuxEndpoint::UnixSocket(path) => path,
        endpoint => panic!("expected Unix socket endpoint, got {endpoint:?}"),
    }
}

#[cfg(unix)]
fn compact_label(label: &str) -> String {
    let compact = label
        .chars()
        .filter(|character| character.is_ascii_alphanumeric())
        .take(12)
        .collect::<String>();
    if compact.is_empty() {
        "x".to_owned()
    } else {
        compact
    }
}

#[cfg(unix)]
fn bind_unix_socket(path: &Path) -> std::os::unix::net::UnixListener {
    let parent = path.parent().expect("socket parent");
    std::fs::create_dir_all(parent).expect("create socket parent");
    let _ = std::fs::remove_file(path);
    std::os::unix::net::UnixListener::bind(path).expect("bind Unix socket")
}

#[cfg(windows)]
fn expect_windows_endpoint(endpoint: rmux_sdk::Result<RmuxEndpoint>) -> String {
    match endpoint.expect("resolve endpoint") {
        RmuxEndpoint::WindowsPipe(pipe) => pipe,
        endpoint => panic!("expected Windows pipe endpoint, got {endpoint:?}"),
    }
}
