use super::support::*;

#[test]
fn tmux_compat_list_clients_reports_stable_session_id_for_pty_and_control_clients(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-list-clients-session-id")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let _guard = pty_tmux_compat_lock();
    let config = tmux_compat_config();

    for session in ["alpha", "beta"] {
        let create = harness.run_pair_with(
            &tmux_binary,
            &["new-session", "-d", "-s", session],
            config.clone(),
        )?;
        assert_quiet_success(&create);
    }

    let mut tmux_pty = spawn_tmux_attached_client(&harness, &tmux_binary, "alpha")?;
    let mut rmux_pty = spawn_rmux_attached_client(&harness, "alpha")?;
    let format = "#{session_id}|#{session_name}|#{client_session}|#{client_control_mode}";

    // Spawning a PTY client does not wait for its registration. Observe it on
    // both servers before starting control clients so the asserted order is fixed.
    let pty_registered = wait_for_pair_run(
        &harness,
        &tmux_binary,
        &["list-clients", "-F", format],
        config.clone(),
        Duration::from_secs(5),
        |run| {
            run.tmux.stdout_string().lines().count() == 1
                && run.rmux.stdout_string().lines().count() == 1
        },
    )?;
    assert_success_without_stderr(&pty_registered);
    assert_eq!(pty_registered.tmux.stdout_string(), "$0|alpha|alpha|0\n");
    assert_exact_tmux_compat(&pty_registered);

    let mut tmux_control =
        LiveControlClient::spawn(tmux_control_mode_command(&harness, &tmux_binary, &[], &[])?)?;
    let mut rmux_control =
        LiveControlClient::spawn(rmux_control_mode_command(&harness, &[], &[])?)?;
    tmux_control.send("attach-session -t alpha\n")?;
    rmux_control.send("attach-session -t alpha\n")?;

    // A control client can be listed before its attach command has completed.
    let listed = wait_for_pair_run(
        &harness,
        &tmux_binary,
        &["list-clients", "-F", format],
        config.clone(),
        Duration::from_secs(5),
        |run| {
            run.tmux.stdout_string().lines().count() == 2
                && run.rmux.stdout_string().lines().count() == 2
                && run.tmux.stdout_string().contains("|alpha|alpha|1\n")
                && run.rmux.stdout_string().contains("|alpha|alpha|1\n")
        },
    )?;

    tmux_pty.assert_running("tmux PTY")?;
    rmux_pty.assert_running("rmux PTY")?;
    tmux_control.assert_running("tmux control")?;
    rmux_control.assert_running("rmux control")?;
    assert_success_without_stderr(&listed);
    assert_eq!(
        listed.tmux.stdout_string(),
        "$0|alpha|alpha|0\n$0|alpha|alpha|1\n"
    );
    assert_exact_tmux_compat(&listed);

    tmux_control.send("switch-client -t beta\n")?;
    rmux_control.send("switch-client -t beta\n")?;
    let switched = wait_for_pair_run(
        &harness,
        &tmux_binary,
        &["list-clients", "-F", format],
        config.clone(),
        Duration::from_secs(5),
        |run| {
            run.tmux.stdout_string().contains("$1|beta|beta|1")
                && run.rmux.stdout_string().contains("$1|beta|beta|1")
        },
    )?;
    assert_success_without_stderr(&switched);
    assert_eq!(
        switched.tmux.stdout_string(),
        "$0|alpha|alpha|0\n$1|beta|beta|1\n"
    );
    assert_exact_tmux_compat(&switched);

    let rename = harness.run_pair_with(
        &tmux_binary,
        &["rename-session", "-t", "alpha", "renamed"],
        config.clone(),
    )?;
    assert_quiet_success(&rename);
    let renamed = wait_for_pair_run(
        &harness,
        &tmux_binary,
        &["list-clients", "-F", format],
        config,
        Duration::from_secs(5),
        |run| {
            run.tmux.stdout_string().contains("$0|renamed|renamed|0")
                && run.rmux.stdout_string().contains("$0|renamed|renamed|0")
        },
    )?;
    assert_success_without_stderr(&renamed);
    assert_eq!(
        renamed.tmux.stdout_string(),
        "$0|renamed|renamed|0\n$1|beta|beta|1\n"
    );
    assert_exact_tmux_compat(&renamed);

    Ok(())
}

#[test]
fn tmux_compat_client_termfeatures_truecolor_requires_explicit_256() -> Result<(), Box<dyn Error>> {
    // Measured on tmux 3.7b: xterm-256color + COLORTERM=truecolor reports
    // RGB, but not 256. Adding -2 reports both. Compare these tokens rather
    // than the entire list, which includes unrelated terminal capabilities.
    for (label, args, expected_256) in [
        ("implicit", &[][..], false),
        ("explicit", &["-2"][..], true),
    ] {
        let harness = TmuxCompatHarness::new(&format!("termfeatures-256-{label}"))?;
        let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
            return Ok(());
        };
        let _guard = pty_tmux_compat_lock();
        let (config, _) = config_with_clean_homes(&harness)?;
        let create = harness.run_pair_with(
            &tmux_binary,
            &["new-session", "-d", "-s", "alpha"],
            config.clone(),
        )?;
        assert_quiet_success(&create);
        let environment = [
            ("TERM", "xterm-256color"),
            ("COLORTERM", "truecolor"),
            ("TERM_PROGRAM", "Windows Terminal"),
            ("WT_SESSION", "termfeatures-oracle"),
        ];
        let mut tmux_client =
            spawn_tmux_attached_client_with(&harness, &tmux_binary, "alpha", args, &environment)?;
        let mut rmux_client =
            spawn_rmux_attached_client_with(&harness, "alpha", args, &environment)?;
        let listed = wait_for_pair_run(
            &harness,
            &tmux_binary,
            &[
                "list-clients",
                "-F",
                "#{client_termname}|#{client_termfeatures}",
            ],
            config,
            Duration::from_secs(5),
            |run| {
                run.tmux.stdout_string().lines().count() == 1
                    && run.rmux.stdout_string().lines().count() == 1
            },
        )?;
        assert_success_without_stderr(&listed);
        tmux_client.assert_running("tmux termfeatures")?;
        rmux_client.assert_running("rmux termfeatures")?;
        let tmux_line = listed.tmux.stdout_string();
        let rmux_line = listed.rmux.stdout_string();
        eprintln!("termfeatures {label}: tmux={tmux_line:?} rmux={rmux_line:?}");
        let projection = |line: &str| {
            let (term, features) = line.trim_end().split_once('|').expect("client metadata");
            assert_eq!(term, "xterm-256color");
            let features = features.split(',').collect::<Vec<_>>();
            (features.contains(&"256"), features.contains(&"RGB"))
        };
        let oracle = projection(&tmux_line);
        assert_eq!(oracle, (expected_256, true), "tmux oracle changed: {label}");
        assert_eq!(
            projection(&rmux_line),
            oracle,
            "termfeatures mismatch: {label}"
        );
    }
    Ok(())
}
