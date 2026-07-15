use std::io;
use std::os::unix::fs::PermissionsExt as _;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rmux_ipc::{LocalStream, PeerIdentity};
use rmux_proto::{
    encode_frame, CreateWebShareRequest, ListWebSharesRequest, NewSessionRequest, Request,
    Response, SessionName, TerminalSize, WebShareRequest, WebShareScope,
};
use tokio::io::AsyncWriteExt as _;
use tokio::sync::watch;

use super::{run_connection_with_cleanup, UndeliveredWebShareGuard};
use crate::daemon::ShutdownHandle;
use crate::handler::RequestHandler;

#[tokio::test]
async fn client_disconnect_cancels_web_share_tunnel_start() -> io::Result<()> {
    let _env_lock = crate::test_env::lock_async().await;
    let fixture = TestDirectory::new("rmux-web-share-disconnect")?;
    let marker = fixture.path().join("provider.pid");
    let provider = fixture.path().join("provider.sh");
    std::fs::write(
        &provider,
        format!(
            "#!/bin/sh\nprintf '%s' \"$$\" >'{}'\nexec sleep 30\n",
            marker.display()
        ),
    )?;
    std::fs::set_permissions(&provider, std::fs::Permissions::from_mode(0o700))?;
    std::fs::write(
        fixture.path().join("slow.toml"),
        format!(
            "name = \"slow\"\nprogram = \"{}\"\nurl_pattern = \"https://example.invalid\"\nurl_source = \"stdout\"\nready_timeout_secs = 10\n",
            provider.display()
        ),
    )?;
    let fixture_path = fixture.path().to_string_lossy();
    let _preset_dir =
        crate::test_env::EnvVarGuard::set("RMUX_TUNNEL_PRESET_DIR", Some(&fixture_path));

    let handler = Arc::new(RequestHandler::new());
    handler.mark_web_listener_available();
    let session_name = create_session(&handler, "disconnect-tunnel").await;
    let (mut client, _shutdown_tx, mut connection_task) = spawn_test_connection(&handler)?;
    write_test_request(
        &mut client,
        create_web_share_request(session_name, Some("slow")),
    )
    .await?;

    let provider_pid = wait_for_pid_marker(&marker).await;
    let mut provider_cleanup = ProcessCleanup::new(provider_pid);
    drop(client);
    let connection_result =
        tokio::time::timeout(Duration::from_secs(2), &mut connection_task).await;
    let connection_stopped = matches!(&connection_result, Ok(Ok(Ok(()))));
    let provider_stopped = wait_for_process_exit(provider_pid).await;

    if connection_result.is_err() {
        connection_task.abort();
        let _ = connection_task.await;
    }
    if provider_stopped {
        provider_cleanup.disarm();
    }

    assert!(
        connection_stopped,
        "disconnected web-share create remained blocked on tunnel startup"
    );
    assert!(
        provider_stopped,
        "disconnected web-share create left its tunnel provider running"
    );
    Ok(())
}

#[tokio::test]
async fn undelivered_web_share_response_is_rolled_back() {
    let handler = Arc::new(RequestHandler::new());
    handler.mark_web_listener_available();
    let session_name = create_session(&handler, "undelivered-share").await;
    let response = handler
        .handle(create_web_share_request(session_name, None))
        .await;
    let guard = UndeliveredWebShareGuard::for_response(Arc::clone(&handler), &response)
        .expect("create response arms rollback");

    drop(guard);

    let listed = handler
        .handle(Request::WebShare(Box::new(WebShareRequest::List(
            ListWebSharesRequest,
        ))))
        .await;
    assert!(matches!(
        listed,
        Response::WebShare(response)
            if matches!(response.as_ref(), rmux_proto::WebShareResponse::List(list) if list.shares.is_empty())
    ));
}

async fn create_session(handler: &RequestHandler, name: &str) -> SessionName {
    let session_name = SessionName::new(name).expect("valid session name");
    assert!(matches!(
        handler
            .handle(Request::NewSession(NewSessionRequest {
                session_name: session_name.clone(),
                detached: true,
                size: Some(TerminalSize { cols: 80, rows: 24 }),
                environment: None,
            }))
            .await,
        Response::NewSession(_)
    ));
    session_name
}

fn create_web_share_request(session_name: SessionName, tunnel_provider: Option<&str>) -> Request {
    Request::WebShare(Box::new(WebShareRequest::Create(CreateWebShareRequest {
        scope: WebShareScope::Session(session_name),
        public_base_url: None,
        tunnel_provider: tunnel_provider.map(str::to_owned),
        frontend_url: None,
        ttl_seconds: None,
        expires_at_unix: None,
        max_spectators: Some(1),
        max_operators: None,
        url_options: Default::default(),
        require_pin: false,
        operator_pin: None,
        spectator_pin: None,
        terminal_palette: None,
        operator: false,
        spectator: true,
        controls: false,
        kill_session_on_expire: false,
    })))
}

fn spawn_test_connection(
    handler: &Arc<RequestHandler>,
) -> io::Result<(
    LocalStream,
    watch::Sender<()>,
    tokio::task::JoinHandle<io::Result<()>>,
)> {
    let (server, client) = LocalStream::pair()?;
    let handler = Arc::clone(handler);
    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let connection_id = handler.allocate_connection_id();
    let task = tokio::spawn(async move {
        run_connection_with_cleanup(
            server,
            PeerIdentity {
                pid: std::process::id(),
                uid: rmux_os::identity::real_user_id(),
                user: rmux_os::identity::UserIdentity::Uid(rmux_os::identity::real_user_id()),
            },
            handler,
            connection_id,
            shutdown_rx,
            shutdown_handle,
        )
        .await
    });
    Ok((client, shutdown_tx, task))
}

async fn write_test_request(stream: &mut LocalStream, request: Request) -> io::Result<()> {
    let frame = encode_frame(&request).map_err(io::Error::other)?;
    stream.write_all(&frame).await
}

async fn wait_for_pid_marker(path: &Path) -> i32 {
    for _ in 0..200 {
        if let Some(pid) = std::fs::read_to_string(path)
            .ok()
            .and_then(|value| value.parse::<i32>().ok())
        {
            return pid;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("tunnel provider did not write its PID marker");
}

async fn wait_for_process_exit(pid: i32) -> bool {
    let Some(pid) = rustix::process::Pid::from_raw(pid) else {
        return true;
    };
    for _ in 0..100 {
        if rustix::process::test_kill_process(pid).is_err() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    false
}

struct TestDirectory(PathBuf);

impl TestDirectory {
    fn new(prefix: &str) -> io::Result<Self> {
        let nonce = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("clock after Unix epoch")
            .as_nanos();
        let path = std::env::temp_dir().join(format!("{prefix}-{}-{nonce}", std::process::id(),));
        std::fs::create_dir_all(&path)?;
        Ok(Self(path))
    }

    fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDirectory {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

struct ProcessCleanup(Option<rustix::process::Pid>);

impl ProcessCleanup {
    fn new(pid: i32) -> Self {
        Self(rustix::process::Pid::from_raw(pid))
    }

    fn disarm(&mut self) {
        self.0 = None;
    }
}

impl Drop for ProcessCleanup {
    fn drop(&mut self) {
        if let Some(pid) = self.0.take() {
            let _ = rustix::process::kill_process(pid, rustix::process::Signal::KILL);
        }
    }
}
