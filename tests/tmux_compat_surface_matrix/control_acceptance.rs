use super::control_acceptance_support::{assert_same_exit, command, frames, ControlPeer, Frame};
use super::control_criteria::{EOF, FRAMING, NOTIFICATIONS, ORDER, QUEUED};
use super::support::*;

// The pipe closes after a queued unclosed quote and two successful commands.
// tmux 3.7b supplies both the frame outcome and the reply ordering.
#[test]
fn tmux_compat_control_queued_unclosed_quote_preserves_order_through_eof(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("control-queued-parse-eof")?;
    let Some(tmux) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    FRAMING.report(&tmux);
    let (config, _) = config_with_clean_homes(&harness)?;
    assert_quiet_success(&harness.run_pair_with(
        &tmux,
        &["new-session", "-d", "-s", "alpha"],
        config,
    )?);
    let commands =
        "display-message -p FIRST\ndisplay-message -p 'unterminated\ndisplay-message -p LAST\n";
    let mut oracle = ControlPeer::spawn(command(&harness, &tmux, true, &[])?)?;
    oracle.send(commands)?;
    let oracle = oracle.finish_output(FRAMING.timeout)?;
    let mut candidate = ControlPeer::spawn(command(&harness, &tmux, false, &[])?)?;
    candidate.send(commands)?;
    let candidate = candidate.finish_output(FRAMING.timeout)?;
    assert_eq!(oracle.status_code, Some(0));
    assert_eq!(candidate.status_code, oracle.status_code);
    assert!(oracle.stderr.is_empty(), "{}", oracle.stderr);
    assert!(candidate.stderr.is_empty(), "{}", candidate.stderr);
    for (label, output) in [("tmux", &oracle.stdout), ("rmux", &candidate.stdout)] {
        assert!(output.ends_with("%exit\n"), "{label}: {output}");
        let first = output.find("\nFIRST\n").ok_or("FIRST missing")?;
        let middle = output
            .find("\nunterminated\n")
            .ok_or("quoted payload missing")?;
        let last = output.find("\nLAST\n").ok_or("LAST missing")?;
        assert!(first < middle && middle < last, "{label}: {output}");
    }
    let outcomes = |output: &str| {
        output
            .lines()
            .filter_map(|line| {
                let fields = line.split_whitespace().collect::<Vec<_>>();
                match fields.first().copied() {
                    Some("%end" | "%error") if fields.last() == Some(&"1") => {
                        Some(fields[0].to_owned())
                    }
                    _ => None,
                }
            })
            .collect::<Vec<_>>()
    };
    assert_eq!(outcomes(&oracle.stdout), ["%end", "%end", "%end"]);
    assert_eq!(
        outcomes(&candidate.stdout),
        outcomes(&oracle.stdout),
        "{}",
        candidate.stdout
    );
    let oracle_frames = frames(&oracle.stdout);
    assert_eq!(oracle_frames[0].flag, 0);
    assert!(oracle_frames[1..].iter().all(|frame| frame.flag == 1));
    assert_eq!(frames(&candidate.stdout), oracle_frames);
    Ok(())
}

fn create(harness: &TmuxCompatHarness, tmux: &Path) -> Result<(), Box<dyn Error>> {
    let (config, _) = config_with_clean_homes(harness)?;
    assert_quiet_success(&harness.run_pair_with(
        tmux,
        &["new-session", "-d", "-s", "alpha", "sleep 60"],
        config,
    )?);
    Ok(())
}

#[test]
fn tmux_compat_control_eof_and_empty_line_terminate_after_complete_commands(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("control-eof-complete-input")?;
    let Some(tmux) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    EOF.report(&tmux);
    create(&harness, &tmux)?;
    for input in [
        "",
        "display-message -p FIRST\ndisplay-message -p LAST",
        "display-message -p FIRST\n\ndisplay-message -p LAST\n",
    ] {
        let mut transcripts = Vec::new();
        for oracle in [true, false] {
            let mut peer = ControlPeer::spawn(command(&harness, &tmux, oracle, &EOF.argv[1..])?)?;
            peer.send(input)?;
            let output = peer.finish(EOF.timeout)?;
            assert!(output.ends_with("%exit\n"), "{output}");
            assert_eq!(output.lines().filter(|line| *line == "%exit").count(), 1);
            assert!(!output.contains("LAST"), "{output}");
            assert!(!output.contains(rmux_proto::CONTROL_CONTROL_START));
            assert!(!output.contains(rmux_proto::CONTROL_CONTROL_END));
            let transcript = frames(&output);
            let expected = if input.is_empty() {
                vec![]
            } else {
                vec!["FIRST".to_owned()]
            };
            assert_eq!(
                transcript
                    .iter()
                    .flat_map(|frame| frame.payload.clone())
                    .collect::<Vec<_>>(),
                expected
            );
            transcripts.push(transcript);
        }
        assert_eq!(transcripts[1], transcripts[0]);
    }
    Ok(())
}

#[test]
fn tmux_compat_control_group_replies_keep_individual_frames_and_order() -> Result<(), Box<dyn Error>>
{
    let harness = TmuxCompatHarness::new("control-group-frame-order")?;
    let Some(tmux) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    ORDER.report(&tmux);
    create(&harness, &tmux)?;
    let mut transcripts = Vec::new();
    for oracle in [true, false] {
        let mut peer = ControlPeer::spawn(command(&harness, &tmux, oracle, &ORDER.argv[1..])?)?;
        peer.send("display-message -p ONE ; display-message -p TWO\nno-such-command\ndisplay-message -p THREE\n")?;
        let output = peer.finish(ORDER.timeout)?;
        assert!(output.ends_with("%exit\n"), "{output}");
        transcripts.push(frames(&output));
    }
    let expected = [
        (0, "%end", vec![]),
        (1, "%end", vec!["ONE"]),
        (1, "%end", vec!["TWO"]),
        (
            1,
            "%error",
            vec!["parse error: unknown command: no-such-command"],
        ),
        (1, "%end", vec!["THREE"]),
    ]
    .into_iter()
    .map(|(flag, terminal, payload)| Frame {
        flag,
        terminal: terminal.to_owned(),
        payload: payload.into_iter().map(str::to_owned).collect(),
    })
    .collect::<Vec<_>>();
    assert_eq!(transcripts[0], expected);
    assert_eq!(transcripts[1], transcripts[0]);
    // Public argv commands are serialized as separate initial lines. Keep
    // their flag-0 replies distinct without splitting an internal owner batch.
    let mut initial = Vec::new();
    for oracle in [true, false] {
        let peer = ControlPeer::spawn(command(
            &harness,
            &tmux,
            oracle,
            &[
                "display-message",
                "-p",
                "ONE",
                ";",
                "display-message",
                "-p",
                "TWO",
            ],
        )?)?;
        initial.push(frames(&peer.finish(ORDER.timeout)?));
    }
    assert_eq!(initial[0].len(), 2);
    assert!(initial[0]
        .iter()
        .all(|frame| frame.flag == 0 && frame.terminal == "%end"));
    assert_eq!(initial[0][0].payload, ["ONE"]);
    assert_eq!(initial[0][1].payload, ["TWO"]);
    assert_eq!(initial[1], initial[0]);
    Ok(())
}

// tmux 3.7b runs the control-mode argv as one command list: the failed guard
// stops it, so `kill-session` never runs and `alpha` survives (R3511 M1).
// tmux then exits 1, and rmux is held to tmux's status, not to 0.
#[test]
fn tmux_compat_control_argv_list_stops_at_its_failed_guard() -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("control-argv-failed-guard")?;
    let Some(tmux) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    ORDER.report(&tmux);
    create(&harness, &tmux)?;
    let mut outputs = Vec::new();
    for oracle in [true, false] {
        let peer = ControlPeer::spawn(command(
            &harness,
            &tmux,
            oracle,
            &[
                "display-message",
                "-p",
                "FIRST",
                ";",
                "has-session",
                "-t",
                "missing",
                ";",
                "kill-session",
                "-t",
                "alpha",
            ],
        )?)?;
        outputs.push(peer.finish_output(ORDER.timeout)?);
    }
    let expected = [
        (0, "%end", "FIRST"),
        (0, "%error", "can't find session: missing"),
    ]
    .into_iter()
    .map(|(flag, terminal, payload)| Frame {
        flag,
        terminal: terminal.to_owned(),
        payload: vec![payload.to_owned()],
    })
    .collect::<Vec<_>>();
    assert_eq!(frames(&outputs[0].stdout), expected);
    assert_eq!(frames(&outputs[1].stdout), expected);
    let (config, _) = config_with_clean_homes(&harness)?;
    assert_quiet_success(&harness.run_pair_with(&tmux, &["has-session", "-t", "alpha"], config)?);
    assert_eq!(outputs[0].status_code, Some(1), "{:?}", outputs[0]);
    assert_same_exit(&outputs[0], &outputs[1]);
    Ok(())
}

#[test]
fn tmux_compat_control_window_notifications_follow_mutations_once() -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("control-window-notifications")?;
    let Some(tmux) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    NOTIFICATIONS.report(&tmux);
    create(&harness, &tmux)?;
    let mutations = [
        (
            "rename-window -t alpha:0 renamed\n",
            "%window-renamed @0 renamed",
        ),
        (
            "new-window -d -t alpha:1 -n second 'sleep 60'\n",
            "%unlinked-window-add @1",
        ),
        ("kill-window -t alpha:1\n", "%unlinked-window-close @1"),
    ];
    let mut transcripts = Vec::new();
    for oracle in [true, false] {
        let mut peer =
            ControlPeer::spawn(command(&harness, &tmux, oracle, &NOTIFICATIONS.argv[1..])?)?;
        peer.send("display-message -p READY\n")?;
        peer.wait(NOTIFICATIONS.timeout, |output| {
            output.contains("\nREADY\n%end ")
        })?;
        peer.send(
            &mutations
                .iter()
                .map(|(mutation, _)| mutation.trim_end())
                .collect::<Vec<_>>()
                .join(" ; "),
        )?;
        peer.send("\n")?;
        peer.send("display-message -p DONE\n")?;
        peer.wait(NOTIFICATIONS.timeout, |output| {
            output.contains("\nDONE\n%end ")
        })?;
        peer.wait(NOTIFICATIONS.timeout, |output| {
            mutations
                .iter()
                .all(|(_, expected)| output.lines().any(|line| line == *expected))
        })?;
        let output = peer.finish(NOTIFICATIONS.timeout)?;
        let notifications = output
            .lines()
            .filter(|line| {
                line.starts_with("%window-renamed ") || line.starts_with("%unlinked-window-")
            })
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(notifications, mutations.map(|(_, expected)| expected));
        let transcript = frames(&output);
        assert!(transcript.iter().all(|frame| frame.terminal == "%end"));
        transcripts.push((transcript, notifications));
    }
    assert_eq!(transcripts[1], transcripts[0]);
    Ok(())
}

#[test]
fn tmux_compat_control_blocked_queue_recovers_after_command_error() -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("control-blocked-command-queue")?;
    let Some(tmux) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    QUEUED.report(&tmux);
    create(&harness, &tmux)?;
    let mut transcripts = Vec::new();
    for oracle in [true, false] {
        let mut peer = ControlPeer::spawn(command(&harness, &tmux, oracle, &QUEUED.argv[1..])?)?;
        peer.send("display-message -p READY\n")?;
        let ready = peer.wait(QUEUED.timeout, |output| output.contains("\nREADY\n%end "))?;
        let cursor = ready.len();
        peer.send("wait-for RELEASE\ndisplay-message -p FIRST\nno-such-control-command\ndisplay-message -p LAST\n")?;
        let blocked = peer.wait(QUEUED.timeout, |output| {
            output[cursor..].contains("%end ") && output.ends_with('\n')
        })?;
        assert!(
            !blocked[cursor..].contains("FIRST"),
            "oracle={oracle}: {blocked}"
        );
        assert!(
            !blocked[cursor..].contains("LAST"),
            "oracle={oracle}: {blocked}"
        );
        assert_eq!(
            frames(&blocked[cursor..]).len(),
            1,
            "oracle={oracle}: {blocked}"
        );
        if oracle {
            let signal = Command::new(&tmux)
                .args(["-S"])
                .arg(harness.tmux_socket_path())
                .args(["wait-for", "-S", "RELEASE"])
                .output()?;
            assert!(signal.status.success(), "{signal:?}");
        } else {
            let signal = harness.run_rmux(&["wait-for", "-S", "RELEASE"])?;
            assert_eq!(signal.status_code, Some(0), "{}", signal.stderr_string());
        }
        peer.wait(QUEUED.timeout, |output| output.contains("\nLAST\n%end "))?;
        let output = peer.finish(QUEUED.timeout)?;
        assert!(output.ends_with("%exit\n"), "{output}");
        transcripts.push(frames(&output));
    }
    assert_eq!(transcripts[0].len(), 6);
    assert_eq!(transcripts[0][2].payload, Vec::<String>::new());
    assert_eq!(transcripts[0][3].payload, ["FIRST"]);
    assert_eq!(transcripts[0][4].terminal, "%error");
    assert_eq!(transcripts[0][5].payload, ["LAST"]);
    assert_eq!(transcripts[1], transcripts[0]);
    Ok(())
}
