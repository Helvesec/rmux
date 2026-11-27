//! A pane can end between a wait's process probe and its next pane request:
//! the quiet wait's snapshot, or the pane-exit wait's output subscription.
//! A scripted daemon replays those interleavings deterministically.
#[cfg(not(target_has_atomic = "64"))]
use portable_atomic::AtomicU64;
use std::collections::VecDeque;
use std::io::{Read, Write};
use std::os::unix::net::UnixListener;
#[cfg(target_has_atomic = "64")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use std::time::Duration;

use rmux_bootstrap::deadline::StartupDeadline;
use rmux_proto::{
    decode_internal_pane_exit_probe, encode_frame, CommandOutput, ErrorResponse, FrameDecoder,
    HandshakeResponse, ListPanesResponse, PaneId, PaneTarget, Request, ResolveTargetResponse,
    Response, RmuxError, SessionName, Target,
};

use crate::cli_response::tmux_cli_error_message;

use super::super::common::connect_wait;
use super::super::wait_target;
use super::{wait_condition, ExitFailure, PaneExitStatus, WaitCompletion, WaitCondition};

static NEXT_SOCKET: AtomicU64 = AtomicU64::new(0);

const SESSION: &str = "race";
const PANE_ID: u32 = 5;
const IDENTITY_LISTING: &str = "0\t0\t%5\t$3\n";
const PROBE_ALIVE: &str = "%5\t0\t\t\t0\n";
const PROBE_EXITED_7: &str = "%5\t1\t7\t\t1\n";
const PROBE_EXITED_7_UNRETAINED: &str = "%5\t1\t7\t\t0\n";

fn session_name() -> SessionName {
    SessionName::new(SESSION).expect("valid session name")
}

fn pane_not_found(pane_id: u32) -> RmuxError {
    RmuxError::PaneNotFound {
        session_name: session_name(),
        pane_id: PaneId::new(pane_id),
    }
}

/// Answers every request by kind; process probes replay `probes` in order,
/// every snapshot reports that the pane id no longer exists in the session,
/// and every output subscription is refused with `subscription_error`.
fn serve_lost_pane(
    listener: UnixListener,
    mut probes: VecDeque<&'static str>,
    subscription_error: RmuxError,
) -> usize {
    let (mut stream, _) = listener.accept().expect("accept wait client");
    stream
        .set_read_timeout(Some(Duration::from_secs(10)))
        .expect("bound fixture reads");
    let mut decoder = FrameDecoder::new();
    let mut bytes = [0; 8192];
    let mut probes_served = 0;
    loop {
        let request: Request = match decoder.next_frame().expect("decode request") {
            Some(request) => request,
            None => match stream.read(&mut bytes) {
                Ok(0) | Err(_) => return probes_served,
                Ok(count) => {
                    decoder.push_bytes(&bytes[..count]);
                    continue;
                }
            },
        };
        let response = match request {
            Request::Handshake(_) => Response::Handshake(HandshakeResponse::current()),
            Request::ListPanes(request) => {
                let is_probe = request
                    .format
                    .as_deref()
                    .and_then(decode_internal_pane_exit_probe)
                    .is_some();
                let stdout = if is_probe {
                    probes_served += 1;
                    let line = probes.front().copied().expect("scripted probe");
                    if probes.len() > 1 {
                        probes.pop_front();
                    }
                    line
                } else {
                    IDENTITY_LISTING
                };
                Response::ListPanes(ListPanesResponse {
                    output: CommandOutput::from_stdout(stdout),
                })
            }
            Request::PaneSnapshotRef(_) => Response::Error(ErrorResponse {
                error: pane_not_found(PANE_ID),
            }),
            Request::SubscribePaneOutputRef(_) => Response::Error(ErrorResponse {
                error: subscription_error.clone(),
            }),
            Request::ResolveTarget(_) => Response::ResolveTarget(ResolveTargetResponse {
                target: Target::Session(session_name()),
            }),
            other => panic!("unexpected '{}' request", other.command_name()),
        };
        stream
            .write_all(&encode_frame(&response).expect("encode response"))
            .expect("write response");
    }
}

/// Runs one wait against the scripted daemon; returns the wait result and how
/// many process probes the daemon answered.
fn wait_with_lost_pane(
    condition: &WaitCondition,
    probes: &[&'static str],
    subscription_error: RmuxError,
    baseline_revision: Option<u64>,
) -> (Result<WaitCompletion, ExitFailure>, usize) {
    let path = std::env::temp_dir().join(format!(
        "s1982-quiet-{}-{}.sock",
        std::process::id(),
        NEXT_SOCKET.fetch_add(1, Ordering::Relaxed)
    ));
    let listener = UnixListener::bind(&path).expect("bind fixture socket");
    let probes = probes.iter().copied().collect();
    let server = std::thread::spawn(move || serve_lost_pane(listener, probes, subscription_error));

    let deadline = StartupDeadline::from_timeout(Some(Duration::from_secs(5)));
    let result = connect_wait(&path, deadline).and_then(|mut connection| {
        let slot = PaneTarget::with_window(session_name(), 0, 0);
        let target = wait_target::for_slot(&mut connection, &slot, "wait-pane")?;
        wait_condition(
            &mut connection,
            target,
            condition,
            deadline,
            baseline_revision,
        )
    });
    let probes_served = server.join().expect("fixture daemon");
    std::fs::remove_file(path).expect("remove owned socket");
    (result, probes_served)
}

fn quiet_wait_with_lost_snapshot(
    probes: &[&'static str],
    baseline_revision: Option<u64>,
) -> (Result<WaitCompletion, ExitFailure>, usize) {
    let condition = WaitCondition::Quiet(Duration::from_secs(30));
    let unused = RmuxError::Server("quiet waits never subscribe".to_owned());
    wait_with_lost_pane(&condition, probes, unused, baseline_revision)
}

/// The probe sees the exit, then the pane leaves before its output drain.
fn pane_exit_wait_with_lost_subscription(
    subscription_error: RmuxError,
) -> Result<WaitCompletion, ExitFailure> {
    let (result, probes_served) = wait_with_lost_pane(
        &WaitCondition::PaneExit,
        &[PROBE_EXITED_7_UNRETAINED],
        subscription_error,
        None,
    );
    assert_eq!(probes_served, 1);
    result
}

#[test]
fn quiet_wait_ends_on_the_exit_that_removed_the_pane_before_its_snapshot() {
    // send-keys --wait quiet arms a baseline revision; wait-pane --quiet does not.
    for baseline_revision in [Some(0), None] {
        let (result, probes_served) =
            quiet_wait_with_lost_snapshot(&[PROBE_ALIVE, PROBE_EXITED_7], baseline_revision);

        match result {
            Ok(WaitCompletion::Matched { pane_exit }) => assert_eq!(
                pane_exit,
                Some(PaneExitStatus::known(Some(7), None)),
                "baseline {baseline_revision:?}"
            ),
            Ok(_) => panic!("baseline {baseline_revision:?}: quiet wait did not match"),
            Err(error) => panic!(
                "baseline {baseline_revision:?}: quiet wait failed on the snapshot race: {}",
                error.message()
            ),
        }
        assert_eq!(probes_served, 2, "baseline {baseline_revision:?}");
    }
}

#[test]
fn quiet_wait_keeps_the_snapshot_error_while_the_pane_still_runs() {
    let (result, probes_served) =
        quiet_wait_with_lost_snapshot(&[PROBE_ALIVE, PROBE_ALIVE], Some(0));

    let Err(error) = result else {
        panic!("a snapshot failure on a running pane must stay an error")
    };
    assert_eq!(error.exit_code(), 1);
    assert!(
        error
            .message()
            .contains("pane id does not exist in session"),
        "{}",
        error.message()
    );
    assert_eq!(probes_served, 2);
}

#[test]
fn pane_exit_wait_ends_on_the_exit_when_the_pane_leaves_before_its_output_drain() {
    let missing_session = RmuxError::SessionNotFound(SESSION.to_owned());
    for subscription_error in [pane_not_found(PANE_ID), missing_session] {
        let shown = subscription_error.to_string();
        match pane_exit_wait_with_lost_subscription(subscription_error) {
            Ok(WaitCompletion::Matched { pane_exit }) => assert_eq!(
                pane_exit,
                Some(PaneExitStatus::known(Some(7), None)),
                "{shown}"
            ),
            Ok(_) => panic!("{shown}: pane-exit wait did not match"),
            Err(error) => panic!(
                "{shown}: pane-exit wait failed on the gone pane: {}",
                error.message()
            ),
        }
    }
}

#[test]
fn pane_exit_wait_keeps_subscription_errors_that_do_not_name_its_pane() {
    let other_pane = pane_not_found(PANE_ID + 1);
    let other_session = RmuxError::SessionNotFound("elsewhere".to_owned());
    let runtime = RmuxError::Server("missing pane terminal for race:0.0".to_owned());
    for subscription_error in [other_pane, other_session, runtime] {
        let shown = tmux_cli_error_message("stream-pane", &subscription_error);
        let Err(error) = pane_exit_wait_with_lost_subscription(subscription_error) else {
            panic!("{shown}: a foreign subscription error must stay an error")
        };
        assert_eq!(error.exit_code(), 1, "{shown}");
        assert_eq!(error.message(), shown);
    }
}
