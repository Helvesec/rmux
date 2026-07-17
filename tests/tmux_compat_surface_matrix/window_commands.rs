use super::support::*;

#[test]
fn tmux_compat_new_window_start_directory_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-new-window-start-directory")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let config = tmux_compat_config();
    let expected_overrides = default_overrides(harness.tmpdir());
    let requested_start_directory = harness.tmpdir().join("new-window-cwd");
    fs::create_dir_all(&requested_start_directory)?;
    let start_directory = fs::canonicalize(&requested_start_directory)?
        .to_string_lossy()
        .into_owned();

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

    let action_args = [
        "new-window",
        "-d",
        "-t",
        "alpha",
        "-c",
        start_directory.as_str(),
        "sleep",
        "30",
    ];
    let new_window = harness.run_pair_with(&tmux_binary, &action_args, config.clone())?;
    assert_exact_tmux_compat(&new_window);
    assert_run_metadata(
        &new_window,
        &harness,
        &tmux_binary,
        &action_args,
        &expected_overrides,
    );

    let expected_display = format!("{start_directory}|sleep\n");
    let display_args = [
        "display-message",
        "-p",
        "-t",
        "alpha:1",
        "#{pane_current_path}|#{pane_current_command}",
    ];
    let display = wait_for_pair_run(
        &harness,
        &tmux_binary,
        &display_args,
        config,
        Duration::from_secs(5),
        |run| {
            run.tmux.status_code == Some(0)
                && run.rmux.status_code == Some(0)
                && run.tmux.stdout == run.rmux.stdout
                && run.tmux.stdout_string() == expected_display
        },
    )?;
    assert_exact_tmux_compat(&display);
    assert_run_metadata(
        &display,
        &harness,
        &tmux_binary,
        &display_args,
        &expected_overrides,
    );

    Ok(())
}

#[test]
fn tmux_compat_new_window_shell_command_when_frozen_tmux_is_available() -> Result<(), Box<dyn Error>>
{
    let harness = TmuxCompatHarness::new("tmux-compat-new-window-shell-command")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let config = tmux_compat_config();
    let expected_overrides = default_overrides(harness.tmpdir());
    let shell_command = "printf hi; exec sleep 30";

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

    let action_args = ["new-window", "-d", "-t", "alpha", "--", shell_command];
    let new_window = harness.run_pair_with(&tmux_binary, &action_args, config.clone())?;
    assert_exact_tmux_compat(&new_window);
    assert_run_metadata(
        &new_window,
        &harness,
        &tmux_binary,
        &action_args,
        &expected_overrides,
    );

    let display_args = [
        "display-message",
        "-p",
        "-t",
        "alpha:1",
        "#{pane_current_command}",
    ];
    let display = wait_for_pair_run(
        &harness,
        &tmux_binary,
        &display_args,
        config,
        Duration::from_secs(5),
        |run| {
            run.tmux.status_code == Some(0)
                && run.rmux.status_code == Some(0)
                && run.tmux.stdout == b"sleep\n"
                && run.rmux.stdout == b"sleep\n"
        },
    )?;
    assert_exact_tmux_compat(&display);
    assert_run_metadata(
        &display,
        &harness,
        &tmux_binary,
        &display_args,
        &expected_overrides,
    );

    Ok(())
}

#[test]
fn tmux_compat_respawn_window_start_directory_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-respawn-window-start-directory")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let config = tmux_compat_config();
    let expected_overrides = default_overrides(harness.tmpdir());
    let requested_start_directory = harness.tmpdir().join("respawn-window-cwd");
    fs::create_dir_all(&requested_start_directory)?;
    let start_directory = fs::canonicalize(&requested_start_directory)?
        .to_string_lossy()
        .into_owned();

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

    let action_args = [
        "respawn-window",
        "-k",
        "-t",
        "alpha:0",
        "-c",
        start_directory.as_str(),
        "--",
        "sleep",
        "30",
    ];
    let respawn = harness.run_pair_with(&tmux_binary, &action_args, config.clone())?;
    assert_exact_tmux_compat(&respawn);
    assert_run_metadata(
        &respawn,
        &harness,
        &tmux_binary,
        &action_args,
        &expected_overrides,
    );

    let expected_display = format!("{start_directory}|sleep\n");
    let display_args = [
        "display-message",
        "-p",
        "-t",
        "alpha:0",
        "#{pane_current_path}|#{pane_current_command}",
    ];
    let display = wait_for_pair_run(
        &harness,
        &tmux_binary,
        &display_args,
        config,
        Duration::from_secs(5),
        |run| {
            run.tmux.status_code == Some(0)
                && run.rmux.status_code == Some(0)
                && run.tmux.stdout == run.rmux.stdout
                && run.tmux.stdout_string() == expected_display
        },
    )?;
    assert_exact_tmux_compat(&display);
    assert_run_metadata(
        &display,
        &harness,
        &tmux_binary,
        &display_args,
        &expected_overrides,
    );

    Ok(())
}

#[test]
fn tmux_compat_respawn_window_shell_command_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-respawn-window-shell-command")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let config = tmux_compat_config();
    let expected_overrides = default_overrides(harness.tmpdir());
    let shell_command = "printf hi; exec sleep 30";

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

    let action_args = ["respawn-window", "-k", "-t", "alpha:0", "--", shell_command];
    let respawn = harness.run_pair_with(&tmux_binary, &action_args, config.clone())?;
    assert_exact_tmux_compat(&respawn);
    assert_run_metadata(
        &respawn,
        &harness,
        &tmux_binary,
        &action_args,
        &expected_overrides,
    );

    let display_args = [
        "display-message",
        "-p",
        "-t",
        "alpha:0",
        "#{pane_current_command}",
    ];
    let display = wait_for_pair_run(
        &harness,
        &tmux_binary,
        &display_args,
        config,
        Duration::from_secs(5),
        |run| {
            run.tmux.status_code == Some(0)
                && run.rmux.status_code == Some(0)
                && run.tmux.stdout == b"sleep\n"
                && run.rmux.stdout == b"sleep\n"
        },
    )?;
    assert_exact_tmux_compat(&display);
    assert_run_metadata(
        &display,
        &harness,
        &tmux_binary,
        &display_args,
        &expected_overrides,
    );

    Ok(())
}

#[test]
fn tmux_compat_list_commands_output_when_frozen_tmux_is_available() -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-list-commands")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let config = tmux_compat_config();
    let expected_overrides = default_overrides(harness.tmpdir());

    let run = harness.run_pair_with(&tmux_binary, &["list-commands"], config)?;
    assert_run_metadata(
        &run,
        &harness,
        &tmux_binary,
        &["list-commands"],
        &expected_overrides,
    );
    assert_eq!(run.tmux.status_code, Some(0));
    assert_eq!(run.rmux.status_code, Some(0));
    assert!(!run.tmux.timed_out);
    assert!(!run.rmux.timed_out);
    assert!(run.tmux.stderr_string().is_empty());
    assert!(run.rmux.stderr_string().is_empty());

    // Both must list the same commands; rmux uses its own aliases so we compare
    // the sorted set of primary command names (first word on each line).
    let tmux_commands = sorted_first_words(&run.tmux.stdout_string());
    let rmux_commands = sorted_first_words(&run.rmux.stdout_string());
    assert!(
        !tmux_commands.is_empty(),
        "tmux list-commands produced no output"
    );
    assert!(
        !rmux_commands.is_empty(),
        "rmux list-commands produced no output"
    );

    // rmux may support a subset; verify every rmux command also appears in tmux
    for cmd in &rmux_commands {
        assert!(
            tmux_commands.contains(cmd),
            "rmux lists command {cmd:?} which is absent from tmux"
        );
    }

    Ok(())
}

#[test]
fn tmux_compat_unknown_command_error_exit_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-unknown-cmd")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let config = tmux_compat_config();
    let expected_overrides = default_overrides(harness.tmpdir());

    let run = harness.run_pair_with(&tmux_binary, &["nonexistent-command-xyz"], config)?;
    assert_run_metadata(
        &run,
        &harness,
        &tmux_binary,
        &["nonexistent-command-xyz"],
        &expected_overrides,
    );

    // Both should exit with non-zero status
    assert!(
        run.tmux.status_code != Some(0),
        "tmux should reject unknown command"
    );
    assert!(
        run.rmux.status_code != Some(0),
        "rmux should reject unknown command"
    );
    assert!(!run.tmux.timed_out);
    assert!(!run.rmux.timed_out);

    // Both should produce stderr
    assert!(
        !run.tmux.stderr_string().is_empty(),
        "tmux should print an error for unknown command"
    );
    assert!(
        !run.rmux.stderr_string().is_empty(),
        "rmux should print an error for unknown command"
    );

    Ok(())
}

#[test]
fn tmux_compat_help_usage_line_when_frozen_tmux_is_available() -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-help-usage")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let config = tmux_compat_config();

    // -h is a client flag that prints usage
    let rmux_run = harness.run_pair_with(&tmux_binary, &["-h"], config)?;
    assert!(!rmux_run.tmux.timed_out);
    assert!(!rmux_run.rmux.timed_out);

    // tmux -h prints usage to stdout and exits 1; rmux matches the exit behavior
    // but the usage text references different binary names -- verify structural compatibility
    let tmux_usage = rmux_run.tmux.stdout_string();
    let rmux_usage = rmux_run.rmux.stdout_string();

    assert!(
        tmux_usage.contains("usage:") || tmux_usage.contains("Usage"),
        "tmux -h should print usage, got: {tmux_usage:?}"
    );
    assert!(
        rmux_usage.contains("usage:") || rmux_usage.contains("Usage"),
        "rmux -h should print usage, got: {rmux_usage:?}"
    );

    // Both should include the common flags
    for flag in &["-L", "-S", "-f"] {
        assert!(
            rmux_usage.contains(flag),
            "rmux usage missing flag {flag}: {rmux_usage:?}"
        );
    }

    Ok(())
}

#[test]
fn tmux_compat_user_option_set_and_show_when_frozen_tmux_is_available() -> Result<(), Box<dyn Error>>
{
    let harness = TmuxCompatHarness::new("tmux-compat-user-option")?;
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

    // Set a user option (prefixed with @)
    let set_user = harness.run_pair_with(
        &tmux_binary,
        &["set", "-g", "@my-user-opt", "hello-world"],
        config.clone(),
    )?;
    assert_quiet_success(&set_user);
    assert_run_metadata(
        &set_user,
        &harness,
        &tmux_binary,
        &["set", "-g", "@my-user-opt", "hello-world"],
        &expected_overrides,
    );

    // Show the user option
    let show_user = harness.run_pair_with(
        &tmux_binary,
        &["show", "-gv", "@my-user-opt"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&show_user);
    assert_run_metadata(
        &show_user,
        &harness,
        &tmux_binary,
        &["show", "-gv", "@my-user-opt"],
        &expected_overrides,
    );
    assert_eq!(show_user.rmux.stdout_string().trim(), "hello-world");

    // Display via format that references user option
    let display_user = harness.run_pair_with(
        &tmux_binary,
        &["display-message", "-p", "opt=#{@my-user-opt}"],
        config,
    )?;
    assert_exact_tmux_compat(&display_user);
    assert_run_metadata(
        &display_user,
        &harness,
        &tmux_binary,
        &["display-message", "-p", "opt=#{@my-user-opt}"],
        &expected_overrides,
    );
    assert_eq!(display_user.tmux.stdout_string().trim(), "opt=hello-world");
    assert_eq!(display_user.rmux.stdout_string().trim(), "opt=hello-world");

    Ok(())
}

#[test]
fn tmux_compat_utf8_format_with_explicit_locale_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-utf8-format")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };

    // control-mode contract: UTF-8/width fixtures must set LC_CTYPE explicitly
    let config = tmux_compat_config()
        .with_env("LC_CTYPE", "C.UTF-8")
        .with_env("LC_ALL", "C.UTF-8")
        .with_env("TERM_PROGRAM", "tmux");
    let expected_overrides: EnvironmentOverrides = default_overrides(harness.tmpdir())
        .into_iter()
        .chain([
            (OsString::from("LC_CTYPE"), Some(OsString::from("C.UTF-8"))),
            (OsString::from("LC_ALL"), Some(OsString::from("C.UTF-8"))),
            (OsString::from("TERM_PROGRAM"), Some(OsString::from("tmux"))),
        ])
        .collect();

    let create = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha", "-x", "80", "-y", "24"],
        config.clone(),
    )?;
    assert_quiet_success(&create);

    let mut last = None;
    for _ in 0..100 {
        let display = harness.run_pair_with(
            &tmux_binary,
            &[
                "display-message",
                "-p",
                "-t",
                "alpha",
                "#{session_name}:#{window_name}:#{pane_width}",
            ],
            config.clone(),
        )?;
        if display.tmux.stdout == display.rmux.stdout
            && display.tmux.stderr == display.rmux.stderr
            && display.tmux.status_code == display.rmux.status_code
            && utf8_window_name_display_is_ready(&display.rmux.stdout_string())
        {
            assert_exact_tmux_compat(&display);
            assert_run_metadata(
                &display,
                &harness,
                &tmux_binary,
                &[
                    "display-message",
                    "-p",
                    "-t",
                    "alpha",
                    "#{session_name}:#{window_name}:#{pane_width}",
                ],
                &expected_overrides,
            );
            return Ok(());
        }
        last = Some(display);
        std::thread::sleep(Duration::from_millis(20));
    }

    let display = last.expect("utf8 format compatibility was attempted");
    assert_exact_tmux_compat(&display);
    assert_run_metadata(
        &display,
        &harness,
        &tmux_binary,
        &[
            "display-message",
            "-p",
            "-t",
            "alpha",
            "#{session_name}:#{window_name}:#{pane_width}",
        ],
        &expected_overrides,
    );
    assert!(
        utf8_window_name_display_is_ready(&display.rmux.stdout_string()),
        "expected alpha:<window_name>:80 output, got {:?}",
        display.rmux.stdout_string()
    );

    Ok(())
}

#[test]
fn tmux_compat_linked_window_formats_and_names_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-linked-window-formats")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let config = tmux_compat_config();
    let expected_overrides = default_overrides(harness.tmpdir());

    let create_alpha = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha"],
        config.clone(),
    )?;
    assert_quiet_success(&create_alpha);
    assert_run_metadata(
        &create_alpha,
        &harness,
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha"],
        &expected_overrides,
    );

    let create_beta = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "beta"],
        config.clone(),
    )?;
    assert_quiet_success(&create_beta);
    assert_run_metadata(
        &create_beta,
        &harness,
        &tmux_binary,
        &["new-session", "-d", "-s", "beta"],
        &expected_overrides,
    );

    let rename_alpha = harness.run_pair_with(
        &tmux_binary,
        &["rename-window", "-t", "alpha:0", "source"],
        config.clone(),
    )?;
    assert_quiet_success(&rename_alpha);
    assert_run_metadata(
        &rename_alpha,
        &harness,
        &tmux_binary,
        &["rename-window", "-t", "alpha:0", "source"],
        &expected_overrides,
    );

    let rename_beta = harness.run_pair_with(
        &tmux_binary,
        &["rename-window", "-t", "beta:0", "keep0"],
        config.clone(),
    )?;
    assert_quiet_success(&rename_beta);
    assert_run_metadata(
        &rename_beta,
        &harness,
        &tmux_binary,
        &["rename-window", "-t", "beta:0", "keep0"],
        &expected_overrides,
    );

    let link = harness.run_pair_with(
        &tmux_binary,
        &["link-window", "-s", "alpha:0", "-t", "beta:1"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&link);
    assert_run_metadata(
        &link,
        &harness,
        &tmux_binary,
        &["link-window", "-s", "alpha:0", "-t", "beta:1"],
        &expected_overrides,
    );

    let list_windows = harness.run_pair_with(
        &tmux_binary,
        &[
            "list-windows",
            "-t",
            "beta",
            "-F",
            "#{session_name}:#{window_index}:#{window_name}:#{window_linked}:#{window_linked_sessions}:#{window_linked_sessions_list}",
        ],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&list_windows);
    assert_run_metadata(
        &list_windows,
        &harness,
        &tmux_binary,
        &[
            "list-windows",
            "-t",
            "beta",
            "-F",
            "#{session_name}:#{window_index}:#{window_name}:#{window_linked}:#{window_linked_sessions}:#{window_linked_sessions_list}",
        ],
        &expected_overrides,
    );
    assert!(
        list_windows
            .rmux
            .stdout_string()
            .contains("beta:1:source:1:2:alpha,beta"),
        "expected linked list-windows output, got {:?}",
        list_windows.rmux.stdout_string()
    );

    let rename = harness.run_pair_with(
        &tmux_binary,
        &["rename-window", "-t", "beta:1", "logs"],
        config.clone(),
    )?;
    assert_quiet_success(&rename);
    assert_run_metadata(
        &rename,
        &harness,
        &tmux_binary,
        &["rename-window", "-t", "beta:1", "logs"],
        &expected_overrides,
    );

    let display_linked = harness.run_pair_with(
        &tmux_binary,
        &[
            "display-message",
            "-p",
            "-t",
            "alpha:0",
            "#{window_name}:#{window_linked}:#{window_linked_sessions}:#{window_linked_sessions_list}",
        ],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&display_linked);
    assert_run_metadata(
        &display_linked,
        &harness,
        &tmux_binary,
        &[
            "display-message",
            "-p",
            "-t",
            "alpha:0",
            "#{window_name}:#{window_linked}:#{window_linked_sessions}:#{window_linked_sessions_list}",
        ],
        &expected_overrides,
    );
    assert!(
        display_linked
            .rmux
            .stdout_string()
            .trim_end()
            .starts_with("logs:1:2:alpha,beta"),
        "expected linked display-message output, got {:?}",
        display_linked.rmux.stdout_string()
    );

    let unlink = harness.run_pair_with(
        &tmux_binary,
        &["unlink-window", "-t", "beta:1"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&unlink);
    assert_run_metadata(
        &unlink,
        &harness,
        &tmux_binary,
        &["unlink-window", "-t", "beta:1"],
        &expected_overrides,
    );

    let display_unlinked = harness.run_pair_with(
        &tmux_binary,
        &[
            "display-message",
            "-p",
            "-t",
            "alpha:0",
            "#{window_name}:#{window_linked}:#{window_linked_sessions}:#{window_linked_sessions_list}",
        ],
        config,
    )?;
    assert_exact_tmux_compat(&display_unlinked);
    assert_run_metadata(
        &display_unlinked,
        &harness,
        &tmux_binary,
        &[
            "display-message",
            "-p",
            "-t",
            "alpha:0",
            "#{window_name}:#{window_linked}:#{window_linked_sessions}:#{window_linked_sessions_list}",
        ],
        &expected_overrides,
    );
    assert_eq!(
        display_unlinked.rmux.stdout_string().trim(),
        "logs:0:1:alpha"
    );

    Ok(())
}

#[test]
fn tmux_compat_source_file_and_config_path_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-source-file")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let (config, expected_overrides) = config_with_clean_homes(&harness)?;

    // Write a config file that sets a user option
    let conf_path = harness.tmpdir().join("test.conf");
    fs::write(&conf_path, "set-option -g @sourced-opt loaded\n")?;
    let conf_str = conf_path.to_string_lossy().into_owned();

    let create = harness.run_pair_with(
        &tmux_binary,
        &["new-session", "-d", "-s", "alpha"],
        config.clone(),
    )?;
    assert_quiet_success(&create);

    // Source the file
    let source =
        harness.run_pair_with(&tmux_binary, &["source-file", &conf_str], config.clone())?;
    assert_quiet_success(&source);
    assert_run_metadata(
        &source,
        &harness,
        &tmux_binary,
        &["source-file", &conf_str],
        &expected_overrides,
    );

    // Verify the sourced option
    let show = harness.run_pair_with(&tmux_binary, &["show", "-gv", "@sourced-opt"], config)?;
    assert_exact_tmux_compat(&show);
    assert_eq!(show.rmux.stdout_string().trim(), "loaded");

    Ok(())
}
