use super::characterization::transcript;
use super::*;

fn sync(control: &mut ControlProcess, marker: &str) -> Result<String, Box<dyn Error>> {
    control.write_input(&format!("display-message -p {marker}\n"))?;
    control.wait_for_output(&format!("{marker}\n"))?;
    Ok(transcript(control))
}

#[test]
fn l05_1_cli_lifecycle_event_uniqueness() -> Result<(), Box<dyn Error>> {
    for variant in ["surviving-window", "last-window", "kill-session"] {
        let harness = CliHarness::new("cli-lifecycle-once")?;
        let _daemon = harness.start_hidden_daemon()?;
        assert_success(&harness.run(&["new-session", "-d", "-s", "keeper", "sleep 120"])?);
        let mut observer = ControlProcess::spawn(&harness, &["attach-session", "-t", "keeper"])?;
        observer.wait_for_output("%session-changed ")?;
        let start = sync(&mut observer, "BEFORE-CREATE")?.len();
        assert_success(&harness.run(&["new-session", "-d", "-s", "subject", "sleep 120"])?);
        observer.wait_for_output("%sessions-changed\n")?;
        let created = sync(&mut observer, "AFTER-CREATE")?;
        assert_eq!(
            created[start..]
                .lines()
                .filter(|line| *line == "%sessions-changed")
                .count(),
            1
        );
        let pane = common::stdout(&harness.run(&[
            "display-message",
            "-p",
            "-t",
            "subject",
            "#{pane_id}",
        ])?)
        .trim()
        .to_owned();
        let window = common::stdout(&harness.run(&[
            "display-message",
            "-p",
            "-t",
            "subject",
            "#{window_id}",
        ])?)
        .trim()
        .to_owned();
        if variant == "surviving-window" {
            assert_success(&harness.run(&["new-window", "-d", "-t", "subject:1", "sleep 120"])?);
        }
        let start = sync(&mut observer, "BEFORE-KILL")?.len();
        if variant == "kill-session" {
            assert_success(&harness.run(&["kill-session", "-t", "subject"])?);
        } else {
            assert_success(&harness.run(&["kill-pane", "-t", &pane])?);
        }
        let after = sync(&mut observer, "AFTER-KILL")?;
        let delta = &after[start..];
        assert_eq!(
            delta
                .lines()
                .filter(|line| *line == format!("%unlinked-window-close {window}"))
                .count(),
            1,
            "{variant}: {delta}"
        );
        assert_eq!(
            delta
                .lines()
                .filter(|line| *line == "%sessions-changed")
                .count(),
            usize::from(variant != "surviving-window"),
            "{variant}: {delta}"
        );
        assert_eq!(
            harness
                .run(&["has-session", "-t", "subject"])?
                .status
                .success(),
            variant == "surviving-window"
        );
        if variant == "surviving-window" {
            assert_success(&harness.run(&["kill-session", "-t", "subject"])?);
        }
        assert_success(&harness.run(&["new-session", "-d", "-s", "subject", "sleep 120"])?);
        let replacement = common::stdout(&harness.run(&[
            "display-message",
            "-p",
            "-t",
            "subject",
            "#{pane_id}",
        ])?)
        .trim()
        .to_owned();
        assert_ne!(pane, replacement);
        let stale = harness.run(&[
            "kill-pane",
            "-t",
            &pane,
            ";",
            "set-option",
            "-t",
            "subject",
            "@stale",
            "forbidden",
        ])?;
        assert!(!stale.status.success());
        assert_eq!(
            common::stdout(&harness.run(&["show-options", "-qv", "-t", "subject", "@stale"])?),
            ""
        );
        assert_success(&harness.run(&["has-session", "-t", "subject"])?);
    }
    Ok(())
}

#[test]
fn l05_1_control_response_precedes_wire_rehome_or_exit() -> Result<(), Box<dyn Error>> {
    for (policy, expected) in [
        ("on", None),
        ("off", Some("z")),
        ("no-detached", Some("a")),
        ("previous", Some("a")),
        ("next", Some("z")),
    ] {
        let harness = CliHarness::new("control-teardown-order")?;
        let _daemon = harness.start_hidden_daemon()?;
        for name in ["a", "z", "m"] {
            assert_success(&harness.run(&["new-session", "-d", "-s", name, "sleep 120"])?);
        }
        assert_success(&harness.run(&["set-option", "-t", "m", "detach-on-destroy", policy])?);
        let mut control = ControlProcess::spawn(&harness, &["attach-session", "-t", "m"])?;
        control.wait_for_output("%session-changed $2 m\n")?;
        let _occupied = if policy == "no-detached" {
            let occupied = ControlProcess::spawn(&harness, &["attach-session", "-t", "z"])?;
            occupied.wait_for_output("%session-changed $1 z\n")?;
            Some(occupied)
        } else {
            None
        };
        let start = sync(&mut control, "BEFORE")?.len();
        control.write_input("kill-session\n")?;
        let transition = expected.map_or_else(
            || "%exit\n".to_owned(),
            |name| format!("%session-changed ${} {name}\n", usize::from(name == "z")),
        );
        control.wait_for_output(&transition)?;
        let text = transcript(&control);
        let delta = &text[start..];
        let response = delta.find("%end ").expect("successful kill response");
        assert!(
            response < delta.find(&transition).unwrap(),
            "response precedes transition: {policy}: {delta}"
        );
        assert_eq!(delta.matches(&transition).count(), 1, "{delta}");
        if let Some(name) = expected {
            control.write_input("display-message -p 'CURRENT:#{session_name}'\n")?;
            control.wait_for_output(&format!("CURRENT:{name}\n"))?;
        } else {
            assert!(control.wait_for_exit_with_open_stdin()?.status.success());
        }
    }
    Ok(())
}
