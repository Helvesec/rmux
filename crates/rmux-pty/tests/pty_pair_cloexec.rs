#![cfg(unix)]

//! A PTY pair is close-on-exec from the moment it exists.
//!
//! The server also starts ordinary children (`run-shell`, `if-shell`, status
//! jobs, copy pipes) with `std::process::Command`, which forks or spawns
//! without the PTY child's descriptor sweep. A pair that is inheritable for
//! even an instant can be copied into such a child by another thread, which
//! then holds the pane's terminal - its output and its input - for its life.

use std::process::Stdio;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;

use rmux_pty::PtyPair;

#[path = "../../../tests/support/rerun_exact.rs"]
mod test_rerun;

const TEST: &str = "a_pty_pair_never_reaches_a_concurrently_spawned_child";
const MODE: &str = "RMUX_TEST_PTY_PAIR_CLOEXEC";
const LEAK_REPORT: &str = "probe leaked=";
const SPAWNS: usize = 256;
const ALLOCATORS: usize = 2;

/// The terminals open above the standard descriptors in this process.
fn terminals_above_standard() -> Vec<i32> {
    // SAFETY: isatty only queries the supplied descriptor number.
    (3..1024)
        .filter(|&fd| unsafe { libc::isatty(fd) } == 1)
        .collect()
}

/// Reports after [`LEAK_REPORT`] whether this process holds a terminal above
/// the standard descriptors that the driver did not already hold before any
/// pair existed.
fn probe(preexisting: &str) {
    let preexisting: Vec<i32> = preexisting
        .split(',')
        .filter_map(|fd| fd.parse().ok())
        .collect();
    let leaked = terminals_above_standard()
        .into_iter()
        .any(|fd| !preexisting.contains(&fd));
    println!("{LEAK_REPORT}{leaked}");
}

/// The verdict of a probe's output: exactly one [`LEAK_REPORT`] followed by
/// `true` or `false`. libtest may print the test's name on the same line.
fn leak_report(stdout: &str) -> Option<bool> {
    let mut reports = stdout.split(LEAK_REPORT).skip(1);
    let verdict = reports.next()?.split_whitespace().next()?.parse().ok();
    match reports.next() {
        None => verdict,
        Some(_) => None,
    }
}

#[test]
fn leak_report_reads_exactly_one_verdict() {
    assert_eq!(leak_report(&format!("{LEAK_REPORT}false\n")), Some(false));
    // One libtest thread prints the name, then the output, then the result.
    assert_eq!(
        leak_report(&format!("test {TEST} ... {LEAK_REPORT}true\nok\n")),
        Some(true)
    );
    assert_eq!(leak_report("running 1 test\n"), None);
    assert_eq!(leak_report(&format!("{LEAK_REPORT}maybe\n")), None);
    assert_eq!(
        leak_report(&format!("{LEAK_REPORT}false\n{LEAK_REPORT}false\n")),
        None
    );
}

#[test]
fn a_pty_pair_never_reaches_a_concurrently_spawned_child() {
    if let Ok(preexisting) = std::env::var(MODE) {
        probe(&preexisting);
        return;
    }
    // A terminal this process inherited from whatever launched it is not the
    // property under test; only a descriptor of a pair opened here is.
    let preexisting = terminals_above_standard()
        .iter()
        .map(i32::to_string)
        .collect::<Vec<_>>()
        .join(",");
    let executable = std::env::current_exe().expect("current test binary");
    let stop = Arc::new(AtomicBool::new(false));
    let allocators: Vec<_> = (0..ALLOCATORS)
        .map(|_| {
            let stop = Arc::clone(&stop);
            std::thread::spawn(move || {
                let mut pairs = 0_usize;
                while !stop.load(Ordering::Relaxed) {
                    drop(PtyPair::open().expect("a PTY pair opens"));
                    pairs += 1;
                }
                pairs
            })
        })
        .collect();

    let mut leaked = 0_usize;
    for _ in 0..SPAWNS {
        // An ordinary child: no pre-exec hook, so nothing but the
        // descriptors' own close-on-exec flags keeps the pair out of it.
        let output = test_rerun::exact_command(&executable, TEST)
            .arg("--test-threads=1")
            .env(MODE, &preexisting)
            .stdin(Stdio::null())
            .output()
            .expect("the probe starts");
        if let Some(failure) = test_rerun::exact_run_failure(TEST, &output) {
            panic!("the probe ended unexpectedly: {failure}");
        }
        let stdout = String::from_utf8_lossy(&output.stdout);
        match leak_report(&stdout) {
            Some(true) => leaked += 1,
            Some(false) => {}
            None => panic!("the probe reported no single verdict: {stdout}"),
        }
    }
    stop.store(true, Ordering::Relaxed);
    let pairs: usize = allocators
        .into_iter()
        .map(|allocator| allocator.join().expect("the allocator ends"))
        .sum();

    assert!(pairs > 0, "no PTY pair was opened while the probes ran");
    assert_eq!(
        leaked, 0,
        "{leaked} of {SPAWNS} children inherited a PTY descriptor while {pairs} pairs were opened"
    );
}
