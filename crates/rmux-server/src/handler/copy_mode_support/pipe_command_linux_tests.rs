use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::time::Duration;

use rmux_os::process_tree_test_support::HeldExit;

use super::run_pipe_command;
use crate::handler::RequestHandler;

#[tokio::test(flavor = "current_thread")]
async fn shutdown_reports_copy_pipe_descendant_held_at_exit() {
    assert_shutdown_reports_held_descendant(false).await;
}

#[tokio::test(flavor = "current_thread")]
async fn shutdown_reports_copy_pipe_descendant_held_at_exit_under_load() {
    assert_shutdown_reports_held_descendant(true).await;
}

/// A killed descendant whose exit the scheduler runs late, as on a loaded
/// host, still counts as stopped once it exits within the shutdown grace.
#[tokio::test(flavor = "current_thread")]
async fn shutdown_waits_for_a_late_copy_pipe_descendant_exit() {
    let handler = RequestHandler::new();
    let pids = start_copy_pipe_tree(&handler, "late").await;
    let mut descendant = HeldExit::seize(pids[1]);
    let (unfinished_tx, unfinished_rx) = std::sync::mpsc::channel();
    let shutdown = std::thread::spawn(move || {
        let unfinished = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("shutdown runtime")
            .block_on(handler.shutdown_background_tasks_and_shell_processes());
        let _ = unfinished_tx.send(unfinished);
    });

    // Hold the exit well past the former 100 ms settle budget. Shutdown must
    // keep waiting for it rather than give up while the descendant still runs.
    descendant.wait_for_exit_stop();
    let gave_up_early = unfinished_rx.recv_timeout(LATE_EXIT_HOLD).ok();
    descendant.release();
    shutdown.join().expect("join shutdown");
    let unfinished = gave_up_early.or_else(|| unfinished_rx.try_recv().ok());
    assert_eq!(
        unfinished,
        Some(Vec::new()),
        "shutdown must wait for a killed descendant that exits late"
    );
    assert!(!rmux_os::process::is_live(pids[0] as u32));
    assert!(!rmux_os::process::is_live(pids[1] as u32));
}

/// Longer than the former settle budget, well inside the shutdown grace.
const LATE_EXIT_HOLD: Duration = Duration::from_millis(500);

async fn start_copy_pipe_tree(handler: &RequestHandler, label: &str) -> Vec<i32> {
    let marker = std::env::temp_dir().join(format!(
        "rmux-copy-pipe-held-exit-{}-{label}",
        std::process::id()
    ));
    let command = format!(
        "sleep 30 & printf '%s %s' \"$$\" \"$!\" > {0}.tmp; mv {0}.tmp {0}; wait",
        marker.display()
    );
    run_pipe_command(handler, "/bin/sh", &command, None, b"selection")
        .await
        .expect("copy-pipe starts");
    tokio::time::timeout(Duration::from_secs(2), async {
        while !marker.exists() {
            tokio::task::yield_now().await;
        }
    })
    .await
    .expect("copy-pipe publishes complete process ids");
    let pids = std::fs::read_to_string(&marker).expect("read process ids");
    std::fs::remove_file(&marker).expect("remove marker");
    pids.split_whitespace()
        .map(|pid| pid.parse().expect("numeric pid"))
        .collect()
}

async fn assert_shutdown_reports_held_descendant(with_load: bool) {
    let handler = RequestHandler::new();
    let pids = start_copy_pipe_tree(&handler, if with_load { "load" } else { "idle" }).await;
    let mut descendant = HeldExit::seize(pids[1]);
    let load = ControlledLoad::new(with_load);
    let shutdown = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("shutdown runtime")
            .block_on(handler.shutdown_background_tasks_and_shell_processes())
    });

    // TRACEEXIT holds this owned descendant before its exit becomes observable.
    // This freezes the scheduling window after group SIGKILL, without sleeps,
    // retries, or relying on how quickly Linux schedules the descendant.
    descendant.wait_for_exit_stop();
    let unfinished = shutdown.join().expect("join shutdown");
    let descendant_live = rmux_os::process::is_live(pids[1] as u32);
    let leader_live = rmux_os::process::is_live(pids[0] as u32);
    descendant.release();
    drop(load);
    eprintln!(
        "held copy-pipe exit: load={with_load}, leader_live={leader_live}, \
         descendant_live={descendant_live}, unfinished={unfinished:?}"
    );
    assert!(!leader_live, "the shell has already stopped");
    assert!(descendant_live, "the descendant exit must still be held");
    assert_eq!(
        unfinished,
        vec!["rmux-shell-process-tree"],
        "shutdown must not report completion while a descendant is still live"
    );
}

struct ControlledLoad {
    stop: Arc<AtomicBool>,
    worker: Option<std::thread::JoinHandle<()>>,
}

impl ControlledLoad {
    fn new(enabled: bool) -> Self {
        let stop = Arc::new(AtomicBool::new(false));
        let worker = enabled.then(|| {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                while !stop.load(Ordering::Relaxed) {
                    std::hint::black_box(1_u64.wrapping_add(1));
                    std::hint::spin_loop();
                }
            })
        });
        Self { stop, worker }
    }
}

impl Drop for ControlledLoad {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(worker) = self.worker.take() {
            worker.join().expect("join controlled CPU load");
        }
    }
}
