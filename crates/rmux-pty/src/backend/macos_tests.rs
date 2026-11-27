use std::sync::Barrier;

use super::open_pty_pair;

// Both allocation salvos are deliberately opt-in. The PTY allocator is global
// to the Darwin kernel, so a salvo makes every other process opening PTYs on
// the host (sibling test binaries, tmux, older RMUX checkouts) fail with the
// leaked errno -6 that this lock and retry only absorb inside this process.
// Run them alone on a quiet host:
// cargo test -p rmux-pty --lib backend::macos::tests -- --ignored --test-threads=1

#[test]
#[ignore = "PTY salvo injects Darwin errno -6 into neighbours; run alone with --ignored"]
fn concurrent_pair_allocation_does_not_leak_darwin_redrive_errors() {
    // Bound live pairs to the existing server-test concurrency, far below the
    // host PTY limit. No child processes or retained pairs are needed to expose
    // the Darwin allocator race.
    const WORKERS: usize = 24;
    const ALLOCATIONS: usize = 128;
    let start = Barrier::new(WORKERS);
    let failures = std::thread::scope(|scope| {
        let workers: Vec<_> = (0..WORKERS)
            .map(|_| {
                scope.spawn(|| {
                    start.wait();
                    let mut failures = Vec::new();
                    for _ in 0..ALLOCATIONS {
                        if let Err(error) = open_pty_pair() {
                            failures.push(error.to_string());
                        }
                    }
                    failures
                })
            })
            .collect();
        workers
            .into_iter()
            .flat_map(|worker| worker.join().expect("allocation worker panicked"))
            .collect::<Vec<_>>()
    });
    assert!(failures.is_empty(), "PTY allocation failures: {failures:?}");
}

// Each process holds its own allocation lock, so only processes allocating at
// the same time expose the kernel race that the lock cannot serialize.
const CROSS_PROCESS_CHILD_ENV: &str = "RMUX_PTY_CROSS_PROCESS_ALLOCATION_CHILD";

#[test]
#[ignore = "PTY salvo injects Darwin errno -6 into neighbours; run alone with --ignored"]
fn cross_process_pair_allocation_does_not_leak_darwin_redrive_errors() {
    // One live pair per process keeps at most 24 pairs owned at once.
    const PROCESSES: usize = 24;
    const ALLOCATIONS: usize = 1024;
    if std::env::var_os(CROSS_PROCESS_CHILD_ENV).is_some() {
        allocate_after_start_signal(ALLOCATIONS);
        return;
    }

    let test_name = concat!(
        module_path!(),
        "::cross_process_pair_allocation_does_not_leak_darwin_redrive_errors"
    );
    let test_name = test_name
        .split_once("::")
        .map_or(test_name, |(_, path)| path);
    let executable = std::env::current_exe().expect("test executable path");
    let mut children: Vec<_> = (0..PROCESSES)
        .map(|_| {
            crate::test_rerun::exact_command(&executable, test_name)
                // The parent test is ignored, so the child must opt in too or
                // it would run nothing and report success.
                .args(["--include-ignored", "--test-threads=1"])
                .env(CROSS_PROCESS_CHILD_ENV, "1")
                .stdin(std::process::Stdio::piped())
                .stdout(std::process::Stdio::piped())
                .stderr(std::process::Stdio::piped())
                .spawn()
                .expect("spawn allocation child")
        })
        .collect();
    // Closing every start pipe together releases the children at once.
    for child in &mut children {
        drop(child.stdin.take());
    }
    let failures: Vec<String> = children
        .into_iter()
        .filter_map(|child| {
            let output = child.wait_with_output().expect("wait allocation child");
            // A child that filtered its test out would exit 0 without allocating.
            crate::test_rerun::exact_run_failure(test_name, &output)
        })
        .collect();
    assert!(failures.is_empty(), "child failures: {failures:?}");
}

fn allocate_after_start_signal(allocations: usize) {
    use std::io::Read;
    let mut start = Vec::new();
    std::io::stdin()
        .read_to_end(&mut start)
        .expect("wait for start signal");
    let failures: Vec<String> = (0..allocations)
        .filter_map(|_| open_pty_pair().err().map(|error| error.to_string()))
        .collect();
    // Keep the report small so a full stderr pipe never stalls the parent.
    assert!(
        failures.is_empty(),
        "{} of {allocations} PTY allocations failed, first: {:?}",
        failures.len(),
        failures.first()
    );
}
