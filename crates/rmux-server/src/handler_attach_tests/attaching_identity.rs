//! Issue #182: what an attaching client's `#{client_*}` bindings resolve to
//! when the requester pid does not identify one local process.
//!
//! `listener_attach_identity_tests` drives the whole funnel and proves the
//! frame the wire really carries. This pins the decision itself, including the
//! two outcomes that funnel cannot reach at once: an in-process dispatch, which
//! is legitimately the server owner's, and a pid whose open connections were
//! authenticated as different peers, which is nobody's.

use super::*;

use rmux_ipc::PeerIdentity;
use rmux_os::identity::UserIdentity;

use crate::server_access::AccessMode;

fn delegated_uids() -> [u32; 2] {
    // Advance through nonzero UIDs so neither delegated peer is the owner,
    // and the two peers remain distinct even when the owner's UID wraps.
    let first = crate::server_access::current_owner_uid()
        .wrapping_add(1)
        .max(1);
    let second = first.wrapping_add(1).max(1);
    [first, second]
}

fn peer(pid: u32, uid: u32) -> PeerIdentity {
    PeerIdentity {
        pid,
        uid,
        user: UserIdentity::Uid(uid),
    }
}

fn admit(
    handler: &RequestHandler,
    peer: &PeerIdentity,
) -> crate::server_access::ServerAccessAdmission {
    handler
        .set_test_access_mode_for_uid(peer.uid, AccessMode::ReadWrite)
        .expect("the delegated peer can be granted access");
    handler
        .server_access_admission_for_peer(peer)
        .expect("the granted peer is admitted")
}

/// The blocking finding: two connections authenticated as different peers can
/// share one numeric pid after the OS recycles it. Neither may lend the other
/// its identity, and substituting the server owner is not "closed" either —
/// registration would still store the peer the listener authenticated, so the
/// first frame and `list-clients` would disagree.
#[tokio::test]
async fn conflicting_authenticated_peers_fail_closed_rather_than_becoming_the_owner() {
    let handler = RequestHandler::new();
    let shared_pid = 60_190;
    let [first_uid, second_uid] = delegated_uids();
    let first = peer(shared_pid, first_uid);
    let second = peer(shared_pid, second_uid);
    let first_admission = admit(&handler, &first);
    let second_admission = admit(&handler, &second);
    let owner = handler.server_owner_identity();
    assert!(
        owner != first.user && owner != second.user,
        "this test only means something while neither peer is the server owner"
    );

    // No connection scope at all: an in-process dispatch stays the owner's.
    let (_, user) = handler
        .attaching_client_identity(shared_pid)
        .expect("an in-process dispatch resolves");
    assert_eq!(
        user, owner,
        "an unauthenticated requester is still the owner"
    );

    let first_scope = handler.begin_authenticated_peer_access(&first, first_admission);
    let (uid, user) = handler
        .attaching_client_identity(shared_pid)
        .expect("one authenticated peer resolves");
    assert_eq!(
        (uid, user),
        (first.uid, first.user.clone()),
        "a single authenticated connection still names its own peer"
    );

    let second_scope = handler.begin_authenticated_peer_access(&second, second_admission);
    let error = handler
        .attaching_client_identity(shared_pid)
        .expect_err("two peers on one pid cannot name one attaching client");
    assert!(
        matches!(&error, RmuxError::Server(message) if message.contains("ambiguous")),
        "the refusal must say why, got {error:?}"
    );

    // Once the reused pid belongs to one peer again, that peer is named.
    drop(second_scope);
    let (uid, user) = handler
        .attaching_client_identity(shared_pid)
        .expect("the surviving peer resolves");
    assert_eq!(
        (uid, user),
        (first.uid, first.user.clone()),
        "closing the colliding connection restores the survivor's identity"
    );
    drop(first_scope);
}

/// The same rule at the scope-set level, so the distinction between "no
/// authenticated peer" and "more than one" cannot silently collapse again.
#[tokio::test]
async fn a_pid_with_one_authenticated_peer_is_never_reported_as_conflicting() {
    let handler = RequestHandler::new();
    let shared_pid = 60_191;
    let [first_uid, _] = delegated_uids();
    let only = peer(shared_pid, first_uid);
    let admission = admit(&handler, &only);

    let outer = handler.begin_authenticated_peer_access(&only, admission);
    // A hook or shell running under that connection inherits its peer rather
    // than turning the requester ambiguous.
    let inner = handler
        .begin_inherited_detached_requester_access(shared_pid)
        .await;
    let (uid, user) = handler
        .attaching_client_identity(shared_pid)
        .expect("nested scopes of one peer resolve");
    assert_eq!(
        (uid, user),
        (only.uid, only.user.clone()),
        "a nested scope must not erase the peer it inherited"
    );
    drop(inner);
    drop(outer);
}

/// Attaches a fresh, not yet registered client to `session` and returns the
/// first frame the attach renders for it, before registration publishes it.
async fn first_attach_frame(
    handler: &RequestHandler,
    requester_pid: u32,
    session: &SessionName,
    read_only: bool,
    flags: Option<&str>,
) -> String {
    let outcome = handler
        .dispatch(
            requester_pid,
            Request::AttachSessionExt3(Box::new(AttachSessionExt3Request {
                target: Some(session.clone()),
                target_spec: None,
                detach_other_clients: false,
                kill_other_clients: false,
                read_only,
                skip_environment_update: true,
                flags: flags.map(|flag| vec![flag.to_owned()]),
                working_directory: None,
                client_terminal: rmux_proto::ClientTerminalContext::default(),
                client_size: Some(TerminalSize { cols: 80, rows: 24 }),
                attach_capabilities: Vec::new(),
            })),
        )
        .await;
    assert!(
        matches!(outcome.response, Response::AttachSession(_)),
        "the attach must succeed, got {:?}",
        outcome.response
    );
    String::from_utf8(outcome.attach.expect("attach upgrade").target.render_frame)
        .expect("render frame must be utf-8")
}

/// The attaching client's own flags drive `#{client_readonly}` in the frame it
/// is shown before registration, as tmux 3.7b sets `CLIENT_READONLY` in
/// `cmd_attach_session()` before that client is ever drawn. A record built
/// without its flags would show `attach -r` a status line that says `0` while
/// `list-clients` reports `1` for the same client.
#[tokio::test]
async fn the_first_attach_frame_reports_the_attaching_client_readonly_flag() {
    let handler = RequestHandler::new();
    let alpha = session_name("attaching-readonly");
    create_quiet_session(&handler, &alpha).await;
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::StatusLeft,
            value: "<RO=#{client_readonly}> ".to_owned(),
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");

    for (requester_pid, read_only, flags, expected) in [
        (60_192, false, None, "<RO=0>"),
        (60_193, true, None, "<RO=1>"),
        (60_194, false, Some("read-only"), "<RO=1>"),
    ] {
        let frame = first_attach_frame(&handler, requester_pid, &alpha, read_only, flags).await;
        assert!(
            frame.contains(expected),
            "attach read_only={read_only} flags={flags:?} must open on {expected}, got {frame:?}"
        );
    }
}
