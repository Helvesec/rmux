#![cfg(windows)]

use std::error::Error;
use std::io;

use rmux_ipc::{endpoint_for_label, LocalEndpoint, LocalListener, LocalStream};
use rmux_proto::{encode_frame, FrameDecoder, HasSessionResponse, Request, Response};
use rmux_sdk::bootstrap::startup_windows::{
    connect_or_start_with, DEFAULT_STARTUP_DEADLINE, STARTUP_POLL_INTERVAL,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::sync::oneshot;

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

#[tokio::test]
async fn public_startup_outcome_drops_safely_inside_tokio_task() -> TestResult {
    let endpoint = endpoint_for_label(format!("sdk-startup-drop-{}", std::process::id()))?;
    let listener = LocalListener::bind(&endpoint)?;
    let server = tokio::spawn(async move {
        let (mut stream, _) = listener.accept().await?;
        let request = read_request(&mut stream).await?;
        assert!(
            matches!(request, Request::HasSession(_)),
            "public bootstrap must probe the listening daemon"
        );
        let frame = encode_frame(&Response::HasSession(HasSessionResponse { exists: false }))?;
        stream.write_all(&frame).await?;
        stream.flush().await?;
        TestResult::Ok(())
    });
    let pipe_name = endpoint.as_path().to_path_buf();

    tokio::spawn(async move {
        let outcome = connect_or_start_with(
            &pipe_name,
            |_| async { Err(io::Error::other("launcher must not run for a live daemon")) },
            DEFAULT_STARTUP_DEADLINE,
            STARTUP_POLL_INTERVAL,
        )
        .await
        .expect("public bootstrap should join the live daemon");
        assert!(!outcome.is_owner(), "live daemon probe must join existing");
        // Exercise the public ownership contract: `StartupOutcome` and its
        // blocking named-pipe stream are destroyed on a Tokio worker thread.
        drop(outcome);
    })
    .await
    .expect("dropping StartupOutcome inside Tokio must not panic");

    server.await??;
    Ok(())
}

#[tokio::test]
async fn managed_restart_launches_and_probes_only_the_reserved_generation() -> TestResult {
    let old_endpoint =
        endpoint_for_label(format!("sdk-restart-generation-{}", std::process::id()))?;
    let old_pipe = old_endpoint.as_path().to_path_buf();
    drop(LocalListener::bind(&old_endpoint)?);

    let (reserved_tx, reserved_rx) = oneshot::channel();
    let (server_done_tx, server_done_rx) = oneshot::channel();
    let outcome = connect_or_start_with(
        &old_pipe,
        move |reserved_pipe| {
            let reserved_pipe = reserved_pipe.to_path_buf();
            async move {
                let endpoint = LocalEndpoint::from_path(reserved_pipe.clone());
                let listener = LocalListener::bind(&endpoint)?;
                let _ = reserved_tx.send(reserved_pipe);
                tokio::spawn(async move {
                    let result = answer_startup_probe(listener).await;
                    let _ = server_done_tx.send(result);
                });
                Ok(())
            }
        },
        DEFAULT_STARTUP_DEADLINE,
        STARTUP_POLL_INTERVAL,
    )
    .await?;

    assert!(outcome.is_owner());
    let reserved_pipe = reserved_rx.await?;
    assert_ne!(
        reserved_pipe, old_pipe,
        "a stopped, publicly observed generation must rotate before restart"
    );
    server_done_rx.await??;
    Ok(())
}

async fn answer_startup_probe(listener: LocalListener) -> TestResult {
    let (mut stream, _) = listener.accept().await?;
    let request = read_request(&mut stream).await?;
    assert!(matches!(request, Request::HasSession(_)));
    let frame = encode_frame(&Response::HasSession(HasSessionResponse { exists: false }))?;
    stream.write_all(&frame).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_request(stream: &mut LocalStream) -> TestResult<Request> {
    let mut decoder = FrameDecoder::new();
    let mut buffer = [0_u8; 1024];
    loop {
        if let Some(request) = decoder.next_frame::<Request>()? {
            return Ok(request);
        }
        let read = stream.read(&mut buffer).await?;
        if read == 0 {
            return Err("startup probe closed before sending its request".into());
        }
        decoder.push_bytes(&buffer[..read]);
    }
}
