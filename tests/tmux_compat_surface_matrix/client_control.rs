use super::support::*;

#[test]
fn tmux_compat_list_clients_attached_readonly_ignore_size_flags_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-list-clients-attached-flags")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let _guard = pty_tmux_compat_lock();
    let (config, expected_overrides) = config_with_clean_homes(&harness)?;

    let create = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha"],
        config.clone(),
    )?;
    assert_quiet_success(&create);

    let mut rmux_attach = spawn_rmux_attached_client(&harness, "alpha")?;
    let mut tmux_attach = spawn_tmux_attached_client(&harness, &tmux_binary, "alpha")?;

    let list_clients = wait_for_pair_run(
        &harness,
        &tmux_binary,
        &["list-clients", "-F", "#{client_flags}"],
        config,
        Duration::from_secs(5),
        |run| {
            run.tmux.status_code == Some(0)
                && run.rmux.status_code == Some(0)
                && !run.tmux.stdout.is_empty()
                && !run.rmux.stdout.is_empty()
        },
    )?;
    rmux_attach.assert_running("rmux")?;
    tmux_attach.assert_running("tmux")?;
    assert_run_metadata(
        &list_clients,
        &harness,
        &tmux_binary,
        &["list-clients", "-F", "#{client_flags}"],
        &expected_overrides,
    );
    assert_exact_tmux_compat(&list_clients);
    assert_eq!(
        list_clients.tmux.stdout_string(),
        "attached,focused,ignore-size,read-only,UTF-8\n"
    );

    Ok(())
}

#[test]
fn tmux_compat_detached_new_session_ignores_populated_tmux_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-detached-new-session-tmux")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let (config, mut expected_overrides) = config_with_clean_homes(&harness)?;
    let tmux_value = format!("{},1,0", harness.rmux_socket_path().display());
    let config = config.with_tmux(tmux_value.clone());
    for (name, value) in &mut expected_overrides {
        if name == "TMUX" {
            *value = Some(OsString::from(&tmux_value));
        }
    }
    let argv = ["new-session", "-d", "-s", "alpha"];
    let create = harness.run_pair_with(&tmux_binary, &argv, config)?;

    assert_run_metadata(&create, &harness, &tmux_binary, &argv, &expected_overrides);
    assert_quiet_success(&create);

    Ok(())
}

#[test]
fn tmux_compat_attached_client_utf8_flags_follow_ascii_locale_without_top_level_u_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-client-utf8-ascii-attach")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let _guard = pty_tmux_compat_lock();
    let (config, _) = config_with_clean_homes(&harness)?;
    let create =
        harness.run_pair_with(&tmux_binary, &["new-session", "-d", "-s", "alpha"], config)?;
    assert_quiet_success(&create);

    let client_environment = [("TERM", "vt100"), ("LC_ALL", "C"), ("LANG", "C")];
    let mut rmux_attach =
        spawn_rmux_attached_client_with(&harness, "alpha", &[], &client_environment)?;
    let mut tmux_attach =
        spawn_tmux_attached_client_with(&harness, &tmux_binary, "alpha", &[], &client_environment)?;

    let list_clients = wait_for_pair_run(
        &harness,
        &tmux_binary,
        &["list-clients", "-F", "#{client_utf8}|#{client_flags}"],
        tmux_compat_config(),
        Duration::from_secs(5),
        |run| {
            run.tmux.status_code == Some(0)
                && run.rmux.status_code == Some(0)
                && !run.tmux.stdout.is_empty()
                && !run.rmux.stdout.is_empty()
        },
    )?;
    rmux_attach.assert_running("rmux")?;
    tmux_attach.assert_running("tmux")?;
    assert_exact_tmux_compat(&list_clients);
    assert_eq!(
        list_clients.tmux.stdout_string(),
        "0|attached,focused,ignore-size,read-only\n"
    );

    Ok(())
}

#[test]
fn tmux_compat_choose_client_multi_attached_overlay_rows_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-choose-client-multi-overlay")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let _guard = pty_tmux_compat_lock();
    let config = tmux_compat_config()
        .with_env("LC_ALL", "C.UTF-8")
        .with_env("LC_CTYPE", "C.UTF-8");

    let create = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"],
        config.clone(),
    )?;
    assert_quiet_success(&create);

    let mut rmux_first = spawn_rmux_attached_client(&harness, "alpha")?;
    let mut rmux_second = spawn_rmux_attached_client(&harness, "alpha")?;
    let mut tmux_first = spawn_tmux_attached_client(&harness, &tmux_binary, "alpha")?;
    let mut tmux_second = spawn_tmux_attached_client(&harness, &tmux_binary, "alpha")?;

    let ready = Instant::now() + Duration::from_secs(5);
    while Instant::now() < ready {
        let run = harness.run_pair_with(
            &tmux_binary,
            &["list-clients", "-F", "#{session_name}"],
            config.clone(),
        )?;
        if run.rmux.stdout_string().lines().count() >= 2
            && run.tmux.stdout_string().lines().count() >= 2
        {
            break;
        }
        std::thread::sleep(Duration::from_millis(50));
    }

    rmux_first.assert_running("rmux")?;
    rmux_second.assert_running("rmux")?;
    tmux_first.assert_running("tmux")?;
    tmux_second.assert_running("tmux")?;
    let _ = drain_pty(&mut rmux_first)?;
    let _ = drain_pty(&mut tmux_first)?;

    let choose = harness.run_pair_with(&tmux_binary, &["choose-client"], config)?;
    assert_quiet_success(&choose);
    let mut rmux_bytes = Vec::new();
    let mut tmux_bytes = Vec::new();
    let mut rmux_cells = Vec::new();
    let mut tmux_cells = Vec::new();
    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(100));
        rmux_bytes.extend(drain_pty(&mut rmux_first)?);
        tmux_bytes.extend(drain_pty(&mut tmux_first)?);
        rmux_cells = render_cells(&rmux_bytes, 80, 24)
            .into_iter()
            .map(|line| normalize_pts_paths(line.trim_end()))
            .collect::<Vec<_>>();
        tmux_cells = render_cells(&tmux_bytes, 80, 24)
            .into_iter()
            .map(|line| normalize_pts_paths(line.trim_end()))
            .collect::<Vec<_>>();
        if [0usize, 1, 11, 22].into_iter().all(|row| {
            if row == 11 {
                collapse_repeated_horizontal_borders(&rmux_cells[row])
                    == collapse_repeated_horizontal_borders(&tmux_cells[row])
            } else {
                rmux_cells[row] == tmux_cells[row]
            }
        }) {
            break;
        }
    }

    for row in [0usize, 1, 11, 22] {
        if row == 11 {
            assert_eq!(
                collapse_repeated_horizontal_borders(&rmux_cells[row]),
                collapse_repeated_horizontal_borders(&tmux_cells[row]),
                "row {row} mismatch"
            );
        } else {
            assert_eq!(rmux_cells[row], tmux_cells[row], "row {row} mismatch");
        }
    }

    Ok(())
}

#[test]
fn tmux_compat_control_mode_guard_tuple_and_exit_framing_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    // Cluster H compatibility scenario: check the tmux-observed `%begin`/`%end`/
    // `%error`/`%exit` tuple across the two deterministic control-mode exit
    // triggers (immediate EOF and command-followed-by-EOF). tmux terminates
    // both transcripts with a bare `%exit\n`. rmux baseline build silently closes the
    // EOF-only stream and omits the trailing `%exit\n` after a command, so
    // both assertions below fail on the baseline build release HEAD
    // 0b03537875071738f9a49b01b42b8b6d7f10e5a8 and pass only after the
    // EOF-to-`%exit` promotion in `forward_control` lands.
    let harness = TmuxCompatHarness::new("tmux-compat-control-mode-guard-exit-framing")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };

    // Both scenarios below run against a pre-created session so that the
    // rmux daemon and tmux server are live and plain `-C` has a control
    // client to attach to. This keeps the EOF-only scenario from degrading
    // into a no-daemon cold-start case that produces an empty transcript
    // on rmux (which is not the Cluster H compatibility target).
    let (config, _) = config_with_clean_homes(&harness)?;
    let create =
        harness.run_pair_with(&tmux_binary, &["new-session", "-d", "-s", "alpha"], config)?;
    assert_quiet_success(&create);

    // Scenario 1: immediate EOF. tmux-observed expected final tuple is the
    // bare `%exit\n` terminator (kind=exit, reason=None). Plain `-C` must
    // emit no `-CC` DCS wrapper, so the raw bytes end in `%exit\n` with no
    // `\u001b\\` suffix and no `\u001bP1000p` prefix.
    let eof_tmux = run_tmux_control_mode(&harness, &tmux_binary, "")?;
    let eof_rmux = run_rmux_control_mode(&harness, "")?;
    assert_eq!(eof_tmux.status_code, Some(0));
    assert_eq!(eof_rmux.status_code, Some(0));
    assert!(
        eof_tmux.stderr.is_empty(),
        "tmux stderr must be empty on EOF: {:?}",
        eof_tmux.stderr
    );
    assert!(
        eof_rmux.stderr.is_empty(),
        "rmux stderr must be empty on EOF: {:?}",
        eof_rmux.stderr
    );
    assert_eq!(
        last_control_line(&eof_tmux.stdout).as_deref(),
        Some("%exit"),
        "tmux EOF transcript must end with bare %exit: {:?}",
        eof_tmux.stdout
    );
    assert_eq!(
        last_control_line(&eof_rmux.stdout).as_deref(),
        Some("%exit"),
        "rmux EOF transcript must end with bare %exit: {:?}",
        eof_rmux.stdout
    );
    assert!(
        !eof_rmux.stdout.contains(rmux_proto::CONTROL_CONTROL_START),
        "plain -C must not emit the -CC DCS prefix: {:?}",
        eof_rmux.stdout
    );
    assert!(
        !eof_rmux.stdout.contains(rmux_proto::CONTROL_CONTROL_END),
        "plain -C must not emit the -CC DCS suffix: {:?}",
        eof_rmux.stdout
    );
    assert!(
        !eof_tmux.stdout.contains(rmux_proto::CONTROL_CONTROL_START),
        "tmux plain -C must not emit the -CC DCS prefix: {:?}",
        eof_tmux.stdout
    );

    // Scenario 2: single command + EOF. The tmux-observed tuple shape for
    // the user command is (Begin, <time>, N, 1) paired with (End, <time>,
    // N, 1), followed by a bare `%exit\n`. The `<time>` field is
    // wall-clock and is normalized away; the command number is normalized
    // within each implementation because tmux uses a long-lived global
    // counter and rmux restarts it per control session, but the paired
    // begin/end must reuse the exact same command number and the flags
    // column must be `1` on both.
    let commands = "display-message -p hello\n";
    let cmd_tmux = run_tmux_control_mode(&harness, &tmux_binary, commands)?;
    let cmd_rmux = run_rmux_control_mode(&harness, commands)?;
    assert_eq!(cmd_tmux.status_code, Some(0));
    assert_eq!(cmd_rmux.status_code, Some(0));
    assert!(cmd_tmux.stderr.is_empty());
    assert!(cmd_rmux.stderr.is_empty());
    assert_eq!(
        last_control_line(&cmd_tmux.stdout).as_deref(),
        Some("%exit"),
        "tmux command transcript must end with bare %exit: {:?}",
        cmd_tmux.stdout
    );
    assert_eq!(
        last_control_line(&cmd_rmux.stdout).as_deref(),
        Some("%exit"),
        "rmux command transcript must end with bare %exit: {:?}",
        cmd_rmux.stdout
    );

    let tmux_guards = control_guard_tuples(&cmd_tmux.stdout);
    let rmux_guards = control_guard_tuples(&cmd_rmux.stdout);
    let tmux_last_begin = tmux_guards
        .iter()
        .rev()
        .find(|guard| guard.kind == "begin")
        .expect("tmux must emit at least one %begin for the user command");
    let tmux_last_end = tmux_guards
        .iter()
        .rev()
        .find(|guard| guard.kind == "end")
        .expect("tmux must emit at least one %end for the user command");
    let rmux_last_begin = rmux_guards
        .iter()
        .rev()
        .find(|guard| guard.kind == "begin")
        .expect("rmux must emit at least one %begin for the user command");
    let rmux_last_end = rmux_guards
        .iter()
        .rev()
        .find(|guard| guard.kind == "end")
        .expect("rmux must emit at least one %end for the user command");
    assert_eq!(
        tmux_last_begin.flags, 1,
        "tmux anchors user-command %begin flags to 1"
    );
    assert_eq!(
        tmux_last_end.flags, 1,
        "tmux anchors user-command %end flags to 1"
    );
    assert_eq!(rmux_last_begin.flags, tmux_last_begin.flags);
    assert_eq!(rmux_last_end.flags, tmux_last_end.flags);
    assert_eq!(
        tmux_last_begin.command_number, tmux_last_end.command_number,
        "tmux pairs %begin and %end with the same command number"
    );
    assert_eq!(
        rmux_last_begin.command_number, rmux_last_end.command_number,
        "rmux must pair %begin and %end with the same command number"
    );

    let tmux_payload = extract_control_frame_payload_lines(&cmd_tmux.stdout);
    let rmux_payload = extract_control_frame_payload_lines(&cmd_rmux.stdout);
    assert!(
        tmux_payload.iter().any(|line| line == "hello"),
        "tmux payload must contain the display-message output: {tmux_payload:?}"
    );
    assert!(
        rmux_payload.iter().any(|line| line == "hello"),
        "rmux payload must contain the display-message output: {rmux_payload:?}"
    );

    // tmux emits an initial control attach guard pair with flags=0, then
    // every user command guard uses flags=1. The concrete absolute command
    // numbers may differ, but rmux must keep them positive and monotonic.
    assert!(
        !rmux_guards.is_empty(),
        "rmux transcript must contain at least one guard tuple: {:?}",
        cmd_rmux.stdout
    );
    assert!(
        rmux_guards
            .iter()
            .any(|guard| guard.kind == "begin" && guard.flags == 0),
        "rmux transcript must include the initial flags=0 control guard: {rmux_guards:?}"
    );
    assert_eq!(
        rmux_last_begin.flags, 1,
        "last rmux %begin must be the user command guard: {rmux_last_begin:?}"
    );
    assert_eq!(
        rmux_last_end.flags, 1,
        "last rmux %end must be the user command guard: {rmux_last_end:?}"
    );
    let rmux_begin_numbers = rmux_guards
        .iter()
        .filter(|guard| guard.kind == "begin")
        .map(|guard| guard.command_number)
        .collect::<Vec<_>>();
    assert!(
        rmux_begin_numbers.iter().all(|number| *number >= 1),
        "rmux command numbers must be positive: {rmux_begin_numbers:?}"
    );
    assert!(
        rmux_begin_numbers.windows(2).all(|pair| pair[1] > pair[0]),
        "rmux command numbers must be strictly monotonic: {rmux_begin_numbers:?}"
    );

    // Plain `-C` must not wrap either transcript in the `-CC` DCS envelope.
    assert!(
        !cmd_rmux.stdout.contains(rmux_proto::CONTROL_CONTROL_START),
        "plain -C command transcript must not contain DCS prefix: {:?}",
        cmd_rmux.stdout
    );
    assert!(
        !cmd_rmux.stdout.contains(rmux_proto::CONTROL_CONTROL_END),
        "plain -C command transcript must not contain DCS suffix: {:?}",
        cmd_rmux.stdout
    );
    assert!(
        !cmd_tmux.stdout.contains(rmux_proto::CONTROL_CONTROL_START),
        "tmux plain -C command transcript must not contain DCS prefix: {:?}",
        cmd_tmux.stdout
    );

    // Scenario 3: command parse failure + EOF. The tmux-observed failure
    // tuple is (Begin, <time>, N, 1) followed by (Error, <time>, N, 1)
    // and then the same bare `%exit\n` terminator. The concrete diagnostic
    // text is not part of Cluster H; this assertion checks the tuple shape
    // and the flags/command-number relationship.
    let error_commands = "no-such-rmux-cluster-h-command\n";
    let error_tmux = run_tmux_control_mode(&harness, &tmux_binary, error_commands)?;
    let error_rmux = run_rmux_control_mode(&harness, error_commands)?;
    assert_eq!(error_tmux.status_code, Some(0));
    assert_eq!(error_rmux.status_code, Some(0));
    assert!(error_tmux.stderr.is_empty());
    assert!(error_rmux.stderr.is_empty());
    assert_eq!(
        last_control_line(&error_tmux.stdout).as_deref(),
        Some("%exit"),
        "tmux error transcript must end with bare %exit: {:?}",
        error_tmux.stdout
    );
    assert_eq!(
        last_control_line(&error_rmux.stdout).as_deref(),
        Some("%exit"),
        "rmux error transcript must end with bare %exit: {:?}",
        error_rmux.stdout
    );

    let tmux_error_guards = control_guard_tuples(&error_tmux.stdout);
    let rmux_error_guards = control_guard_tuples(&error_rmux.stdout);
    let tmux_error_begin = tmux_error_guards
        .iter()
        .rev()
        .find(|guard| guard.kind == "begin")
        .expect("tmux must emit %begin before the parse error");
    let tmux_error = tmux_error_guards
        .iter()
        .rev()
        .find(|guard| guard.kind == "error")
        .expect("tmux must emit %error for the parse error");
    let rmux_error_begin = rmux_error_guards
        .iter()
        .rev()
        .find(|guard| guard.kind == "begin")
        .expect("rmux must emit %begin before the parse error");
    let rmux_error = rmux_error_guards
        .iter()
        .rev()
        .find(|guard| guard.kind == "error")
        .expect("rmux must emit %error for the parse error");
    assert_eq!(tmux_error_begin.flags, 1);
    assert_eq!(tmux_error.flags, 1);
    assert_eq!(rmux_error_begin.flags, tmux_error_begin.flags);
    assert_eq!(rmux_error.flags, tmux_error.flags);
    assert_eq!(
        tmux_error_begin.command_number, tmux_error.command_number,
        "tmux pairs parse-error %begin and %error with one command number"
    );
    assert_eq!(
        rmux_error_begin.command_number, rmux_error.command_number,
        "rmux must pair parse-error %begin and %error with one command number"
    );

    Ok(())
}

#[derive(Debug, Clone)]
struct ControlGuardTuple {
    kind: String,
    command_number: u64,
    flags: u8,
}

fn control_guard_tuples(output: &str) -> Vec<ControlGuardTuple> {
    let mut guards = Vec::new();
    for line in output.lines() {
        let parsed = line
            .strip_prefix("%begin ")
            .map(|rest| ("begin", rest))
            .or_else(|| line.strip_prefix("%end ").map(|rest| ("end", rest)))
            .or_else(|| line.strip_prefix("%error ").map(|rest| ("error", rest)));
        let Some((kind, rest)) = parsed else {
            continue;
        };
        let mut parts = rest.split_whitespace();
        let _time = parts.next();
        let command_number = parts.next().and_then(|value| value.parse::<u64>().ok());
        let flags = parts.next().and_then(|value| value.parse::<u8>().ok());
        if let (Some(command_number), Some(flags)) = (command_number, flags) {
            guards.push(ControlGuardTuple {
                kind: kind.to_owned(),
                command_number,
                flags,
            });
        }
    }
    guards
}

fn last_control_line(output: &str) -> Option<String> {
    output
        .lines()
        .rfind(|line| !line.is_empty())
        .map(ToOwned::to_owned)
}

#[test]
fn tmux_compat_list_clients_control_mode_flags_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-list-clients-control-flags")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let (config, _) = config_with_clean_homes(&harness)?;
    let create =
        harness.run_pair_with(&tmux_binary, &["new-session", "-d", "-s", "alpha"], config)?;
    assert_quiet_success(&create);

    let commands = "attach-session -t alpha\nlist-clients -F '#{client_flags}'\n";
    let tmux_run = run_tmux_control_mode(&harness, &tmux_binary, commands)?;
    let rmux_run = run_rmux_control_mode(&harness, commands)?;
    assert_eq!(tmux_run.status_code, Some(0));
    assert_eq!(rmux_run.status_code, Some(0));
    assert!(tmux_run.stderr.is_empty());
    assert!(rmux_run.stderr.is_empty());

    let tmux_flags = extract_control_frame_payload_lines(&tmux_run.stdout);
    let rmux_flags = extract_control_frame_payload_lines(&rmux_run.stdout);
    assert_eq!(rmux_flags, tmux_flags);
    assert_eq!(
        tmux_flags,
        vec!["attached,focused,control-mode,UTF-8".to_owned()]
    );

    Ok(())
}

#[test]
fn tmux_compat_attached_client_top_level_terminal_runtime_overrides_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-client-runtime-top-level-attach")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let _guard = pty_tmux_compat_lock();
    let (config, _) = config_with_clean_homes(&harness)?;
    let create =
        harness.run_pair_with(&tmux_binary, &["new-session", "-d", "-s", "alpha"], config)?;
    assert_quiet_success(&create);

    let client_environment = [("TERM", "vt100"), ("LC_ALL", "C"), ("LANG", "C")];
    let top_level_args = ["-u", "-2", "-T", "RGB"];
    let mut rmux_attach =
        spawn_rmux_attached_client_with(&harness, "alpha", &top_level_args, &client_environment)?;
    let mut tmux_attach = spawn_tmux_attached_client_with(
        &harness,
        &tmux_binary,
        "alpha",
        &top_level_args,
        &client_environment,
    )?;

    let list_clients = wait_for_pair_run(
        &harness,
        &tmux_binary,
        &[
            "list-clients",
            "-F",
            "#{client_termname}|#{client_termtype}|#{client_termfeatures}|#{client_utf8}|#{client_flags}",
        ],
        tmux_compat_config(),
        Duration::from_secs(5),
        |run| {
            run.tmux.status_code == Some(0)
                && run.rmux.status_code == Some(0)
                && !run.tmux.stdout.is_empty()
                && !run.rmux.stdout.is_empty()
        },
    )?;
    rmux_attach.assert_running("rmux")?;
    tmux_attach.assert_running("tmux")?;
    let tmux_line = list_clients.tmux.stdout_string();
    let rmux_line = list_clients.rmux.stdout_string();
    let tmux_parts = tmux_line
        .trim_end()
        .split('|')
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    let rmux_parts = rmux_line
        .trim_end()
        .split('|')
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    assert_eq!(tmux_parts.len(), 5);
    assert_eq!(rmux_parts.len(), 5);
    assert_eq!(tmux_parts[0], "vt100");
    assert_eq!(rmux_parts[0], tmux_parts[0]);
    assert_eq!(rmux_parts[1], tmux_parts[1]);
    assert_eq!(rmux_parts[3], tmux_parts[3]);
    assert_eq!(rmux_parts[4], tmux_parts[4]);
    assert_eq!(tmux_parts[3], "1");
    assert!(
        tmux_parts[2].split(',').any(|feature| feature == "256"),
        "expected tmux termfeatures to include 256, got {:?}",
        tmux_parts[2]
    );
    assert!(
        tmux_parts[2].split(',').any(|feature| feature == "RGB"),
        "expected tmux termfeatures to include RGB, got {:?}",
        tmux_parts[2]
    );
    assert!(
        rmux_parts[2].split(',').any(|feature| feature == "256"),
        "expected rmux termfeatures to include 256, got {:?}",
        rmux_parts[2]
    );
    assert!(
        rmux_parts[2].split(',').any(|feature| feature == "RGB"),
        "expected rmux termfeatures to include RGB, got {:?}",
        rmux_parts[2]
    );

    Ok(())
}

#[test]
fn tmux_compat_control_mode_top_level_terminal_runtime_overrides_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-client-runtime-top-level-control")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let (config, _) = config_with_clean_homes(&harness)?;
    let create =
        harness.run_pair_with(&tmux_binary, &["new-session", "-d", "-s", "alpha"], config)?;
    assert_quiet_success(&create);

    let client_environment = [("TERM", "vt100"), ("LC_ALL", "C"), ("LANG", "C")];
    let top_level_args = ["-u", "-2", "-T", "RGB"];
    let commands = "attach-session -t alpha\nlist-clients -F '#{client_termname}|#{client_termtype}|#{client_termfeatures}|#{client_utf8}|#{client_flags}'\n";
    let tmux_run = run_tmux_control_mode_with(
        &harness,
        &tmux_binary,
        commands,
        &top_level_args,
        &client_environment,
    )?;
    let rmux_run =
        run_rmux_control_mode_with(&harness, commands, &top_level_args, &client_environment)?;
    assert_eq!(tmux_run.status_code, Some(0));
    assert_eq!(rmux_run.status_code, Some(0));
    assert!(tmux_run.stderr.is_empty());
    assert!(rmux_run.stderr.is_empty());

    let tmux_lines = extract_control_frame_payload_lines(&tmux_run.stdout);
    let rmux_lines = extract_control_frame_payload_lines(&rmux_run.stdout);
    assert_eq!(rmux_lines, tmux_lines);
    assert_eq!(
        tmux_lines,
        vec!["vt100||256,RGB|1|attached,focused,control-mode,UTF-8".to_owned()]
    );

    Ok(())
}

#[test]
fn tmux_compat_new_window_control_mode_start_directory_and_shell_command_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-new-window-control-mode-spawn")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let config = tmux_compat_config();
    let start_directory = harness.tmpdir().join("new-window-cwd");
    fs::create_dir_all(&start_directory)?;
    let start_directory = start_directory.to_string_lossy().into_owned();

    let create = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha"],
        config.clone(),
    )?;
    assert_quiet_success(&create);

    let commands = format!(
        "new-window -d -t alpha -c {} -- sh -c 'pwd; printf \"ARGV0=[%s]\\n\" \"$0\"; printf \"ARGV=\"; for arg in \"$@\"; do printf \"[%s]\" \"$arg\"; done; printf \"\\n\"; printf \"shell=quoted ; value\\n\"; exec sleep 30' foo 'bar baz'\n",
        shell_quote(&start_directory)
    );
    let tmux_run = run_tmux_control_mode(&harness, &tmux_binary, &commands)?;
    let rmux_run = run_rmux_control_mode(&harness, &commands)?;
    assert_eq!(tmux_run.status_code, Some(0));
    assert_eq!(rmux_run.status_code, Some(0));
    assert!(tmux_run.stderr.is_empty());
    assert!(rmux_run.stderr.is_empty());

    let expected_lines = vec![
        start_directory.clone(),
        "ARGV0=[foo]".to_owned(),
        "ARGV=[bar baz]".to_owned(),
        "shell=quoted ; value".to_owned(),
    ];
    let capture = wait_for_pair_run(
        &harness,
        &tmux_binary,
        &["capture-pane", "-p", "-t", "alpha:1.0"],
        config,
        Duration::from_secs(5),
        |run| {
            run.tmux.status_code == Some(0)
                && run.rmux.status_code == Some(0)
                && nonempty_capture_lines(&run.tmux.stdout_string()) == expected_lines
                && nonempty_capture_lines(&run.rmux.stdout_string()) == expected_lines
        },
    )?;
    assert_exact_tmux_compat(&capture);

    Ok(())
}

#[test]
fn tmux_compat_respawn_window_control_mode_start_directory_and_shell_command_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-respawn-window-control-mode-spawn")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let config = tmux_compat_config();
    let start_directory = harness.tmpdir().join("respawn-window-cwd");
    fs::create_dir_all(&start_directory)?;
    let start_directory = start_directory.to_string_lossy().into_owned();

    let create = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha"],
        config.clone(),
    )?;
    assert_quiet_success(&create);

    let commands = format!(
        "respawn-window -k -t alpha:0 -c {} -- sh -c 'pwd; printf \"ARGV0=[%s]\\n\" \"$0\"; printf \"ARGV=\"; for arg in \"$@\"; do printf \"[%s]\" \"$arg\"; done; printf \"\\n\"; printf \"shell=quoted ; value\\n\"; exec sleep 30' foo 'bar baz'\n",
        shell_quote(&start_directory)
    );
    let tmux_run = run_tmux_control_mode(&harness, &tmux_binary, &commands)?;
    let rmux_run = run_rmux_control_mode(&harness, &commands)?;
    assert_eq!(tmux_run.status_code, Some(0));
    assert_eq!(rmux_run.status_code, Some(0));
    assert!(tmux_run.stderr.is_empty());
    assert!(rmux_run.stderr.is_empty());

    let expected_lines = vec![
        start_directory.clone(),
        "ARGV0=[foo]".to_owned(),
        "ARGV=[bar baz]".to_owned(),
        "shell=quoted ; value".to_owned(),
    ];
    let capture = wait_for_pair_run(
        &harness,
        &tmux_binary,
        &["capture-pane", "-p", "-t", "alpha:0.0"],
        config,
        Duration::from_secs(5),
        |run| {
            run.tmux.status_code == Some(0)
                && run.rmux.status_code == Some(0)
                && nonempty_capture_lines(&run.tmux.stdout_string()) == expected_lines
                && nonempty_capture_lines(&run.rmux.stdout_string()) == expected_lines
        },
    )?;
    assert_exact_tmux_compat(&capture);

    Ok(())
}

#[test]
fn tmux_compat_control_mode_window_id_targets_and_new_window_exact_slot_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-control-mode-window-id-targets")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let config = tmux_compat_config();

    let create = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha"],
        config.clone(),
    )?;
    assert_quiet_success(&create);

    let window_id_run = harness.run_pair_with(
        &tmux_binary,
        &["display-message", "-p", "-t", "alpha:0", "#{window_id}"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&window_id_run);
    let window_id = window_id_run.tmux.stdout_string().trim().to_owned();

    let new_window_commands = "new-window -d -t alpha:2 -- sleep 30\n";
    let tmux_new_window = run_tmux_control_mode(&harness, &tmux_binary, new_window_commands)?;
    let rmux_new_window = run_rmux_control_mode(&harness, new_window_commands)?;
    assert_eq!(tmux_new_window.status_code, Some(0));
    assert_eq!(rmux_new_window.status_code, Some(0));
    assert!(tmux_new_window.stderr.is_empty());
    assert!(rmux_new_window.stderr.is_empty());

    let new_window_display = wait_for_pair_run(
        &harness,
        &tmux_binary,
        &[
            "display-message",
            "-p",
            "-t",
            "alpha:2",
            "#{window_index}|#{pane_current_command}",
        ],
        config.clone(),
        Duration::from_secs(5),
        |run| {
            run.tmux.status_code == Some(0)
                && run.rmux.status_code == Some(0)
                && run.tmux.stdout == b"2|sleep\n"
                && run.rmux.stdout == b"2|sleep\n"
        },
    )?;
    assert_exact_tmux_compat(&new_window_display);

    let respawn_and_display_commands = format!("respawn-window -k -t {window_id} -- sleep 30\n");
    let tmux_respawn =
        run_tmux_control_mode(&harness, &tmux_binary, &respawn_and_display_commands)?;
    let rmux_respawn = run_rmux_control_mode(&harness, &respawn_and_display_commands)?;
    assert_eq!(tmux_respawn.status_code, Some(0));
    assert_eq!(rmux_respawn.status_code, Some(0));
    assert!(tmux_respawn.stderr.is_empty());
    assert!(rmux_respawn.stderr.is_empty());

    let expected_respawn = format!("alpha|0|{window_id}|sleep");
    let respawn_display_commands = format!(
        "display-message -p -t {window_id} '#{{session_name}}|#{{window_index}}|#{{window_id}}|#{{pane_current_command}}'\n"
    );
    let deadline = Instant::now() + Duration::from_secs(5);
    let (tmux_display, rmux_display) = loop {
        let tmux_display =
            run_tmux_control_mode(&harness, &tmux_binary, &respawn_display_commands)?;
        let rmux_display = run_rmux_control_mode(&harness, &respawn_display_commands)?;
        if tmux_display.status_code == Some(0)
            && rmux_display.status_code == Some(0)
            && tmux_display.stderr.is_empty()
            && rmux_display.stderr.is_empty()
            && extract_control_frame_payload_lines(&tmux_display.stdout)
                == vec![expected_respawn.clone()]
            && extract_control_frame_payload_lines(&rmux_display.stdout)
                == vec![expected_respawn.clone()]
        {
            break (tmux_display, rmux_display);
        }

        if Instant::now() >= deadline {
            return Err(format!(
                "timed out waiting for respawn-window compatibility readiness: tmux stdout={:?} stderr={:?} rmux stdout={:?} stderr={:?}",
                tmux_display.stdout, tmux_display.stderr, rmux_display.stdout, rmux_display.stderr
            )
            .into());
        }

        std::thread::sleep(Duration::from_millis(50));
    };
    assert_eq!(
        extract_control_frame_payload_lines(&tmux_display.stdout),
        vec![expected_respawn.clone()]
    );
    assert_eq!(
        extract_control_frame_payload_lines(&rmux_display.stdout),
        vec![expected_respawn]
    );

    Ok(())
}
