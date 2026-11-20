#[cfg(unix)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use rmux_core::Style;
#[cfg(unix)]
use rmux_core::{EnvironmentStore, OptionStore};
use rmux_proto::TerminalSize;
#[cfg(unix)]
use rmux_pty::{PtyPair, Signal};

#[cfg(unix)]
use crate::terminal::TerminalProfile;

#[cfg(unix)]
use super::{popup_job_command, read_async_fd, spawn_popup_job};
use super::{PopupIoOperation, PopupIoQueue, PopupSurface, POPUP_IO_QUEUE_CAPACITY};

#[test]
fn popup_surface_frame_captures_rows_cursor_and_visibility_together() {
    let mut surface = PopupSurface::new(TerminalSize { cols: 10, rows: 4 });
    surface.append(b"\x1b[3;5Hhi\x1b[?25l");

    let frame = surface.frame(&Style::default());

    assert_eq!(frame.rows.len(), 4);
    assert_eq!(frame.cursor.x, 6);
    assert_eq!(frame.cursor.y, 2);
    assert!(!frame.cursor.visible);
}

fn release_blocked_io(release: &Arc<(Mutex<bool>, Condvar)>) {
    let (released, released_cv) = &**release;
    *released.lock().expect("I/O release") = true;
    released_cv.notify_all();
}

fn arm_blocked_io_watchdog(release: &Arc<(Mutex<bool>, Condvar)>) {
    let release = Arc::clone(release);
    std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_secs(3));
        release_blocked_io(&release);
    });
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread")]
async fn popup_async_reader_yields_after_stale_readiness() {
    use std::io::Write;

    let pair = PtyPair::open().expect("pty pair");
    let (master, slave) = pair.into_split();
    #[allow(deprecated)] // tokio 1.53.3; the AsyncFd owns the PtyIo.
    let reader = tokio::io::unix::AsyncFd::new(master.into_io()).expect("async pty reader");
    let mut writer = std::fs::File::from(slave.into_owned_fd());
    writer.write_all(b"a").expect("seed popup output");

    let stale_ready = tokio::time::timeout(std::time::Duration::from_secs(1), reader.readable())
        .await
        .expect("seed output should become readable")
        .expect("popup readiness");
    let mut seed = [0_u8; 1];
    assert_eq!(
        reader
            .get_ref()
            .try_read(&mut seed)
            .expect("consume seed outside readiness guard"),
        1
    );
    drop(stale_ready);

    let heartbeat_ran = Arc::new(AtomicBool::new(false));
    let heartbeat_task = {
        let heartbeat_ran = Arc::clone(&heartbeat_ran);
        tokio::spawn(async move {
            tokio::time::sleep(std::time::Duration::from_millis(25)).await;
            heartbeat_ran.store(true, Ordering::SeqCst);
        })
    };
    let writer = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        writer.write_all(b"b").expect("write fresh popup output");
    });

    let mut fresh = [0_u8; 1];
    let bytes_read = tokio::time::timeout(
        std::time::Duration::from_secs(2),
        read_async_fd(&reader, &mut fresh),
    )
    .await
    .expect("popup read should remain cancellable")
    .expect("fresh popup read");

    assert_eq!(bytes_read, 1);
    assert_eq!(fresh, *b"b");
    assert!(
        heartbeat_ran.load(Ordering::SeqCst),
        "stale readiness must not block the current-thread runtime"
    );
    heartbeat_task.await.expect("heartbeat task");
    writer.join().expect("popup writer thread");
}

#[cfg(unix)]
#[tokio::test]
async fn popup_termination_escalates_when_the_job_ignores_hangup() {
    assert_popup_termination(
        "trap '' HUP TERM; printf '%s' \"$$\" >\"$RMUX_POPUP_READY\"; while :; do sleep 1; done",
        "leader",
        PopupTerminationTrigger::Explicit,
    )
    .await;
}

#[cfg(unix)]
#[tokio::test]
async fn popup_termination_kills_resistant_descendant_after_leader_exits() {
    assert_popup_termination(
        "sh -c 'trap \"\" HUP TERM; printf \"%s\" \"$$\" >\"$RMUX_POPUP_READY\"; while :; do sleep 1; done' & wait",
        "descendant",
        PopupTerminationTrigger::Explicit,
    )
    .await;
}

#[cfg(unix)]
#[tokio::test]
async fn dropping_popup_job_terminates_its_process() {
    assert_popup_termination(
        "trap '' HUP TERM; printf '%s' \"$$\" >\"$RMUX_POPUP_READY\"; while :; do sleep 1; done",
        "drop",
        PopupTerminationTrigger::Drop,
    )
    .await;
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum PopupTerminationTrigger {
    Explicit,
    Drop,
}

#[cfg(unix)]
async fn assert_popup_termination(command: &str, label: &str, trigger: PopupTerminationTrigger) {
    let nonce = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("clock after Unix epoch")
        .as_nanos();
    let marker = std::env::temp_dir().join(format!(
        "rmux-popup-termination-{label}-{}-{nonce}.ready",
        std::process::id(),
    ));
    let socket = marker.with_extension("sock");
    let profile = TerminalProfile::for_run_shell(
        &EnvironmentStore::new(),
        &OptionStore::new(),
        None,
        None,
        &socket,
        false,
        Some(std::env::temp_dir().as_path()),
    )
    .expect("popup terminal profile");
    let environment = [format!("RMUX_POPUP_READY={}", marker.display())];
    let (job, _) = spawn_popup_job(
        &rmux_pty::ProcessShell,
        TerminalSize { cols: 20, rows: 6 },
        &profile,
        Some(command),
        &environment,
    )
    .expect("spawn signal-resistant popup job");

    let mut resistant_pid = None;
    for _ in 0..100 {
        resistant_pid = std::fs::read_to_string(&marker)
            .ok()
            .and_then(|value| value.parse::<i32>().ok());
        if resistant_pid.is_some() {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }
    let resistant_pid = resistant_pid.expect("popup command did not reach its ready state");
    assert!(unix_process_exists(resistant_pid));

    let process = job.process.control.clone();
    match trigger {
        PopupTerminationTrigger::Explicit => {
            job.terminate();
            job.terminate();
            drop(job);
        }
        PopupTerminationTrigger::Drop => drop(job),
    }
    let mut stopped = false;
    for _ in 0..200 {
        if !process.child_is_running() && !unix_process_exists(resistant_pid) {
            stopped = true;
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(10)).await;
    }

    let child = process.child.lock().expect("popup child").take();
    if let Some(mut child) = child {
        if !stopped && child.kill(Signal::KILL).is_err() {
            let _ = child.kill_session_leader(Signal::KILL);
        }
        let _ = child.wait();
    }
    let _ = std::fs::remove_file(&marker);
    assert!(stopped, "popup job survived its bounded termination path");
}

#[cfg(unix)]
fn unix_process_exists(pid: i32) -> bool {
    rustix::process::Pid::from_raw(pid)
        .is_some_and(|pid| rustix::process::test_kill_process(pid).is_ok())
}

/// Runs `command` in its own PTY and returns what its shell printed for `$0`, between < and >.
#[cfg(unix)]
fn dollar_zero_of(command: rmux_pty::ChildCommand) -> String {
    let (master, mut child) = command.spawn().expect("spawn shell").into_parts();
    let io = master.into_io();
    io.set_nonblocking().expect("nonblocking shell pty");
    let mut output = Vec::new();
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
    while !output.contains(&b'>') && std::time::Instant::now() < deadline {
        let mut buffer = [0_u8; 64];
        match io.try_read(&mut buffer) {
            Ok(0) => break,
            Ok(read) => output.extend_from_slice(&buffer[..read]),
            Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                std::thread::sleep(std::time::Duration::from_millis(10));
            }
            Err(_) => break,
        }
    }
    child.wait().expect("reap shell");
    String::from_utf8_lossy(&output).into_owned()
}

#[cfg(unix)]
fn sh_profile() -> TerminalProfile {
    TerminalProfile::for_run_shell(
        &EnvironmentStore::new(),
        &OptionStore::new(),
        None,
        None,
        std::path::Path::new("/nonexistent/rmux-argv0.sock"),
        false,
        Some(std::path::Path::new("/")),
    )
    .expect("terminal profile")
    .with_respawn_shell(std::path::PathBuf::from("/bin/sh"))
}

#[cfg(unix)]
#[test]
fn popup_job_shell_sees_its_file_name_as_dollar_zero() {
    let command = popup_job_command(&sh_profile(), Some("printf '<%s>' \"$0\""));
    assert_eq!(dollar_zero_of(command), "<sh>");
}

#[cfg(unix)]
#[test]
fn pane_shell_command_keeps_the_shell_path_as_dollar_zero() {
    let command = sh_profile().shell_child_command("printf '<%s>' \"$0\"");
    assert_eq!(dollar_zero_of(command), "</bin/sh>");
}

#[tokio::test]
async fn popup_io_queue_preserves_write_resize_write_enqueue_order() {
    let observed = Arc::new(Mutex::new(Vec::<String>::new()));
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    arm_blocked_io_watchdog(&release);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started_tx = Arc::new(Mutex::new(Some(started_tx)));
    let callback_observed = Arc::clone(&observed);
    let callback_release = Arc::clone(&release);
    let callback_started = Arc::clone(&started_tx);
    let queue = PopupIoQueue::spawn(move |operation| {
        let label = match operation {
            PopupIoOperation::Write(bytes) => {
                format!("write:{}", String::from_utf8_lossy(&bytes))
            }
            PopupIoOperation::Resize(size) => {
                format!("resize:{}x{}", size.cols, size.rows)
            }
        };
        let first = {
            let mut observed = callback_observed.lock().expect("observed popup I/O");
            observed.push(label);
            observed.len() == 1
        };
        if first {
            if let Some(started_tx) = callback_started.lock().expect("I/O start").take() {
                let _ = started_tx.send(());
            }
            let (released, released_cv) = &*callback_release;
            let mut released = released.lock().expect("I/O release");
            while !*released {
                released = released_cv.wait(released).expect("I/O release wait");
            }
        }
        Ok(())
    });

    let first = queue
        .enqueue(PopupIoOperation::Write(b"a".to_vec()))
        .expect("enqueue first write");
    let started = tokio::time::timeout(std::time::Duration::from_secs(2), started_rx).await;
    let second = queue
        .enqueue(PopupIoOperation::Resize(TerminalSize {
            cols: 41,
            rows: 17,
        }))
        .expect("enqueue resize");
    let third = queue
        .enqueue(PopupIoOperation::Write(b"b".to_vec()))
        .expect("enqueue second write");
    release_blocked_io(&release);
    started
        .expect("first popup I/O should start")
        .expect("popup I/O start sender should remain connected");

    let (first, second, third) = tokio::join!(
        within_wall_clock(first.wait(), "the first write receipt"),
        within_wall_clock(second.wait(), "the resize receipt"),
        within_wall_clock(third.wait(), "the second write receipt"),
    );
    first.expect("first write completes");
    second.expect("resize completes");
    third.expect("second write completes");
    assert_eq!(
        *observed.lock().expect("observed popup I/O"),
        ["write:a", "resize:41x17", "write:b"]
    );
}

#[tokio::test]
async fn popup_io_receipt_times_out_when_blocking_write_never_acknowledges() {
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    arm_blocked_io_watchdog(&release);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started_tx = Arc::new(Mutex::new(Some(started_tx)));
    let callback_release = Arc::clone(&release);
    let callback_started = Arc::clone(&started_tx);
    let cancellations = Arc::new(AtomicUsize::new(0));
    let cancellation_count = Arc::clone(&cancellations);
    let queue = PopupIoQueue::spawn_with_cancel(
        move |_| {
            if let Some(started_tx) = callback_started.lock().expect("I/O start").take() {
                let _ = started_tx.send(());
            }
            let (released, released_cv) = &*callback_release;
            let mut released = released.lock().expect("I/O release");
            while !*released {
                released = released_cv.wait(released).expect("I/O release wait");
            }
            Ok(())
        },
        move || {
            cancellation_count.fetch_add(1, Ordering::AcqRel);
        },
    );
    let active = queue
        .enqueue(PopupIoOperation::Write(b"blocked".to_vec()))
        .expect("enqueue blocked write");
    tokio::time::timeout(std::time::Duration::from_secs(2), started_rx)
        .await
        .expect("blocking popup I/O should start")
        .expect("popup I/O start sender should remain connected");
    let pending = queue
        .enqueue(PopupIoOperation::Write(b"pending".to_vec()))
        .expect("enqueue pending write");

    let error = within_wall_clock(active.wait(), "the blocked write receipt")
        .await
        .expect_err("blocked popup I/O must have a deadline");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert_eq!(cancellations.load(Ordering::Acquire), 1);
    let pending_error = within_wall_clock(pending.wait(), "the queued write receipt")
        .await
        .expect_err("timeout must drain queued popup I/O");
    assert_eq!(pending_error.kind(), std::io::ErrorKind::Interrupted);
    assert_eq!(cancellations.load(Ordering::Acquire), 1);

    release_blocked_io(&release);
}

#[tokio::test]
async fn popup_io_queue_saturation_cancels_active_and_pending_work() {
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    arm_blocked_io_watchdog(&release);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started_tx = Arc::new(Mutex::new(Some(started_tx)));
    let callback_started = Arc::clone(&started_tx);
    let callback_release = Arc::clone(&release);
    let executions = Arc::new(AtomicUsize::new(0));
    let execution_count = Arc::clone(&executions);
    let cancellations = Arc::new(AtomicUsize::new(0));
    let cancellation_count = Arc::clone(&cancellations);
    let queue = PopupIoQueue::spawn_with_cancel(
        move |_| {
            execution_count.fetch_add(1, Ordering::AcqRel);
            if let Some(started_tx) = callback_started.lock().expect("I/O start").take() {
                let _ = started_tx.send(());
            }
            let (released, released_cv) = &*callback_release;
            let mut released = released.lock().expect("I/O release");
            while !*released {
                released = released_cv.wait(released).expect("I/O release wait");
            }
            Ok(())
        },
        move || {
            cancellation_count.fetch_add(1, Ordering::AcqRel);
        },
    );

    let active = queue
        .enqueue(PopupIoOperation::Write(b"active".to_vec()))
        .expect("enqueue active write");
    tokio::time::timeout(std::time::Duration::from_secs(2), started_rx)
        .await
        .expect("blocking popup I/O should start")
        .expect("popup I/O start sender should remain connected");
    let pending = (0..POPUP_IO_QUEUE_CAPACITY)
        .map(|index| {
            queue
                .enqueue(PopupIoOperation::Write(vec![index as u8]))
                .expect("enqueue bounded pending write")
        })
        .collect::<Vec<_>>();
    let saturated = queue
        .enqueue(PopupIoOperation::Write(b"overflow".to_vec()))
        .expect("saturation is reported through the receipt");

    let saturated_error = within_wall_clock(saturated.wait(), "the saturated write receipt")
        .await
        .expect_err("a saturated popup queue must fail closed");
    assert_eq!(saturated_error.kind(), std::io::ErrorKind::WouldBlock);
    let active_error = within_wall_clock(active.wait(), "the active write receipt")
        .await
        .expect_err("saturation must cancel the active popup write");
    assert_eq!(active_error.kind(), std::io::ErrorKind::Interrupted);
    for receipt in pending {
        let error = within_wall_clock(receipt.wait(), "a queued write receipt")
            .await
            .expect_err("saturation must release every queued receipt");
        assert_eq!(error.kind(), std::io::ErrorKind::Interrupted);
    }
    assert_eq!(cancellations.load(Ordering::Acquire), 1);
    assert_eq!(executions.load(Ordering::Acquire), 1);
    assert_eq!(
        queue
            .enqueue(PopupIoOperation::Resize(TerminalSize { cols: 2, rows: 2 }))
            .expect_err("cancelled queue must reject new work")
            .kind(),
        std::io::ErrorKind::BrokenPipe
    );

    release_blocked_io(&release);
}

#[tokio::test]
async fn dropping_last_popup_io_queue_cancels_worker_and_releases_receipts() {
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    arm_blocked_io_watchdog(&release);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started_tx = Arc::new(Mutex::new(Some(started_tx)));
    let callback_started = Arc::clone(&started_tx);
    let callback_release = Arc::clone(&release);
    let cancellations = Arc::new(AtomicUsize::new(0));
    let cancellation_count = Arc::clone(&cancellations);
    let queue = PopupIoQueue::spawn_with_cancel(
        move |_| {
            if let Some(started_tx) = callback_started.lock().expect("I/O start").take() {
                let _ = started_tx.send(());
            }
            let (released, released_cv) = &*callback_release;
            let mut released = released.lock().expect("I/O release");
            while !*released {
                released = released_cv.wait(released).expect("I/O release wait");
            }
            Ok(())
        },
        move || {
            cancellation_count.fetch_add(1, Ordering::AcqRel);
        },
    );
    let active = queue
        .enqueue(PopupIoOperation::Write(b"active".to_vec()))
        .expect("enqueue active write");
    tokio::time::timeout(std::time::Duration::from_secs(2), started_rx)
        .await
        .expect("blocking popup I/O should start")
        .expect("popup I/O start sender should remain connected");
    let pending = queue
        .enqueue(PopupIoOperation::Write(b"pending".to_vec()))
        .expect("enqueue pending write");

    drop(queue);

    assert_eq!(
        cancellations.load(Ordering::Acquire),
        0,
        "dropping a superseded queue stops its worker without killing a shared process"
    );
    let active_error = within_wall_clock(active.wait(), "the active write receipt")
        .await
        .expect_err("queue drop must release active receipt");
    assert_eq!(active_error.kind(), std::io::ErrorKind::Interrupted);
    let pending_error = within_wall_clock(pending.wait(), "the pending write receipt")
        .await
        .expect_err("queue drop must release pending receipt");
    assert_eq!(pending_error.kind(), std::io::ErrorKind::Interrupted);
    assert_eq!(cancellations.load(Ordering::Acquire), 1);

    release_blocked_io(&release);
}

#[tokio::test]
async fn dropping_unacknowledged_popup_io_receipt_cancels_worker() {
    let release = Arc::new((Mutex::new(false), Condvar::new()));
    arm_blocked_io_watchdog(&release);
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let started_tx = Arc::new(Mutex::new(Some(started_tx)));
    let callback_started = Arc::clone(&started_tx);
    let callback_release = Arc::clone(&release);
    let cancellations = Arc::new(AtomicUsize::new(0));
    let cancellation_count = Arc::clone(&cancellations);
    let queue = PopupIoQueue::spawn_with_cancel(
        move |_| {
            if let Some(started_tx) = callback_started.lock().expect("I/O start").take() {
                let _ = started_tx.send(());
            }
            let (released, released_cv) = &*callback_release;
            let mut released = released.lock().expect("I/O release");
            while !*released {
                released = released_cv.wait(released).expect("I/O release wait");
            }
            Ok(())
        },
        move || {
            cancellation_count.fetch_add(1, Ordering::AcqRel);
        },
    );
    let receipt = queue
        .enqueue(PopupIoOperation::Write(b"active".to_vec()))
        .expect("enqueue active write");
    tokio::time::timeout(std::time::Duration::from_secs(2), started_rx)
        .await
        .expect("blocking popup I/O should start")
        .expect("popup I/O start sender should remain connected");

    drop(receipt);

    assert_eq!(cancellations.load(Ordering::Acquire), 1);
    assert_eq!(
        queue
            .enqueue(PopupIoOperation::Write(b"late".to_vec()))
            .expect_err("receipt drop must stop future popup I/O")
            .kind(),
        std::io::ErrorKind::BrokenPipe
    );
    release_blocked_io(&release);
}

#[cfg(windows)]
mod windows {
    use std::sync::mpsc;
    use std::thread;
    use std::time::Duration;

    use rmux_pty::{ChildCommand, TerminalSize as PtyTerminalSize};

    use super::super::spawn_popup_windows_teardown;

    fn reader_completion(master: rmux_pty::PtyMaster) -> mpsc::Receiver<()> {
        let reader = master.into_io();
        let (reader_done_tx, reader_done_rx) = mpsc::channel();
        thread::spawn(move || {
            let mut buffer = [0_u8; 1024];
            loop {
                match reader.read(&mut buffer) {
                    Ok(0) | Err(_) => {
                        let _ = reader_done_tx.send(());
                        return;
                    }
                    Ok(_) => {}
                }
            }
        });
        reader_done_rx
    }

    #[test]
    fn popup_natural_exit_closes_pseudoconsole_and_releases_reader(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let spawned = ChildCommand::new(r"C:\Windows\System32\cmd.exe")
            .args(["/D", "/C", "exit 0"])
            .size(PtyTerminalSize::new(20, 6))
            .spawn()?;
        let (master, mut child) = spawned.into_parts();
        let reader_done = reader_completion(master);
        let close_child = child.try_clone_for_wait()?;
        assert!(child.wait()?.success());
        spawn_popup_windows_teardown(Some(child), Some(close_child));
        reader_done.recv_timeout(Duration::from_secs(5))?;
        Ok(())
    }

    #[test]
    fn popup_shutdown_drops_job_before_closing_pseudoconsole(
    ) -> Result<(), Box<dyn std::error::Error>> {
        let spawned = ChildCommand::new(r"C:\Windows\System32\cmd.exe")
            .args(["/D", "/Q", "/C", "ping -n 120 127.0.0.1 >NUL"])
            .size(PtyTerminalSize::new(20, 6))
            .spawn()?;
        let (master, child) = spawned.into_parts();
        let reader_done = reader_completion(master);
        let close_child = child.try_clone_for_wait()?;
        spawn_popup_windows_teardown(Some(child), Some(close_child));
        reader_done.recv_timeout(Duration::from_secs(5))?;
        Ok(())
    }
}

const PRODUCT_WAIT_WALL_CLOCK_BOUND: std::time::Duration = std::time::Duration::from_secs(10);

/// Awaits a product future under a real wall-clock bound, so a regression that
/// never completes fails by name instead of hanging the test binary. Under a
/// paused Tokio clock, a `tokio::time` timeout auto-advances as soon as the
/// runtime idles and can expire before the kernel delivers bytes, while an
/// unbounded await parks forever on a future that never completes.
async fn within_wall_clock<F: std::future::Future>(future: F, awaited: &str) -> F::Output {
    within_wall_clock_for(future, awaited, PRODUCT_WAIT_WALL_CLOCK_BOUND)
        .await
        .unwrap_or_else(|failure| panic!("{failure}"))
}

async fn within_wall_clock_for<F: std::future::Future>(
    future: F,
    awaited: &str,
    wall_clock_bound: std::time::Duration,
) -> Result<F::Output, String> {
    let (expired_sender, expired) = tokio::sync::oneshot::channel::<()>();
    let (cancel, cancelled) = std::sync::mpsc::channel::<()>();
    let watchdog = std::thread::spawn(move || {
        if let Err(std::sync::mpsc::RecvTimeoutError::Timeout) =
            cancelled.recv_timeout(wall_clock_bound)
        {
            let _ = expired_sender.send(());
        }
    });
    let outcome = tokio::select! {
        biased;
        output = future => Ok(output),
        _ = expired => Err(format!(
            "did not complete within {wall_clock_bound:?} of wall-clock time \
             while waiting for {awaited}"
        )),
    };
    drop(cancel);
    watchdog.join().expect("wall-clock watchdog");
    outcome
}

#[cfg(unix)]
async fn pty_readable<'a>(
    reader: &'a tokio::io::unix::AsyncFd<rmux_pty::PtyIo>,
    awaited: &str,
) -> tokio::io::unix::AsyncFdReadyGuard<'a, rmux_pty::PtyIo> {
    within_wall_clock(reader.readable(), &format!("PTY readiness for {awaited}"))
        .await
        .unwrap_or_else(|error| panic!("PTY readiness failed while waiting for {awaited}: {error}"))
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn wall_clock_guard_names_a_product_wait_that_never_completes() {
    let (master, slave) = PtyPair::open().unwrap().into_split();
    #[allow(deprecated)] // tokio 1.53.3; the AsyncFd owns the PtyIo.
    let reader = tokio::io::unix::AsyncFd::new(master.into_io()).unwrap();
    let _silent_writer = std::fs::File::from(slave.into_owned_fd());
    // A pending simulated timer lets the paused clock auto-advance while the
    // runtime idles; the bound must still elapse in wall-clock time.
    let simulated_day = tokio::spawn(tokio::time::sleep(std::time::Duration::from_secs(86_400)));
    let wall_clock_bound = std::time::Duration::from_millis(200);
    let started = std::time::Instant::now();
    let failure = within_wall_clock_for(
        super::read_popup_batch(&reader),
        "silent popup output",
        wall_clock_bound,
    )
    .await
    .expect_err("a popup batch without output must not complete");
    assert!(
        started.elapsed() >= wall_clock_bound,
        "the wall-clock bound expired after {:?}",
        started.elapsed()
    );
    assert!(failure.contains("silent popup output"), "{failure}");
    simulated_day.abort();
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn popup_reader_batches_split_output_without_waiting_for_eof() {
    use std::future::{poll_fn, Future};
    use std::io::Write;
    use std::task::Poll;
    use std::time::Duration;

    let pair = PtyPair::open().unwrap();
    let (master, slave) = pair.into_split();
    #[allow(deprecated)] // tokio 1.53.3; the AsyncFd owns the PtyIo.
    let reader = tokio::io::unix::AsyncFd::new(master.into_io()).unwrap();
    let mut writer = std::fs::File::from(slave.into_owned_fd());
    writer.write_all(b"first").unwrap();
    drop(pty_readable(&reader, "the first fragment").await);
    let mut batch = std::pin::pin!(super::read_popup_batch(&reader));
    // Consume the first fragment and arm the batching timer before writing the
    // second. A wall-clock sleep in a competing task does not establish this
    // order: under load both timers can expire before that task runs again.
    poll_fn(|cx| {
        assert!(batch.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    pty_readable(&reader, "the consumed first fragment")
        .await
        .clear_ready();
    writer.write_all(b"second").unwrap();
    // A completed slave write alone need not mean the master is readable yet.
    drop(pty_readable(&reader, "the second fragment").await);
    let batch = within_wall_clock(
        tokio::time::timeout(Duration::from_secs(1), batch),
        "the split-output batch",
    )
    .await
    .expect("the split-output batch exceeded its simulated 1s bound")
    .unwrap();
    assert_eq!(batch, b"firstsecond");
    assert!(
        within_wall_clock(
            tokio::time::timeout(Duration::from_millis(25), super::read_popup_batch(&reader)),
            "the idle split-output read",
        )
        .await
        .is_err(),
        "an idle popup must sleep rather than render empty ticks"
    );
}

#[cfg(unix)]
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn popup_reader_preserves_fragment_when_writer_wakes_after_batch_deadline() {
    use std::future::{poll_fn, Future};
    use std::io::Write;
    use std::task::Poll;
    use std::time::Duration;

    let (master, slave) = PtyPair::open().unwrap().into_split();
    #[allow(deprecated)] // tokio 1.53.3; the AsyncFd owns the PtyIo.
    let reader = tokio::io::unix::AsyncFd::new(master.into_io()).unwrap();
    let mut writer = std::fs::File::from(slave.into_owned_fd());
    writer.write_all(b"first").unwrap();
    drop(pty_readable(&reader, "the first fragment").await);
    let mut batch = std::pin::pin!(super::read_popup_batch(&reader));
    poll_fn(|cx| {
        assert!(batch.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    let mut write = std::pin::pin!(async {
        tokio::time::sleep(Duration::from_millis(1)).await;
        writer.write_all(b"second").unwrap();
    });
    poll_fn(|cx| {
        assert!(write.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    // Reproduce both timers becoming ready with the reader scheduled first.
    tokio::time::advance(Duration::from_millis(8)).await;
    let first = within_wall_clock(batch, "the batch holding the first fragment")
        .await
        .unwrap();
    assert_eq!(first, b"first");
    pty_readable(&reader, "the consumed first fragment")
        .await
        .clear_ready();
    write.await;
    drop(pty_readable(&reader, "the late second fragment").await);
    let second = within_wall_clock(
        tokio::time::timeout(Duration::from_secs(1), super::read_popup_batch(&reader)),
        "the batch holding the late second fragment",
    )
    .await
    .expect("the late second batch exceeded its simulated 1s bound")
    .unwrap();
    assert_eq!(second, b"second");
    assert_eq!([first, second].concat(), b"firstsecond");
    assert!(
        within_wall_clock(
            tokio::time::timeout(Duration::from_millis(25), super::read_popup_batch(&reader)),
            "the idle read after the late fragment",
        )
        .await
        .is_err(),
        "both fragments must be consumed exactly once without waiting for EOF"
    );
}

// A loaded host can install a popup after its job has printed (measured: 0.75 s
// after spawn). When a job's exit closes the last slave before any master read,
// Darwin waits about 0.6 s for a reader and then discards the queued output;
// `zsh -c` jobs exit this way (`sh` and `bash` keep waiting), and so does the
// job below, since /bin/echo as the shell writes and exits directly.
#[cfg(unix)]
#[tokio::test]
async fn popup_keeps_the_output_of_a_job_that_exits_before_its_reader_is_armed() {
    let profile = sh_profile().with_respawn_shell(std::path::PathBuf::from("/bin/echo"));
    let (job, _) = spawn_popup_job(
        &rmux_pty::ProcessShell,
        TerminalSize { cols: 20, rows: 2 },
        &profile,
        Some("ABCD"),
        &[],
    )
    .expect("spawn popup job");
    // Arm the reader only once the job has exited, or after two seconds if the
    // job is still waiting for its output to be read.
    let started = std::time::Instant::now();
    while job.child_is_running_for_test() && started.elapsed() < std::time::Duration::from_secs(2) {
        tokio::time::sleep(std::time::Duration::from_millis(5)).await;
    }

    #[allow(deprecated)] // tokio 1.53.3; the AsyncFd owns the PtyIo.
    let reader =
        tokio::io::unix::AsyncFd::new(job.reader.try_clone().expect("clone popup reader")).unwrap();
    let mut output = super::arm_popup_reader(&reader, &job.reader);
    while let Ok(batch) = within_wall_clock(super::read_popup_batch(&reader), "popup output").await
    {
        if batch.is_empty() {
            break;
        }
        output.extend_from_slice(&batch);
    }
    assert_eq!(String::from_utf8_lossy(&output), "-c ABCD\r\n");
}

// The popup reader closes on either PTY hangup spelling: an empty read or EIO
// (`reader.rs` treats both alike, as does the pane reader). Which one a master
// returns after its slave closes is backend behavior, not popup behavior, so
// this witness stays Unix-neutral instead of adding a target_os selection.
#[cfg(unix)]
#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn popup_reader_keeps_batch_before_pty_eof() {
    use std::future::{poll_fn, Future};
    use std::io::Write;
    use std::task::Poll;
    use std::time::Duration;

    let (master, slave) = PtyPair::open().unwrap().into_split();
    #[allow(deprecated)] // tokio 1.53.3; the AsyncFd owns the PtyIo.
    let reader = tokio::io::unix::AsyncFd::new(master.into_io()).unwrap();
    let mut writer = std::fs::File::from(slave.into_owned_fd());
    writer.write_all(b"tail").unwrap();
    drop(pty_readable(&reader, "the tail fragment").await);
    let mut batch = std::pin::pin!(super::read_popup_batch(&reader));
    poll_fn(|cx| {
        assert!(batch.as_mut().poll(cx).is_pending());
        Poll::Ready(())
    })
    .await;
    drop(writer);
    assert_eq!(
        within_wall_clock(batch, "the tail batch before EOF")
            .await
            .unwrap(),
        b"tail"
    );
    // Closing our slave fd need not hang up the master at once: a process
    // forked meanwhile, by any concurrent test, shares the slave until it
    // execs. A simulated timeout cannot bound that delivery, since the paused
    // clock jumps to it as soon as the runtime idles. No timer is pending
    // here, so the clock only moves if the product waits on one: bound that
    // simulated time, and the kernel delivery by wall-clock time.
    let eof_started = tokio::time::Instant::now();
    let eof = within_wall_clock(super::read_popup_batch(&reader), "the PTY EOF read").await;
    assert!(
        eof_started.elapsed() < Duration::from_secs(1),
        "the PTY EOF read waited {:?} of simulated time",
        eof_started.elapsed()
    );
    match eof {
        Ok(rest) => assert!(rest.is_empty(), "no bytes may follow the hangup: {rest:?}"),
        Err(error) => assert_eq!(
            error.raw_os_error(),
            Some(rustix::io::Errno::IO.raw_os_error()),
            "a PTY hangup error must be EIO: {error}"
        ),
    }
}
