use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use rmux_ipc::{LocalListener, PeerIdentity};
use rmux_os::identity::UserIdentity;
use rmux_proto::{Response, ServerAccessRequest};

use super::RequestHandler;
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
        let socket_path = parent.join("s");
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
        let _ = fs::remove_dir_all(&self.root_path);
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
async fn narrowing_failure_restores_shared_transport_and_memory_acl() {
    let fixture = UnixTransportFixture::new("narrow");
    let user = delegated_users(1).remove(0);
    assert_success(
        mutate_access(&fixture.handler, &user, AccessMutation::Add).await,
        "add delegated user before injected deny failure",
    );
    fixture
        .handler
        .fail_next_unix_transport_transition_for_test();

    assert!(matches!(
        mutate_access(&fixture.handler, &user, AccessMutation::Deny).await,
        Response::Error(_)
    ));
    fixture.assert_modes(SHARED_DIRECTORY_MODE, SHARED_SOCKET_MODE);
    assert_eq!(
        peer_mode(&fixture.handler, &user),
        Some(AccessMode::ReadWrite)
    );
}

#[tokio::test]
async fn read_only_update_rolls_back_when_the_shared_transport_has_drifted() {
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

    assert!(matches!(
        mutate_access(&fixture.handler, &user, AccessMutation::ReadOnly).await,
        Response::Error(_)
    ));
    assert_eq!(
        peer_mode(&fixture.handler, &user),
        Some(AccessMode::ReadWrite),
        "failed transport validation must roll back the application ACL update"
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
