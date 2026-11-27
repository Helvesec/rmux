//! Listener-level regression: a `server-access -r` peer must not start `#()`
//! format jobs through `display-message -p`.
//!
//! The jobs run under the server owner's uid, so a read-only peer that reaches
//! them runs arbitrary shell as the owner. tmux 3.7b refuses `display-message`
//! to a read-only client. Each request below goes through the real connection
//! loop, and a marker file is the only evidence accepted: the same requests
//! from a read-write peer must create their markers, which proves that the
//! absence of a read-only marker is not an artefact of the harness.

use std::path::{Path, PathBuf};

use rmux_os::identity::UserIdentity;
use rmux_proto::{
    DisplayMessageExtRequest, DisplayMessageRequest, RmuxError, RuntimeCommandExecutionRequest,
    RuntimeCommandExecutionResponse,
};

// The shared connection harness (`connected_streams`, `spawn_connection`, …)
// is the parent module.
use super::*;
use crate::server_access::AccessMode;

const JOB_SETTLE: Duration = Duration::from_secs(3);

struct MarkerDir(PathBuf);

impl MarkerDir {
    fn new(label: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "rmux-read-only-format-job-{label}-{}",
            std::process::id()
        ));
        let _ = std::fs::remove_dir_all(&path);
        std::fs::create_dir_all(&path).expect("marker directory");
        Self(path)
    }

    fn marker(&self, name: &str) -> PathBuf {
        self.0.join(name)
    }
}

impl Drop for MarkerDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

fn job_template(marker: &Path) -> String {
    format!("#(touch '{}'; echo ran)", marker.display())
}

/// Every entry that hands a `display-message -p` template to the server.
fn display_requests(markers: &MarkerDir, prefix: &str) -> Vec<(PathBuf, Request)> {
    let message = markers.marker(&format!("{prefix}-message"));
    let empty_context = markers.marker(&format!("{prefix}-empty-context"));
    let extended = markers.marker(&format!("{prefix}-extended"));
    let argv = markers.marker(&format!("{prefix}-argv"));
    vec![
        (
            message.clone(),
            Request::DisplayMessage(DisplayMessageRequest {
                target: None,
                print: true,
                message: Some(job_template(&message)),
                empty_target_context: false,
            }),
        ),
        (
            empty_context.clone(),
            Request::DisplayMessage(DisplayMessageRequest {
                target: None,
                print: true,
                message: Some(job_template(&empty_context)),
                empty_target_context: true,
            }),
        ),
        (
            extended.clone(),
            Request::DisplayMessageExt(Box::new(DisplayMessageExtRequest {
                target: None,
                print: true,
                message: Some(job_template(&extended)),
                target_client: None,
                empty_target_context: false,
                duration_ms: None,
                ignore_input: false,
            })),
        ),
        (
            argv.clone(),
            Request::RuntimeCommandExecution(Box::new(RuntimeCommandExecutionRequest::new(
                vec![
                    "display-message".to_owned(),
                    "-p".to_owned(),
                    job_template(&argv),
                ],
                None,
            ))),
        ),
    ]
}

fn is_read_only_refusal(response: &Response) -> bool {
    let refusal = RmuxError::Server("client is read-only".to_owned());
    match response {
        Response::Error(error) => error.error == refusal,
        Response::RuntimeCommandExecution(result) => matches!(
            result.as_ref(),
            RuntimeCommandExecutionResponse::Complete { error: Some(error), .. } if *error == refusal
        ),
        _ => false,
    }
}

async fn wait_for_markers(markers: &[PathBuf], deadline: Duration) -> Vec<PathBuf> {
    let started = std::time::Instant::now();
    loop {
        let present: Vec<PathBuf> = markers.iter().filter(|m| m.exists()).cloned().collect();
        if present.len() == markers.len() || started.elapsed() >= deadline {
            return present;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

#[tokio::test]
async fn read_only_peer_cannot_start_format_jobs_through_display_message() -> io::Result<()> {
    let peer_uid = rmux_os::identity::real_user_id().saturating_add(30_000);
    let peer = PeerIdentity {
        pid: std::process::id(),
        uid: peer_uid,
        user: UserIdentity::Uid(peer_uid),
    };
    let handler = Arc::new(RequestHandler::new());
    let _pane = start_quiet_pane(&handler, "read-only-format-job").await;
    handler
        .set_test_access_mode_for_uid(peer_uid, AccessMode::ReadOnly)
        .expect("test peer is read-only");
    let markers = MarkerDir::new("display");

    let (server, mut client) = connected_streams("listener-read-only-format-job").await?;
    let (_shutdown_tx, connection_task) = spawn_connection(&handler, peer, server);

    let read_only = display_requests(&markers, "read-only");
    for (_, request) in &read_only {
        write_test_request(&mut client, request.clone()).await?;
        let response = read_test_response(&mut client).await?;
        assert!(
            is_read_only_refusal(&response),
            "a read-only peer must be refused display-message -p, got {response:?} for {request:?}"
        );
    }
    let read_only_markers: Vec<PathBuf> = read_only.into_iter().map(|(m, _)| m).collect();
    assert_eq!(
        wait_for_markers(&read_only_markers, JOB_SETTLE).await,
        Vec::<PathBuf>::new(),
        "a read-only peer started #() jobs under the owner's uid"
    );

    // Control: the same requests from a read-write peer do start their jobs.
    handler
        .set_test_access_mode_for_uid(peer_uid, AccessMode::ReadWrite)
        .expect("test peer upgrades to read-write");
    let read_write = display_requests(&markers, "read-write");
    for (_, request) in &read_write {
        write_test_request(&mut client, request.clone()).await?;
        let response = read_test_response(&mut client).await?;
        assert!(
            !is_read_only_refusal(&response) && !matches!(response, Response::Error(_)),
            "a read-write peer must reach display-message -p, got {response:?}"
        );
    }
    let read_write_markers: Vec<PathBuf> = read_write.into_iter().map(|(m, _)| m).collect();
    assert_eq!(
        wait_for_markers(&read_write_markers, Duration::from_secs(10)).await,
        read_write_markers,
        "the harness must run #() jobs for a read-write peer, or the read-only check proves nothing"
    );

    drop(client);
    finish_connection(connection_task).await
}
