#[cfg(not(target_has_atomic = "64"))]
use portable_atomic::AtomicU64;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
#[cfg(target_has_atomic = "64")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::Arc;

use rmux_ipc::{LocalListener, PeerIdentity};
use rmux_os::identity::UserIdentity;
use rmux_proto::{
    ControlMode, NewSessionRequest, Request, Response, ServerAccessRequest, SessionName,
    TerminalSize,
};
use tokio::sync::mpsc;

use super::attach_support::{AttachRegistration, ClientFlags};
use super::{ControlRegistration, RequestHandler};
use crate::control::{ControlModeUpgrade, ControlServerEvent};
use crate::outer_terminal::OuterTerminalContext;
use crate::pane_io::AttachControl;
use crate::server_access::{resolve_user, AccessMode, ResolvedUser};
use crate::unix_socket::{
    bind_unix_listener_at, SocketFileIdentity, OWNER_ONLY_DIRECTORY_MODE, OWNER_ONLY_SOCKET_MODE,
    SHARED_DIRECTORY_MODE, SHARED_SOCKET_MODE,
};

static NEXT_SOCKET: AtomicUsize = AtomicUsize::new(0);

struct UnixTransportFixture {
    handler: RequestHandler,
    root_path: PathBuf,
    socket_path: PathBuf,
    socket_identity: Option<SocketFileIdentity>,
    listeners: Vec<LocalListener>,
    owns_root: bool,
}

impl UnixTransportFixture {
    fn new(label: &str) -> Self {
        Self::with_private_depth(label, 1)
    }

    fn with_private_depth(label: &str, depth: usize) -> Self {
        let unique = NEXT_SOCKET.fetch_add(1, Ordering::Relaxed);
        let root_path = PathBuf::from(format!(
            "/tmp/rma{}{}{}",
            std::process::id(),
            label.as_bytes()[0],
            unique
        ));
        fs::create_dir(&root_path).expect("create traversable fixture root");
        fs::set_permissions(
            &root_path,
            fs::Permissions::from_mode(SHARED_DIRECTORY_MODE),
        )
        .expect("make fixture root traversable");
        let mut parent = root_path.join(format!(
            "rmux-{}",
            crate::server_access::current_owner_uid()
        ));
        for index in 1..depth {
            parent.push(format!("d{index}"));
        }
        Self::bind(root_path, parent.join("s"), true)
    }

    /// Starts another server of the same user next to this one, through the
    /// daemon's own bind and controller path.
    fn sibling(&self, name: &str) -> Self {
        Self::bind(
            self.root_path.clone(),
            self.socket_path.with_file_name(name),
            false,
        )
    }

    fn bind(root_path: PathBuf, socket_path: PathBuf, owns_root: bool) -> Self {
        let bound = bind_unix_listener_at(&socket_path).expect("bind fixture Unix socket");
        let handler = RequestHandler::new();
        handler
            .install_unix_socket_access_for_test(&socket_path, bound.identity)
            .expect("install fixture Unix transport controller");
        Self {
            handler,
            root_path,
            socket_path,
            socket_identity: bound.identity,
            listeners: vec![bound.listener],
            owns_root,
        }
    }

    async fn rebind(&mut self) {
        let rebound = self
            .handler
            .rebind_unix_socket(&self.socket_path, self.socket_identity)
            .await
            .expect("rebind fixture Unix socket");
        self.socket_identity = rebound.identity;
        self.listeners.push(rebound.listener);
    }

    fn assert_modes(&self, directory: u32, socket: u32) {
        let parent = self.socket_path.parent().expect("fixture socket parent");
        assert_eq!(
            fs::metadata(parent)
                .expect("stat fixture socket parent")
                .permissions()
                .mode()
                & 0o777,
            directory
        );
        assert_eq!(
            fs::symlink_metadata(&self.socket_path)
                .expect("stat fixture socket")
                .permissions()
                .mode()
                & 0o777,
            socket
        );
    }

    fn assert_private_chain_modes(&self, expected: u32, depth: usize) {
        let mut current = self.socket_path.parent().expect("fixture socket parent");
        for _ in 0..depth {
            assert_eq!(
                fs::metadata(current)
                    .expect("stat fixture private directory")
                    .permissions()
                    .mode()
                    & 0o777,
                expected,
                "unexpected mode for {}",
                current.display()
            );
            current = current.parent().expect("fixture private directory parent");
        }
    }
}

impl Drop for UnixTransportFixture {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.socket_path);
        if self.owns_root {
            let _ = fs::remove_dir_all(&self.root_path);
        }
    }
}

#[tokio::test]
async fn add_read_only_write_and_last_deny_update_transport_atomically() {
    let fixture = UnixTransportFixture::new("matrix");
    let user = delegated_users(1).remove(0);
    fixture.assert_modes(OWNER_ONLY_DIRECTORY_MODE, OWNER_ONLY_SOCKET_MODE);

    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Add).await,
        "add delegated user",
    );
    fixture.assert_modes(SHARED_DIRECTORY_MODE, SHARED_SOCKET_MODE);
    let unlisted_uid = crate::server_access::current_owner_uid()
        .wrapping_add(50_000)
        .max(1);
    assert_eq!(
        fixture.handler.access_mode_for_peer(&PeerIdentity {
            pid: 0,
            uid: unlisted_uid,
            user: UserIdentity::Uid(unlisted_uid),
        }),
        None,
        "world-connectable transport must remain protected by the application UID allowlist"
    );

    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::ReadOnly).await,
        "downgrade delegated user",
    );
    fixture.assert_modes(SHARED_DIRECTORY_MODE, SHARED_SOCKET_MODE);
    assert_eq!(
        peer_mode(&fixture.handler, &user),
        Some(AccessMode::ReadOnly)
    );

    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Write).await,
        "upgrade delegated user",
    );
    fixture.assert_modes(SHARED_DIRECTORY_MODE, SHARED_SOCKET_MODE);
    assert_eq!(
        peer_mode(&fixture.handler, &user),
        Some(AccessMode::ReadWrite)
    );

    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Deny).await,
        "deny last delegated user",
    );
    fixture.assert_modes(OWNER_ONLY_DIRECTORY_MODE, OWNER_ONLY_SOCKET_MODE);
    assert_eq!(peer_mode(&fixture.handler, &user), None);
}

#[tokio::test]
async fn deny_keeps_transport_shared_until_the_last_delegated_uid_is_removed() {
    let fixture = UnixTransportFixture::new("count");
    let mut users = delegated_users(2);
    if users.len() < 2 {
        return;
    }
    let second = users.pop().expect("second delegated user");
    let first = users.pop().expect("first delegated user");

    assert_success(
        mutate_access(&fixture.handler, &first, AccessMutation::Add).await,
        "add first delegated user",
    );
    assert_success(
        mutate_access(&fixture.handler, &second, AccessMutation::ReadOnly).await,
        "add second delegated user read-only",
    );
    assert_success(
        mutate_access(&fixture.handler, &first, AccessMutation::Deny).await,
        "deny first delegated user",
    );
    fixture.assert_modes(SHARED_DIRECTORY_MODE, SHARED_SOCKET_MODE);
    assert_eq!(peer_mode(&fixture.handler, &first), None);
    assert_eq!(
        peer_mode(&fixture.handler, &second),
        Some(AccessMode::ReadOnly)
    );

    assert_success(
        mutate_access(&fixture.handler, &second, AccessMutation::Deny).await,
        "deny second delegated user",
    );
    fixture.assert_modes(OWNER_ONLY_DIRECTORY_MODE, OWNER_ONLY_SOCKET_MODE);
}

#[tokio::test]
async fn widening_failure_rolls_back_socket_directory_and_memory_acl() {
    let fixture = UnixTransportFixture::new("widen");
    let user = delegated_users(1).remove(0);
    fixture
        .handler
        .fail_next_unix_transport_transition_for_test();

    assert!(matches!(
        mutate_access(&fixture.handler, &user, AccessMutation::Add).await,
        Response::Error(_)
    ));
    fixture.assert_modes(OWNER_ONLY_DIRECTORY_MODE, OWNER_ONLY_SOCKET_MODE);
    assert_eq!(peer_mode(&fixture.handler, &user), None);
}

#[tokio::test]
async fn narrowing_failure_still_revokes_and_leaves_the_transport_retryable() {
    let fixture = UnixTransportFixture::new("narrow");
    let user = delegated_users(1).remove(0);
    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Add).await,
        "add delegated user before injected deny failure",
    );
    fixture
        .handler
        .fail_next_unix_transport_transition_for_test();

    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Deny).await,
        "deny must not depend on narrowing the filesystem transport",
    );
    assert_eq!(peer_mode(&fixture.handler, &user), None);
    fixture.assert_modes(SHARED_DIRECTORY_MODE, SHARED_SOCKET_MODE);

    fixture
        .handler
        .restore_owner_only_unix_transport()
        .await
        .expect("the transport narrowing stays retryable");
    fixture.assert_modes(OWNER_ONLY_DIRECTORY_MODE, OWNER_ONLY_SOCKET_MODE);
}

#[tokio::test]
async fn drifted_transport_blocks_upgrades_but_never_a_read_only_downgrade() {
    let fixture = UnixTransportFixture::new("drift");
    let user = delegated_users(1).remove(0);
    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Add).await,
        "add delegated user before transport drift",
    );
    fs::set_permissions(
        &fixture.socket_path,
        fs::Permissions::from_mode(OWNER_ONLY_SOCKET_MODE),
    )
    .expect("simulate external socket mode drift");

    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::ReadOnly).await,
        "a read-only downgrade must not depend on the filesystem transport",
    );
    assert_eq!(
        peer_mode(&fixture.handler, &user),
        Some(AccessMode::ReadOnly)
    );
    assert!(matches!(
        mutate_access(&fixture.handler, &user, AccessMutation::Write).await,
        Response::Error(_)
    ));
    assert_eq!(
        peer_mode(&fixture.handler, &user),
        Some(AccessMode::ReadOnly),
        "failed transport validation must roll back an upgrade"
    );

    fs::set_permissions(
        &fixture.socket_path,
        fs::Permissions::from_mode(SHARED_SOCKET_MODE),
    )
    .expect("restore fixture socket mode");
}

#[tokio::test]
async fn rebind_preserves_shared_transport_policy_and_subsequent_deny_restores_private_modes() {
    let mut fixture = UnixTransportFixture::new("rebind");
    let user = delegated_users(1).remove(0);
    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Add).await,
        "add delegated user before rebind",
    );

    fixture.rebind().await;
    fixture.assert_modes(SHARED_DIRECTORY_MODE, SHARED_SOCKET_MODE);
    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Deny).await,
        "deny delegated user after rebind",
    );
    fixture.assert_modes(OWNER_ONLY_DIRECTORY_MODE, OWNER_ONLY_SOCKET_MODE);
}

#[tokio::test]
async fn rebind_recreates_a_removed_socket_without_losing_shared_transport_policy() {
    let mut fixture = UnixTransportFixture::new("missing-rebind");
    let user = delegated_users(1).remove(0);
    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Add).await,
        "add delegated user before removing the socket",
    );

    fs::remove_file(&fixture.socket_path).expect("remove fixture Unix socket");
    fixture.rebind().await;

    fixture.assert_modes(SHARED_DIRECTORY_MODE, SHARED_SOCKET_MODE);
    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Deny).await,
        "deny delegated user after recreating the socket",
    );
    fixture.assert_modes(OWNER_ONLY_DIRECTORY_MODE, OWNER_ONLY_SOCKET_MODE);
}

#[tokio::test]
async fn rebind_still_rejects_an_existing_socket_with_permission_drift() {
    let fixture = UnixTransportFixture::new("rebind-drift");
    let user = delegated_users(1).remove(0);
    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Add).await,
        "add delegated user before drifting the socket permissions",
    );
    fs::set_permissions(
        &fixture.socket_path,
        fs::Permissions::from_mode(OWNER_ONLY_SOCKET_MODE),
    )
    .expect("simulate external socket permission drift");

    assert!(
        fixture
            .handler
            .rebind_unix_socket(&fixture.socket_path, fixture.socket_identity)
            .await
            .is_err(),
        "an existing socket with permission drift must not be rebound"
    );

    fs::set_permissions(
        &fixture.socket_path,
        fs::Permissions::from_mode(SHARED_SOCKET_MODE),
    )
    .expect("restore fixture socket permissions");
}

#[tokio::test]
async fn r3407_rebind_after_a_sibling_closed_the_directory_reopens_it() {
    // The nested directory proves that the rebind reopens every managed
    // directory a sibling closed, not only `rmux-UID`.
    let mut fixture = UnixTransportFixture::with_private_depth("r3407-m11", 2);
    let user = delegated_users(1).remove(0);
    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Add).await,
        "add on the first server",
    );
    let sibling = fixture.sibling("b");
    assert_success(
        mutate_access(&sibling.handler, &user, AccessMutation::Add).await,
        "add on the sibling",
    );
    fs::remove_file(&fixture.socket_path).expect("remove the first server's socket");
    assert_success(
        mutate_access(&sibling.handler, &user, AccessMutation::Deny).await,
        "deny on the sibling while the first socket is missing",
    );
    sibling.assert_private_chain_modes(OWNER_ONLY_DIRECTORY_MODE, 2);

    fixture.rebind().await;
    fixture.assert_private_chain_modes(SHARED_DIRECTORY_MODE, 2);
    fixture.assert_modes(SHARED_DIRECTORY_MODE, SHARED_SOCKET_MODE);
    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Deny).await,
        "deny on the rebound server",
    );
    fixture.assert_private_chain_modes(OWNER_ONLY_DIRECTORY_MODE, 2);
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn rebind_reopens_the_shared_directory_only_under_the_managed_lock() {
    // `rmux-UID` itself holds the socket here. The test holds the managed lock
    // the way a narrowing sibling controller does: the rebind must not change
    // the shared directory before that lock is released, then reopen it itself.
    let mut fixture = UnixTransportFixture::new("rebind-lock");
    let user = delegated_users(1).remove(0);
    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Add).await,
        "add on the first server",
    );
    let sibling = fixture.sibling("b");
    assert_success(
        mutate_access(&sibling.handler, &user, AccessMutation::Add).await,
        "add on the sibling",
    );
    fs::remove_file(&fixture.socket_path).expect("remove the first server's socket");
    assert_success(
        mutate_access(&sibling.handler, &user, AccessMutation::Deny).await,
        "deny on the sibling while the first socket is missing",
    );
    let managed = fixture
        .socket_path
        .parent()
        .expect("fixture socket parent")
        .to_path_buf();
    assert_eq!(mode_of(&managed), OWNER_ONLY_DIRECTORY_MODE);

    let sibling_lock = fs::File::open(&managed).expect("open the managed directory");
    rustix::fs::flock(&sibling_lock, rustix::fs::FlockOperation::LockExclusive)
        .expect("hold the managed lock as a sibling controller");
    let socket_path = fixture.socket_path.clone();
    let rebinding = tokio::spawn(async move {
        fixture.rebind().await;
        fixture
    });
    // The rebound socket reaches its shared mode only after the parent
    // directory was prepared, so every pre-adoption mode change is done.
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    while fs::symlink_metadata(&socket_path)
        .map(|metadata| metadata.permissions().mode() & 0o777)
        .ok()
        != Some(SHARED_SOCKET_MODE)
    {
        assert!(
            std::time::Instant::now() < deadline,
            "the rebound socket never reached its shared mode"
        );
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }
    assert_eq!(
        mode_of(&managed),
        OWNER_ONLY_DIRECTORY_MODE,
        "the rebind changed the shared directory while a sibling held the managed lock"
    );
    drop(sibling_lock);

    let fixture = rebinding.await.expect("rebind task");
    fixture.assert_modes(SHARED_DIRECTORY_MODE, SHARED_SOCKET_MODE);
    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Deny).await,
        "deny on the rebound server",
    );
    fixture.assert_modes(OWNER_ONLY_DIRECTORY_MODE, OWNER_ONLY_SOCKET_MODE);
}

fn mode_of(path: &std::path::Path) -> u32 {
    fs::metadata(path)
        .expect("stat fixture directory")
        .permissions()
        .mode()
        & 0o777
}

#[tokio::test]
async fn rebind_of_a_missing_socket_still_rejects_directory_drift_beyond_traverse_bits() {
    let fixture = UnixTransportFixture::with_private_depth("rebind-dir-drift", 2);
    let user = delegated_users(1).remove(0);
    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Add).await,
        "add delegated user before drifting the directory",
    );
    fs::remove_file(&fixture.socket_path).expect("remove fixture Unix socket");
    let parent = fixture.socket_path.parent().expect("fixture socket parent");
    fs::set_permissions(parent, fs::Permissions::from_mode(0o751))
        .expect("simulate group-readable directory drift");

    assert!(
        fixture
            .handler
            .rebind_unix_socket(&fixture.socket_path, fixture.socket_identity)
            .await
            .is_err(),
        "a sibling never grants group read, so this drift must block the rebind"
    );
    assert!(!fixture.socket_path.exists(), "no socket may be bound");
}

#[tokio::test]
async fn nested_private_socket_directories_open_and_restore_as_one_transport_policy() {
    let fixture = UnixTransportFixture::with_private_depth("nested", 3);
    let user = delegated_users(1).remove(0);
    fixture.assert_private_chain_modes(OWNER_ONLY_DIRECTORY_MODE, 3);

    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Add).await,
        "add delegated user through nested private directories",
    );
    fixture.assert_private_chain_modes(SHARED_DIRECTORY_MODE, 3);
    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Deny).await,
        "deny delegated user through nested private directories",
    );
    fixture.assert_private_chain_modes(OWNER_ONLY_DIRECTORY_MODE, 3);
}

#[tokio::test]
async fn private_custom_ancestor_rejects_delegation_without_changing_acl_or_modes() {
    let unique = NEXT_SOCKET.fetch_add(1, Ordering::Relaxed);
    let root_path = PathBuf::from(format!("/tmp/rmacustom{}{}", std::process::id(), unique));
    fs::create_dir(&root_path).expect("create custom fixture root");
    fs::set_permissions(
        &root_path,
        fs::Permissions::from_mode(SHARED_DIRECTORY_MODE),
    )
    .expect("make custom fixture root traversable");
    let private_ancestor = root_path.join("private");
    let socket_parent = private_ancestor.join("socket-parent");
    let socket_path = socket_parent.join("s");
    let bound = bind_unix_listener_at(&socket_path).expect("bind custom fixture Unix socket");
    let handler = RequestHandler::new();
    handler
        .install_unix_socket_access_for_test(&socket_path, bound.identity)
        .expect("install custom fixture Unix transport controller");
    let user = delegated_users(1).remove(0);

    assert!(matches!(
        mutate_access(&handler, &user, AccessMutation::Add).await,
        Response::Error(_)
    ));
    assert_eq!(peer_mode(&handler, &user), None);
    for directory in [&private_ancestor, &socket_parent] {
        assert_eq!(
            fs::metadata(directory)
                .expect("stat private custom directory")
                .permissions()
                .mode()
                & 0o777,
            OWNER_ONLY_DIRECTORY_MODE
        );
    }
    assert_eq!(
        fs::symlink_metadata(&socket_path)
            .expect("stat custom fixture socket")
            .permissions()
            .mode()
            & 0o777,
        OWNER_ONLY_SOCKET_MODE
    );

    drop(bound.listener);
    let _ = fs::remove_file(socket_path);
    let _ = fs::remove_dir_all(root_path);
}

#[tokio::test]
async fn writable_custom_ancestor_allows_owner_only_start_but_rejects_delegation() {
    let unique = NEXT_SOCKET.fetch_add(1, Ordering::Relaxed);
    let root_path = PathBuf::from(format!("/tmp/rmawritable{}{}", std::process::id(), unique));
    fs::create_dir(&root_path).expect("create writable custom fixture root");
    fs::set_permissions(&root_path, fs::Permissions::from_mode(0o775))
        .expect("make custom fixture root group writable");
    let socket_path = root_path.join("s");
    let bound = bind_unix_listener_at(&socket_path).expect("bind custom fixture Unix socket");
    let handler = RequestHandler::new();
    handler
        .install_unix_socket_access_for_test(&socket_path, bound.identity)
        .expect("owner-only transport accepts a tmux-compatible custom parent");
    let user = delegated_users(1).remove(0);

    assert!(matches!(
        mutate_access(&handler, &user, AccessMutation::Add).await,
        Response::Error(_)
    ));
    assert_eq!(peer_mode(&handler, &user), None);
    assert_eq!(
        fs::metadata(&root_path)
            .expect("stat writable custom directory")
            .permissions()
            .mode()
            & 0o777,
        0o775
    );
    assert_eq!(
        fs::symlink_metadata(&socket_path)
            .expect("stat custom fixture socket")
            .permissions()
            .mode()
            & 0o777,
        OWNER_ONLY_SOCKET_MODE
    );

    drop(bound.listener);
    let _ = fs::remove_file(socket_path);
    let _ = fs::remove_dir_all(root_path);
}

#[tokio::test]
async fn clean_shutdown_restore_returns_shared_transport_to_owner_only() {
    let fixture = UnixTransportFixture::new("shutdown");
    let user = delegated_users(1).remove(0);
    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Add).await,
        "add delegated user before shutdown restore",
    );
    fixture.assert_modes(SHARED_DIRECTORY_MODE, SHARED_SOCKET_MODE);

    fixture
        .handler
        .restore_owner_only_unix_transport()
        .await
        .expect("restore owner-only transport during shutdown");
    fixture.assert_modes(OWNER_ONLY_DIRECTORY_MODE, OWNER_ONLY_SOCKET_MODE);
}

#[test]
fn shared_socket_mode_allows_a_second_uid_in_the_socket_owner_group() {
    let socket_owner_uid = 1_001;
    let socket_owner_gid = 77;
    let second_uid = 2_002;
    let second_primary_gid = socket_owner_gid;

    assert_eq!(
        effective_permission_bits(
            SHARED_SOCKET_MODE,
            socket_owner_uid,
            socket_owner_gid,
            second_uid,
            second_primary_gid,
        ),
        0o6,
        "a peer matching the socket group must receive read-write connect bits"
    );
}

#[tokio::test]
async fn deny_after_a_sibling_server_starts_revokes_and_disconnects_the_user() {
    let fixture = UnixTransportFixture::new("sibling-deny");
    let user = delegated_users(1).remove(0);
    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Add).await,
        "add delegated user",
    );
    let (mut attach_rx, mut control_rx) = attach_and_control_clients(&fixture.handler, &user).await;

    let _sibling = fixture.sibling("b");

    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Deny).await,
        "deny after a sibling server started",
    );
    assert_eq!(peer_mode(&fixture.handler, &user), None);
    assert!(
        std::iter::from_fn(|| attach_rx.try_recv().ok()).any(|control| control.is_terminal()),
        "the revoked attached client must be told to leave"
    );
    assert!(
        std::iter::from_fn(|| control_rx.try_recv().ok())
            .any(|event| matches!(event, ControlServerEvent::Exit(_))),
        "the revoked control client must exit"
    );
}

#[tokio::test]
async fn sibling_start_keeps_the_shared_directory_open_for_delegated_users() {
    let fixture = UnixTransportFixture::new("sibling-open");
    let user = delegated_users(1).remove(0);
    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Add).await,
        "add delegated user",
    );

    let sibling = fixture.sibling("b");
    fixture.assert_modes(SHARED_DIRECTORY_MODE, SHARED_SOCKET_MODE);
    sibling.assert_modes(SHARED_DIRECTORY_MODE, OWNER_ONLY_SOCKET_MODE);

    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::ReadOnly).await,
        "downgrade after a sibling server started",
    );
    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Write).await,
        "a grant after a sibling server started",
    );
    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Deny).await,
        "deny the last delegated user",
    );
    fixture.assert_modes(OWNER_ONLY_DIRECTORY_MODE, OWNER_ONLY_SOCKET_MODE);
    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Add).await,
        "add again next to a sibling server",
    );
    fixture.assert_modes(SHARED_DIRECTORY_MODE, SHARED_SOCKET_MODE);
}

#[tokio::test]
async fn narrowing_keeps_the_directory_open_while_a_sibling_still_delegates() {
    let fixture = UnixTransportFixture::new("sibling-keep");
    let user = delegated_users(1).remove(0);
    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Add).await,
        "add delegated user on the first server",
    );
    // The sibling starts while the shared directory is already open, so its
    // own private policy must not adopt the first server's traverse bits.
    let sibling = fixture.sibling("b");
    assert_success(
        mutate_access(&sibling.handler, &user, AccessMutation::Add).await,
        "add delegated user on the sibling server",
    );

    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Deny).await,
        "deny on the first server",
    );
    fixture.assert_modes(SHARED_DIRECTORY_MODE, OWNER_ONLY_SOCKET_MODE);
    sibling.assert_modes(SHARED_DIRECTORY_MODE, SHARED_SOCKET_MODE);
    fixture
        .handler
        .restore_owner_only_unix_transport()
        .await
        .expect("the first server shuts down");
    sibling.assert_modes(SHARED_DIRECTORY_MODE, SHARED_SOCKET_MODE);

    // A restriction stands even on a broken transport, so only a grant
    // proves that the sibling's shared transport is still valid.
    assert_success(
        mutate_access(&sibling.handler, &user, AccessMutation::ReadOnly).await,
        "downgrade on the sibling server",
    );
    assert_success(
        mutate_access(&sibling.handler, &user, AccessMutation::Write).await,
        "the sibling keeps a valid shared transport",
    );
    assert_success(
        mutate_access(&sibling.handler, &user, AccessMutation::Deny).await,
        "deny on the sibling server",
    );
    sibling.assert_modes(OWNER_ONLY_DIRECTORY_MODE, OWNER_ONLY_SOCKET_MODE);
}

async fn attach_and_control_clients(
    handler: &RequestHandler,
    user: &ResolvedUser,
) -> (
    mpsc::UnboundedReceiver<AttachControl>,
    mpsc::Receiver<ControlServerEvent>,
) {
    let session_name = SessionName::new("revoked").expect("valid session name");
    let response = handler
        .handle(Request::NewSession(NewSessionRequest {
            session_name: session_name.clone(),
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }))
        .await;
    crate::session_fixture::assert_new_session(&response);
    let admission = handler
        .server_access_admission_for_peer(&peer(user.uid))
        .expect("delegated user has an admission");

    let (attach_tx, attach_rx) = mpsc::unbounded_channel();
    handler
        .register_attach_identity_with_server_access(
            925_001,
            session_name,
            None,
            AttachRegistration {
                control_tx: attach_tx,
                control_backlog: Arc::new(AtomicUsize::new(0)),
                closing: Arc::new(AtomicBool::new(false)),
                persistent_overlay_epoch: Arc::new(AtomicU64::new(0)),
                terminal_context: OuterTerminalContext::default(),
                client_title: None,
                flags: ClientFlags::default(),
                render_stream: false,
                uid: user.uid,
                user: UserIdentity::Uid(user.uid),
                can_write: true,
                client_size: None,
            },
            admission.clone(),
        )
        .await
        .expect("delegated attach client is published");

    let (event_tx, control_rx) = mpsc::channel(8);
    handler
        .register_control_with_server_access(
            925_002,
            ControlModeUpgrade {
                initial_command_count: 0,
                mode: ControlMode::Plain,
                terminal_context: OuterTerminalContext::default(),
            },
            ControlRegistration {
                event_tx,
                closing: Arc::new(AtomicBool::new(false)),
                uid: user.uid,
                user: UserIdentity::Uid(user.uid),
                can_write: true,
            },
            admission,
        )
        .await
        .expect("delegated control client is published");
    (attach_rx, control_rx)
}

fn peer(uid: u32) -> PeerIdentity {
    PeerIdentity {
        pid: 0,
        uid,
        user: UserIdentity::Uid(uid),
    }
}

#[derive(Clone, Copy)]
enum AccessMutation {
    Add,
    Deny,
    ReadOnly,
    Write,
}

async fn mutate_access(
    handler: &RequestHandler,
    user: &ResolvedUser,
    mutation: AccessMutation,
) -> Response {
    handler
        .handle_server_access(ServerAccessRequest {
            add: matches!(mutation, AccessMutation::Add),
            deny: matches!(mutation, AccessMutation::Deny),
            list: false,
            read_only: matches!(mutation, AccessMutation::ReadOnly),
            write: matches!(mutation, AccessMutation::Write),
            target: None,
            user: Some(user.name.clone()),
        })
        .await
}

fn assert_success(response: Response, operation: &str) {
    assert!(
        matches!(response, Response::ServerAccess(_)),
        "{operation} failed: {response:?}"
    );
}

fn peer_mode(handler: &RequestHandler, user: &ResolvedUser) -> Option<AccessMode> {
    handler.access_mode_for_peer(&PeerIdentity {
        pid: 0,
        uid: user.uid,
        user: UserIdentity::Uid(user.uid),
    })
}

fn delegated_users(limit: usize) -> Vec<ResolvedUser> {
    let owner_uid = crate::server_access::current_owner_uid();
    let mut users = ["nobody", "daemon", "_daemon", "www-data", "_www"]
        .iter()
        .filter_map(|candidate| resolve_user(candidate).ok())
        .filter(|user| user.uid != 0 && user.uid != owner_uid)
        .collect::<Vec<_>>();
    users.sort_by_key(|user| user.uid);
    users.dedup_by_key(|user| user.uid);
    users.truncate(limit);
    assert!(
        !users.is_empty(),
        "the Unix transport tests require at least one non-owner account"
    );
    users
}

fn effective_permission_bits(
    mode: u32,
    owner_uid: u32,
    owner_gid: u32,
    peer_uid: u32,
    peer_gid: u32,
) -> u32 {
    if peer_uid == owner_uid {
        (mode >> 6) & 0o7
    } else if peer_gid == owner_gid {
        (mode >> 3) & 0o7
    } else {
        mode & 0o7
    }
}
