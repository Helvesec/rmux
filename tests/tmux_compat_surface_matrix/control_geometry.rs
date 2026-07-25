use super::support::*;

#[derive(Clone, Copy)]
struct GeometryCase {
    label: &'static str,
    policy: &'static str,
    first: PtyTerminalSize,
    second: PtyTerminalSize,
    combined: PtyTerminalSize,
    remove_first: bool,
}

#[test]
fn tmux_compat_control_geometry_survives_attach_and_control_departures_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-control-geometry-lifecycle")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let _guard = pty_tmux_compat_lock();

    // Frozen tmux 3.7b oracle, measured 2026-07-25. These cases pin the
    // server-global client ordering used by latest as well as the full client
    // set used by largest and smallest.
    for case in [
        GeometryCase {
            label: "mixed-latest",
            policy: "latest",
            first: size(100, 40),
            second: size(60, 20),
            combined: size(60, 20),
            remove_first: false,
        },
        GeometryCase {
            label: "mixed-largest",
            policy: "largest",
            first: size(100, 40),
            second: size(60, 20),
            combined: size(100, 40),
            remove_first: false,
        },
        GeometryCase {
            label: "mixed-smallest",
            policy: "smallest",
            first: size(60, 20),
            second: size(100, 40),
            combined: size(60, 20),
            remove_first: false,
        },
    ] {
        assert_control_and_attach_departure(&harness, &tmux_binary, case)?;
    }

    for case in [
        GeometryCase {
            label: "controls-latest",
            policy: "latest",
            first: size(100, 40),
            second: size(60, 20),
            combined: size(60, 20),
            remove_first: false,
        },
        GeometryCase {
            label: "controls-largest",
            policy: "largest",
            first: size(100, 40),
            second: size(60, 20),
            combined: size(100, 40),
            remove_first: true,
        },
        GeometryCase {
            label: "controls-smallest",
            policy: "smallest",
            first: size(60, 20),
            second: size(100, 40),
            combined: size(60, 20),
            remove_first: true,
        },
    ] {
        assert_control_departure(&harness, &tmux_binary, case)?;
    }

    Ok(())
}

#[test]
fn tmux_compat_control_switch_reconciles_source_geometry_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-control-switch-source-geometry")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let _guard = pty_tmux_compat_lock();
    let config = config();

    for argv in [
        ["new-session", "-d", "-s", "source"].as_slice(),
        ["new-session", "-d", "-s", "target"].as_slice(),
        ["set-option", "-t", "source", "status", "off"].as_slice(),
        ["set-option", "-t", "target", "status", "off"].as_slice(),
        ["set-option", "-w", "-t", "source", "window-size", "largest"].as_slice(),
        ["set-option", "-w", "-t", "target", "window-size", "largest"].as_slice(),
    ] {
        let run = harness.run_pair_with(&tmux_binary, argv, config.clone())?;
        assert_quiet_success(&run);
    }

    let mut tmux_switching =
        LiveControlClient::spawn(tmux_control_mode_command(&harness, &tmux_binary, &[], &[])?)?;
    let mut rmux_switching =
        LiveControlClient::spawn(rmux_control_mode_command(&harness, &[], &[])?)?;
    let mut tmux_surviving =
        LiveControlClient::spawn(tmux_control_mode_command(&harness, &tmux_binary, &[], &[])?)?;
    let mut rmux_surviving =
        LiveControlClient::spawn(rmux_control_mode_command(&harness, &[], &[])?)?;
    attach_control_pair(
        &mut tmux_switching,
        &mut rmux_switching,
        "source",
        size(101, 41),
    )?;
    attach_control_pair(
        &mut tmux_surviving,
        &mut rmux_surviving,
        "source",
        size(60, 20),
    )?;
    wait_for_state(
        &harness,
        &tmux_binary,
        "source",
        &[size(101, 41), size(60, 20)],
        size(101, 41),
        config.clone(),
    )?;

    tmux_switching.send("switch-client -t target\n")?;
    rmux_switching.send("switch-client -t target\n")?;
    let switched = wait_for_pair_run(
        &harness,
        &tmux_binary,
        &[
            "display-message",
            "-p",
            "-t",
            "source",
            "#{window_width}x#{window_height}",
            ";",
            "display-message",
            "-p",
            "-t",
            "target",
            "#{window_width}x#{window_height}",
        ],
        config,
        Duration::from_secs(5),
        |run| {
            run.tmux.stdout_string() == "60x20\n101x41\n"
                && run.rmux.stdout_string() == "60x20\n101x41\n"
        },
    )?;
    assert_success_without_stderr(&switched);
    assert_eq!(switched.rmux.stdout, switched.tmux.stdout);

    tmux_switching.assert_running("tmux switched control")?;
    rmux_switching.assert_running("rmux switched control")?;
    tmux_surviving.assert_running("tmux source control")?;
    rmux_surviving.assert_running("rmux source control")?;
    Ok(())
}

#[test]
fn tmux_compat_latest_control_order_survives_an_older_client_resize_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-control-latest-resize-order")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let _guard = pty_tmux_compat_lock();
    let case = GeometryCase {
        label: "controls-latest-resize",
        policy: "latest",
        first: size(100, 40),
        second: size(60, 20),
        combined: size(60, 20),
        remove_first: false,
    };
    setup_session(&harness, &tmux_binary, case)?;
    let config = config();
    let mut tmux_first =
        LiveControlClient::spawn(tmux_control_mode_command(&harness, &tmux_binary, &[], &[])?)?;
    let mut rmux_first = LiveControlClient::spawn(rmux_control_mode_command(&harness, &[], &[])?)?;
    let mut tmux_latest =
        LiveControlClient::spawn(tmux_control_mode_command(&harness, &tmux_binary, &[], &[])?)?;
    let mut rmux_latest = LiveControlClient::spawn(rmux_control_mode_command(&harness, &[], &[])?)?;

    attach_control_pair(&mut tmux_first, &mut rmux_first, case.label, case.first)?;
    wait_for_state(
        &harness,
        &tmux_binary,
        case.label,
        &[case.first],
        case.first,
        config.clone(),
    )?;
    attach_control_pair(&mut tmux_latest, &mut rmux_latest, case.label, case.second)?;
    wait_for_state(
        &harness,
        &tmux_binary,
        case.label,
        &[case.first, case.second],
        case.combined,
        config.clone(),
    )?;

    let resized_first = size(101, 41);
    let refresh = format!(
        "refresh-client -C {}x{}\n",
        resized_first.cols, resized_first.rows
    );
    tmux_first.send(&refresh)?;
    rmux_first.send(&refresh)?;
    wait_for_state(
        &harness,
        &tmux_binary,
        case.label,
        &[resized_first, case.second],
        case.second,
        config,
    )?;
    tmux_first.assert_running("tmux older control")?;
    rmux_first.assert_running("rmux older control")?;
    tmux_latest.assert_running("tmux latest control")?;
    rmux_latest.assert_running("rmux latest control")?;
    Ok(())
}

fn assert_control_and_attach_departure(
    harness: &TmuxCompatHarness,
    tmux_binary: &Path,
    case: GeometryCase,
) -> Result<(), Box<dyn Error>> {
    setup_session(harness, tmux_binary, case)?;
    let config = config();
    let mut tmux_control =
        LiveControlClient::spawn(tmux_control_mode_command(harness, tmux_binary, &[], &[])?)?;
    let mut rmux_control = LiveControlClient::spawn(rmux_control_mode_command(harness, &[], &[])?)?;
    attach_control_pair(&mut tmux_control, &mut rmux_control, case.label, case.first)?;
    wait_for_state(
        harness,
        tmux_binary,
        case.label,
        &[case.first],
        case.first,
        config.clone(),
    )?;

    let tmux_attach =
        spawn_tmux_attached_input_client_at_size(harness, tmux_binary, case.label, case.second)?;
    let rmux_attach = spawn_rmux_attached_input_client_at_size(harness, case.label, case.second)?;
    wait_for_state(
        harness,
        tmux_binary,
        case.label,
        &[case.first, case.second],
        case.combined,
        config.clone(),
    )?;

    drop(tmux_attach);
    drop(rmux_attach);
    wait_for_state(
        harness,
        tmux_binary,
        case.label,
        &[case.first],
        case.first,
        config,
    )?;
    tmux_control.assert_running("tmux surviving control")?;
    rmux_control.assert_running("rmux surviving control")?;
    Ok(())
}

fn assert_control_departure(
    harness: &TmuxCompatHarness,
    tmux_binary: &Path,
    case: GeometryCase,
) -> Result<(), Box<dyn Error>> {
    setup_session(harness, tmux_binary, case)?;
    let config = config();
    let mut tmux_first =
        LiveControlClient::spawn(tmux_control_mode_command(harness, tmux_binary, &[], &[])?)?;
    let mut rmux_first = LiveControlClient::spawn(rmux_control_mode_command(harness, &[], &[])?)?;
    let mut tmux_second =
        LiveControlClient::spawn(tmux_control_mode_command(harness, tmux_binary, &[], &[])?)?;
    let mut rmux_second = LiveControlClient::spawn(rmux_control_mode_command(harness, &[], &[])?)?;
    attach_control_pair(&mut tmux_first, &mut rmux_first, case.label, case.first)?;
    wait_for_state(
        harness,
        tmux_binary,
        case.label,
        &[case.first],
        case.first,
        config.clone(),
    )?;
    attach_control_pair(&mut tmux_second, &mut rmux_second, case.label, case.second)?;
    wait_for_state(
        harness,
        tmux_binary,
        case.label,
        &[case.first, case.second],
        case.combined,
        config.clone(),
    )?;

    let surviving = if case.remove_first {
        drop(tmux_first);
        drop(rmux_first);
        case.second
    } else {
        drop(tmux_second);
        drop(rmux_second);
        case.first
    };
    wait_for_state(
        harness,
        tmux_binary,
        case.label,
        &[surviving],
        surviving,
        config,
    )?;
    Ok(())
}

fn setup_session(
    harness: &TmuxCompatHarness,
    tmux_binary: &Path,
    case: GeometryCase,
) -> Result<(), Box<dyn Error>> {
    let config = config();
    for argv in [
        ["new-session", "-d", "-s", case.label].as_slice(),
        ["set-option", "-t", case.label, "status", "off"].as_slice(),
        [
            "set-option",
            "-w",
            "-t",
            case.label,
            "window-size",
            case.policy,
        ]
        .as_slice(),
    ] {
        let run = harness.run_pair_with(tmux_binary, argv, config.clone())?;
        assert_quiet_success(&run);
    }
    Ok(())
}

fn attach_control_pair(
    tmux: &mut LiveControlClient,
    rmux: &mut LiveControlClient,
    session: &str,
    geometry: PtyTerminalSize,
) -> Result<(), Box<dyn Error>> {
    let commands = format!(
        "attach-session -t {session}\nrefresh-client -C {}x{}\n",
        geometry.cols, geometry.rows
    );
    tmux.send(&commands)?;
    rmux.send(&commands)?;
    Ok(())
}

fn wait_for_state(
    harness: &TmuxCompatHarness,
    tmux_binary: &Path,
    session: &str,
    expected_clients: &[PtyTerminalSize],
    expected_window: PtyTerminalSize,
    config: TmuxCompatRunConfig,
) -> Result<(), Box<dyn Error>> {
    let expected = state_output(expected_clients, expected_window);
    let run = wait_for_pair_run(
        harness,
        tmux_binary,
        &[
            "list-clients",
            "-t",
            session,
            "-F",
            "#{client_width}",
            ";",
            "display-message",
            "-p",
            "-t",
            session,
            "window=#{window_width}x#{window_height}",
        ],
        config,
        Duration::from_secs(5),
        |run| {
            normalized_state(&run.tmux.stdout_string()) == expected
                && normalized_state(&run.rmux.stdout_string()) == expected
        },
    )?;
    assert_success_without_stderr(&run);
    assert_eq!(
        normalized_state(&run.rmux.stdout_string()),
        normalized_state(&run.tmux.stdout_string())
    );
    Ok(())
}

fn normalized_state(output: &str) -> String {
    let mut clients = output
        .lines()
        .filter(|line| !line.starts_with("window="))
        .map(ToOwned::to_owned)
        .collect::<Vec<_>>();
    clients.sort();
    clients.extend(
        output
            .lines()
            .filter(|line| line.starts_with("window="))
            .map(ToOwned::to_owned),
    );
    format!("{}\n", clients.join("\n"))
}

fn state_output(clients: &[PtyTerminalSize], window: PtyTerminalSize) -> String {
    let mut clients = clients
        .iter()
        .map(|size| size.cols.to_string())
        .collect::<Vec<_>>();
    clients.sort();
    clients.push(format!("window={}x{}", window.cols, window.rows));
    format!("{}\n", clients.join("\n"))
}

const fn size(cols: u16, rows: u16) -> PtyTerminalSize {
    PtyTerminalSize { cols, rows }
}

fn config() -> TmuxCompatRunConfig {
    tmux_compat_config().with_timeout(Duration::from_secs(10))
}
