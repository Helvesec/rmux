//! Re-execution of the current test binary on exactly one test.
//!
//! The one copy for every crate whose tests re-execute their own binary: a
//! library includes it with `#[path]` from its test configuration, an
//! integration test binary from its root, each as a module named `test_rerun`.
//!
//! libtest exits 0 when `--exact` matches no test, so a renamed test would
//! pass its re-execution without running.

use std::path::Path;
use std::process::{Command, Output};

/// The start of libtest's summary when exactly one test ran and passed.
const ONE_PASSED_ALONE: &str = "test result: ok. 1 passed; 0 failed; 0 ignored;";

/// Re-executes this test binary on exactly `test` and asserts that it ran.
pub(crate) fn rerun_exact(test: &str, configure: impl FnOnce(&mut Command)) {
    rerun_exact_with(&std::env::current_exe().unwrap(), test, configure);
}

/// Re-executes `program`, a copy of this test binary, on exactly `test` and
/// asserts that it ran.
pub(crate) fn rerun_exact_with(program: &Path, test: &str, configure: impl FnOnce(&mut Command)) {
    let mut command = exact_command(program, test);
    configure(&mut command);
    let output = command.output().unwrap();
    if let Some(failure) = exact_run_failure(test, &output) {
        panic!("{failure}");
    }
}

/// The command that re-executes `program` on exactly `test`, for a caller
/// that spawns it itself and then checks it with [`exact_run_failure`].
pub(crate) fn exact_command(program: &Path, test: &str) -> Command {
    let mut command = Command::new(program);
    command.args(["--exact", test, "--nocapture"]);
    command
}

/// Why `output` does not prove that exactly `test` ran and passed, or `None`
/// when it does.
///
/// The process must exit 0, announce `test <test> ... ` and end with a libtest
/// summary of exactly one passed test, none failed and none ignored. The
/// announcement ties the run to the named test even when a caller adds another
/// filter; an ignored test is announced too, so the summary must count no
/// ignored test. The summary is the last one printed, since a re-executed
/// stage may print its own earlier. Together they state `test <test> ... ok`
/// whether libtest runs one thread, which prints the name before the test's
/// own output, or several, which print it with the result.
pub(crate) fn exact_run_failure(test: &str, output: &Output) -> Option<String> {
    let stdout = String::from_utf8_lossy(&output.stdout);
    let announced = stdout.contains(&format!("test {test} ... "));
    let ran_one = stdout
        .lines()
        .rev()
        .find(|line| line.starts_with("test result: "))
        .is_some_and(|summary| summary.starts_with(ONE_PASSED_ALONE));
    if output.status.success() && announced && ran_one {
        return None;
    }
    Some(format!(
        "re-executed {test} did not run exactly one passing test ({}, announced: {announced}):\n{stdout}\n{}",
        output.status,
        String::from_utf8_lossy(&output.stderr)
    ))
}

/// The path of this module in the including test binary, without the crate.
fn module_test_path(test: &str) -> String {
    let module = module_path!()
        .split_once("::")
        .map_or("", |(_, module)| module);
    format!("{module}::{test}")
}

const EXIT_AFTER_REPORT: &str = "RMUX_TEST_RERUN_EXIT_AFTER_REPORT";
const EARLIER_SUMMARY: &str = "RMUX_TEST_RERUN_EARLIER_SUMMARY";

#[test]
fn rerun_target() {
    if std::env::var_os(EARLIER_SUMMARY).is_some() {
        // A re-executed stage's own summary, before libtest's final one, on
        // a line of its own even when libtest printed the test's name first.
        println!("\n{ONE_PASSED_ALONE} 0 measured; 0 filtered out");
    }
    if std::env::var_os(EXIT_AFTER_REPORT).is_some() {
        // libtest's report of a pass, then a failing exit.
        println!("test {} ... ok", module_test_path("rerun_target"));
        println!("{ONE_PASSED_ALONE} 0 measured; 0 filtered out");
        std::process::exit(1);
    }
}

#[test]
fn other_rerun_target() {}

#[test]
#[ignore = "re-executed by rerun_of_an_ignored_test_beside_another_fails"]
fn ignored_rerun_target() {}

#[test]
fn rerun_of_the_named_test_passes() {
    rerun_exact(&module_test_path("rerun_target"), |_| {});
}

#[test]
#[should_panic(expected = "did not run exactly one passing test")]
fn rerun_of_a_missing_test_fails() {
    rerun_exact(&module_test_path("no_such_test"), |_| {});
}

#[test]
#[should_panic(expected = "did not run exactly one passing test")]
fn rerun_that_runs_another_test_fails() {
    // One passing test runs, but not the named one.
    let other = module_test_path("other_rerun_target");
    rerun_exact(&module_test_path("no_such_test"), |command| {
        command.arg(other);
    });
}

#[test]
#[should_panic(expected = "did not run exactly one passing test")]
fn rerun_that_also_runs_another_test_fails() {
    // The named test runs and passes, but not alone.
    let other = module_test_path("other_rerun_target");
    rerun_exact(&module_test_path("rerun_target"), |command| {
        command.arg(other);
    });
}

#[test]
#[should_panic(expected = "did not run exactly one passing test")]
fn rerun_of_an_ignored_test_beside_another_fails() {
    // The named test is announced but ignored; another one passes.
    let other = module_test_path("other_rerun_target");
    rerun_exact(&module_test_path("ignored_rerun_target"), |command| {
        command.arg(other);
    });
}

#[test]
#[should_panic(expected = "did not run exactly one passing test")]
fn rerun_judged_by_an_earlier_summary_fails() {
    // The named test prints a summary of one pass, but libtest's own, last
    // summary counts a second test beside it.
    let other = module_test_path("other_rerun_target");
    rerun_exact(&module_test_path("rerun_target"), |command| {
        command.arg(other).env(EARLIER_SUMMARY, "1");
    });
}

#[test]
#[should_panic(expected = "did not run exactly one passing test")]
fn rerun_that_fails_after_reporting_a_pass_fails() {
    rerun_exact(&module_test_path("rerun_target"), |command| {
        command.env(EXIT_AFTER_REPORT, "1");
    });
}
