#![cfg(unix)]

mod common;

use std::error::Error;
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

use common::{assert_success, stderr, stdout, terminate_child, AttachedSession, CliHarness};
use rmux_proto::TerminalSize;

const FILE_TIMEOUT: Duration = Duration::from_secs(5);

#[test]
fn pane_transfer_commands_round_trip_through_the_binary() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("pane-transfer-binary")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let join_path = harness.tmpdir().join("join.txt");
    let break_path = harness.tmpdir().join("break.txt");
    let swap_source_path = harness.tmpdir().join("swap-source.txt");
    let swap_target_path = harness.tmpdir().join("swap-target.txt");

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha"])?);
    assert_success(&harness.run(&["select-pane", "-t", "alpha:0.1"])?);
    assert_success(&harness.run(&["select-pane", "-t", "alpha:0.0"])?);
    assert_success(&harness.run(&["last-pane", "-t", "alpha:0"])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:0.1",
        "export RMUX_TRANSFER_MARK=joined",
        "Enter",
    ])?);

    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "dest"])?);
    assert_success(&harness.run(&["join-pane", "-d", "-s", "alpha:0.1", "-t", "alpha:1.0"])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:1.1",
        &format!(
            "printf \"$RMUX_TRANSFER_MARK\" > {}",
            shell_quote(&join_path)
        ),
        "Enter",
    ])?);
    wait_for_file_contents(&join_path, "joined", FILE_TIMEOUT)?;

    assert_success(&harness.run(&[
        "break-pane",
        "-d",
        "-s",
        "alpha:1.1",
        "-t",
        "alpha:2",
        "-n",
        "broken",
    ])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:2.0",
        &format!(
            "printf \"$RMUX_TRANSFER_MARK\" > {}",
            shell_quote(&break_path)
        ),
        "Enter",
    ])?);
    wait_for_file_contents(&break_path, "joined", FILE_TIMEOUT)?;

    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "swap"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha:3.0"])?);
    assert_success(&harness.run(&["kill-pane", "-t", "alpha:3.0"])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:3.0",
        "export RMUX_TRANSFER_MARK=swapped",
        "Enter",
    ])?);
    assert_success(&harness.run(&["swap-pane", "-d", "-s", "alpha:2.0", "-t", "alpha:3.0"])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:2.0",
        &format!(
            "printf \"$RMUX_TRANSFER_MARK\" > {}",
            shell_quote(&swap_source_path)
        ),
        "Enter",
    ])?);
    assert_success(&harness.run(&[
        "send-keys",
        "-t",
        "alpha:3.0",
        &format!(
            "printf \"$RMUX_TRANSFER_MARK\" > {}",
            shell_quote(&swap_target_path)
        ),
        "Enter",
    ])?);
    wait_for_file_contents(&swap_source_path, "swapped", FILE_TIMEOUT)?;
    wait_for_file_contents(&swap_target_path, "joined", FILE_TIMEOUT)?;

    assert_success(&harness.run(&["new-window", "-t", "alpha", "-d", "-n", "relative"])?);
    assert_success(&harness.run(&["split-window", "-v", "-t", "alpha:4.0"])?);
    let relative = harness.run(&["swap-pane", "-U", "-t", "alpha:4.1"])?;
    assert_success(&relative);
    assert!(
        stderr(&relative).is_empty(),
        "relative swap-pane should stay silent on success"
    );

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn cross_session_join_pane_renumbers_colliding_source_pane_index() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("cross-session-join-pane-renumber")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "alpha", "sleep 60"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "beta", "sleep 60"])?);
    assert_success(&harness.run(&["split-window", "-d", "-t", "alpha:0.0", "sleep 60"])?);
    assert_success(&harness.run(&["join-pane", "-d", "-s", "alpha:0.0", "-t", "beta:0.0"])?);

    let listed = harness.run(&[
        "list-panes",
        "-t",
        "beta:0",
        "-F",
        "#{pane_index}:#{pane_id}",
    ])?;
    assert_eq!(listed.status.code(), Some(0));
    assert_eq!(stdout(&listed).lines().count(), 2);
    assert!(stdout(&listed).starts_with("0:%"));
    assert!(stdout(&listed).contains("\n1:%"));
    assert!(stderr(&listed).is_empty());

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn break_pane_without_target_lands_in_the_current_session() -> Result<(), Box<dyn Error>> {
    // tmux 3.7b: without -t and without a client, the destination is the most
    // recently created session, honouring its base-index; never the source's.
    let harness = CliHarness::new("break-pane-current-session")?;
    let mut daemon = harness.start_hidden_daemon()?;

    assert_success(&harness.run(&["new-session", "-d", "-s", "p", "sleep 60"])?);
    assert_success(&harness.run(&["split-window", "-d", "-t", "p:0.0", "sleep 60"])?);
    assert_success(&harness.run(&["split-window", "-d", "-t", "p:0.0", "sleep 60"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "other", "sleep 60"])?);
    let other = query(
        &harness,
        &["display-message", "-p", "-t", "other:0.0", "#{pane_id}"],
    )?;
    let direct = query(
        &harness,
        &["display-message", "-p", "-t", "p:0.0", "#{pane_id}"],
    )?;
    let queued = query(
        &harness,
        &["display-message", "-p", "-t", "p:0.1", "#{pane_id}"],
    )?;
    let kept = query(
        &harness,
        &["display-message", "-p", "-t", "p:0.2", "#{pane_id}"],
    )?;

    assert_success(&harness.run(&["break-pane", "-d", "-s", "p:0.0"])?);
    assert_success(&harness.run(&["set-option", "-t", "other", "base-index", "5"])?);
    assert_success(&harness.run(&["if-shell", "-F", "1", "break-pane -d -s p:0.0"])?);

    let listed = query(
        &harness,
        &[
            "list-windows",
            "-a",
            "-F",
            "#{session_name}:#{window_index}=#{pane_id}",
        ],
    )?;
    assert_eq!(
        listed,
        format!("other:0={other}\nother:1={direct}\nother:5={queued}\np:0={kept}")
    );

    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn placement_without_target_anchors_on_the_active_window_from_an_inactive_pane(
) -> Result<(), Box<dyn Error>> {
    // tmux 3.7b: a command run from a pane of a non-active window takes the
    // session's active window (s->curw) as its current window, so -a/-b
    // without -t anchor there, directly and through the command queue.
    let cases: [(&[&str], &str); 6] = [
        (
            &["break-pane", "-d", "-a", "-s", "p:0.1"],
            "0=a 1*=c 2=b 3=d",
        ),
        (
            &["break-pane", "-d", "-b", "-s", "p:0.1"],
            "0=a 1=b 2*=c 3=d",
        ),
        (
            &["if-shell", "-F", "1", "break-pane -d -b -s p:0.1"],
            "0=a 1=b 2*=c 3=d",
        ),
        (
            &["break-pane", "-d", "-a", "-s", "other:0.1"],
            "0=a 1*=c 2=o1 3=d",
        ),
        (
            &["link-window", "-d", "-a", "-s", "other:0"],
            "0=a 1*=c 2=o0 3=d",
        ),
        (
            &["if-shell", "-F", "1", "link-window -d -b -s other:0"],
            "0=a 1=o0 2*=c 3=d",
        ),
    ];
    let mut mismatches = Vec::new();
    for (index, (command, expected)) in cases.into_iter().enumerate() {
        let harness = CliHarness::new(&format!("placement-inactive-pane-{index}"))?;
        let mut daemon = harness.start_hidden_daemon()?;
        for args in [
            &["new-session", "-d", "-s", "p", "sleep 60"][..],
            &["split-window", "-d", "-t", "p:0.0", "sleep 60"],
            &["new-window", "-d", "-t", "p:1", "sleep 60"],
            &["new-window", "-d", "-t", "p:2", "sleep 60"],
            &["select-window", "-t", "p:1"],
            &["new-session", "-d", "-s", "other", "sleep 60"],
            &["split-window", "-d", "-t", "other:0.0", "sleep 60"],
        ] {
            assert_success(&harness.run(args)?);
        }
        let mut names = Vec::new();
        for (name, pane) in [
            ("a", "p:0.0"),
            ("b", "p:0.1"),
            ("c", "p:1.0"),
            ("d", "p:2.0"),
            ("o0", "other:0.0"),
            ("o1", "other:0.1"),
        ] {
            let id = query(
                &harness,
                &["display-message", "-p", "-t", pane, "#{pane_id}"],
            )?;
            names.push((id, name));
        }
        let session_id = query(
            &harness,
            &["display-message", "-p", "-t", "p", "#{session_id}"],
        )?;
        let inherited_rmux = format!(
            "{},1,{}",
            harness.socket_path().display(),
            session_id.trim_start_matches('$')
        );
        // The caller runs in pane a, in p:0, while p:1 is the active window.
        let caller_pane = names[0].0.clone();
        let output = harness.run_with(command, |process| {
            process.env("RMUX", &inherited_rmux);
            process.env("RMUX_PANE", &caller_pane);
        })?;
        assert_success(&output);

        let listed = query(
            &harness,
            &[
                "list-windows",
                "-t",
                "p",
                "-F",
                "#{window_index}#{?window_active,*,}=#{pane_id}",
            ],
        )?;
        let mut actual = listed.replace('\n', " ");
        for (id, name) in &names {
            actual = actual.replace(&format!("={id}"), &format!("={name}"));
        }
        if actual != expected {
            mismatches.push(format!("{command:?}: expected {expected}, got {actual}"));
        }
        terminate_child(daemon.child_mut())?;
    }
    assert!(mismatches.is_empty(), "{}", mismatches.join("\n"));
    Ok(())
}

#[test]
fn source_file_defers_omitted_target_to_live_mark_or_stale_current() -> Result<(), Box<dyn Error>> {
    check_source_file_marked_context(false, false)
}

#[test]
fn aliased_source_file_defers_omitted_target_to_live_mark_or_stale_current(
) -> Result<(), Box<dyn Error>> {
    check_source_file_marked_context(true, false)
}

#[test]
fn nested_source_file_defers_omitted_target_to_live_mark_or_stale_current(
) -> Result<(), Box<dyn Error>> {
    check_source_file_marked_context(false, true)
}

fn check_source_file_marked_context(alias: bool, nested: bool) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-file-marked-stale-current")?;
    let mut daemon = harness.start_hidden_daemon()?;
    let mut config = harness.tmpdir().join("marked-swap.conf");

    assert_success(&harness.run(&["new-session", "-d", "-s", "source", "sleep", "60"])?);
    assert_success(&harness.run(&["new-session", "-d", "-s", "decoy", "sleep", "60"])?);
    fs::write(&config, "swap-pane -d -t decoy:0.0\n")?;
    if alias {
        assert_success(&harness.run(&[
            "set-option",
            "-s",
            "command-alias[20]",
            "markedsource=source-file",
        ])?);
    }
    if nested {
        let outer = harness.tmpdir().join("outer.conf");
        fs::write(&outer, format!("source-file {}\n", shell_quote(&config)))?;
        config = outer;
    }
    let source_command = if alias { "markedsource" } else { "source-file" };

    let source_id = query(
        &harness,
        &["display-message", "-p", "-t", "source:0.0", "#{pane_id}"],
    )?;
    let original_target = query(
        &harness,
        &["display-message", "-p", "-t", "decoy:0.0", "#{pane_id}"],
    )?;
    let stale_session = query(
        &harness,
        &["display-message", "-p", "-t", "decoy:0.0", "#{session_id}"],
    )?;
    let mut attach = AttachedSession::spawn(&harness, "source", TerminalSize::new(80, 24))?;
    attach.wait_for_raw_mode(FILE_TIMEOUT)?;
    let inherited_rmux = format!(
        "{},1,{}",
        harness.socket_path().display(),
        stale_session.trim_start_matches('$')
    );
    let config_arg = config.to_str().expect("UTF-8 config path");
    let run_inherited_source = || {
        harness.run_with(&[source_command, config_arg], |command| {
            command.env("RMUX", &inherited_rmux);
            command.env("RMUX_PANE", &source_id);
        })
    };

    assert_success(&harness.run(&["select-pane", "-m", "-t", "source:0.0"])?);
    assert_success(&run_inherited_source()?);
    assert_eq!(
        query(
            &harness,
            &["display-message", "-p", "-t", "decoy:0.0", "#{pane_id}"]
        )?,
        source_id
    );

    assert_success(&harness.run(&["swap-pane", "-d", "-s", "decoy:0.0", "-t", "source:0.0"])?);
    assert_success(&harness.run(&["select-pane", "-M", "-t", "source:0.0"])?);
    let denied = run_inherited_source()?;
    let diagnostic = format!("{}{}", stdout(&denied), stderr(&denied));
    assert_eq!(denied.status.code(), Some(1), "{diagnostic}");
    assert!(
        diagnostic.contains("is not present in inherited session"),
        "unexpected diagnostic: {diagnostic}"
    );
    assert_eq!(
        query(
            &harness,
            &["display-message", "-p", "-t", "decoy:0.0", "#{pane_id}"]
        )?,
        original_target
    );

    assert_success(&harness.run(&[source_command, config_arg])?);
    assert_eq!(
        query(
            &harness,
            &["display-message", "-p", "-t", "decoy:0.0", "#{pane_id}"]
        )?,
        source_id,
        "without inherited identity, the sole attached client remains the implicit source"
    );

    assert_success(&harness.run(&["detach-client"])?);
    assert_eq!(attach.wait_for_exit(FILE_TIMEOUT)?.code(), Some(0));
    attach.assert_restored()?;
    terminate_child(daemon.child_mut())?;
    Ok(())
}

#[test]
fn source_file_join_and_move_renumber_destroyed_source() -> Result<(), Box<dyn Error>> {
    for command in ["join-pane", "move-pane"] {
        let harness = CliHarness::new(&format!("source-file-{command}-renumber"))?;
        let mut daemon = harness.start_hidden_daemon()?;
        create_renumber_transfer_source(&harness, "alpha")?;
        let config = harness.tmpdir().join(format!("{command}-renumber.conf"));
        fs::write(&config, format!("{command} -d -s alpha:1.0 -t alpha:0.0\n"))?;

        assert_success(
            &harness.run(&["source-file", config.to_str().expect("UTF-8 config path")])?,
        );
        assert_contiguous_windows(&harness, "alpha")?;
        terminate_child(daemon.child_mut())?;
    }
    Ok(())
}

#[test]
fn startup_join_and_move_renumber_destroyed_source() -> Result<(), Box<dyn Error>> {
    for command in ["join-pane", "move-pane"] {
        let harness = CliHarness::new(&format!("startup-{command}-renumber"))?;
        let config = harness.tmpdir().join(format!("startup-{command}.conf"));
        fs::write(
            &config,
            format!(
                "new-session -d -s alpha sleep 60\n\
                 new-window -d -t alpha:1 -n SOURCE sleep 60\n\
                 new-window -d -t alpha:2 -n SURVIVOR sleep 60\n\
                 set-option -t alpha renumber-windows on\n\
                 {command} -d -s alpha:1.0 -t alpha:0.0\n"
            ),
        )?;

        assert_success(&harness.run(&[
            "-f",
            config.to_str().expect("UTF-8 startup config path"),
            "new-session",
            "-d",
            "-s",
            "boot",
            "sleep",
            "60",
        ])?);
        assert_contiguous_windows(&harness, "alpha")?;
    }
    Ok(())
}

fn create_renumber_transfer_source(
    harness: &CliHarness,
    session: &str,
) -> Result<(), Box<dyn Error>> {
    assert_success(&harness.run(&["new-session", "-d", "-s", session, "sleep", "60"])?);
    assert_success(&harness.run(&[
        "new-window",
        "-d",
        "-t",
        &format!("{session}:1"),
        "-n",
        "SOURCE",
        "sleep",
        "60",
    ])?);
    assert_success(&harness.run(&[
        "new-window",
        "-d",
        "-t",
        &format!("{session}:2"),
        "-n",
        "SURVIVOR",
        "sleep",
        "60",
    ])?);
    assert_success(&harness.run(&["set-option", "-t", session, "renumber-windows", "on"])?);
    Ok(())
}

fn assert_contiguous_windows(harness: &CliHarness, session: &str) -> Result<(), Box<dyn Error>> {
    let listed = harness.run(&["list-windows", "-t", session, "-F", "#{window_index}"])?;
    assert_eq!(listed.status.code(), Some(0), "{}", stderr(&listed));
    assert_eq!(stdout(&listed), "0\n1\n");
    assert!(stderr(&listed).is_empty());
    Ok(())
}

fn query(harness: &CliHarness, args: &[&str]) -> Result<String, Box<dyn Error>> {
    let output = harness.run(args)?;
    assert_eq!(output.status.code(), Some(0), "{}", stderr(&output));
    assert!(stderr(&output).is_empty());
    Ok(stdout(&output).trim_end().to_owned())
}

fn wait_for_file_contents(
    path: &Path,
    expected: &str,
    timeout: Duration,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + timeout;

    while Instant::now() < deadline {
        match fs::read_to_string(path) {
            Ok(contents) if contents == expected => return Ok(()),
            Ok(_) | Err(_) => std::thread::sleep(Duration::from_millis(25)),
        }
    }

    Err(std::io::Error::other(format!(
        "timed out waiting for '{}' to contain '{}'",
        path.display(),
        expected
    ))
    .into())
}

fn shell_quote(path: &Path) -> String {
    let path = path.display().to_string();
    format!("'{}'", path.replace('\'', r"'\''"))
}
