use std::path::Path;
use std::time::{Duration, Instant};

use rmux_core::{EnvironmentStore, OptionStore, PaneGeometry};
use rmux_proto::{OptionName, ProcessCommand, ScopeSelector, SetOptionMode};
use rustix::process::{kill_process, test_kill_process, Pid, Signal};

use super::{open_pane_terminal, PaneTerminal};
use crate::terminal::TerminalProfile;

#[derive(Clone, Copy)]
enum Close {
    Drop,
    Remove,
    Shutdown,
}

fn check_close(close: Close, resistant: bool) {
    let mut options = OptionStore::new();
    options
        .set(
            ScopeSelector::Global,
            OptionName::DefaultShell,
            "/bin/sh".into(),
            SetOptionMode::Replace,
        )
        .expect("shell option");
    let profile = TerminalProfile::for_run_shell_with_base_environment(
        &EnvironmentStore::new(),
        &options,
        None,
        None,
        Path::new("/unused-close-test.sock"),
        None,
        false,
        None,
        None,
    )
    .expect("profile");
    let trap = if resistant { "trap '' HUP TERM; " } else { "" };
    let command = ProcessCommand::Shell(format!("{trap}printf ready; exec sleep 30"));
    let mut terminal = open_pane_terminal(
        &rmux_pty::ProcessShell,
        PaneGeometry::new(0, 0, 80, 24),
        profile,
        None,
        Some(&command),
    )
    .expect("terminal");
    let pid = Pid::from_raw(terminal.pid() as i32).expect("owned child pid");
    let master = terminal.clone_master().expect("master");
    master.io().set_nonblocking().expect("nonblocking");
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut ready = false;
    while Instant::now() < deadline {
        if master.io().try_read(&mut [0; 32]).is_ok_and(|n| n > 0) {
            ready = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(master);
    match close {
        Close::Drop => drop(terminal),
        Close::Remove => terminal.terminate_in_background(),
        Close::Shutdown => {
            PaneTerminal::shutdown_all(std::iter::once(&mut terminal)).expect("shutdown");
            drop(terminal);
        }
    }
    // Exceeds the former bounded escalation and lets the asynchronous reaper
    // observe a normal HUP exit. The ignored dispositions survive exec.
    std::thread::sleep(Duration::from_millis(750));
    let survived = test_kill_process(pid).is_ok();
    if survived {
        let _ = kill_process(pid, Signal::KILL);
    }
    assert!(ready, "child must install its dispositions before close");
    assert_eq!(
        survived, resistant,
        "terminal close must respect HUP disposition"
    );
}

#[test]
fn closing_a_pane_preserves_hangup_resistant_jobs() {
    for close in [Close::Drop, Close::Remove, Close::Shutdown] {
        check_close(close, true);
    }
}

#[test]
fn closing_a_pane_still_reaps_ordinary_jobs() {
    for close in [Close::Drop, Close::Remove, Close::Shutdown] {
        check_close(close, false);
    }
}
