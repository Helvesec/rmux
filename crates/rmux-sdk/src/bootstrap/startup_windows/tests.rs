use super::mutex::StartupMutexHolder;
use super::name::{startup_mutex_name, test_pipe, validate_pipe_name};
use super::*;
use std::error::Error;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc;
use std::thread;

use rmux_ipc::{LocalListener, LocalStream, MAX_NAMED_MUTEX_LEN};
use rmux_proto::{encode_frame, FrameDecoder, HasSessionResponse, Request, Response};
use tokio::io::{AsyncReadExt, AsyncWriteExt};

type TestResult<T = ()> = Result<T, Box<dyn Error + Send + Sync>>;

static UNIQUE_ENDPOINT: AtomicU64 = AtomicU64::new(0);

fn pipe(label: &str) -> PathBuf {
    test_pipe(label)
}

#[test]
fn validate_pipe_name_accepts_real_endpoint() {
    let pipe = pipe("default");
    validate_pipe_name(&pipe).expect("real pipe name accepted");
}

#[test]
fn validate_pipe_name_rejects_unix_path() {
    let error = validate_pipe_name(Path::new("/tmp/not-a-pipe"))
        .expect_err("unix paths are not Windows pipe names");
    assert!(matches!(error, StartupError::InvalidPipeName { .. }));
}

#[test]
fn validate_pipe_name_rejects_empty_path() {
    let error = validate_pipe_name(Path::new("")).expect_err("empty pipe path is invalid");
    assert!(matches!(error, StartupError::InvalidPipeName { .. }));
}

#[test]
fn startup_mutex_name_strips_pipe_prefix() {
    let pipe = pipe("default");
    let name = startup_mutex_name(&pipe).expect("derive mutex name");
    let value = name.to_string_lossy();
    assert!(value.starts_with(STARTUP_MUTEX_PREFIX));
    assert!(value.ends_with("-default"));
}

#[test]
fn startup_mutex_name_is_case_insensitive() {
    // The Win32 named-pipe namespace is case-insensitive, but the kernel
    // mutex namespace is case-sensitive. Two callers using the same
    // logical pipe with different case must derive the SAME mutex name
    // or they will fail to serialize against each other.
    let lower = PathBuf::from(r"\\.\pipe\rmux-s-1-5-21-1000-il-medium-default");
    let upper = PathBuf::from(r"\\.\PIPE\RMUX-S-1-5-21-1000-IL-MEDIUM-DEFAULT");
    let lower_name = startup_mutex_name(&lower).expect("lower pipe name accepted");
    let upper_name = startup_mutex_name(&upper).expect("upper pipe name accepted");
    assert_eq!(lower_name, upper_name);
}

#[test]
fn startup_mutex_name_rejects_pipe_without_label() {
    let prefix_only = PathBuf::from(PIPE_PREFIX);
    let error = startup_mutex_name(&prefix_only).expect_err("prefix-only path must be rejected");
    assert!(matches!(error, StartupError::InvalidPipeName { .. }));
}

#[test]
fn startup_mutex_name_rejects_oversized_label() {
    let label = "x".repeat(MAX_NAMED_MUTEX_LEN);
    let pipe = PathBuf::from(format!("{PIPE_PREFIX}rmux-{label}"));
    let error = startup_mutex_name(&pipe).expect_err("oversize mutex name must be rejected");
    assert!(matches!(error, StartupError::InvalidMutexName { .. }));
}

#[test]
fn blocking_startup_reuses_pipe_validation() {
    let error = connect_or_start_blocking_with(
        Path::new("/tmp/not-a-pipe"),
        |_| panic!("invalid pipes must not launch"),
        DEFAULT_STARTUP_DEADLINE,
        STARTUP_POLL_INTERVAL,
    )
    .expect_err("invalid pipe should fail before launching");

    assert!(matches!(error, StartupError::InvalidPipeName { .. }));
}

#[test]
fn startup_mutex_holder_release_is_idempotent() {
    // The holder must tolerate `release()` running before `Drop` (and vice
    // versa) without panicking on the second call; the bootstrap fast-path
    // returns the holder by value but loser-paths drop it implicitly.
    let mut holder = StartupMutexHolder {
        release: None,
        thread: None,
    };
    holder.release();
    drop(holder);
}

#[tokio::test]
async fn startup_mutex_holder_releases_on_acquiring_thread() {
    // Build a holder backed by a real dedicated OS thread but driven by a
    // local channel pair so the test can observe that:
    //
    // 1. The release signal is sent from the (potentially) async-runtime
    //    drop site.
    // 2. The holder thread itself observes the signal and performs the
    //    drop work on its own thread.
    //
    // This matches the production code path without needing an actual
    // Win32 mutex (which only exists on Windows).
    let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
    let (observed_tx, observed_rx) = mpsc::channel::<thread::ThreadId>();
    let thread = thread::Builder::new()
        .name("rmux-startup-mutex-test".to_owned())
        .spawn(move || {
            let _ = release_rx.recv();
            let _ = observed_tx.send(thread::current().id());
        })
        .expect("spawn holder test thread");
    let holder_thread_id = thread.thread().id();

    let mut holder = StartupMutexHolder {
        release: Some(release_tx),
        thread: Some(thread),
    };

    // First release: signals the worker.
    holder.release();
    let observed = observed_rx
        .recv_timeout(Duration::from_secs(2))
        .expect("holder thread observes release");
    assert_eq!(
        observed, holder_thread_id,
        "release work must run on the holder thread, not the caller's thread"
    );

    // Second release / drop: must not block or panic even though the
    // worker already exited and the channel is closed.
    holder.release();
    drop(holder);
}

#[tokio::test]
async fn concurrent_loser_reports_the_running_rotated_generation() -> TestResult {
    let unique = UNIQUE_ENDPOINT.fetch_add(1, Ordering::Relaxed);
    let old_endpoint = rmux_ipc::endpoint_for_label(format!(
        "sdk-selected-loser-{}-{unique}",
        std::process::id()
    ))?;
    let old_pipe = old_endpoint.as_path().to_path_buf();
    drop(LocalListener::bind(&old_endpoint)?);

    let reservation = rmux_ipc::reserve_managed_endpoint_start(&old_pipe)?;
    assert!(
        reservation.is_owner(),
        "the setup caller must own the replacement generation"
    );
    let selected_endpoint = reservation.endpoint().clone();
    assert_ne!(selected_endpoint.as_path(), old_pipe);
    let listener = LocalListener::bind(&selected_endpoint)?;
    let server = tokio::spawn(answer_startup_probe(listener));

    let (outcome, reported_endpoint) = connect_or_start_selected_with_timeout(
        &old_pipe,
        |_| async { Err(io::Error::other("joiner must not launch another daemon")) },
        Some(DEFAULT_STARTUP_DEADLINE),
        STARTUP_POLL_INTERVAL,
    )
    .await?;

    assert!(
        matches!(&outcome, StartupOutcome::JoinedExisting(_)),
        "the losing caller must join the running replacement"
    );
    assert_eq!(
        reported_endpoint, selected_endpoint,
        "the losing caller must report the generation it actually joined"
    );
    tokio::task::spawn_blocking(move || drop(outcome)).await?;
    server.await??;
    Ok(())
}

#[tokio::test(flavor = "multi_thread")]
async fn blocking_loser_reports_the_running_rotated_generation() -> TestResult {
    let unique = UNIQUE_ENDPOINT.fetch_add(1, Ordering::Relaxed);
    let old_endpoint = rmux_ipc::endpoint_for_label(format!(
        "sdk-blocking-selected-loser-{}-{unique}",
        std::process::id()
    ))?;
    let old_pipe = old_endpoint.as_path().to_path_buf();
    drop(LocalListener::bind(&old_endpoint)?);

    let reservation = rmux_ipc::reserve_managed_endpoint_start(&old_pipe)?;
    assert!(
        reservation.is_owner(),
        "the setup caller must own the replacement generation"
    );
    let selected_endpoint = reservation.endpoint().clone();
    assert_ne!(selected_endpoint.as_path(), old_pipe);
    let listener = LocalListener::bind(&selected_endpoint)?;
    let server = tokio::spawn(answer_startup_probe(listener));

    let startup_pipe = old_pipe.clone();
    let (outcome, reported_endpoint) = tokio::task::spawn_blocking(move || {
        connect_or_start_blocking_selected_with(
            &startup_pipe,
            |_| Err(io::Error::other("joiner must not launch another daemon")),
            DEFAULT_STARTUP_DEADLINE,
            STARTUP_POLL_INTERVAL,
        )
    })
    .await??;

    assert!(
        matches!(&outcome, StartupOutcome::JoinedExisting(_)),
        "the blocking loser must join the running replacement"
    );
    assert_eq!(
        reported_endpoint, selected_endpoint,
        "blocking startup must report the generation it actually joined"
    );
    tokio::task::spawn_blocking(move || drop(outcome)).await?;
    server.await??;
    Ok(())
}

async fn answer_startup_probe(listener: LocalListener) -> TestResult {
    let (mut stream, _) = listener.accept().await?;
    let request = read_startup_request(&mut stream).await?;
    assert!(matches!(request, Request::HasSession(_)));
    let frame = encode_frame(&Response::HasSession(HasSessionResponse { exists: false }))?;
    stream.write_all(&frame).await?;
    stream.flush().await?;
    Ok(())
}

async fn read_startup_request(stream: &mut LocalStream) -> TestResult<Request> {
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

#[test]
fn recoverable_matrix_matches_documented_contract() {
    let recoverable = [
        StartupError::Mutex {
            pipe_name: pipe("default"),
            source: io::Error::other("mutex"),
        },
        StartupError::MutexTimeout {
            pipe_name: pipe("default"),
            waited: Duration::from_millis(1),
        },
        StartupError::PipeBusy {
            pipe_name: pipe("default"),
        },
        StartupError::PipeNotFound {
            pipe_name: pipe("default"),
        },
        StartupError::PipeNoData {
            pipe_name: pipe("default"),
        },
        StartupError::Launcher {
            source: io::Error::other("launcher"),
        },
        StartupError::StartupTimeout {
            pipe_name: pipe("default"),
            waited: Duration::from_millis(1),
        },
    ];
    for error in recoverable {
        assert!(
            error.is_recoverable(),
            "expected recoverable, got {error:?}"
        );
    }

    let not_recoverable = [
        StartupError::InvalidPipeName {
            reason: "no prefix".into(),
            pipe_name: PathBuf::from("/tmp/x"),
        },
        StartupError::InvalidMutexName {
            reason: "too long".into(),
            pipe_name: pipe("default"),
        },
        StartupError::MutexAccessDenied {
            pipe_name: pipe("default"),
            source: io::Error::other("denied"),
        },
        StartupError::PipeAccessDenied {
            pipe_name: pipe("default"),
        },
        StartupError::PipeIo {
            operation: "stat",
            pipe_name: pipe("default"),
            source: io::Error::other("io"),
        },
    ];
    for error in not_recoverable {
        assert!(
            !error.is_recoverable(),
            "expected non-recoverable, got {error:?}"
        );
    }
}
