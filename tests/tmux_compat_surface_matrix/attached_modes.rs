use super::support::*;

#[test]
fn tmux_compat_nested_attach_session_inside_tmux_uses_switch_client_surface(
) -> Result<(), Box<dyn Error>> {
    // Cluster J nested-TMUX coverage: the explicit `-f <missing> new-session -d`
    // row is the designated baseline build failure check for this cluster. This row
    // closes the still-untested nested-attach half without pretending that it
    // is itself the release-baseline red case.
    let harness = TmuxCompatHarness::new("tmux-compat-nested-attach-switch-client")?;
    let (config, expected_overrides) = config_with_clean_homes(&harness)?;

    let create = harness.run_rmux_with(&["new-session", "-d", "-s", "alpha"], &config)?;
    assert_rmux_metadata(
        &create,
        &harness,
        &["new-session", "-d", "-s", "alpha"],
        &expected_overrides,
    );
    assert_eq!(create.status_code, Some(0));
    assert!(!create.timed_out);
    assert!(create.stdout.is_empty());
    assert!(create.stderr.is_empty());

    let nested_tmux = format!("{},1,0", harness.rmux_socket_path().display());
    let nested_attach = harness.run_rmux_with(
        &["attach-session", "-t", "alpha"],
        &config.clone().with_env("RMUX", nested_tmux.as_str()),
    )?;
    assert_eq!(nested_attach.status_code, Some(1));
    assert!(!nested_attach.timed_out);
    assert!(nested_attach.stdout.is_empty());
    assert_eq!(nested_attach.stderr_string(), "no current client\n");

    let still_present = harness.run_rmux_with(&["has-session", "-t", "alpha"], &config)?;
    assert_rmux_metadata(
        &still_present,
        &harness,
        &["has-session", "-t", "alpha"],
        &expected_overrides,
    );
    assert_eq!(still_present.status_code, Some(0));
    assert!(!still_present.timed_out);
    assert!(still_present.stdout.is_empty());
    assert!(still_present.stderr.is_empty());

    Ok(())
}

#[test]
fn prefix_pane_right_matches_attached_client_surface_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("cluster-b-prefix-pane-right")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let _guard = pty_tmux_compat_lock();
    let deadline = AttachedClientDeadline::new();
    let config = attached_client_config();

    attached_client_new_session(&harness, &tmux_binary, config.clone(), &deadline)?;
    let split = harness.run_pair_with(
        &tmux_binary,
        &["split-window", "-h", "-t", "alpha"],
        config.clone(),
    )?;
    assert_quiet_success(&split);
    let select_zero = harness.run_pair_with(
        &tmux_binary,
        &["select-pane", "-t", "alpha:0.0"],
        config.clone(),
    )?;
    assert_quiet_success(&select_zero);
    let cat = harness.run_pair_with(
        &tmux_binary,
        &["send-keys", "-t", "alpha:0.0", "cat -v", "Enter"],
        config.clone(),
    )?;
    assert_quiet_success(&cat);

    let mut rmux_attach = spawn_rmux_attached_input_client(&harness, "alpha")?;
    let mut tmux_attach = spawn_tmux_attached_input_client(&harness, &tmux_binary, "alpha")?;
    wait_for_attached_clients(&harness, &tmux_binary, config.clone(), &deadline)?;
    write_attached_keys(&mut rmux_attach, b"\x02\x02", &deadline)?;
    write_attached_keys(&mut tmux_attach, b"\x02\x02", &deadline)?;
    write_attached_keys(&mut rmux_attach, b"x", &deadline)?;
    write_attached_keys(&mut tmux_attach, b"x", &deadline)?;
    std::thread::sleep(Duration::from_millis(150));

    let send_prefix_capture = attached_capture_pair(
        &harness,
        &tmux_binary,
        "alpha:0.0",
        config.clone(),
        &deadline,
    )?;
    assert_eq!(
        send_prefix_capture
            .rmux
            .stdout_string()
            .matches("^B")
            .count(),
        1,
        "prefix_pane_right rmux capture should record one C-b byte, got {:?}",
        send_prefix_capture.rmux.stdout_string()
    );
    assert_eq!(
        send_prefix_capture
            .tmux
            .stdout_string()
            .matches("^B")
            .count(),
        1,
        "prefix_pane_right tmux capture should record one C-b byte, got {:?}",
        send_prefix_capture.tmux.stdout_string()
    );
    assert!(
        send_prefix_capture.rmux.stdout_string().contains("^Bx"),
        "prefix_pane_right rmux capture should return to root input after send-prefix, got {:?}",
        send_prefix_capture.rmux.stdout_string()
    );
    assert!(
        send_prefix_capture.tmux.stdout_string().contains("^Bx"),
        "prefix_pane_right tmux capture should return to root input after send-prefix, got {:?}",
        send_prefix_capture.tmux.stdout_string()
    );

    write_attached_keys(&mut rmux_attach, b"\x02\x1b[C", &deadline)?;
    write_attached_keys(&mut tmux_attach, b"\x02\x1b[C", &deadline)?;
    let panes = wait_for_attached_pair(
        &harness,
        &tmux_binary,
        &[
            "list-panes",
            "-t",
            "alpha",
            "-F",
            "#{pane_index}:#{pane_active}",
        ],
        config.clone(),
        &deadline,
        |run| run.rmux.stdout_string().contains("1:1") && run.tmux.stdout_string().contains("1:1"),
    )?;
    rmux_attach.assert_running("rmux")?;
    tmux_attach.assert_running("tmux")?;
    assert_eq!(panes.rmux.stdout_string(), "0:0\n1:1\n");
    assert_eq!(panes.tmux.stdout_string(), "0:0\n1:1\n");

    drop(rmux_attach);
    drop(tmux_attach);

    let new_window = harness.run_pair_with(
        &tmux_binary,
        &["new-window", "-d", "-t", "alpha"],
        config.clone(),
    )?;
    assert_quiet_success(&new_window);
    let select_window_zero = harness.run_pair_with(
        &tmux_binary,
        &["select-window", "-t", "alpha:0"],
        config.clone(),
    )?;
    assert_quiet_success(&select_window_zero);
    let mut rmux_attach = spawn_rmux_attached_input_client(&harness, "alpha")?;
    let mut tmux_attach = spawn_tmux_attached_input_client(&harness, &tmux_binary, "alpha")?;
    wait_for_attached_clients(&harness, &tmux_binary, config.clone(), &deadline)?;
    write_attached_keys(&mut rmux_attach, b"\x02n", &deadline)?;
    write_attached_keys(&mut tmux_attach, b"\x02n", &deadline)?;
    let windows = wait_for_attached_pair(
        &harness,
        &tmux_binary,
        &[
            "list-windows",
            "-t",
            "alpha",
            "-F",
            "#{window_index}:#{window_active}",
        ],
        config,
        &deadline,
        |run| run.rmux.stdout_string().contains("1:1") && run.tmux.stdout_string().contains("1:1"),
    )?;
    assert_eq!(windows.rmux.stdout_string(), "0:0\n1:1\n");
    assert_eq!(windows.tmux.stdout_string(), "0:0\n1:1\n");

    drop(rmux_attach);
    drop(tmux_attach);
    shutdown_attached_rmux(&harness)?;
    Ok(())
}

#[test]
fn prefix_q_display_panes_timeout_matches_tmux_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("cluster-b-prefix-q-timeout")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let _guard = pty_tmux_compat_lock();
    let deadline = AttachedClientDeadline::new();
    let config = attached_client_config();

    attached_client_new_session(&harness, &tmux_binary, config.clone(), &deadline)?;
    let split = harness.run_pair_with(
        &tmux_binary,
        &["split-window", "-h", "-t", "alpha"],
        config.clone(),
    )?;
    assert_quiet_success(&split);

    let mut rmux_attach = spawn_rmux_attached_input_client(&harness, "alpha")?;
    let mut tmux_attach = spawn_tmux_attached_input_client(&harness, &tmux_binary, "alpha")?;
    wait_for_attached_clients(&harness, &tmux_binary, config, &deadline)?;

    std::thread::sleep(Duration::from_millis(1_500));
    let mut rmux_bytes = drain_pty(&mut rmux_attach)?;
    let mut tmux_bytes = drain_pty(&mut tmux_attach)?;

    write_attached_keys(&mut rmux_attach, b"\x02q", &deadline)?;
    write_attached_keys(&mut tmux_attach, b"\x02q", &deadline)?;

    for _ in 0..10 {
        std::thread::sleep(Duration::from_millis(100));
        rmux_bytes.extend(drain_pty(&mut rmux_attach)?);
        tmux_bytes.extend(drain_pty(&mut tmux_attach)?);
        if display_panes_overlay_visible(&render_transcript(&rmux_bytes, 80, 24))
            && display_panes_overlay_visible(&render_transcript(&tmux_bytes, 80, 24))
        {
            break;
        }
    }

    let rmux_early = render_transcript(&rmux_bytes, 80, 24);
    let tmux_early = render_transcript(&tmux_bytes, 80, 24);
    assert!(
        display_panes_overlay_visible(&rmux_early),
        "rmux should show display-panes shortly after prefix q, got {rmux_early:?}"
    );
    assert!(
        display_panes_overlay_visible(&tmux_early),
        "tmux should show display-panes shortly after prefix q, got {tmux_early:?}"
    );

    std::thread::sleep(Duration::from_millis(1_300));
    rmux_bytes.extend(drain_pty(&mut rmux_attach)?);
    tmux_bytes.extend(drain_pty(&mut tmux_attach)?);

    let rmux_late = render_transcript(&rmux_bytes, 80, 24);
    let tmux_late = render_transcript(&tmux_bytes, 80, 24);
    assert_eq!(
        display_panes_overlay_visible(&tmux_late),
        display_panes_overlay_visible(&rmux_late),
        "display-panes timeout visibility diverged\n--- tmux ---\n{tmux_late}\n--- rmux ---\n{rmux_late}"
    );

    drop(rmux_attach);
    drop(tmux_attach);
    shutdown_attached_rmux(&harness)?;
    Ok(())
}

#[test]
fn copy_mode_search_select_matches_attached_client_surface_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("cluster-b-copy-mode-search-select")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let _guard = pty_tmux_compat_lock();
    let deadline = AttachedClientDeadline::new();
    let config = attached_client_config();

    attached_client_new_session(&harness, &tmux_binary, config.clone(), &deadline)?;
    let mode_keys = harness.run_pair_with(
        &tmux_binary,
        &["set-window-option", "-g", "mode-keys", "vi"],
        config.clone(),
    )?;
    assert_quiet_success(&mode_keys);
    let copy_mode = harness.run_pair_with(
        &tmux_binary,
        &["copy-mode", "-t", "alpha:0.0"],
        config.clone(),
    )?;
    assert_quiet_success(&copy_mode);

    let mut rmux_attach = spawn_rmux_attached_input_client(&harness, "alpha")?;
    let mut tmux_attach = spawn_tmux_attached_input_client(&harness, &tmux_binary, "alpha")?;
    wait_for_attached_clients(&harness, &tmux_binary, config.clone(), &deadline)?;
    write_attached_keys(&mut rmux_attach, b"/P0-LINE-12\r \r", &deadline)?;
    write_attached_keys(&mut tmux_attach, b"/P0-LINE-12\r \r", &deadline)?;
    std::thread::sleep(Duration::from_millis(150));

    let after_select = attached_capture_pair(
        &harness,
        &tmux_binary,
        "alpha:0.0",
        config.clone(),
        &deadline,
    )?;
    assert!(
        !after_select.rmux.stdout_string().contains("/P0-LINE-12"),
        "copy_mode_search_select rmux capture should consume attached search keys, got {:?}",
        after_select.rmux.stdout_string()
    );
    assert!(
        !after_select.tmux.stdout_string().contains("/P0-LINE-12"),
        "copy_mode_search_select tmux capture should consume attached search keys, got {:?}",
        after_select.tmux.stdout_string()
    );

    drop(rmux_attach);
    drop(tmux_attach);
    shutdown_attached_rmux(&harness)?;
    Ok(())
}

#[test]
fn copy_mode_q_exit_matches_attached_client_surface_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("cluster-b-copy-mode-q-exit")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let _guard = pty_tmux_compat_lock();
    let deadline = AttachedClientDeadline::new();
    let config = attached_client_config();

    attached_client_new_session(&harness, &tmux_binary, config.clone(), &deadline)?;
    let copy_mode = harness.run_pair_with(
        &tmux_binary,
        &["copy-mode", "-t", "alpha:0.0"],
        config.clone(),
    )?;
    assert_quiet_success(&copy_mode);
    let mut rmux_attach = spawn_rmux_attached_input_client(&harness, "alpha")?;
    let mut tmux_attach = spawn_tmux_attached_input_client(&harness, &tmux_binary, "alpha")?;
    wait_for_attached_clients(&harness, &tmux_binary, config.clone(), &deadline)?;

    let before = wait_for_attached_pair(
        &harness,
        &tmux_binary,
        &[
            "display-message",
            "-p",
            "-t",
            "alpha:0.0",
            "#{pane_in_mode}|#{pane_mode}",
        ],
        config.clone(),
        &deadline,
        |run| {
            run.rmux.stdout_string() == "1|copy-mode\n"
                && run.tmux.stdout_string() == "1|copy-mode\n"
        },
    )?;
    assert_eq!(before.rmux.stdout_string(), "1|copy-mode\n");
    assert_eq!(before.tmux.stdout_string(), "1|copy-mode\n");

    write_attached_keys(&mut rmux_attach, b"q", &deadline)?;
    write_attached_keys(&mut tmux_attach, b"q", &deadline)?;
    let after = wait_for_attached_pair(
        &harness,
        &tmux_binary,
        &[
            "display-message",
            "-p",
            "-t",
            "alpha:0.0",
            "#{pane_in_mode}|#{pane_mode}",
        ],
        config.clone(),
        &deadline,
        |run| run.rmux.stdout_string() == "0|\n" && run.tmux.stdout_string() == "0|\n",
    )?;
    assert_eq!(after.rmux.stdout_string(), "0|\n");
    assert_eq!(after.tmux.stdout_string(), "0|\n");
    let after_exit_keys = attached_capture_pair(
        &harness,
        &tmux_binary,
        "alpha:0.0",
        config.clone(),
        &deadline,
    )?;
    assert!(
        !after_exit_keys.rmux.stdout_string().contains("\nq"),
        "copy_mode_q_exit rmux capture should keep q out of pane input, got {:?}",
        after_exit_keys.rmux.stdout_string()
    );
    assert!(
        !after_exit_keys.tmux.stdout_string().contains("\nq"),
        "copy_mode_q_exit tmux capture should keep q out of pane input, got {:?}",
        after_exit_keys.tmux.stdout_string()
    );

    drop(rmux_attach);
    drop(tmux_attach);
    shutdown_attached_rmux(&harness)?;
    Ok(())
}

#[test]
fn copy_mode_escape_exit_matches_attached_client_surface_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("cluster-b-copy-mode-escape-exit")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let _guard = pty_tmux_compat_lock();
    let deadline = AttachedClientDeadline::new();
    let config = attached_client_config();

    attached_client_new_session(&harness, &tmux_binary, config.clone(), &deadline)?;
    let copy_mode = harness.run_pair_with(
        &tmux_binary,
        &["copy-mode", "-t", "alpha:0.0"],
        config.clone(),
    )?;
    assert_quiet_success(&copy_mode);
    let mut rmux_attach = spawn_rmux_attached_input_client(&harness, "alpha")?;
    let mut tmux_attach = spawn_tmux_attached_input_client(&harness, &tmux_binary, "alpha")?;
    wait_for_attached_clients(&harness, &tmux_binary, config.clone(), &deadline)?;

    let before = wait_for_attached_pair(
        &harness,
        &tmux_binary,
        &[
            "display-message",
            "-p",
            "-t",
            "alpha:0.0",
            "#{pane_in_mode}|#{pane_mode}",
        ],
        config.clone(),
        &deadline,
        |run| {
            run.rmux.stdout_string() == "1|copy-mode\n"
                && run.tmux.stdout_string() == "1|copy-mode\n"
        },
    )?;
    assert_eq!(before.rmux.stdout_string(), "1|copy-mode\n");
    assert_eq!(before.tmux.stdout_string(), "1|copy-mode\n");

    write_attached_keys(&mut rmux_attach, b"\x1b", &deadline)?;
    write_attached_keys(&mut tmux_attach, b"\x1b", &deadline)?;
    let after = wait_for_attached_pair(
        &harness,
        &tmux_binary,
        &[
            "display-message",
            "-p",
            "-t",
            "alpha:0.0",
            "#{pane_in_mode}|#{pane_mode}",
        ],
        config.clone(),
        &deadline,
        |run| run.rmux.stdout_string() == "0|\n" && run.tmux.stdout_string() == "0|\n",
    )?;
    assert_eq!(after.rmux.stdout_string(), "0|\n");
    assert_eq!(after.tmux.stdout_string(), "0|\n");

    rmux_attach.assert_running("rmux")?;
    tmux_attach.assert_running("tmux")?;
    drop(rmux_attach);
    drop(tmux_attach);
    shutdown_attached_rmux(&harness)?;
    Ok(())
}

#[test]
fn copy_mode_u_render_matches_attached_client_surface_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("cluster-b-copy-mode-u-render")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let _guard = pty_tmux_compat_lock();
    let deadline = AttachedClientDeadline::new();
    let config = attached_client_config();

    attached_client_new_session(&harness, &tmux_binary, config.clone(), &deadline)?;
    let mut rmux_attach = spawn_rmux_attached_input_client(&harness, "alpha")?;
    let mut tmux_attach = spawn_tmux_attached_input_client(&harness, &tmux_binary, "alpha")?;
    wait_for_attached_clients(&harness, &tmux_binary, config.clone(), &deadline)?;
    let _ = drain_pty(&mut rmux_attach)?;
    let _ = drain_pty(&mut tmux_attach)?;

    let copy_mode_u = harness.run_pair_with(
        &tmux_binary,
        &["copy-mode", "-u", "-t", "alpha:0.0"],
        config.clone(),
    )?;
    assert_quiet_success(&copy_mode_u);
    std::thread::sleep(Duration::from_millis(250));
    deadline.check()?;
    let rmux_render = render_cells(&drain_pty(&mut rmux_attach)?, 80, 24).join("\n");
    let tmux_render = render_cells(&drain_pty(&mut tmux_attach)?, 80, 24).join("\n");
    let capture = attached_capture_pair(
        &harness,
        &tmux_binary,
        "alpha:0.0",
        config.clone(),
        &deadline,
    )?;
    let mode = harness.run_pair_with(
        &tmux_binary,
        &[
            "display-message",
            "-p",
            "-t",
            "alpha:0.0",
            "#{pane_in_mode}|#{pane_mode}",
        ],
        config,
    )?;
    assert_eq!(mode.rmux.stdout_string(), "1|copy-mode\n");
    assert_eq!(mode.tmux.stdout_string(), "1|copy-mode\n");
    assert!(
        capture.rmux.stdout_string().contains("P0-LINE-12")
            && capture.tmux.stdout_string().contains("P0-LINE-12"),
        "copy_mode_u_render capture-pane should keep scrollback on both servers; rmux={:?} tmux={:?}",
        capture.rmux.stdout_string(),
        capture.tmux.stdout_string()
    );
    assert!(
        tmux_render.contains("P0-LINE-12"),
        "copy_mode_u_render tmux attached render should show scrollback, got {tmux_render:?}"
    );
    assert!(
        rmux_render.contains("P0-LINE-12"),
        "copy_mode_u_render rmux attached render should show scrollback, got {rmux_render:?}"
    );

    drop(rmux_attach);
    drop(tmux_attach);
    shutdown_attached_rmux(&harness)?;
    Ok(())
}

#[test]
fn choose_tree_window_matches_attached_client_surface_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("cluster-b-choose-tree-window")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let _guard = pty_tmux_compat_lock();
    let deadline = AttachedClientDeadline::new();
    let config = attached_client_config();

    attached_client_new_session(&harness, &tmux_binary, config.clone(), &deadline)?;
    let new_window = harness.run_pair_with(
        &tmux_binary,
        &["new-window", "-d", "-t", "alpha", "-n", "w1"],
        config.clone(),
    )?;
    assert_quiet_success(&new_window);
    let mut rmux_attach = spawn_rmux_attached_input_client(&harness, "alpha")?;
    let mut tmux_attach = spawn_tmux_attached_input_client(&harness, &tmux_binary, "alpha")?;
    wait_for_attached_clients(&harness, &tmux_binary, config.clone(), &deadline)?;

    let choose_tree =
        harness.run_pair_with(&tmux_binary, &["choose-tree", "-Zw"], config.clone())?;
    assert_quiet_success(&choose_tree);
    std::thread::sleep(Duration::from_millis(150));
    let tree_capture = attached_capture_pair(
        &harness,
        &tmux_binary,
        "alpha:0.0",
        config.clone(),
        &deadline,
    )?;
    write_attached_keys(&mut rmux_attach, b"\x0e\r", &deadline)?;
    write_attached_keys(&mut tmux_attach, b"\x0e\r", &deadline)?;

    std::thread::sleep(Duration::from_millis(300));
    deadline.check()?;
    let active_window = harness.run_pair_with(
        &tmux_binary,
        &["display-message", "-p", "#{window_index}:#{window_name}"],
        config,
    )?;
    rmux_attach.assert_running("rmux")?;
    tmux_attach.assert_running("tmux")?;
    assert!(tree_capture.rmux.status_code == Some(0) && tree_capture.tmux.status_code == Some(0));
    assert_eq!(active_window.rmux.stdout_string(), "1:w1\n");
    assert_eq!(active_window.tmux.stdout_string(), "1:w1\n");

    drop(rmux_attach);
    drop(tmux_attach);
    shutdown_attached_rmux(&harness)?;
    Ok(())
}
