use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;

use super::*;

#[cfg(unix)]
use crate::handler::shell_processes::ShellProcessRegistrationError;
#[cfg(unix)]
use rmux_os::process_tree::ProcessTreeChild;

#[test]
fn background_task_limiter_releases_capacity_when_permits_drop() {
    let limiter = BackgroundTaskLimiter::new(2);
    let first = limiter.try_acquire().expect("first permit");
    let second = limiter.try_acquire().expect("second permit");
    let error = limiter
        .try_acquire()
        .expect_err("third permit should exceed capacity");
    assert!(
        error.to_string().contains("too many background tasks"),
        "unexpected error: {error}"
    );

    drop(first);
    let third = limiter
        .try_acquire()
        .expect("dropped permit should restore capacity");
    drop(second);
    drop(third);
}

#[test]
fn shutdown_cancels_and_joins_a_started_background_task() {
    let registry = BackgroundTaskRegistry::new();
    let (started_tx, started_rx) = mpsc::channel();
    let (dropped_tx, dropped_rx) = mpsc::channel();
    registry
        .spawn("rmux-background-registry-test", move || async move {
            let _drop_signal = DropSignal(Some(dropped_tx));
            started_tx.send(()).expect("report task startup");
            std::future::pending::<()>().await;
        })
        .expect("spawn tracked task");
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("background task starts");

    let unfinished = registry.begin_shutdown().join(Duration::from_secs(1));

    assert!(unfinished.is_empty(), "unfinished tasks: {unfinished:?}");
    dropped_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("task future is dropped during shutdown");
    assert!(
        registry
            .spawn("rmux-background-after-close", || async {})
            .is_err(),
        "closed registry must reject new tasks"
    );
}

#[test]
fn lifecycle_worker_pending_is_cancelled_and_releases_its_registration() {
    let handler = RequestHandler::new();
    let (started_tx, started_rx) = mpsc::channel();
    let (dropped_tx, dropped_rx) = mpsc::channel();
    handler
        .spawn_lifecycle_producer_task("rmux-lifecycle-worker-pending-test", move || async move {
            let _drop_signal = DropSignal(Some(dropped_tx));
            started_tx.send(()).expect("report lifecycle worker start");
            std::future::pending::<()>().await;
        })
        .expect("spawn lifecycle worker");
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("lifecycle worker starts");

    let close_handler = handler.clone();
    let (closed_tx, closed_rx) = mpsc::channel();
    let close = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("lifecycle close runtime")
            .block_on(close_handler.close_normal_and_drain_lifecycle_producers());
        closed_tx.send(()).expect("report lifecycle close");
    });

    dropped_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("pending lifecycle worker is cancelled before the lane drains");
    closed_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("normal lifecycle close is bounded");
    close.join().expect("normal lifecycle close joins");
    assert!(
        handler
            .reserve_lifecycle_producer_task("rmux-lifecycle-worker-after-close")
            .is_err(),
        "normal producer registration stays sealed after close"
    );
    handler.shutdown_background_tasks_for_drop();
}

#[test]
fn lifecycle_worker_close_drains_an_active_mutation() {
    let handler = RequestHandler::new();
    let registration = handler
        .reserve_lifecycle_producer_task("rmux-lifecycle-worker-mutation-test")
        .expect("reserve lifecycle worker");
    let mut cancellation = registration.cancellation();
    let (started_tx, started_rx) = mpsc::channel();
    let release = Arc::new(Notify::new());
    let published = Arc::new(AtomicBool::new(false));
    handler
        .spawn_registered_lifecycle_producer_task(
            "rmux-lifecycle-worker-mutation-test",
            registration,
            {
                let release = Arc::clone(&release);
                let published = Arc::clone(&published);
                move || async move {
                    let _mutation =
                        super::super::lifecycle_producer_tasks::begin_current_lifecycle_mutation()
                            .expect("worker mutation admitted");
                    started_tx.send(()).expect("report worker mutation");
                    release.notified().await;
                    published.store(true, Ordering::SeqCst);
                }
            },
        )
        .expect("spawn mutating lifecycle worker");
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("worker mutation starts");

    let close_handler = handler.clone();
    let (closed_tx, closed_rx) = mpsc::channel();
    let close = std::thread::spawn(move || {
        tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .build()
            .expect("lifecycle close runtime")
            .block_on(close_handler.close_normal_and_drain_lifecycle_producers());
        closed_tx.send(()).expect("report lifecycle close");
    });
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("lifecycle cancellation runtime")
        .block_on(cancellation.cancelled());
    assert!(
        closed_rx.try_recv().is_err(),
        "normal close must drain the active mutation"
    );

    release.notify_one();
    closed_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("normal close finishes after mutation publication");
    close.join().expect("normal lifecycle close joins");
    assert!(published.load(Ordering::SeqCst));
    handler.shutdown_background_tasks_for_drop();
}

#[test]
fn lifecycle_worker_preserves_hook_lane_until_final_close() {
    let handler = RequestHandler::new();
    let registration = handler
        .try_begin_lifecycle_hook_producer()
        .expect("hook producer registered");
    let (started_tx, started_rx) = mpsc::channel();
    let (dropped_tx, dropped_rx) = mpsc::channel();
    handler
        .spawn_registered_lifecycle_producer_task(
            "rmux-lifecycle-hook-worker-test",
            registration,
            move || async move {
                let _drop_signal = DropSignal(Some(dropped_tx));
                started_tx.send(()).expect("report hook worker start");
                std::future::pending::<()>().await;
            },
        )
        .expect("spawn hook-lane worker");
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("hook-lane worker starts");
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("lifecycle lane runtime");

    runtime
        .block_on(async {
            tokio::time::timeout(
                Duration::from_secs(1),
                handler.close_normal_and_drain_lifecycle_producers(),
            )
            .await
        })
        .expect("normal lane close is bounded");
    assert!(
        dropped_rx.try_recv().is_err(),
        "normal close cannot cancel a lifecycle-hook worker"
    );

    runtime
        .block_on(async {
            tokio::time::timeout(
                Duration::from_secs(1),
                handler.close_and_drain_lifecycle_producers(),
            )
            .await
        })
        .expect("final lane close is bounded");
    dropped_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("final close cancels the hook-lane worker");
    handler.shutdown_background_tasks_for_drop();
}

#[test]
fn background_shutdown_cancels_a_pending_opt_in_lifecycle_worker() {
    let handler = RequestHandler::new();
    let registration = handler
        .try_begin_lifecycle_hook_producer()
        .expect("hook producer registered");
    let (started_tx, started_rx) = mpsc::channel();
    let (dropped_tx, dropped_rx) = mpsc::channel();
    handler
        .spawn_registered_lifecycle_producer_task(
            "rmux-lifecycle-background-pending-test",
            registration,
            move || async move {
                let _drop_signal = DropSignal(Some(dropped_tx));
                started_tx.send(()).expect("report lifecycle worker start");
                std::future::pending::<()>().await;
            },
        )
        .expect("spawn lifecycle worker");
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("lifecycle worker starts");

    let unfinished = handler
        .background_tasks
        .begin_shutdown()
        .join(Duration::from_secs(1));

    assert!(unfinished.is_empty(), "unfinished tasks: {unfinished:?}");
    dropped_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("pending lifecycle worker is cancelled");
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("lifecycle close runtime")
        .block_on(handler.close_and_drain_lifecycle_producers());
}

#[test]
fn background_shutdown_drains_an_active_hook_lane_mutation() {
    let handler = RequestHandler::new();
    let registration = handler
        .try_begin_lifecycle_hook_producer()
        .expect("hook producer registered");
    let (started_tx, started_rx) = mpsc::channel();
    let release = Arc::new(Notify::new());
    let published = Arc::new(AtomicBool::new(false));
    handler
        .spawn_registered_lifecycle_producer_task(
            "rmux-lifecycle-background-mutation-test",
            registration,
            {
                let release = Arc::clone(&release);
                let published = Arc::clone(&published);
                move || async move {
                    let _mutation =
                        super::super::lifecycle_producer_tasks::begin_current_lifecycle_mutation()
                            .expect("hook mutation admitted");
                    started_tx.send(()).expect("report lifecycle mutation");
                    release.notified().await;
                    published.store(true, Ordering::SeqCst);
                }
            },
        )
        .expect("spawn mutating lifecycle worker");
    started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("lifecycle mutation starts");

    let shutdown = handler.background_tasks.begin_shutdown();
    release.notify_one();
    let unfinished = shutdown.join(Duration::from_secs(1));

    assert!(unfinished.is_empty(), "unfinished tasks: {unfinished:?}");
    assert!(
        published.load(Ordering::SeqCst),
        "background shutdown must drain an admitted mutation"
    );
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("lifecycle close runtime")
        .block_on(handler.close_and_drain_lifecycle_producers());
}

#[cfg(unix)]
#[test]
fn shutdown_joins_a_task_between_process_spawn_and_registration() {
    assert_shutdown_joins_process_registration_race(ProcessRaceTaskKind::Async);
}

#[cfg(unix)]
#[test]
fn shutdown_joins_a_blocking_task_between_process_spawn_and_registration() {
    assert_shutdown_joins_process_registration_race(ProcessRaceTaskKind::Blocking);
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum ProcessRaceTaskKind {
    Async,
    Blocking,
}

#[cfg(unix)]
fn assert_shutdown_joins_process_registration_race(task_kind: ProcessRaceTaskKind) {
    let registry = BackgroundTaskRegistry::new();
    let shell_processes = Arc::new(super::super::shell_processes::ShellProcessRegistry::new());
    let task_shell_processes = shell_processes.clone();
    let (started_tx, started_rx) = mpsc::channel();
    let (release_tx, release_rx) = mpsc::channel();
    let spawn_result = match task_kind {
        ProcessRaceTaskKind::Async => {
            registry.spawn("rmux-background-spawn-race-test", move || async move {
                run_process_registration_race(task_shell_processes, started_tx, release_rx);
            })
        }
        ProcessRaceTaskKind::Blocking => registry.spawn_blocking_process_task(
            "rmux-blocking-process-spawn-race-test",
            move || {
                run_process_registration_race(task_shell_processes, started_tx, release_rx);
            },
        ),
    };
    spawn_result.expect("spawn tracked race task");
    let (parent_pid, descendant_pid) = started_rx
        .recv_timeout(Duration::from_secs(1))
        .expect("process tree starts before registration");

    let shutdown = registry.begin_shutdown();
    shell_processes.close_and_terminate();
    let joiner = std::thread::spawn(move || shutdown.join(Duration::from_secs(1)));
    std::thread::sleep(Duration::from_millis(50));
    let detached_before_registration_resolved = joiner.is_finished();
    release_tx.send(()).expect("release registration");
    let unfinished = joiner.join().expect("join shutdown worker");

    assert!(
        !detached_before_registration_resolved,
        "shutdown detached the task before registration resolved"
    );
    assert!(unfinished.is_empty(), "unfinished tasks: {unfinished:?}");
    assert!(!rmux_os::process::is_live(parent_pid));
    assert!(!rmux_os::process::is_live(descendant_pid));
}

#[cfg(unix)]
fn run_process_registration_race(
    shell_processes: Arc<super::super::shell_processes::ShellProcessRegistry>,
    started: mpsc::Sender<(u32, u32)>,
    release: mpsc::Receiver<()>,
) {
    let mut command = std::process::Command::new("/bin/sh");
    command
        .args(["-c", "trap '' HUP TERM; sleep 30 & echo $!; wait"])
        .stdin(std::process::Stdio::null())
        .stdout(std::process::Stdio::piped())
        .stderr(std::process::Stdio::null());
    let mut child = ProcessTreeChild::spawn(&mut command).expect("spawn unregistered process tree");
    let parent_pid = child.child_mut().id();
    let stdout = child
        .child_mut()
        .stdout
        .take()
        .expect("capture descendant pid");
    let mut stdout = std::io::BufReader::new(stdout);
    let mut descendant_pid = String::new();
    std::io::BufRead::read_line(&mut stdout, &mut descendant_pid).expect("read descendant pid");
    let descendant_pid = descendant_pid
        .lines()
        .next()
        .expect("descendant pid line")
        .parse::<u32>()
        .expect("numeric descendant pid");
    started
        .send((parent_pid, descendant_pid))
        .expect("report spawned tree");

    // Hold registration across shutdown to model the production race in
    // which the daemon could otherwise detach a just-spawned process tree.
    release.recv().expect("release registration race");
    match shell_processes.register(child.controller()) {
        Err(ShellProcessRegistrationError::Closing) => {
            let _ = child.terminate();
            let _ = child.wait();
        }
        Err(error) => panic!("unexpected registration error: {error:?}"),
        Ok(_guard) => panic!("closed registry accepted a process tree"),
    }
}

struct DropSignal(Option<mpsc::Sender<()>>);

impl Drop for DropSignal {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}
