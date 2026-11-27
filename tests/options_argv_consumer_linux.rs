#![cfg(target_os = "linux")]

mod common;

use std::error::Error;
use std::fs;
use std::thread;
use std::time::{Duration, Instant};

use common::{assert_success, stdout, CliHarness};

#[test]
fn default_shell_keeps_multiple_argument_consumers_direct() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("options-argv-consumer")?;
    assert_success(&harness.run(&["new-session", "-d", "-s", "bank"])?);
    assert_success(&harness.run(&["set-option", "-g", "default-shell", "/bin/sh"])?);

    for legacy in [false, true] {
        for args in [
            vec![
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{pane_pid}",
                "-t",
                "bank:",
                "-n",
                "effect",
                "/bin/sleep",
                "240",
            ],
            vec![
                "new-session",
                "-d",
                "-P",
                "-F",
                "#{pane_pid}",
                "-s",
                if legacy { "legacy" } else { "sibling" },
                "/bin/sleep",
                "240",
            ],
            vec![
                "split-window",
                "-d",
                "-P",
                "-F",
                "#{pane_pid}",
                "-t",
                "bank:0",
                "/bin/sleep",
                "240",
            ],
            vec![
                "new-window",
                "-d",
                "-P",
                "-F",
                "#{pane_pid}",
                "-t",
                "bank:",
                "exec /bin/sleep 240",
            ],
        ] {
            let output = harness.run_with(&args, |command| {
                if legacy {
                    command.env("RMUX_DISABLE_CLI_TARGET_ACTIONS", "1");
                } else {
                    command.env_remove("RMUX_DISABLE_CLI_TARGET_ACTIONS");
                }
            })?;
            assert!(output.status.success(), "{} failed: {output:?}", args[0]);
            let pid = stdout(&output).trim().parse::<u32>()?;
            let deadline = Instant::now() + Duration::from_secs(2);
            loop {
                let argv = fs::read(format!("/proc/{pid}/cmdline"))?;
                if argv == b"/bin/sleep\x00240\x00" {
                    break;
                }
                assert!(Instant::now() < deadline, "{} pane argv: {argv:?}", args[0]);
                thread::sleep(Duration::from_millis(10));
            }
        }
    }
    Ok(())
}
