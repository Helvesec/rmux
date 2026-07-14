use super::*;

use rmux_proto::{
    encode_frame, CreateSessionLeaseResponse, ErrorResponse, FrameDecoder, HandshakeResponse,
    KillSessionResponse, NewSessionResponse, ReleaseSessionLeaseResponse,
    RenewSessionLeaseResponse, CAPABILITY_SDK_OWNED_SESSION_STABLE_IDENTITY,
    CAPABILITY_SDK_SESSION_LEASE_BY_ID, RMUX_WIRE_VERSION, SUPPORTED_CAPABILITIES,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

struct FakeDaemon {
    stream: tokio::io::DuplexStream,
    decoder: FrameDecoder,
}

impl FakeDaemon {
    fn new(stream: tokio::io::DuplexStream) -> Self {
        Self {
            stream,
            decoder: FrameDecoder::new(),
        }
    }

    async fn read_request(&mut self) -> Request {
        let mut buffer = [0_u8; 4096];
        loop {
            if let Some(request) = self
                .decoder
                .next_frame::<Request>()
                .expect("request frame decodes")
            {
                return request;
            }
            let read = self
                .stream
                .read(&mut buffer)
                .await
                .expect("read request bytes");
            assert_ne!(read, 0, "SDK transport closed before request arrived");
            self.decoder.push_bytes(&buffer[..read]);
        }
    }

    async fn write_response(&mut self, response: Response) {
        let frame = encode_frame(&response).expect("response encodes");
        self.stream
            .write_all(&frame)
            .await
            .expect("write response bytes");
        self.stream.flush().await.expect("flush response bytes");
    }

    async fn assert_no_follow_up_request(&mut self) {
        let mut buffer = [0_u8; 4096];
        match tokio::time::timeout(Duration::from_millis(250), self.stream.read(&mut buffer)).await
        {
            Err(_) | Ok(Ok(0)) => {}
            Ok(Ok(read)) => {
                panic!("daemon received {read} unexpected request bytes after capability rejection")
            }
            Ok(Err(error)) => panic!("failed while checking for an unexpected request: {error}"),
        }
    }
}

#[cfg(any(unix, windows))]
#[test]
fn failed_signal_install_can_retry_then_detach_without_killing_the_session() {
    let (runtime, owned, mut daemon, installed) = signal_install_fixture("retry-signal-owner");

    assert_signal_install_fails_without_latching(&owned, &installed);

    let guard = runtime
        .block_on(async { owned.install_default_signal_handlers() })
        .expect("installation can be retried inside a Tokio runtime");
    let duplicate = runtime
        .block_on(async { owned.install_default_signal_handlers() })
        .expect_err("an installed guard must keep the uniqueness reservation");
    assert!(
        duplicate.to_string().contains("already installed"),
        "unexpected duplicate-install error: {duplicate}"
    );
    drop(guard);
    let detached = runtime
        .block_on(owned.detach_owned())
        .expect("dropping the retried guard permits detaching ownership");

    runtime.block_on(async {
        assert!(
            tokio::time::timeout(Duration::from_millis(250), daemon.read_request())
                .await
                .is_err(),
            "detach after a recovered installation must not kill the session"
        );
    });
    drop(detached);
}

#[cfg(any(unix, windows))]
#[test]
fn failed_signal_install_can_preserve_without_killing_the_session() {
    let (runtime, owned, mut daemon, installed) = signal_install_fixture("preserve-signal-owner");

    assert_signal_install_fails_without_latching(&owned, &installed);

    let preserved = runtime
        .block_on(owned.preserve())
        .expect("failed installation must not block preserving ownership");
    let keepalive = preserved.session().transport().clone();
    drop(preserved);

    runtime.block_on(async {
        assert!(
            tokio::time::timeout(Duration::from_millis(250), daemon.read_request())
                .await
                .is_err(),
            "preserve after a failed installation must disarm Drop cleanup"
        );
    });
    drop(keepalive);
}

#[cfg(any(unix, windows))]
fn signal_install_fixture(
    name: &str,
) -> (
    tokio::runtime::Runtime,
    OwnedSession,
    FakeDaemon,
    Arc<AtomicBool>,
) {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime builds");
    let (owned, daemon, installed) = runtime.block_on(async {
        let (client_stream, server_stream) = tokio::io::duplex(1024);
        let installed = Arc::new(AtomicBool::new(false));
        let owned = OwnedSession {
            session: Some(Session::new(
                SessionName::new(name).expect("valid session name"),
                crate::RmuxEndpoint::Default,
                None,
                TransportClient::spawn(client_stream),
                true,
                None,
            )),
            session_id: SessionId::new(42),
            cleanup_policy: CleanupPolicy::KillOnDrop,
            lease: None,
            signal_handlers_installed: Arc::clone(&installed),
        };
        (owned, FakeDaemon::new(server_stream), installed)
    });
    (runtime, owned, daemon, installed)
}

#[cfg(any(unix, windows))]
fn assert_signal_install_fails_without_latching(owned: &OwnedSession, installed: &AtomicBool) {
    let error = owned
        .install_default_signal_handlers()
        .expect_err("installation outside a Tokio runtime must fail");
    assert!(
        error.to_string().contains("require a Tokio runtime"),
        "unexpected installation error: {error}"
    );
    assert!(
        !installed.load(Ordering::Acquire),
        "failed installation must release the single-handler reservation"
    );
}

#[tokio::test]
async fn released_owner_rejects_signal_handlers_without_latching_installation() {
    let (client_stream, _server_stream) = tokio::io::duplex(1024);
    let installed = Arc::new(AtomicBool::new(false));
    let owned = OwnedSession {
        session: Some(Session::new(
            SessionName::new("preserved-owner").expect("valid session name"),
            crate::RmuxEndpoint::Default,
            None,
            TransportClient::spawn(client_stream),
            true,
            None,
        )),
        session_id: SessionId::new(42),
        cleanup_policy: CleanupPolicy::Preserve,
        lease: None,
        signal_handlers_installed: Arc::clone(&installed),
    };

    let error = owned
        .install_default_signal_handlers()
        .expect_err("released ownership cannot install token-guarded signal cleanup");

    assert!(
        error
            .to_string()
            .contains("owned session ownership has already been released"),
        "unexpected error: {error}"
    );
    assert!(
        !installed.load(Ordering::Acquire),
        "rejected installation must not latch the single-handler flag"
    );
}

#[tokio::test]
async fn preserve_rejects_an_already_installed_stable_identity_signal_cleanup() {
    let (client_stream, _server_stream) = tokio::io::duplex(1024);
    let owned = OwnedSession {
        session: Some(Session::new(
            SessionName::new("preserved-signal-owner").expect("valid session name"),
            crate::RmuxEndpoint::Default,
            None,
            TransportClient::spawn(client_stream),
            true,
            None,
        )),
        session_id: SessionId::new(43),
        cleanup_policy: CleanupPolicy::KillOnDrop,
        lease: None,
        signal_handlers_installed: Arc::new(AtomicBool::new(true)),
    };

    let error = owned
        .preserve()
        .await
        .expect_err("an active signal guard must block the preserve handoff");

    assert!(
        error
            .to_string()
            .contains("drop owned-session signal handlers before preserving or detaching"),
        "unexpected error: {error}"
    );
}

#[tokio::test]
async fn current_stable_identity_capability_allows_kill_on_drop_creation_and_cleanup() {
    let (builder, mut daemon, session_name) = start_owned_builder(CleanupPolicy::KillOnDrop).await;
    answer_new_session(&mut daemon, session_name).await;

    let owned = builder
        .await
        .expect("builder task joins")
        .expect("kill-on-drop owner builds directly from new-session response");
    drop(owned);

    let Request::KillSession(cleanup) = daemon.read_request().await else {
        panic!("kill-on-drop must not insert a persistent claim RPC after creation");
    };
    assert_eq!(cleanup.target.as_str(), "$42");
    daemon
        .write_response(Response::KillSession(KillSessionResponse { existed: true }))
        .await;
}

#[tokio::test]
async fn legacy_wire_peer_is_rejected_before_owned_session_mutation_for_every_policy() {
    for cleanup_policy in [
        CleanupPolicy::KillOnDrop,
        CleanupPolicy::KillOnOwnerExit,
        CleanupPolicy::Preserve,
    ] {
        let capabilities = SUPPORTED_CAPABILITIES
            .iter()
            .copied()
            .filter(|capability| *capability != CAPABILITY_SDK_OWNED_SESSION_STABLE_IDENTITY)
            .map(str::to_owned)
            .collect();
        let (builder, mut daemon, _) =
            start_owned_builder_with_capabilities_and_replace(cleanup_policy, capabilities, true)
                .await;

        let error = builder
            .await
            .expect("builder task joins")
            .expect_err("legacy wire peer must not construct an owned session");
        assert!(
            error
                .to_string()
                .contains(CAPABILITY_SDK_OWNED_SESSION_STABLE_IDENTITY),
            "unexpected missing-capability error for {cleanup_policy:?}: {error}"
        );
        daemon.assert_no_follow_up_request().await;
    }
}

#[tokio::test]
async fn owner_exit_uses_existing_bounded_lease_with_nominal_wire_address() {
    let (builder, mut daemon, session_name) =
        start_owned_builder(CleanupPolicy::KillOnOwnerExit).await;
    answer_new_session(&mut daemon, session_name.clone()).await;

    let Request::CreateSessionLease(lease) = daemon.read_request().await else {
        panic!("owner-exit must use the existing bounded lease endpoint");
    };
    assert_eq!(lease.session_name, session_name);
    assert_eq!(lease.ttl_millis, 600);
    daemon
        .write_response(Response::CreateSessionLease(CreateSessionLeaseResponse {
            token: 7,
            ttl_millis: 600,
        }))
        .await;

    let owned = builder
        .await
        .expect("builder task joins")
        .expect("leased owner builds");
    drop(owned);

    let Request::KillSession(cleanup) = daemon.read_request().await else {
        panic!("owner-exit Drop must kill the stable identity, not a mutable name");
    };
    assert_eq!(cleanup.target.as_str(), "$42");
    daemon
        .write_response(Response::KillSession(KillSessionResponse { existed: true }))
        .await;
}

#[tokio::test]
async fn owner_exit_retains_nominal_wire_address_for_renew_and_release() {
    let capabilities = SUPPORTED_CAPABILITIES
        .iter()
        .copied()
        .map(str::to_owned)
        .collect();
    let (builder, mut daemon, session_name) =
        start_owned_builder_with_capabilities(CleanupPolicy::KillOnOwnerExit, capabilities).await;
    answer_new_session(&mut daemon, session_name.clone()).await;

    let Request::CreateSessionLease(lease) = daemon.read_request().await else {
        panic!("current daemon must receive the existing nominal lease request");
    };
    assert_eq!(lease.session_name, session_name);
    daemon
        .write_response(Response::CreateSessionLease(CreateSessionLeaseResponse {
            token: 9,
            ttl_millis: 600,
        }))
        .await;

    let owned = builder
        .await
        .expect("builder task joins")
        .expect("current lease capability must keep owner-exit usable");

    let Request::RenewSessionLease(renew) = daemon.read_request().await else {
        panic!("nominal lease address must be retained for heartbeat renewal");
    };
    assert_eq!(renew.session_name, session_name);
    daemon
        .write_response(Response::RenewSessionLease(RenewSessionLeaseResponse {
            renewed: true,
        }))
        .await;

    let preserve = tokio::spawn(async move { owned.preserve().await });
    let Request::ReleaseSessionLease(release) = daemon.read_request().await else {
        panic!("nominal lease address must be retained for ownership release");
    };
    assert_eq!(release.session_name, session_name);
    daemon
        .write_response(Response::ReleaseSessionLease(ReleaseSessionLeaseResponse {
            released: true,
        }))
        .await;
    let preserved = preserve
        .await
        .expect("preserve task joins")
        .expect("nominal lease release succeeds");
    drop(preserved);
}

#[tokio::test]
async fn owner_exit_honors_explicit_identity_lease_capability_from_compatible_daemon() {
    let mut capabilities = SUPPORTED_CAPABILITIES
        .iter()
        .map(|capability| (*capability).to_owned())
        .collect::<Vec<_>>();
    capabilities.push(CAPABILITY_SDK_SESSION_LEASE_BY_ID.to_owned());
    let (builder, mut daemon, session_name) =
        start_owned_builder_with_capabilities(CleanupPolicy::KillOnOwnerExit, capabilities).await;
    answer_new_session(&mut daemon, session_name).await;

    let Request::CreateSessionLease(lease) = daemon.read_request().await else {
        panic!("identity-capable daemon must receive the negotiated lease request");
    };
    assert_eq!(lease.session_name.as_str(), "$42");
    daemon
        .write_response(Response::CreateSessionLease(CreateSessionLeaseResponse {
            token: 11,
            ttl_millis: 600,
        }))
        .await;

    let owned = builder
        .await
        .expect("builder task joins")
        .expect("explicit identity-capable lease builds");
    drop(owned);
    let Request::KillSession(cleanup) = daemon.read_request().await else {
        panic!("owner-exit Drop must retain stable cleanup identity");
    };
    assert_eq!(cleanup.target.as_str(), "$42");
    daemon
        .write_response(Response::KillSession(KillSessionResponse { existed: true }))
        .await;
}

#[tokio::test]
async fn owner_exit_rolls_back_created_session_when_lease_creation_fails() {
    let (builder, mut daemon, session_name) =
        start_owned_builder(CleanupPolicy::KillOnOwnerExit).await;
    answer_new_session(&mut daemon, session_name.clone()).await;

    let Request::CreateSessionLease(lease) = daemon.read_request().await else {
        panic!("owner-exit must attempt its lease after session creation");
    };
    assert_eq!(lease.session_name, session_name);
    daemon
        .write_response(Response::Error(ErrorResponse {
            error: rmux_proto::RmuxError::Server("injected lease rejection".to_owned()),
        }))
        .await;

    let Request::KillSession(rollback) = daemon.read_request().await else {
        panic!("failed post-creation lease must trigger compensating cleanup");
    };
    assert_eq!(rollback.target.as_str(), "$42");
    daemon
        .write_response(Response::KillSession(KillSessionResponse { existed: true }))
        .await;

    let error = builder
        .await
        .expect("builder task joins")
        .expect_err("rejected lease must still fail owned-session construction");
    assert!(
        error.to_string().contains("injected lease rejection"),
        "rollback must preserve the source error: {error}"
    );
}

async fn start_owned_builder(
    cleanup_policy: CleanupPolicy,
) -> (
    tokio::task::JoinHandle<Result<OwnedSession>>,
    FakeDaemon,
    SessionName,
) {
    start_owned_builder_with_capabilities(
        cleanup_policy,
        SUPPORTED_CAPABILITIES
            .iter()
            .map(|capability| (*capability).to_owned())
            .collect(),
    )
    .await
}

async fn start_owned_builder_with_capabilities(
    cleanup_policy: CleanupPolicy,
    capabilities: Vec<String>,
) -> (
    tokio::task::JoinHandle<Result<OwnedSession>>,
    FakeDaemon,
    SessionName,
) {
    start_owned_builder_with_capabilities_and_replace(cleanup_policy, capabilities, false).await
}

async fn start_owned_builder_with_capabilities_and_replace(
    cleanup_policy: CleanupPolicy,
    capabilities: Vec<String>,
    replace_existing: bool,
) -> (
    tokio::task::JoinHandle<Result<OwnedSession>>,
    FakeDaemon,
    SessionName,
) {
    let (client_stream, server_stream) = tokio::io::duplex(8192);
    let rmux = Rmux::from_transport_for_test(TransportClient::spawn(client_stream), None);
    let session_name = SessionName::new("stable-owned-builder").expect("valid session name");
    let builder_session_name = session_name.clone();
    let builder = tokio::spawn(async move {
        rmux.owned_session(builder_session_name)
            .replace_existing(replace_existing)
            .cleanup_policy(cleanup_policy)
            .lease_ttl(Duration::from_millis(600))
            .await
    });
    let mut daemon = FakeDaemon::new(server_stream);

    assert!(matches!(daemon.read_request().await, Request::Handshake(_)));
    daemon
        .write_response(Response::Handshake(HandshakeResponse {
            wire_version: RMUX_WIRE_VERSION,
            capabilities,
        }))
        .await;

    (builder, daemon, session_name)
}

async fn answer_new_session(daemon: &mut FakeDaemon, session_name: SessionName) {
    let Request::NewSessionExt(request) = daemon.read_request().await else {
        panic!("owned-session builder must create the session after preflight");
    };
    assert_eq!(request.session_name.as_ref(), Some(&session_name));
    assert!(request.detached);
    assert!(request.print_session_info);
    assert_eq!(request.print_format.as_deref(), Some("#{session_id}"));
    daemon
        .write_response(Response::NewSession(NewSessionResponse {
            session_name,
            detached: true,
            output: Some(rmux_proto::CommandOutput::from_stdout(b"$42\n")),
        }))
        .await;
}
