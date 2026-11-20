//! Daemon-wide PTY shutdown treats an unobservable child as reaped, as the
//! per-pane bounded wait does, instead of escalating and failing the shutdown.

use std::path::Path;
use std::time::{Duration, Instant};

use rmux_core::{EnvironmentStore, OptionStore, PaneGeometry};
use rmux_proto::{OptionName, ProcessCommand, ScopeSelector, SetOptionMode};
use rustix::process::{kill_process, waitpid, Pid, Signal as OsSignal, WaitOptions};

use super::super::open_pane_terminal;
use super::PaneTerminal;
use crate::terminal::TerminalProfile;

fn sh_profile(cwd: &Path) -> TerminalProfile {
    let mut options = OptionStore::default();
    options
        .set(
            ScopeSelector::Global,
            OptionName::DefaultShell,
            "/bin/sh".to_owned(),
            SetOptionMode::Replace,
        )
        .expect("default-shell test option is valid");
    TerminalProfile::for_run_shell_with_base_environment(
        &EnvironmentStore::default(),
        &options,
        None,
        None,
        Path::new("/tmp/rmux-shutdown-reap-witness.sock"),
        None,
        false,
        None,
        Some(cwd),
    )
    .expect("test profile")
}

#[test]
fn shutdown_all_settles_a_child_that_can_no_longer_be_waited_on() {
    let cwd = std::env::temp_dir();
    let command = ProcessCommand::Shell("exec /bin/sleep 30".to_owned());
    let mut terminal = open_pane_terminal(
        &rmux_pty::ProcessShell,
        PaneGeometry::new(0, 0, 80, 24),
        sh_profile(&cwd),
        None,
        Some(&command),
    )
    .expect("the pane opens");
    let pid = Pid::from_raw(i32::try_from(terminal.pid()).expect("pid fits")).expect("live pid");

    // Reap the child behind the owner's back: its own try_wait now fails with
    // ECHILD, which is the "can no longer be waited on" observation.
    kill_process(pid, OsSignal::KILL).expect("kill the pane child");
    let reaped = waitpid(Some(pid), WaitOptions::empty()).expect("external reap");
    assert!(reaped.is_some(), "the fixture must really reap the child");

    let result = PaneTerminal::shutdown_all(std::iter::once(&mut terminal));
    assert!(
        result.is_ok(),
        "an unobservable child is settled, not a shutdown failure: {result:?}"
    );
}

/// Daemon shutdown reaps panes before releasing the socket, so kill-server
/// waits for this reap. A HUP-terminated shell must be observed well within
/// one 1 ms poll interval instead of always paying a full interval.
///
/// Wall-clock witness only: preemption on a loaded host exceeds the bound, so
/// the deterministic guard is the exit poll schedule test in the parent module.
#[test]
#[ignore = "timing witness: run alone on a calm host; daemon_churn gates the gain"]
fn shutdown_all_observes_a_hup_terminated_pane_within_one_poll_interval() {
    let cwd = std::env::temp_dir();
    let command = ProcessCommand::Shell("exec /bin/sleep 30".to_owned());
    let fastest = (0..5)
        .map(|_| {
            let mut terminal = open_pane_terminal(
                &rmux_pty::ProcessShell,
                PaneGeometry::new(0, 0, 80, 24),
                sh_profile(&cwd),
                None,
                Some(&command),
            )
            .expect("the pane opens");
            let started = Instant::now();
            PaneTerminal::shutdown_all(std::iter::once(&mut terminal)).expect("the pane is reaped");
            started.elapsed()
        })
        .min()
        .expect("five shutdowns ran");
    assert!(
        fastest < Duration::from_micros(900),
        "the fastest of five HUP reaps took {fastest:?}"
    );
}
