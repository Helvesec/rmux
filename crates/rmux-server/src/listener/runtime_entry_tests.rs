use super::*;
use rmux_proto::{RuntimeCommandExecutionRequest, RuntimeCommandExecutionResponse};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[cfg(unix)]
type TestClient = LocalStream;
#[cfg(windows)]
type TestClient = tokio::net::windows::named_pipe::NamedPipeClient;

async fn connection(
    handler: &Arc<RequestHandler>,
) -> io::Result<(
    TestClient,
    watch::Sender<()>,
    tokio::task::JoinHandle<io::Result<()>>,
)> {
    #[cfg(unix)]
    let (server, client) = LocalStream::pair()?;
    #[cfg(windows)]
    let (server, client) = {
        use tokio::net::windows::named_pipe::{ClientOptions, ServerOptions};
        let name = format!(
            r"\\.\pipe\rmux-runtime-entry-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        );
        let server = ServerOptions::new()
            .first_pipe_instance(true)
            .create(&name)?;
        let client = ClientOptions::new().open(&name)?;
        server.connect().await?;
        (server, client)
    };
    let peer = PeerIdentity {
        pid: std::process::id(),
        uid: crate::server_access::current_owner_uid(),
        user: rmux_os::identity::IdentityResolver::current()?,
    };
    let (shutdown_tx, shutdown_rx) = watch::channel(());
    let (shutdown_handle, _shutdown_request_rx) = ShutdownHandle::new();
    let handler = Arc::clone(handler);
    let id = handler.allocate_connection_id();
    let task = tokio::spawn(async move {
        run_connection_with_cleanup(server, peer, handler, id, shutdown_rx, shutdown_handle).await
    });
    Ok((client, shutdown_tx, task))
}

fn offer(arguments: &[&str]) -> Request {
    Request::RuntimeCommandExecution(Box::new(RuntimeCommandExecutionRequest::new(
        arguments.iter().map(|s| (*s).to_owned()).collect(),
        None,
    )))
}

fn versioned_offer(arguments: &[&str], v2: bool) -> Request {
    if v2 {
        Request::RuntimeCommandExecutionV2(Box::new(
            rmux_proto::RuntimeCommandExecutionV2Request::new(
                arguments.iter().map(|s| (*s).to_owned()).collect(),
                None,
                rmux_proto::RuntimeCommandIntent::GenericCli,
            ),
        ))
    } else {
        offer(arguments)
    }
}

async fn execute(handler: &RequestHandler, arguments: &[&str]) {
    let response = handler.handle(offer(arguments)).await;
    assert!(
        matches!(
            &response,
            Response::RuntimeCommandExecution(result) if matches!(result.as_ref(), RuntimeCommandExecutionResponse::Complete {
                error: None,
                exit_status: 0,
                ..
            })
        ),
        "{arguments:?}: {response:?}"
    );
}

async fn counts(handler: &RequestHandler, expected: (usize, usize, bool)) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while handler.wait_for_counts("entry-gate") != expected {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("entry reaches the expected wait/lock registration");
}

async fn pending_entry(
    handler: &RequestHandler,
    client: &mut TestClient,
    expected: (usize, usize, bool),
    entry: &str,
) {
    tokio::select! {
        () = counts(handler, expected) => {}
        response = async {
            let mut decoder = FrameDecoder::new();
            let mut bytes = [0; 1024];
            loop {
                if let Some(response) = decoder.next_frame::<Response>().unwrap() { break response; }
                let count = client.read(&mut bytes).await.expect("read pending entry");
                assert_ne!(count, 0, "connection closed during {entry} setup");
                decoder.push_bytes(&bytes[..count]);
            }
        } => panic!("{entry} completed before its gate: {response:?}"),
    }
}

async fn option_value(handler: &RequestHandler, name: &str) -> Vec<u8> {
    let response = handler.handle(offer(&["show-options", "-gqv", name])).await;
    let Response::RuntimeCommandExecution(result) = response else {
        panic!("read marker: {response:?}");
    };
    match *result {
        RuntimeCommandExecutionResponse::Complete {
            stdout,
            error: None,
            ..
        } => stdout,
        result => panic!("read marker: {result:?}"),
    }
}

// Hook queues have no waiting client, so use their existing finite run-shell
// delay instead of wait-for. No OS shell child is started by this fixture.
const DELAYED_HOOK: &str =
    "run-shell -C 'set-option -g @entered yes ; run-shell -d1 ; set-option -g @late yes'";

async fn hook_started(handler: &RequestHandler) {
    tokio::time::timeout(Duration::from_secs(2), async {
        while option_value(handler, "@entered").await != b"yes\n" {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("finite hook delay starts");
}

async fn cancel_entries(shutdown: bool, v2: bool) -> io::Result<()> {
    for entry in ["queue", "wait", "lock", "load", "save"] {
        let handler = Arc::new(RequestHandler::new());
        let file = std::env::temp_dir().join(format!(
            "rmux-entry-{}-{}",
            std::process::id(),
            NEXT_ID.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::write(&file, b"entry buffer")?;
        let path = file.to_str().expect("test file path");
        let arguments = match entry {
            "queue" => vec![
                "if-shell",
                "-F",
                "1",
                "wait-for entry-gate ; set-option -g @late yes",
            ],
            "wait" => vec!["wait-for", "entry-gate"],
            "lock" => {
                execute(&handler, &["wait-for", "-L", "entry-gate"]).await;
                vec!["wait-for", "-L", "entry-gate"]
            }
            "load" | "save" => {
                execute(
                    &handler,
                    &[
                        "set-hook",
                        "-g",
                        &format!("after-{entry}-buffer"),
                        DELAYED_HOOK,
                    ],
                )
                .await;
                execute(&handler, &["set-buffer", "-b", "entry", "entry buffer"]).await;
                vec![
                    if entry == "load" {
                        "load-buffer"
                    } else {
                        "save-buffer"
                    },
                    "-b",
                    "entry",
                    path,
                ]
            }
            _ => unreachable!(),
        };
        let (mut client, shutdown_tx, mut task) = connection(&handler).await?;
        client
            .write_all(&encode_frame(&versioned_offer(&arguments, v2)).map_err(io::Error::other)?)
            .await?;
        if matches!(entry, "load" | "save") {
            hook_started(&handler).await;
        } else {
            pending_entry(
                &handler,
                &mut client,
                if entry == "lock" {
                    (0, 1, true)
                } else {
                    (1, 0, false)
                },
                entry,
            )
            .await;
        }
        assert!(!task.is_finished(), "setup entry is pending: {entry}");
        if shutdown {
            handler.close_normal_request_admission();
            shutdown_tx.send_replace(());
        } else {
            drop(client);
        }
        let completed = tokio::time::timeout(Duration::from_millis(250), &mut task).await;
        // Release a failed fixture too; the regression never strands its gate.
        if completed.is_err() {
            execute(
                &handler,
                &[
                    "wait-for",
                    if entry == "lock" { "-U" } else { "-S" },
                    "entry-gate",
                ],
            )
            .await;
            task.abort();
            let _ = task.await;
        }
        std::fs::remove_file(file)?;
        assert!(
            completed.is_ok(),
            "{entry} must cancel (shutdown={shutdown})"
        );
        counts(
            &handler,
            if entry == "lock" {
                (0, 0, true)
            } else {
                (0, 0, false)
            },
        )
        .await;
        assert!(
            option_value(&handler, "@late").await.is_empty(),
            "cancelled {entry} must not run its late mutation"
        );
        if entry == "lock" {
            execute(&handler, &["wait-for", "-U", "entry-gate"]).await;
            execute(&handler, &["wait-for", "-L", "entry-gate"]).await;
            execute(&handler, &["wait-for", "-U", "entry-gate"]).await;
        }
    }
    Ok(())
}

#[tokio::test]
async fn runtime_entry_disconnect_cancels_queue_wait_lock_and_buffer_hooks() -> io::Result<()> {
    cancel_entries(false, false).await
}

#[tokio::test]
async fn runtime_entry_shutdown_cancels_queue_wait_lock_and_buffer_hooks() -> io::Result<()> {
    cancel_entries(true, false).await
}

#[tokio::test]
async fn runtime_entry_disconnect_preserves_admitted_direct_mutation() -> io::Result<()> {
    preserve_direct_mutation(false).await
}

#[tokio::test]
async fn runtime_v2_entry_disconnect_cancels_queue_wait_lock_and_buffer_hooks() -> io::Result<()> {
    cancel_entries(false, true).await
}

#[tokio::test]
async fn runtime_v2_entry_shutdown_cancels_queue_wait_lock_and_buffer_hooks() -> io::Result<()> {
    cancel_entries(true, true).await
}

#[tokio::test]
async fn runtime_v2_entry_disconnect_preserves_admitted_direct_mutation() -> io::Result<()> {
    preserve_direct_mutation(true).await
}

async fn preserve_direct_mutation(v2: bool) -> io::Result<()> {
    let handler = Arc::new(RequestHandler::new());
    execute(
        &handler,
        &["set-hook", "-g", "after-set-option", DELAYED_HOOK],
    )
    .await;
    let (mut client, _shutdown_tx, mut task) = connection(&handler).await?;
    client
        .write_all(
            &encode_frame(&versioned_offer(&["set-option", "-g", "status", "off"], v2))
                .map_err(io::Error::other)?,
        )
        .await?;
    hook_started(&handler).await;
    drop(client);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), &mut task)
            .await
            .is_err(),
        "admitted direct mutation drains after disconnect"
    );
    let _ = tokio::time::timeout(Duration::from_secs(2), task)
        .await
        .expect("direct mutation finishes");
    assert_eq!(option_value(&handler, "@late").await, b"yes\n");
    Ok(())
}

#[test]
fn default_stack_replays_runtime_entry_fixtures() {
    // Gates raise RUST_MIN_STACK, which hid a debug connection path that
    // exceeded the default test-thread stack. Replay every fixture without it.
    let output = std::process::Command::new(std::env::current_exe().expect("current test binary"))
        .args([
            "listener::runtime_entry_tests::runtime_",
            "--test-threads=1",
        ])
        .env_remove("RUST_MIN_STACK")
        .output()
        .expect("spawn default-stack runtime entry fixtures");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        output.status.success() && stdout.contains("6 passed"),
        "runtime entry fixtures failed on the default stack: {:?}\nstdout:\n{stdout}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr),
    );
}
