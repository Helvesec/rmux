use super::*;
use crate::common::{read_until_contains, AttachedSession};
use rmux_pty::TerminalSize;
use std::os::unix::process::CommandExt;

pub(super) fn transcript(control: &ControlProcess) -> String {
    String::from_utf8(control.stdout.lock().expect("output lock").clone()).expect("UTF-8 control")
}

pub(super) fn client_name(harness: &CliHarness, pid: u32) -> Result<String, Box<dyn Error>> {
    let output = harness.run(&["list-clients", "-F", "#{client_pid}:#{client_name}"])?;
    assert!(output.status.success(), "{output:?}");
    let prefix = format!("{pid}:");
    common::stdout(&output)
        .lines()
        .find_map(|line| line.strip_prefix(&prefix).map(str::to_owned))
        .ok_or_else(|| format!("client {pid} not registered: {}", common::stdout(&output)).into())
}

pub(super) fn attach(
    harness: &CliHarness,
    session: &str,
) -> Result<AttachedSession, Box<dyn Error>> {
    attach_with_terminal(harness, session, false)
}

fn attach_with_terminal(
    harness: &CliHarness,
    session: &str,
    controlling: bool,
) -> Result<AttachedSession, Box<dyn Error>> {
    let mut attached = AttachedSession::spawn_command_configured(
        harness,
        &["attach-session", "-t", session],
        TerminalSize::new(101, 41),
        |command| {
            // SAFETY: only async-signal-safe system calls run between fork and
            // exec. The child already owns the PTY slave as stdin.
            unsafe {
                command.pre_exec(move || {
                    if controlling
                        && (libc::setsid() == -1 || libc::ioctl(0, libc::TIOCSCTTY as _, 0) == -1)
                    {
                        return Err(io::Error::last_os_error());
                    }
                    Ok(())
                });
            }
        },
    )?;
    attached.wait_for_raw_mode(PROCESS_TIMEOUT)?;
    read_until_contains(
        attached.master_mut(),
        &format!("[{session}]"),
        PROCESS_TIMEOUT,
    )?;
    Ok(attached)
}

#[test]
fn l05_1_detach_cli_target_and_exec_variants() -> Result<(), Box<dyn Error>> {
    for mode in ["target", "session", "others", "exec"] {
        let harness = CliHarness::new(&format!("detach-characterization-{mode}"))?;
        let _daemon = harness.start_hidden_daemon()?;
        for session in ["alpha", "beta"] {
            assert_success(&harness.run(&["new-session", "-d", "-s", session, "sleep 120"])?);
        }
        let mut first = attach_with_terminal(&harness, "alpha", mode == "exec")?;
        let mut second = attach(&harness, "beta")?;
        let first_name = client_name(&harness, first.child_mut().id())?;
        let second_name = client_name(&harness, second.child_mut().id())?;
        let mut control = ControlProcess::spawn(&harness, &["attach-session", "-t", "beta"])?;
        control.wait_for_output("%session-changed ")?;
        let control_name = client_name(&harness, control.child_mut().id())?;
        let mut peer = if mode == "others" {
            let mut peer = ControlProcess::spawn(&harness, &["attach-session", "-t", "alpha"])?;
            peer.wait_for_output("%session-changed ")?;
            let name = client_name(&harness, peer.child_mut().id())?;
            Some((peer, name))
        } else {
            None
        };
        control.write_input("display-message -p BEFORE\n")?;
        control.wait_for_output("BEFORE\n")?;
        let start = transcript(&control).len();
        let args: Vec<&str> = match mode {
            "target" => vec!["detach-client", "-t", &first_name],
            "session" => vec!["detach-client", "-s", "alpha"],
            "others" => vec!["detach-client", "-a", "-t", &control_name],
            "exec" => vec![
                "detach-client",
                "-t",
                &first_name,
                "-E",
                "/bin/sh -c 'printf EXEC-OWNED; exit 17'",
            ],
            _ => unreachable!(),
        };
        let result = harness.run(&args)?;
        assert!(result.status.success(), "{mode}: {result:?}");
        let (status, output) = first.wait_for_exit_with_output(PROCESS_TIMEOUT)?;
        assert_eq!(
            status.code(),
            Some(if mode == "exec" { 17 } else { 0 }),
            "{mode}: {}",
            String::from_utf8_lossy(&output)
        );
        if mode == "exec" {
            assert!(String::from_utf8_lossy(&output).contains("EXEC-OWNED"));
        }
        control.wait_for_output(&format!("%client-detached {first_name}\n"))?;
        if mode == "others" {
            assert!(second.wait_for_exit(PROCESS_TIMEOUT)?.success());
            control.wait_for_output(&format!("%client-detached {second_name}\n"))?;
        } else {
            assert!(
                second.child_mut().try_wait()?.is_none(),
                "unselected client survives"
            );
        }
        if let Some((peer, name)) = peer.as_mut() {
            assert!(peer.wait_for_exit_with_open_stdin()?.status.success());
            control.wait_for_output(&format!("%client-detached {name}\n"))?;
        }
        control.write_input("display-message -p AFTER\n")?;
        control.wait_for_output("AFTER\n")?;
        let text = transcript(&control);
        let mut detached = text[start..]
            .lines()
            .filter_map(|line| line.strip_prefix("%client-detached "))
            .collect::<Vec<_>>();
        detached.sort();
        let mut expected = vec![first_name.as_str()];
        if mode == "others" {
            expected.push(second_name.as_str());
        }
        if let Some((_, name)) = peer.as_ref() {
            expected.push(name.as_str());
        }
        expected.sort();
        assert_eq!(
            detached, expected,
            "exact targets, once per identity: {text}"
        );
        for session in ["alpha", "beta"] {
            assert_success(&harness.run(&["has-session", "-t", session])?);
        }
        let clients = harness.run(&["list-clients", "-F", "#{client_name}"])?;
        let actual = common::stdout(&clients);
        assert!(!actual.lines().any(|name| name == first_name));
        assert!(actual.lines().any(|name| name == control_name));
        assert_eq!(
            actual.lines().any(|name| name == second_name),
            mode != "others"
        );
        if mode != "others" {
            assert_success(&harness.run(&["detach-client", "-t", &second_name])?);
            assert!(second.wait_for_exit(PROCESS_TIMEOUT)?.success());
        }
    }
    Ok(())
}
