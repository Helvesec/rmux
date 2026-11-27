use super::*;
#[cfg(any(unix, windows))]
use std::ffi::{OsStr, OsString};
#[cfg(unix)]
use std::sync::atomic::{AtomicUsize, Ordering};
#[cfg(any(unix, windows))]
use std::sync::Mutex;

#[cfg(any(unix, windows))]
static ENV_LOCK: Mutex<()> = Mutex::new(());
#[cfg(unix)]
static TEMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

#[cfg(unix)]
#[test]
fn default_endpoint_uses_the_spec_layout() {
    let path = default_endpoint().expect("default endpoint").into_path();
    let path_string = path.to_string_lossy();

    assert!(path_string.ends_with("/default"));
    assert!(path_string.contains("/rmux-"));
}

#[cfg(target_os = "linux")]
#[test]
fn empty_socket_path_uses_a_stable_non_default_endpoint() {
    let empty = resolve_endpoint(None, Some(Path::new(""))).expect("empty -S endpoint");
    assert!(!empty.is_filesystem_path());
    let empty_path = empty.clone().into_path();
    let repeated = resolve_endpoint(None, Some(Path::new("")))
        .expect("repeated empty -S endpoint")
        .into_path();
    let default = default_endpoint().expect("default endpoint").into_path();

    assert_eq!(empty_path, repeated);
    assert_ne!(empty_path, default);
    assert!(empty_path.as_os_str().is_empty());
    assert_eq!(
        resolve_tmux_compatible_endpoint(None, Some(Path::new("")))
            .expect("tmux-compatible empty -S endpoint"),
        empty
    );
}

#[cfg(any(windows, all(unix, not(target_os = "linux"))))]
#[test]
fn empty_socket_path_is_rejected_without_linux_abstract_sockets() {
    for resolve in [resolve_endpoint, resolve_tmux_compatible_endpoint] {
        let error = resolve(None, Some(Path::new("")))
            .expect_err("empty -S endpoint should be unsupported");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}

#[cfg(unix)]
#[test]
fn unresolved_rmux_tmpdir_falls_back_to_tmp() {
    assert_eq!(
        socket_root_from_parts(Some(OsStr::new(
            "relative-rmux-test-path-that-does-not-exist"
        )))
        .expect("socket root"),
        std::fs::canonicalize("/tmp").expect("canonical /tmp")
    );
}

#[cfg(unix)]
#[test]
fn tmux_tmpdir_sets_the_label_socket_root() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let root = unique_socket_root("tmux-tmpdir-fallback");
    let root = std::fs::canonicalize(root).expect("canonical tmpdir root");
    let _rmux = EnvGuard::remove(RMUX_TMPDIR_ENV);
    let _tmux = EnvGuard::set(TMUX_TMPDIR_ENV, root.as_os_str());

    let path = endpoint_for_label("tmux-tmpdir-label")
        .expect("endpoint")
        .into_path();

    assert_eq!(socket_label_root(&path), Some(root.as_path()));
}

#[cfg(unix)]
#[test]
fn rmux_tmpdir_wins_over_tmux_tmpdir() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let rmux_root = unique_socket_root("rmux-tmpdir-priority");
    let rmux_root = std::fs::canonicalize(rmux_root).expect("canonical rmux tmpdir root");
    let tmux_root = unique_socket_root("tmux-tmpdir-priority");
    let tmux_root = std::fs::canonicalize(tmux_root).expect("canonical tmux tmpdir root");
    let _rmux = EnvGuard::set(RMUX_TMPDIR_ENV, rmux_root.as_os_str());
    let _tmux = EnvGuard::set(TMUX_TMPDIR_ENV, tmux_root.as_os_str());

    let path = endpoint_for_label("tmpdir-priority-label")
        .expect("endpoint")
        .into_path();

    assert_eq!(socket_label_root(&path), Some(rmux_root.as_path()));
}

#[cfg(windows)]
#[test]
fn default_endpoint_uses_a_user_scoped_named_pipe() {
    let path = default_endpoint()
        .expect("default named-pipe endpoint")
        .into_path();
    let path = path.to_string_lossy();

    assert!(path.starts_with(r"\\.\pipe\rmux-S-"));
    assert!(path.contains("-il-"));
    assert!(path.contains("-g-"));
}

#[cfg(windows)]
#[test]
fn windows_integrity_label_is_endpoint_safe() {
    let integrity = current_integrity_label().expect("current integrity label");

    assert!(matches!(
        integrity,
        "untrusted" | "low" | "medium" | "high" | "system"
    ));
}

#[cfg(windows)]
#[test]
fn pipe_labels_are_injective() {
    assert_ne!(
        pipe_component(OsStr::new("alpha/beta")),
        pipe_component(OsStr::new("alpha:beta"))
    );
    assert_eq!(
        pipe_component(OsStr::new("alpha/beta:gamma")),
        "alpha~002Fbeta~003Agamma"
    );
}

#[cfg(windows)]
#[test]
fn pipe_label_components_are_ascii_case_insensitive() {
    assert_eq!(
        canonical_pipe_label_component(OsStr::new("Alpha/BETA")),
        canonical_pipe_label_component(OsStr::new("aLPHA/beta"))
    );
    assert_eq!(
        canonical_pipe_label_component(OsStr::new("Alpha/BETA")),
        "alpha~002fbeta"
    );
}

#[cfg(windows)]
#[test]
fn pipe_label_components_keep_escaped_units_injective() {
    assert_ne!(
        canonical_pipe_label_component(OsStr::new("alpha/beta")),
        canonical_pipe_label_component(OsStr::new("alpha~002Fbeta"))
    );
    assert_ne!(
        canonical_pipe_label_component(OsStr::new("É")),
        canonical_pipe_label_component(OsStr::new("é"))
    );
}

#[cfg(unix)]
#[test]
fn tmux_env_accepts_rmux_owned_socket_endpoint() {
    let path = socket_path_from_tmux_env(Some(OsStr::new("/tmp/rmux-1000/default,123,0")))
        .expect("tmux socket endpoint");

    assert_eq!(path, PathBuf::from("/tmp/rmux-1000/default"));
}

#[cfg(unix)]
#[test]
fn mux_env_accepts_explicit_unix_socket_endpoint() {
    let path = socket_path_from_tmux_env(Some(OsStr::new("/tmp/custom-rmux.sock,123,0")))
        .expect("explicit socket endpoint");

    assert_eq!(path, PathBuf::from("/tmp/custom-rmux.sock"));
}

#[cfg(unix)]
#[test]
fn rmux_env_accepts_explicit_unix_socket_without_tmux_suffix() {
    let path = socket_path_from_rmux_env(Some(OsStr::new("/tmp/custom-rmux.sock")))
        .expect("explicit rmux socket endpoint");

    assert_eq!(path, PathBuf::from("/tmp/custom-rmux.sock"));
}

#[cfg(unix)]
#[test]
fn mux_env_rejects_relative_socket_endpoint() {
    assert_eq!(
        socket_path_from_tmux_env(Some(OsStr::new("relative.sock,123,0"))),
        None
    );
    assert_eq!(
        socket_path_from_rmux_env(Some(OsStr::new("relative.sock"))),
        None
    );
}

#[cfg(unix)]
#[test]
fn resolve_endpoint_uses_rmux_owned_tmux_env_when_no_cli_endpoint_is_set() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _rmux = EnvGuard::remove(RMUX_ENV);
    let tmux_socket = format!(
        "/tmp/rmux-{}/custom,123,0",
        rmux_os::identity::real_user_id()
    );
    let expected = PathBuf::from(tmux_socket.split(',').next().expect("socket path"));
    let _tmux = EnvGuard::set(TMUX_ENV, OsStr::new(&tmux_socket));

    let path = resolve_endpoint(None, None)
        .expect("tmux env endpoint")
        .into_path();

    assert_eq!(path, expected);
}

#[cfg(unix)]
#[test]
fn resolve_endpoint_ignores_foreign_tmux_env_when_no_cli_endpoint_is_set() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _rmux = EnvGuard::remove(RMUX_ENV);
    let _tmux = EnvGuard::set(TMUX_ENV, OsStr::new("/tmp/tmux-1000/default,123,0"));

    let path = resolve_endpoint(None, None)
        .expect("default endpoint")
        .into_path();

    assert!(path.ends_with("default"));
    assert_ne!(path, PathBuf::from("/tmp/tmux-1000/default"));
}

#[cfg(unix)]
#[test]
fn tmux_compatible_resolve_endpoint_uses_tmux_env_when_no_cli_endpoint_is_set() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _rmux = EnvGuard::remove(RMUX_ENV);
    let _tmux = EnvGuard::set(TMUX_ENV, OsStr::new("/tmp/rmux-1000/custom,123,0"));

    let path = resolve_tmux_compatible_endpoint(None, None)
        .expect("tmux env endpoint")
        .into_path();

    assert_eq!(path, PathBuf::from("/tmp/rmux-1000/custom"));
}

#[cfg(unix)]
#[test]
fn tmux_compatible_resolve_endpoint_prefers_rmux_env_over_tmux_env() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _rmux = EnvGuard::set(RMUX_ENV, OsStr::new("/tmp/rmux-1000/native,123,0"));
    let _tmux = EnvGuard::set(TMUX_ENV, OsStr::new("/tmp/rmux-1000/tmux,123,0"));

    let path = resolve_tmux_compatible_endpoint(None, None)
        .expect("rmux env endpoint")
        .into_path();

    assert_eq!(path, PathBuf::from("/tmp/rmux-1000/native"));
}

#[cfg(any(unix, windows))]
#[test]
fn tmux_compatible_resolve_endpoint_redirects_claude_swarm_socket_only() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _main = EnvGuard::set(
        RMUX_INTERNAL_CLAUDE_MAIN_SOCKET_ENV,
        OsStr::new("rmux-claude-test"),
    );
    let _swarm = EnvGuard::set(
        RMUX_INTERNAL_CLAUDE_SWARM_SOCKET_ENV,
        OsStr::new("claude-swarm-test"),
    );

    let redirected = resolve_tmux_compatible_endpoint(Some(OsStr::new("claude-swarm-test")), None)
        .expect("redirected endpoint")
        .into_path();
    let expected = endpoint_for_label("rmux-claude-test")
        .expect("main endpoint")
        .into_path();
    assert_eq!(redirected, expected);

    let native = resolve_endpoint(Some(OsStr::new("claude-swarm-test")), None)
        .expect("native endpoint")
        .into_path();
    let unredirected = endpoint_for_label("claude-swarm-test")
        .expect("swarm endpoint")
        .into_path();
    assert_eq!(native, unredirected);
}

#[cfg(windows)]
#[test]
fn tmux_compatible_socket_path_names_one_private_server_per_file() {
    let dir = std::env::temp_dir();
    let path = dir.join("rmux-fs-socket.sock");
    let resolve = |path: &Path| {
        resolve_tmux_compatible_endpoint(None, Some(path)).expect("tmux -S file path")
    };

    let endpoint = resolve(&path);

    assert!(socket_path_is_rmux_owned(endpoint.as_path()).expect("owned check"));
    assert_eq!(
        resolve(&dir.join(".").join("RMUX-FS-SOCKET.sock")),
        endpoint
    );
    assert_ne!(resolve(&dir.join("other.sock")), endpoint);
    assert_eq!(
        resolve_endpoint(None, Some(&path))
            .expect_err("rmux -S file path")
            .kind(),
        io::ErrorKind::InvalidInput
    );
}

#[cfg(windows)]
#[test]
fn tmux_compatibility_socket_path_rejects_missing_parent_before_creation() {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after Unix epoch")
        .as_nanos();
    let root = std::env::temp_dir().join(format!(
        "rmux-ipc-missing-parent-{}-{nonce}",
        std::process::id()
    ));
    std::fs::create_dir(&root).expect("create test root");
    let parent = root.join("new-parent");
    let socket = parent.join("server.sock");

    let missing = resolve_tmux_compatible_endpoint(None, Some(&socket));
    let parent_was_created = parent.exists();
    std::fs::create_dir(&parent).expect("create previously missing parent");
    let created = resolve_tmux_compatible_endpoint(None, Some(&socket));
    let repeated = resolve_tmux_compatible_endpoint(None, Some(&socket));
    let canonical_socket = std::fs::canonicalize(&parent)
        .expect("canonical parent")
        .join("server.sock");
    let canonical = resolve_tmux_compatible_endpoint(None, Some(&canonical_socket));
    let socket_was_created = socket.exists();
    std::fs::remove_dir(&parent).expect("remove empty socket parent");
    std::fs::remove_dir(&root).expect("remove test root");

    assert!(!parent_was_created, "resolution must not create the parent");
    assert_eq!(
        missing
            .expect_err("missing parent must not select a noncanonical server")
            .kind(),
        io::ErrorKind::NotFound
    );
    let endpoint = created.expect("existing parent selects a server");
    assert_eq!(repeated.expect("repeated resolution"), endpoint);
    assert_eq!(canonical.expect("canonical spelling"), endpoint);
    assert!(!socket_was_created, "resolution must not create the socket");
}

#[cfg(windows)]
#[test]
fn tmux_compatible_resolve_endpoint_redirects_windows_claude_child_pid_socket() {
    let _lock = ENV_LOCK.lock().expect("env lock");
    let _main = EnvGuard::set(
        RMUX_INTERNAL_CLAUDE_MAIN_SOCKET_ENV,
        OsStr::new("rmux-claude-test"),
    );
    let _swarm = EnvGuard::set(
        RMUX_INTERNAL_CLAUDE_SWARM_SOCKET_ENV,
        OsStr::new("claude-swarm-parent-pid"),
    );

    let redirected =
        resolve_tmux_compatible_endpoint(Some(OsStr::new("claude-swarm-child-pid")), None)
            .expect("redirected endpoint")
            .into_path();
    let expected = endpoint_for_label("rmux-claude-test")
        .expect("main endpoint")
        .into_path();
    assert_eq!(redirected, expected);

    let native = resolve_endpoint(Some(OsStr::new("claude-swarm-child-pid")), None)
        .expect("native endpoint")
        .into_path();
    let unredirected = endpoint_for_label("claude-swarm-child-pid")
        .expect("swarm endpoint")
        .into_path();
    assert_eq!(native, unredirected);
}

#[cfg(unix)]
fn unique_socket_root(name: &str) -> PathBuf {
    let counter = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let root =
        std::env::temp_dir().join(format!("rmux-ipc-{name}-{}-{counter}", std::process::id()));
    std::fs::create_dir_all(&root).expect("create socket root");
    root
}

#[cfg(unix)]
fn socket_label_root(path: &Path) -> Option<&Path> {
    path.parent().and_then(Path::parent)
}

#[cfg(any(unix, windows))]
struct EnvGuard {
    name: &'static str,
    previous: Option<OsString>,
}

#[cfg(any(unix, windows))]
impl EnvGuard {
    fn set(name: &'static str, value: &OsStr) -> Self {
        let previous = std::env::var_os(name);
        std::env::set_var(name, value);
        Self { name, previous }
    }

    #[cfg(unix)]
    fn remove(name: &'static str) -> Self {
        let previous = std::env::var_os(name);
        std::env::remove_var(name);
        Self { name, previous }
    }
}

#[cfg(any(unix, windows))]
impl Drop for EnvGuard {
    fn drop(&mut self) {
        if let Some(previous) = &self.previous {
            std::env::set_var(self.name, previous);
        } else {
            std::env::remove_var(self.name);
        }
    }
}

#[cfg(windows)]
#[test]
fn rmux_env_accepts_windows_named_pipe_endpoint() {
    let endpoint = endpoint_for_label("env-current").expect("current endpoint");
    let env_value = format!("{},123,0", endpoint.as_path().to_string_lossy());
    let path = socket_path_from_rmux_env(Some(OsStr::new(&env_value))).expect("rmux pipe endpoint");

    assert_eq!(path, endpoint.into_path());
}

#[cfg(windows)]
#[test]
fn explicit_socket_path_preserves_the_exact_managed_pipe_generation() {
    let endpoint = endpoint_for_label("explicit-current").expect("managed endpoint");
    for resolve in [resolve_endpoint, resolve_tmux_compatible_endpoint] {
        let explicit = resolve(Some(OsStr::new("ignored-label")), Some(endpoint.as_path()))
            .expect("explicit endpoint");

        assert_eq!(explicit, endpoint);
    }
}

#[cfg(windows)]
#[test]
fn windows_socket_path_rejects_foreign_sid_pipe() {
    let integrity = current_integrity_label().expect("current integrity");
    let path = format!(r"\\.\pipe\rmux-S-1-0-0-il-{integrity}-default");

    for resolve in [resolve_endpoint, resolve_tmux_compatible_endpoint] {
        let error =
            resolve(None, Some(Path::new(&path))).expect_err("foreign SID pipe should be rejected");

        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}

#[cfg(windows)]
#[test]
fn rmux_env_rejects_explicit_custom_windows_pipe_endpoint() {
    let path = socket_path_from_rmux_env(Some(OsStr::new(r"\\.\pipe\external-peer-default,123,0")));

    assert_eq!(path, None);
}

#[cfg(windows)]
#[test]
fn windows_socket_path_rejects_non_rmux_pipe() {
    for resolve in [resolve_endpoint, resolve_tmux_compatible_endpoint] {
        for path in [
            r"\\.\pipe\external-peer-default",
            r"\\.\PIPE\external-peer-default",
        ] {
            let error =
                resolve(None, Some(Path::new(path))).expect_err("non-rmux pipe should be rejected");

            assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
        }
    }
}

#[cfg(windows)]
#[test]
fn windows_socket_path_rejects_non_pipe_path() {
    let error = resolve_endpoint(None, Some(Path::new(r"C:\tmp\rmux.sock")))
        .expect_err("non-pipe path should be rejected");

    assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
}
