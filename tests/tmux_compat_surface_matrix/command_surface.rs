use super::support::*;

#[test]
fn tmux_compat_copy_mode_and_control_mode_exact_surfaces_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-copy-control-exact")?;
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

    let copy_mode =
        harness.run_pair_with(&tmux_binary, &["copy-mode", "-t", "alpha:0.0"], config)?;
    assert_exact_tmux_compat(&copy_mode);
    assert_run_metadata(
        &copy_mode,
        &harness,
        &tmux_binary,
        &["copy-mode", "-t", "alpha:0.0"],
        &expected_overrides,
    );

    let usage = harness.run_rmux(&["-h"])?;
    assert!(
        usage.stdout_string().contains('C'),
        "rmux usage should document C flag for control mode: {:?}",
        usage.stdout_string()
    );

    Ok(())
}

#[test]
fn tmux_compat_hook_allow_list_show_hooks_and_prefix_binding_surface_on_rmux_release_head(
) -> Result<(), Box<dyn Error>> {
    // Cluster L shipped a deliberately rmux-specific surface: rejected hooks
    // are trimmed to the dispatch allow-list, `show-hooks` renders tmux-style
    // `hook[index] command`, and `list-keys -T prefix C-b` preserves the
    // global table alignment. Frozen tmux is not the authority for the exact
    // `list-keys -T prefix C-b` invocation because tmux emits no row there; the
    // release check for this cluster is rmux baseline build vs current HEAD.
    let harness = TmuxCompatHarness::new("tmux-compat-hook-allow-list-release-head")?;
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

    let allowed = harness.run_rmux_with(
        &[
            "set-hook",
            "-t",
            "alpha",
            "client-attached",
            "display-message hi",
        ],
        &config,
    )?;
    assert_rmux_metadata(
        &allowed,
        &harness,
        &[
            "set-hook",
            "-t",
            "alpha",
            "client-attached",
            "display-message hi",
        ],
        &expected_overrides,
    );
    assert_eq!(allowed.status_code, Some(0));
    assert!(!allowed.timed_out);
    assert!(allowed.stdout.is_empty());
    assert!(allowed.stderr.is_empty());

    let window_resized = harness.run_rmux_with(
        &["set-hook", "-g", "window-resized", "display-message hi"],
        &config,
    )?;
    assert_rmux_metadata(
        &window_resized,
        &harness,
        &["set-hook", "-g", "window-resized", "display-message hi"],
        &expected_overrides,
    );
    assert_eq!(window_resized.status_code, Some(0));
    assert!(!window_resized.timed_out);
    assert!(window_resized.stdout.is_empty());
    assert!(window_resized.stderr.is_empty());

    let show_hooks =
        harness.run_rmux_with(&["show-hooks", "-t", "alpha", "client-attached"], &config)?;
    assert_rmux_metadata(
        &show_hooks,
        &harness,
        &["show-hooks", "-t", "alpha", "client-attached"],
        &expected_overrides,
    );
    assert_eq!(show_hooks.status_code, Some(0));
    assert!(!show_hooks.timed_out);
    assert_eq!(
        show_hooks.stdout_string(),
        "client-attached[0] display-message hi\n"
    );
    assert!(show_hooks.stderr.is_empty());

    let list_keys = harness.run_rmux_with(&["list-keys", "-T", "prefix", "C-b"], &config)?;
    assert_rmux_metadata(
        &list_keys,
        &harness,
        &["list-keys", "-T", "prefix", "C-b"],
        &expected_overrides,
    );
    assert_eq!(list_keys.status_code, Some(0));
    assert!(!list_keys.timed_out);
    assert_eq!(
        list_keys.stdout_string(),
        "bind-key -T prefix C-b send-prefix\n"
    );
    assert!(list_keys.stderr.is_empty());

    Ok(())
}

#[test]
fn tmux_compat_key_tables_and_list_keys_exact_surface_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-key-tables")?;
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

    let list_keys = harness.run_pair_with(
        &tmux_binary,
        &[
            "list-keys",
            "-F",
            "#{key_table}:#{key_string}:#{key_repeat}",
            "-T",
            "prefix",
        ],
        config,
    )?;
    assert_eq!(list_keys.tmux.status_code, list_keys.rmux.status_code);
    assert_eq!(list_keys.tmux.timed_out, list_keys.rmux.timed_out);
    assert_eq!(list_keys.tmux.stderr, list_keys.rmux.stderr);
    assert_eq!(
        drop_frozen_mirrored_layout_bindings(&list_keys.tmux.stdout),
        list_keys.rmux.stdout
    );
    assert_run_metadata(
        &list_keys,
        &harness,
        &tmux_binary,
        &[
            "list-keys",
            "-F",
            "#{key_table}:#{key_string}:#{key_repeat}",
            "-T",
            "prefix",
        ],
        &expected_overrides,
    );
    assert!(
        list_keys
            .rmux
            .stdout_string()
            .starts_with("prefix:Space:0\n"),
        "expected list-keys formatter compatibility output to enumerate prefix bindings, got {:?}",
        list_keys.rmux.stdout_string()
    );

    Ok(())
}

#[test]
fn tmux_compat_show_messages_exact_surface_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-alerts-message-log")?;
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

    let show_messages =
        harness.run_pair_with(&tmux_binary, &["show-messages", "-J", "-T"], config)?;
    assert_exact_tmux_compat(&show_messages);
    assert_run_metadata(
        &show_messages,
        &harness,
        &tmux_binary,
        &["show-messages", "-J", "-T"],
        &expected_overrides,
    );

    Ok(())
}

#[test]
fn tmux_compat_prompt_target_client_error_surface_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-prompt-target-client")?;
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

    let command_prompt = harness.run_pair_with(
        &tmux_binary,
        &["command-prompt", "-t", "99999", "display-message hi"],
        config.clone(),
    )?;
    assert_exact_tmux_compat(&command_prompt);
    assert_run_metadata(
        &command_prompt,
        &harness,
        &tmux_binary,
        &["command-prompt", "-t", "99999", "display-message hi"],
        &expected_overrides,
    );

    let confirm_before = harness.run_pair_with(
        &tmux_binary,
        &["confirm-before", "-t", "99999", "display-message confirmed"],
        config,
    )?;
    assert_exact_tmux_compat(&confirm_before);
    assert_run_metadata(
        &confirm_before,
        &harness,
        &tmux_binary,
        &["confirm-before", "-t", "99999", "display-message confirmed"],
        &expected_overrides,
    );

    Ok(())
}

#[test]
fn tmux_compat_command_alias_surface_when_frozen_tmux_is_available() -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-command-alias-surface")?;
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
    assert_success_without_stderr(&run);

    let tmux_commands = sorted_first_words(&run.tmux.stdout_string());
    let rmux_commands = sorted_first_words(&run.rmux.stdout_string());
    for command in [
        "clear-prompt-history",
        "display-menu",
        "display-popup",
        "show-prompt-history",
    ] {
        assert!(
            tmux_commands.contains(&command.to_owned()),
            "tmux list-commands should include {command}: {:?}",
            run.tmux.stdout_string()
        );
        assert!(
            rmux_commands.contains(&command.to_owned()),
            "rmux list-commands should include {command}: {:?}",
            run.rmux.stdout_string()
        );
    }

    let rmux_surface = run.rmux.stdout_string();
    for alias in ["clearphist", "menu", "popup", "showphist"] {
        assert!(
            rmux_surface.contains(alias),
            "rmux list-commands should expose alias {alias}: {rmux_surface:?}"
        );
    }

    Ok(())
}

/// Cluster I (error/exit matrix alignment) live row pinning.
///
/// The durable matrix lives in
/// `tests/reference/cluster_i_error_exit_matrix.yaml`; this test is the
/// live compatibility check for rows 1a/1b. Those rows flow through the existing
/// `ExitFailure` / `main.rs` stream-and-exit pipeline and reuse the
/// `RmuxError` Display shapes documented in
/// `crates/rmux-proto/src/error.rs`.
///
/// Rows 1a/1b are the baseline-failure check for this pass: on baseline build
/// the rmux stderr begins with `"server error: "` because both unlock
/// branches in `WaitForStore::unlock` constructed `RmuxError::Server(...)`;
/// on the continuation HEAD both branches use `RmuxError::Message(...)`
/// so the bytes match tmux exactly. `RmuxError` Display shapes in
/// `crates/rmux-proto/src/error.rs:33-37` are unchanged - the fix lands
/// at the construction site, per the control-mode contract.
#[test]
fn tmux_compat_wait_for_unlock_not_locked_channel_when_frozen_tmux_is_available(
) -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("tmux-compat-wait-for-unlock-unknown")?;
    let Some(tmux_binary) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let config = tmux_compat_config();
    let expected_overrides = default_overrides(harness.tmpdir());

    let start_args = ["new-session", "-d", "-s", "alpha"];
    let start = harness.run_pair_with(&tmux_binary, &start_args, config.clone())?;
    assert_quiet_success(&start);
    assert_run_metadata(
        &start,
        &harness,
        &tmux_binary,
        &start_args,
        &expected_overrides,
    );

    // Cluster I row 1a: channel has never been seen by the wait-for
    // store, so `self.channels.get_mut(channel)` is None. tmux-observed
    // tuple is stdout="", stderr="channel unknownchan not locked\n",
    // exit=1. This is the designated baseline-failure row: on the baseline build
    // baseline the rmux stderr is prefixed with "server error: ", on
    // the continuation HEAD it matches tmux byte-for-byte.
    let unlock_unknown_args = ["wait-for", "-U", "unknownchan"];
    let unlock_unknown =
        harness.run_pair_with(&tmux_binary, &unlock_unknown_args, config.clone())?;
    assert_run_metadata(
        &unlock_unknown,
        &harness,
        &tmux_binary,
        &unlock_unknown_args,
        &expected_overrides,
    );
    assert_eq!(unlock_unknown.tmux.stdout, b"");
    assert_eq!(
        unlock_unknown.tmux.stderr_string(),
        "channel unknownchan not locked\n"
    );
    assert_eq!(unlock_unknown.tmux.status_code, Some(1));
    assert!(!unlock_unknown.tmux.timed_out);
    // Pin rmux independently so a future tmux-side byte shift cannot
    // silently pass via `assert_exact_tmux_compat` alone.
    assert_eq!(unlock_unknown.rmux.stdout, b"");
    assert_eq!(
        unlock_unknown.rmux.stderr_string(),
        "channel unknownchan not locked\n"
    );
    assert_eq!(unlock_unknown.rmux.status_code, Some(1));
    assert!(!unlock_unknown.rmux.timed_out);
    assert_exact_tmux_compat(&unlock_unknown);

    // Cluster I row 1b: signaling "signaled-chan" creates an entry in
    // the wait-for store with `woken=true, locked=false`. A subsequent
    // `wait-for -U signaled-chan` now hits the `!state.locked` branch
    // at `crates/rmux-server/src/wait_for.rs:213` rather than the
    // "channel absent from map" branch. Both branches must produce the
    // same bare tmux-compatible error text.
    let signal_args = ["wait-for", "-S", "signaled-chan"];
    let signal = harness.run_pair_with(&tmux_binary, &signal_args, config.clone())?;
    assert_run_metadata(
        &signal,
        &harness,
        &tmux_binary,
        &signal_args,
        &expected_overrides,
    );
    assert_quiet_success(&signal);

    let unlock_signaled_args = ["wait-for", "-U", "signaled-chan"];
    let unlock_signaled =
        harness.run_pair_with(&tmux_binary, &unlock_signaled_args, config.clone())?;
    assert_run_metadata(
        &unlock_signaled,
        &harness,
        &tmux_binary,
        &unlock_signaled_args,
        &expected_overrides,
    );
    assert_eq!(unlock_signaled.tmux.stdout, b"");
    assert_eq!(
        unlock_signaled.tmux.stderr_string(),
        "channel signaled-chan not locked\n"
    );
    assert_eq!(unlock_signaled.tmux.status_code, Some(1));
    assert!(!unlock_signaled.tmux.timed_out);
    assert_eq!(unlock_signaled.rmux.stdout, b"");
    assert_eq!(
        unlock_signaled.rmux.stderr_string(),
        "channel signaled-chan not locked\n"
    );
    assert_eq!(unlock_signaled.rmux.status_code, Some(1));
    assert!(!unlock_signaled.rmux.timed_out);
    assert_exact_tmux_compat(&unlock_signaled);

    Ok(())
}
