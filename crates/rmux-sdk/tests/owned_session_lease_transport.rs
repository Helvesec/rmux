#![cfg(unix)]

use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rmux_proto::{
    encode_frame, CommandOutput, CreateSessionLeaseResponse, FrameDecoder, HandshakeResponse,
    HasSessionResponse, KillSessionResponse, NewSessionResponse, ReleaseSessionLeaseResponse,
    RenewSessionLeaseResponse, Request, Response, SessionName, RMUX_WIRE_VERSION,
    SUPPORTED_CAPABILITIES,
};
use rmux_sdk::{CleanupPolicy, Rmux, RmuxError};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{UnixListener, UnixStream};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

static NEXT_TEST_SOCKET_ID: AtomicU64 = AtomicU64::new(0);

struct TestSocket {
    directory: PathBuf,
    path: PathBuf,
}

impl TestSocket {
    fn new() -> io::Result<Self> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_nanos();
        let socket_id = NEXT_TEST_SOCKET_ID.fetch_add(1, Ordering::Relaxed);
        let directory = PathBuf::from("/tmp").join(format!(
            "rmux-sdk-lease-transport-{}-{nonce}-{socket_id}",
            std::process::id()
        ));
        std::fs::create_dir_all(&directory)?;
        let path = directory.join("daemon.sock");
        Ok(Self { directory, path })
    }

    fn path(&self) -> &Path {
        &self.path
    }
}

impl Drop for TestSocket {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.directory);
    }
}

struct Peer {
    stream: UnixStream,
    decoder: FrameDecoder,
}

impl Peer {
    fn new(stream: UnixStream) -> Self {
        Self {
            stream,
            decoder: FrameDecoder::new(),
        }
    }

    async fn request(&mut self) -> Request {
        let mut buffer = [0_u8; 1024];
        loop {
            if let Some(request) = self
                .decoder
                .next_frame::<Request>()
                .expect("SDK request frame decodes")
            {
                return request;
            }
            let read = self
                .stream
                .read(&mut buffer)
                .await
                .expect("read SDK request bytes");
            assert_ne!(read, 0, "SDK transport closed before the next request");
            self.decoder.push_bytes(&buffer[..read]);
        }
    }

    async fn respond(&mut self, response: Response) {
        let frame = encode_frame(&response).expect("SDK response encodes");
        self.stream
            .write_all(&frame)
            .await
            .expect("write SDK response bytes");
        self.stream.flush().await.expect("flush SDK response");
    }

    async fn handshake(&mut self) {
        assert!(matches!(self.request().await, Request::Handshake(_)));
        self.respond(Response::Handshake(HandshakeResponse {
            wire_version: RMUX_WIRE_VERSION,
            capabilities: SUPPORTED_CAPABILITIES
                .iter()
                .map(|capability| (*capability).to_owned())
                .collect(),
        }))
        .await;
    }
}

#[tokio::test]
async fn lease_heartbeat_uses_a_dedicated_connection_while_app_rpc_is_blocked() -> TestResult {
    let socket = TestSocket::new()?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let (application, _) = listener
            .accept()
            .await
            .expect("accept SDK application peer");
        let mut application = Peer::new(application);
        application.handshake().await;

        let Request::NewSessionExt(request) = application.request().await else {
            panic!("owned-session creation must follow capability preflight");
        };
        let session_name = request
            .session_name
            .clone()
            .expect("test builder supplies an exact session name");
        application
            .respond(Response::NewSession(NewSessionResponse {
                session_name,
                detached: true,
                output: Some(CommandOutput::from_stdout(b"$42\n")),
            }))
            .await;

        let (lease, _) = tokio::time::timeout(Duration::from_secs(1), listener.accept())
            .await
            .expect("lease setup must open a second daemon connection")
            .expect("accept SDK lease peer");
        let mut lease = Peer::new(lease);
        lease.handshake().await;
        assert!(matches!(
            lease.request().await,
            Request::CreateSessionLease(_)
        ));
        lease
            .respond(Response::CreateSessionLease(CreateSessionLeaseResponse {
                token: 7,
                ttl_millis: 600,
            }))
            .await;

        assert!(matches!(
            application.request().await,
            Request::HasSession(_)
        ));
        let renew = tokio::time::timeout(Duration::from_millis(500), lease.request())
            .await
            .expect("lease renewal must bypass the blocked application RPC");
        assert!(matches!(renew, Request::RenewSessionLease(_)));
        lease
            .respond(Response::RenewSessionLease(RenewSessionLeaseResponse {
                renewed: true,
            }))
            .await;
        application
            .respond(Response::HasSession(HasSessionResponse { exists: true }))
            .await;

        assert!(matches!(
            lease.request().await,
            Request::ReleaseSessionLease(_)
        ));
        lease
            .respond(Response::ReleaseSessionLease(ReleaseSessionLeaseResponse {
                released: true,
            }))
            .await;
    });

    let rmux = Rmux::builder().unix_socket(socket.path()).connect().await?;
    let owned = tokio::time::timeout(
        Duration::from_secs(2),
        rmux.owned_session(SessionName::new("dedicated-lease")?)
            .cleanup_policy(CleanupPolicy::KillOnOwnerExit)
            .lease_ttl(Duration::from_millis(600)),
    )
    .await
    .expect("owned-session creation must complete")?;

    assert!(tokio::time::timeout(Duration::from_secs(2), owned.exists())
        .await
        .expect("application RPC must finish after independent renewal")?);
    let preserved = tokio::time::timeout(Duration::from_secs(2), owned.preserve())
        .await
        .expect("lease release must complete")?;
    drop(preserved);

    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("fake daemon must complete")
        .expect("fake daemon must not panic");
    Ok(())
}

#[tokio::test]
async fn lease_creation_timeout_uses_a_fresh_deadline_for_rollback() -> TestResult {
    let socket = TestSocket::new()?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let (application, _) = listener
            .accept()
            .await
            .expect("accept SDK application peer");
        let mut application = Peer::new(application);
        application.handshake().await;

        let Request::NewSessionExt(request) = application.request().await else {
            panic!("owned-session creation must follow capability preflight");
        };
        let session_name = request
            .session_name
            .clone()
            .expect("test builder supplies an exact session name");
        application
            .respond(Response::NewSession(NewSessionResponse {
                session_name,
                detached: true,
                output: Some(CommandOutput::from_stdout(b"$42\n")),
            }))
            .await;

        let (lease, _) = listener.accept().await.expect("accept SDK lease peer");
        let mut lease = Peer::new(lease);
        lease.handshake().await;
        assert!(matches!(
            lease.request().await,
            Request::CreateSessionLease(_)
        ));
        // Leave lease creation unanswered until its shared public-operation
        // deadline expires. Only the dedicated lease actor becomes terminal;
        // compensation must reuse the healthy application actor with a fresh
        // request deadline.

        let request = tokio::time::timeout(Duration::from_secs(2), application.request())
            .await
            .expect("rollback must outlive the expired public-operation deadline");
        let Request::KillSession(request) = request else {
            panic!("application connection must kill the stable session identity");
        };
        assert_eq!(request.target.as_str(), "$42");
        application
            .respond(Response::KillSession(KillSessionResponse { existed: true }))
            .await;
    });

    let rmux = Rmux::builder()
        .unix_socket(socket.path())
        .default_timeout(Duration::from_millis(300))
        .connect()
        .await?;
    let error = rmux
        .owned_session(SessionName::new("rollback-after-timeout")?)
        .cleanup_policy(CleanupPolicy::KillOnOwnerExit)
        .lease_ttl(Duration::from_millis(600))
        .await
        .expect_err("silent lease creation must fail the owned-session operation");
    assert!(
        matches!(
            error,
            RmuxError::Transport { ref source, .. }
                if source.kind() == io::ErrorKind::TimedOut
        ),
        "successful compensation must preserve only the source timeout: {error:?}"
    );

    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("fake daemon must complete")
        .expect("fake daemon must not panic");
    Ok(())
}
