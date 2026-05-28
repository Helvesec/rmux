//! End-to-end smoke tests for prefix keybindings over the *real* attach
//! socket protocol.
//!
//! The lib-level tests in `handler_attach_tests/passthrough_input.rs`
//! exercise `handle_attached_live_input_inner` directly — which is the
//! same function the production socket-message loop ends up calling, so
//! they're necessary but not sufficient.  They miss:
//!   * frame encoding / decoding round-trips (`AttachMessage::Data`)
//!   * the daemon's PTY plumbing and async timing
//!   * the per-pane state visible to other RPC clients via
//!     `display-message`
//!
//! These tests reproduce the exact path a real `rmux` client takes:
//! open a `UnixStream`, send `\x02w` (Ctrl-B + w) inside an
//! `AttachMessage::Data` frame, then verify from a *second* socket
//! connection that the target pane reports `pane_in_mode = 1`.
//!
//! Drives the user-reported regression where `Ctrl-B w` "did nothing"
//! in a passthrough session despite the unit tests being green.

use std::error::Error;
use std::path::Path;
use std::time::Duration;

use rmux_proto::{
    encode_attach_message, AttachFrameDecoder, AttachMessage, AttachSessionRequest,
    AttachedKeystroke, DisplayMessageRequest, KillSessionRequest, NewSessionExtRequest,
    NewSessionRequest, PaneTarget, Request, Response, SessionName, Target, TerminalSize,
};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::time::{sleep, timeout};

use crate::common::{
    send_request, session_name, start_server, ClientConnection, TestHarness, PTY_TEST_LOCK,
};
use crate::support::STEP_TIMEOUT;

/// Read the attach stream until accumulated `Data` payloads contain
/// `needle`, ignoring structured response messages (KeyDispatched,
/// Resize, …) that the production server interleaves alongside data.
async fn read_attach_data_until_contains(
    stream: &mut tokio::net::UnixStream,
    needle: &[u8],
    timeout_duration: Duration,
) -> Result<Vec<u8>, Box<dyn Error>> {
    let deadline = std::time::Instant::now() + timeout_duration;
    let mut decoder = AttachFrameDecoder::new();
    let mut accumulated = Vec::new();
    let mut buf = [0_u8; 4096];

    while std::time::Instant::now() < deadline {
        let remaining = deadline.saturating_duration_since(std::time::Instant::now());
        let bytes_read = timeout(remaining, stream.read(&mut buf)).await??;
        if bytes_read == 0 {
            break;
        }
        decoder.push_bytes(&buf[..bytes_read]);
        while let Some(message) = decoder.next_message()? {
            if let AttachMessage::Data(payload) = message {
                accumulated.extend_from_slice(&payload);
                if accumulated
                    .windows(needle.len())
                    .any(|window| window == needle)
                {
                    return Ok(accumulated);
                }
            }
        }
    }
    Err(format!(
        "timed out waiting for {:?} on attach stream; accumulated {} bytes",
        String::from_utf8_lossy(needle),
        accumulated.len()
    )
    .into())
}

/// Poll `display-message -p '#{pane_in_mode}'` until the response is
/// `expected`, or the timeout fires.
async fn wait_for_pane_in_mode(
    socket_path: &Path,
    target: PaneTarget,
    expected: &str,
) -> Result<(), Box<dyn Error>> {
    let deadline = std::time::Instant::now() + STEP_TIMEOUT;
    let mut last_seen = String::new();
    while std::time::Instant::now() < deadline {
        let response = send_request(
            socket_path,
            &Request::DisplayMessage(DisplayMessageRequest {
                target: Some(Target::Pane(target.clone())),
                print: true,
                message: Some("#{pane_in_mode}".to_owned()),
            }),
        )
        .await?;
        if let Response::DisplayMessage(message) = response {
            if let Some(output) = message.output {
                last_seen = String::from_utf8_lossy(output.stdout()).trim().to_owned();
                if last_seen == expected {
                    return Ok(());
                }
            }
        }
        sleep(Duration::from_millis(25)).await;
    }
    Err(format!(
        "pane_in_mode never became {expected:?} (last seen {last_seen:?}) on {target:?}"
    )
    .into())
}

/// Encode and write a raw byte sequence as an attach `Data` frame.
async fn send_attach_bytes(
    stream: &mut tokio::net::UnixStream,
    bytes: &[u8],
) -> Result<(), Box<dyn Error>> {
    let frame = encode_attach_message(&AttachMessage::Data(bytes.to_vec()))?;
    stream.write_all(&frame).await?;
    Ok(())
}

/// Encode and write a raw byte sequence as an attach `Keystroke` frame
/// — what the production `rmux attach` binary actually sends for every
/// PTY read.  Distinct from [`send_attach_bytes`] because the server
/// dispatches the two variants through slightly different paths.
async fn send_attach_keystroke(
    stream: &mut tokio::net::UnixStream,
    bytes: &[u8],
) -> Result<(), Box<dyn Error>> {
    let frame = encode_attach_message(&AttachMessage::Keystroke(AttachedKeystroke::new(
        bytes.to_vec(),
    )))?;
    stream.write_all(&frame).await?;
    Ok(())
}

async fn new_normal_session(
    socket_path: &Path,
    name: SessionName,
) -> Result<(), Box<dyn Error>> {
    let created = send_request(
        socket_path,
        &Request::NewSession(NewSessionRequest {
            session_name: name,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
        }),
    )
    .await?;
    assert!(matches!(created, Response::NewSession(_)));
    Ok(())
}

async fn new_passthrough_session(
    socket_path: &Path,
    name: SessionName,
) -> Result<(), Box<dyn Error>> {
    let created = send_request(
        socket_path,
        &Request::NewSessionExt(NewSessionExtRequest {
            session_name: Some(name),
            working_directory: None,
            detached: true,
            size: Some(TerminalSize { cols: 80, rows: 24 }),
            environment: None,
            group_target: None,
            attach_if_exists: false,
            detach_other_clients: false,
            kill_other_clients: false,
            flags: None,
            window_name: None,
            print_session_info: false,
            print_format: None,
            command: None,
            process_command: None,
            passthrough: true,
            client_environment: None,
        }),
    )
    .await?;
    assert!(
        matches!(created, Response::NewSession(_)),
        "passthrough session should be created, got {created:?}"
    );
    Ok(())
}

async fn kill_session_and_shutdown(
    socket_path: &Path,
    name: SessionName,
    handle: rmux_server::ServerHandle,
) -> Result<(), Box<dyn Error>> {
    let _ = send_request(
        socket_path,
        &Request::KillSession(KillSessionRequest {
            target: name,
            kill_all_except_target: false,
            clear_alerts: false,
        }),
    )
    .await;
    timeout(STEP_TIMEOUT, handle.shutdown()).await??;
    Ok(())
}

/// Baseline: a non-passthrough session must dispatch `Ctrl-B w` to
/// choose-tree.  Locks down the expected behaviour so regressions in
/// the passthrough branch can be diffed against it.
#[tokio::test]
async fn attached_ctrl_b_w_opens_choose_tree_on_normal_session() -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("prefix-w-normal");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let alpha = session_name("alpha");
    new_normal_session(&socket_path, alpha.clone()).await?;

    let (response, mut attach_stream) = ClientConnection::connect(&socket_path)
        .await?
        .begin_attach(AttachSessionRequest {
            target: alpha.clone(),
        })
        .await?;
    assert_eq!(response.session_name, alpha);

    send_attach_bytes(&mut attach_stream, b"\x02w").await?;

    wait_for_pane_in_mode(&socket_path, PaneTarget::new(alpha.clone(), 0), "1").await?;

    drop(attach_stream);
    kill_session_and_shutdown(&socket_path, alpha, handle).await
}

/// The user's complaint: `Ctrl-B w` reportedly did nothing on a
/// passthrough session.  Lib-level unit tests already cover the
/// in-process input path, so a green result here proves the failure
/// must be elsewhere (client side, terminal, or the user's actual
/// keypress sequence).
#[tokio::test]
async fn attached_ctrl_b_w_opens_choose_tree_on_passthrough_session() -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("prefix-w-passthrough");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let alpha = session_name("alpha");
    new_passthrough_session(&socket_path, alpha.clone()).await?;

    let (response, mut attach_stream) = ClientConnection::connect(&socket_path)
        .await?
        .begin_attach(AttachSessionRequest {
            target: alpha.clone(),
        })
        .await?;
    assert_eq!(response.session_name, alpha);

    send_attach_bytes(&mut attach_stream, b"\x02w").await?;

    wait_for_pane_in_mode(&socket_path, PaneTarget::new(alpha.clone(), 0), "1").await?;

    drop(attach_stream);
    kill_session_and_shutdown(&socket_path, alpha, handle).await
}

/// Real `rmux attach` sends user keystrokes as
/// `AttachMessage::Keystroke`, not `Data` — different server dispatch
/// path.  Exercises the variant the production client actually emits
/// on a passthrough session.
#[tokio::test]
async fn attached_ctrl_b_w_keystroke_opens_choose_tree_on_passthrough_session(
) -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("prefix-w-passthrough-keystroke");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let alpha = session_name("alpha");
    new_passthrough_session(&socket_path, alpha.clone()).await?;

    let (_response, mut attach_stream) = ClientConnection::connect(&socket_path)
        .await?
        .begin_attach(AttachSessionRequest {
            target: alpha.clone(),
        })
        .await?;

    send_attach_keystroke(&mut attach_stream, b"\x02w").await?;

    wait_for_pane_in_mode(&socket_path, PaneTarget::new(alpha.clone(), 0), "1").await?;

    drop(attach_stream);
    kill_session_and_shutdown(&socket_path, alpha, handle).await
}

/// Production client almost never batches a prefix + binding into one
/// `read`: the terminal driver typically delivers them as two separate
/// keystrokes.  Exercises the inter-keystroke `pending_input` path on
/// passthrough.
#[tokio::test]
async fn attached_ctrl_b_w_two_keystrokes_open_choose_tree_on_passthrough_session(
) -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("prefix-w-passthrough-two-keystrokes");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let alpha = session_name("alpha");
    new_passthrough_session(&socket_path, alpha.clone()).await?;

    let (_response, mut attach_stream) = ClientConnection::connect(&socket_path)
        .await?
        .begin_attach(AttachSessionRequest {
            target: alpha.clone(),
        })
        .await?;

    send_attach_keystroke(&mut attach_stream, b"\x02").await?;
    sleep(Duration::from_millis(30)).await;
    send_attach_keystroke(&mut attach_stream, b"w").await?;

    wait_for_pane_in_mode(&socket_path, PaneTarget::new(alpha.clone(), 0), "1").await?;

    drop(attach_stream);
    kill_session_and_shutdown(&socket_path, alpha, handle).await
}

/// The user's *observable* signal that choose-tree opened is the
/// overlay actually rendering on screen — i.e. the attach stream
/// emitting alt-screen-enter (`\x1b[?1049h`) + tree-mode bytes.  A
/// pure server-state assertion misses regressions where the server
/// dispatches the binding but never streams the result back.
#[tokio::test]
async fn attached_ctrl_b_w_streams_alt_screen_enter_to_client_on_passthrough_session(
) -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("prefix-w-passthrough-overlay-stream");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let alpha = session_name("alpha");
    new_passthrough_session(&socket_path, alpha.clone()).await?;

    let (_response, mut attach_stream) = ClientConnection::connect(&socket_path)
        .await?
        .begin_attach(AttachSessionRequest {
            target: alpha.clone(),
        })
        .await?;

    send_attach_keystroke(&mut attach_stream, b"\x02w").await?;

    // Alt-screen enter is the canonical "an overlay opened" signal —
    // present in both normal and passthrough modes.  Passthrough
    // additionally brackets it with the host alt-screen, so the
    // sequence appears at least once on the wire either way.
    let _observed =
        read_attach_data_until_contains(&mut attach_stream, b"\x1b[?1049h", STEP_TIMEOUT).await?;

    drop(attach_stream);
    kill_session_and_shutdown(&socket_path, alpha, handle).await
}

/// Two separate writes (real terminals don't always batch the prefix
/// and the following key into the same `read` syscall).  Covers the
/// split-buffer path on the passthrough fast lane.
#[tokio::test]
async fn attached_ctrl_b_w_split_across_writes_on_passthrough_session() -> Result<(), Box<dyn Error>> {
    let _guard = PTY_TEST_LOCK.lock().await;
    let harness = TestHarness::new("prefix-w-passthrough-split");
    let socket_path = harness.socket_path().to_path_buf();
    let handle = start_server(&harness).await?;

    let alpha = session_name("alpha");
    new_passthrough_session(&socket_path, alpha.clone()).await?;

    let (_response, mut attach_stream) = ClientConnection::connect(&socket_path)
        .await?
        .begin_attach(AttachSessionRequest {
            target: alpha.clone(),
        })
        .await?;

    send_attach_bytes(&mut attach_stream, b"\x02").await?;
    sleep(Duration::from_millis(20)).await;
    send_attach_bytes(&mut attach_stream, b"w").await?;

    wait_for_pane_in_mode(&socket_path, PaneTarget::new(alpha.clone(), 0), "1").await?;

    drop(attach_stream);
    kill_session_and_shutdown(&socket_path, alpha, handle).await
}
