#![cfg(unix)]

use std::io;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rmux_proto::{FrameDecoder, Request, SessionName};
use rmux_sdk::{Rmux, RmuxError};
use tokio::io::AsyncReadExt;
use tokio::net::{UnixListener, UnixStream};

type TestResult<T = ()> = Result<T, Box<dyn std::error::Error>>;

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
        let directory = PathBuf::from("/tmp").join(format!(
            "rmux-sdk-rpc-timeout-{}-{nonce}",
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

#[tokio::test]
async fn connected_facade_closes_a_silent_unix_rpc_at_its_default_timeout() -> TestResult {
    let socket = TestSocket::new()?;
    let listener = UnixListener::bind(socket.path())?;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await.expect("accept SDK client");
        assert!(matches!(
            read_request(&mut stream).await,
            Request::HasSession(_)
        ));

        let mut trailing = Vec::new();
        stream
            .read_to_end(&mut trailing)
            .await
            .expect("timed-out SDK transport closes the Unix stream");
        assert!(
            trailing.is_empty(),
            "no follow-up request may reuse the transport"
        );
    });

    let rmux = Rmux::builder()
        .unix_socket(socket.path())
        .default_timeout(Duration::from_millis(40))
        .connect()
        .await?;
    let error = tokio::time::timeout(
        Duration::from_secs(2),
        rmux.has_session(SessionName::new("silent")?),
    )
    .await
    .expect("the public SDK timeout must beat the test watchdog")
    .expect_err("a silent daemon must not complete has_session");
    match error {
        RmuxError::Transport { source, .. } => {
            assert_eq!(source.kind(), io::ErrorKind::TimedOut);
        }
        error => panic!("expected a transport timeout, got {error:?}"),
    }

    tokio::time::timeout(Duration::from_secs(2), server)
        .await
        .expect("transport must close after timeout")
        .expect("silent server task must not panic");
    Ok(())
}

async fn read_request(stream: &mut UnixStream) -> Request {
    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 512];
    loop {
        if let Some(request) = decoder
            .next_frame::<Request>()
            .expect("request frame decodes")
        {
            return request;
        }
        let read = stream.read(&mut buffer).await.expect("read request bytes");
        assert_ne!(read, 0, "SDK client closed before sending its request");
        decoder.push_bytes(&buffer[..read]);
    }
}
