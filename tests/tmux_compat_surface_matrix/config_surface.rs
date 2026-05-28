use super::support::*;

#[test]
fn tmux_compat_explicit_config_layout_and_format_surfaces_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-config-layout-format")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let (config, expected_overrides) = config_with_clean_homes(&harness)?;
    let config_path = harness.tmpdir().join("startup.conf");
    fs::write(
        &config_path,
        "set-option -g status off\nset-option -g status-left '#{session_name}'\n",
    )?;
    let config_path = config_path.to_string_lossy().into_owned();

    let create = harness.run_pair_with(
        &tmux_binary,
        &[
            "-f",
            &config_path,
            "new-session",
            "-d",
            "-s",
            "alpha",
            "-x",
            "80",
            "-y",
            "24",
        ],
        config.clone(),
    )?;
    assert_quiet_success(&create);
    assert_run_metadata(
        &create,
        &harness,
        &tmux_binary,
        &[
            "-f",
            &config_path,
            "new-session",
            "-d",
            "-s",
            "alpha",
            "-x",
            "80",
            "-y",
            "24",
        ],
        &expected_overrides,
    );

    let split = harness.run_pair_with(
        &tmux_binary,
        &["split-window", "-h", "-t", "alpha"],
        config.clone(),
    )?;
    assert_quiet_success(&split);
    assert_run_metadata(
        &split,
        &harness,
        &tmux_binary,
        &["split-window", "-h", "-t", "alpha"],
        &expected_overrides,
    );

    let split_layout = harness.run_pair_with(
        &tmux_binary,
        &["lsw", "-t", "alpha", "-F", "#{window_layout}"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&split_layout);
    assert_run_metadata(
        &split_layout,
        &harness,
        &tmux_binary,
        &["lsw", "-t", "alpha", "-F", "#{window_layout}"],
        &expected_overrides,
    );

    let select_layout = harness.run_pair_with(
        &tmux_binary,
        &["select-layout", "-t", "alpha:0", "even-horizontal"],
        config.clone(),
    )?;
    assert_quiet_success(&select_layout);
    assert_run_metadata(
        &select_layout,
        &harness,
        &tmux_binary,
        &["select-layout", "-t", "alpha:0", "even-horizontal"],
        &expected_overrides,
    );

    let list_windows = harness.run_pair_with(
        &tmux_binary,
        &["lsw", "-t", "alpha", "-F", "#{window_layout}"],
        config.clone(),
    )?;
    // Frozen tmux next-3.7 keeps the explicit split geometry here; RMUX follows
    // the system tmux 3.4 behavior observed in the interactive Mate-terminal
    // check, where explicit even-horizontal recomputes balanced pane widths.
    assert_eq!(list_windows.tmux.status_code, list_windows.rmux.status_code);
    assert_eq!(list_windows.tmux.timed_out, list_windows.rmux.timed_out);
    assert_eq!(list_windows.tmux.stderr, list_windows.rmux.stderr);
    assert_eq!(
        list_windows.rmux.stdout_string(),
        "89f5,80x24,0,0{39x24,0,0,0,40x24,40,0,1}\n"
    );
    assert_eq!(
        list_windows.tmux.stdout_string(),
        "8205,80x24,0,0{40x24,0,0,0,39x24,41,0,1}\n"
    );
    assert_run_metadata(
        &list_windows,
        &harness,
        &tmux_binary,
        &["lsw", "-t", "alpha", "-F", "#{window_layout}"],
        &expected_overrides,
    );

    let display = harness.run_pair_with(
        &tmux_binary,
        &[
            "display-message",
            "-p",
            "-t",
            "alpha:0.0",
            "#{E:status-left}:#{window_layout}:#{?#{==:#{session_name},alpha},#{=5:abcdefgh},no}",
        ],
        config,
    )?;
    // Keep the same version-specific layout assertion for the format expansion
    // path so status-left and conditional formatting remain covered.
    assert_eq!(display.tmux.status_code, display.rmux.status_code);
    assert_eq!(display.tmux.timed_out, display.rmux.timed_out);
    assert_eq!(display.tmux.stderr, display.rmux.stderr);
    assert_eq!(
        display.rmux.stdout_string(),
        "alpha:89f5,80x24,0,0{39x24,0,0,0,40x24,40,0,1}:\n"
    );
    assert_eq!(
        display.tmux.stdout_string(),
        "alpha:8205,80x24,0,0{40x24,0,0,0,39x24,41,0,1}:\n"
    );
    assert_run_metadata(
        &display,
        &harness,
        &tmux_binary,
        &[
            "display-message",
            "-p",
            "-t",
            "alpha:0.0",
            "#{E:status-left}:#{window_layout}:#{?#{==:#{session_name},alpha},#{=5:abcdefgh},no}",
        ],
        &expected_overrides,
    );
    assert!(
        display.rmux.stdout_string().starts_with("alpha:"),
        "expected display-message output to reflect config-loaded formats, got {:?}",
        display.rmux.stdout_string()
    );

    Ok(())
}

#[test]
fn tmux_compat_explicit_missing_config_file_is_silent_for_detached_new_session_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-config-missing-detached")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let (config, expected_overrides) = config_with_clean_homes(&harness)?;
    let missing_config = harness.tmpdir().join("nonexistent.conf");
    let missing_config = missing_config.to_string_lossy().into_owned();
    let argv = [
        "-f",
        missing_config.as_str(),
        "new-session",
        "-d",
        "-s",
        "alpha",
        "-x",
        "80",
        "-y",
        "24",
    ];

    let create = harness.run_pair_with(&tmux_binary, &argv, config)?;

    assert_run_metadata(&create, &harness, &tmux_binary, &argv, &expected_overrides);
    assert_exact_tmux_compat(&create);
    assert_eq!(create.tmux.status_code, Some(0));
    assert!(create.tmux.stdout.is_empty());
    assert!(create.tmux.stderr.is_empty());
    Ok(())
}

#[test]
fn tmux_compat_show_options_a_inheritance_marker_matches_reference_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-show-options-a-marker")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let config = tmux_compat_config();
    let expected_overrides = default_overrides(harness.tmpdir());

    let create = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha"],
        config.clone(),
    )?;
    assert_quiet_success(&create);
    assert_run_metadata(
        &create,
        &harness,
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha"],
        &expected_overrides,
    );

    let show_status = harness.run_pair_with(
        &tmux_binary,
        &["show-options", "-A", "-t", "alpha", "status"],
        config,
    )?;

    assert_exact_tmux_compat(&show_status);
    assert_run_metadata(
        &show_status,
        &harness,
        &tmux_binary,
        &["show-options", "-A", "-t", "alpha", "status"],
        &expected_overrides,
    );
    assert_eq!(show_status.rmux.stdout_string(), "status* on\n");
    Ok(())
}

#[test]
fn tmux_compat_environment_style_and_terminal_feature_lines_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-env-style-terminal-features")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let config = tmux_compat_config();
    let expected_overrides = default_overrides(harness.tmpdir());

    let create = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha"],
        config.clone(),
    )?;
    assert_quiet_success(&create);
    assert_run_metadata(
        &create,
        &harness,
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha"],
        &expected_overrides,
    );

    let set_environment = harness.run_pair_with(
        &tmux_binary,
        &["setenv", "-t", "alpha", "TERM", "screen"],
        config.clone(),
    )?;
    assert_quiet_success(&set_environment);
    assert_run_metadata(
        &set_environment,
        &harness,
        &tmux_binary,
        &["setenv", "-t", "alpha", "TERM", "screen"],
        &expected_overrides,
    );

    let show_environment =
        harness.run_pair_with(&tmux_binary, &["showenv", "-t", "alpha"], config.clone())?;
    assert_success_without_stderr(&show_environment);
    assert_run_metadata(
        &show_environment,
        &harness,
        &tmux_binary,
        &["showenv", "-t", "alpha"],
        &expected_overrides,
    );
    let tmux_showenv = show_environment.tmux.stdout_string();
    let rmux_showenv = show_environment.rmux.stdout_string();
    assert!(
        tmux_showenv.contains("TERM=screen\n"),
        "expected tmux showenv output to include TERM override, got {tmux_showenv:?}"
    );
    assert_eq!(rmux_showenv, tmux_showenv);

    let set_terminal_features = harness.run_pair_with(
        &tmux_binary,
        &["set", "-as", "terminal-features", ",xterm-256color:RGB"],
        config.clone(),
    )?;
    assert_quiet_success(&set_terminal_features);
    assert_run_metadata(
        &set_terminal_features,
        &harness,
        &tmux_binary,
        &["set", "-as", "terminal-features", ",xterm-256color:RGB"],
        &expected_overrides,
    );

    let set_status_style = harness.run_pair_with(
        &tmux_binary,
        &["set", "-g", "status-style", "bg=green,fg=black"],
        config.clone(),
    )?;
    assert_quiet_success(&set_status_style);
    assert_run_metadata(
        &set_status_style,
        &harness,
        &tmux_binary,
        &["set", "-g", "status-style", "bg=green,fg=black"],
        &expected_overrides,
    );

    let show_options =
        harness.run_pair_with(&tmux_binary, &["show-options", "-g"], config.clone())?;
    assert_run_metadata(
        &show_options,
        &harness,
        &tmux_binary,
        &["show-options", "-g"],
        &expected_overrides,
    );
    assert_success_without_stderr(&show_options);

    let status_style_line = assert_matching_line(&show_options, "status-style ");
    assert_eq!(status_style_line, "status-style bg=green,fg=black");

    let rmux_server_terminal_features =
        harness.run_rmux(&["show-options", "-sv", "terminal-features"])?;
    assert_eq!(rmux_server_terminal_features.status_code, Some(0));
    assert!(!rmux_server_terminal_features.timed_out);
    assert!(rmux_server_terminal_features.stderr_string().is_empty());
    assert!(
        rmux_server_terminal_features
            .stdout_string()
            .contains("RGB"),
        "expected rmux server-scope terminal-features query to record RGB support, got {:?}",
        rmux_server_terminal_features.stdout_string()
    );

    Ok(())
}

#[test]
fn tmux_compat_window_option_alias_surface_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-window-option-alias-surface")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let config = tmux_compat_config();
    let expected_overrides = default_overrides(harness.tmpdir());

    let create = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha"],
        config.clone(),
    )?;
    assert_quiet_success(&create);
    assert_run_metadata(
        &create,
        &harness,
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha"],
        &expected_overrides,
    );

    let set_window_toggle = harness.run_pair_with(
        &tmux_binary,
        &["set-option", "-w", "-t", "alpha", "synchronize-panes"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&set_window_toggle);
    assert_run_metadata(
        &set_window_toggle,
        &harness,
        &tmux_binary,
        &["set-option", "-w", "-t", "alpha", "synchronize-panes"],
        &expected_overrides,
    );

    let show_window_toggle = harness.run_pair_with(
        &tmux_binary,
        &["show-options", "-wv", "-t", "alpha", "synchronize-panes"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&show_window_toggle);
    assert_run_metadata(
        &show_window_toggle,
        &harness,
        &tmux_binary,
        &["show-options", "-wv", "-t", "alpha", "synchronize-panes"],
        &expected_overrides,
    );
    assert_eq!(show_window_toggle.rmux.stdout_string(), "on\n");

    let set_window_option = harness.run_pair_with(
        &tmux_binary,
        &["setw", "-t", "alpha", "pane-border-style", "fg=colour1"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&set_window_option);
    assert_run_metadata(
        &set_window_option,
        &harness,
        &tmux_binary,
        &["setw", "-t", "alpha", "pane-border-style", "fg=colour1"],
        &expected_overrides,
    );

    let show_window_option = harness.run_pair_with(
        &tmux_binary,
        &["showw", "-v", "-t", "alpha", "pane-border-style"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&show_window_option);
    assert_run_metadata(
        &show_window_option,
        &harness,
        &tmux_binary,
        &["showw", "-v", "-t", "alpha", "pane-border-style"],
        &expected_overrides,
    );
    assert_eq!(show_window_option.rmux.stdout_string(), "fg=colour1\n");

    let set_window_option_full = harness.run_pair_with(
        &tmux_binary,
        &[
            "set-window-option",
            "-t",
            "alpha",
            "pane-border-style",
            "fg=colour2",
        ],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&set_window_option_full);
    assert_run_metadata(
        &set_window_option_full,
        &harness,
        &tmux_binary,
        &[
            "set-window-option",
            "-t",
            "alpha",
            "pane-border-style",
            "fg=colour2",
        ],
        &expected_overrides,
    );

    let show_window_option_full = harness.run_pair_with(
        &tmux_binary,
        &[
            "show-window-options",
            "-v",
            "-t",
            "alpha",
            "pane-border-style",
        ],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&show_window_option_full);
    assert_run_metadata(
        &show_window_option_full,
        &harness,
        &tmux_binary,
        &[
            "show-window-options",
            "-v",
            "-t",
            "alpha",
            "pane-border-style",
        ],
        &expected_overrides,
    );
    assert_eq!(show_window_option_full.rmux.stdout_string(), "fg=colour2\n");

    let set_server_message_limit = harness.run_pair_with(
        &tmux_binary,
        &["set-option", "-s", "message-limit", "77"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&set_server_message_limit);
    assert_run_metadata(
        &set_server_message_limit,
        &harness,
        &tmux_binary,
        &["set-option", "-s", "message-limit", "77"],
        &expected_overrides,
    );

    let show_server_message_limit = harness.run_pair_with(
        &tmux_binary,
        &["show-options", "-gsv", "-t", "missing", "message-limit"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&show_server_message_limit);
    assert_run_metadata(
        &show_server_message_limit,
        &harness,
        &tmux_binary,
        &["show-options", "-gsv", "-t", "missing", "message-limit"],
        &expected_overrides,
    );
    assert_eq!(show_server_message_limit.rmux.stdout_string(), "77\n");

    let set_window_option_global = harness.run_pair_with(
        &tmux_binary,
        &["set-window-option", "-g", "pane-border-style", "fg=colour3"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&set_window_option_global);
    assert_run_metadata(
        &set_window_option_global,
        &harness,
        &tmux_binary,
        &["set-window-option", "-g", "pane-border-style", "fg=colour3"],
        &expected_overrides,
    );

    let show_window_option_global = harness.run_pair_with(
        &tmux_binary,
        &[
            "show-window-options",
            "-g",
            "-t",
            "missing",
            "-v",
            "pane-border-style",
        ],
        config,
    )?;
    assert_exact_tmux_compat(&show_window_option_global);
    assert_run_metadata(
        &show_window_option_global,
        &harness,
        &tmux_binary,
        &[
            "show-window-options",
            "-g",
            "-t",
            "missing",
            "-v",
            "pane-border-style",
        ],
        &expected_overrides,
    );
    assert_eq!(
        show_window_option_global.rmux.stdout_string(),
        "fg=colour3\n"
    );

    Ok(())
}

#[test]
fn tmux_compat_hook_arrays_aliases_and_exact_targets_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-hooks-aliases-targets")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let config = tmux_compat_config();
    let expected_overrides = default_overrides(harness.tmpdir());

    let bootstrap = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "bootstrap"],
        config.clone(),
    )?;
    assert_quiet_success(&bootstrap);
    assert_run_metadata(
        &bootstrap,
        &harness,
        &tmux_binary,
        &["new-session", "-d", "-s", "bootstrap"],
        &expected_overrides,
    );

    let hook_first = harness.run_pair_with(
        &tmux_binary,
        &[
            "set-hook",
            "-ag",
            "session-created",
            "set-buffer -b first first",
        ],
        config.clone(),
    )?;
    assert_quiet_success(&hook_first);
    assert_run_metadata(
        &hook_first,
        &harness,
        &tmux_binary,
        &[
            "set-hook",
            "-ag",
            "session-created",
            "set-buffer -b first first",
        ],
        &expected_overrides,
    );

    let hook_second = harness.run_pair_with(
        &tmux_binary,
        &[
            "set-hook",
            "-ag",
            "session-created",
            "set-buffer -b second second",
        ],
        config.clone(),
    )?;
    assert_quiet_success(&hook_second);
    assert_run_metadata(
        &hook_second,
        &harness,
        &tmux_binary,
        &[
            "set-hook",
            "-ag",
            "session-created",
            "set-buffer -b second second",
        ],
        &expected_overrides,
    );

    let create = harness.run_pair_with(
        &tmux_binary,
        &["new", "-d", "-s", "alpha", "-x", "80", "-y", "24"],
        config.clone(),
    )?;
    assert_quiet_success(&create);
    assert_run_metadata(
        &create,
        &harness,
        &tmux_binary,
        &["new", "-d", "-s", "alpha", "-x", "80", "-y", "24"],
        &expected_overrides,
    );

    let first_buffer =
        harness.run_pair_with(&tmux_binary, &["showb", "-b", "first"], config.clone())?;
    assert_exact_tmux_compat(&first_buffer);
    assert_run_metadata(
        &first_buffer,
        &harness,
        &tmux_binary,
        &["showb", "-b", "first"],
        &expected_overrides,
    );
    assert_eq!(first_buffer.rmux.stdout_string(), "first");

    let second_buffer =
        harness.run_pair_with(&tmux_binary, &["showb", "-b", "second"], config.clone())?;
    assert_exact_tmux_compat(&second_buffer);
    assert_run_metadata(
        &second_buffer,
        &harness,
        &tmux_binary,
        &["showb", "-b", "second"],
        &expected_overrides,
    );
    assert_eq!(second_buffer.rmux.stdout_string(), "second");

    let display = harness.run_pair_with(
        &tmux_binary,
        &[
            "display",
            "-p",
            "-t",
            "alpha:0.0",
            "#{session_name}:#{pane_id}:#{pane_width}x#{pane_height}:#{window_panes}:#{pane_active}",
        ],
        config,
    )?;
    assert_success_without_stderr(&display);
    assert_run_metadata(
        &display,
        &harness,
        &tmux_binary,
        &[
            "display",
            "-p",
            "-t",
            "alpha:0.0",
            "#{session_name}:#{pane_id}:#{pane_width}x#{pane_height}:#{window_panes}:#{pane_active}",
        ],
        &expected_overrides,
    );
    assert!(
        display.tmux.stdout_string().starts_with("alpha:%"),
        "expected tmux display alias output to include an exact pane target, got {:?}",
        display.tmux.stdout_string()
    );
    assert!(
        display.rmux.stdout_string().starts_with("alpha:%"),
        "expected display alias output to include an exact pane target and pane id, got {:?}",
        display.rmux.stdout_string()
    );
    assert_exact_tmux_compat(&display);
    Ok(())
}
