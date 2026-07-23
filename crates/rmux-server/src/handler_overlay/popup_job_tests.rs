#[cfg(unix)]
use std::sync::atomic::AtomicBool;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Condvar, Mutex};

#[cfg(unix)]
use rmux_core::{EnvironmentStore, OptionStore};
use rmux_proto::TerminalSize;
#[cfg(unix)]
use rmux_pty::{PtyPair, Signal};

#[cfg(unix)]
use crate::terminal::TerminalProfile;

#[cfg(unix)]
use super::{read_async_fd, spawn_popup_job};
use super::{PopupIoOperation, PopupIoQueue, POPUP_IO_QUEUE_CAPACITY};

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

    let (first, second, third) = tokio::join!(first.wait(), second.wait(), third.wait());
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

    let error = active
        .wait()
        .await
        .expect_err("blocked popup I/O must have a deadline");
    assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    assert_eq!(cancellations.load(Ordering::Acquire), 1);
    let pending_error = pending
        .wait()
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

    let saturated_error = saturated
        .wait()
        .await
        .expect_err("a saturated popup queue must fail closed");
    assert_eq!(saturated_error.kind(), std::io::ErrorKind::WouldBlock);
    let active_error = active
        .wait()
        .await
        .expect_err("saturation must cancel the active popup write");
    assert_eq!(active_error.kind(), std::io::ErrorKind::Interrupted);
    for receipt in pending {
        let error = receipt
            .wait()
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
    let active_error = active
        .wait()
        .await
        .expect_err("queue drop must release active receipt");
    assert_eq!(active_error.kind(), std::io::ErrorKind::Interrupted);
    let pending_error = pending
        .wait()
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
