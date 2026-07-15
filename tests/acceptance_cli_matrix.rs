use std::error::Error;
use std::ffi::OsStr;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rmux_proto::{CONTROL_CONTROL_END, CONTROL_CONTROL_START};

#[test]
fn cli_acceptance_matrix_exercises_real_daemon_state() -> Result<(), Box<dyn Error>> {
    let harness = AcceptanceHarness::new("cli-matrix")?;
    let session = "acceptance";
    let marker = format!("rmux_acceptance_marker_{}", std::process::id());

    harness.success(["new-session", "-d", "-s", session])?;
    harness.success(["has-session", "-t", session])?;
    harness.success(["split-window", "-h", "-t", &format!("{session}:0.0")])?;

    let panes = harness.stdout(["list-panes", "-t", session, "-F", "#{pane_index}"])?;
    assert!(
        panes.lines().any(|line| line == "0") && panes.lines().any(|line| line == "1"),
        "split-window did not create the expected two panes; list-panes output: {panes:?}"
    );

    harness.success(["send-keys", "-t", &format!("{session}:0.1"), &marker])?;
    harness.wait_for_capture_contains(&format!("{session}:0.1"), &marker)?;

    let config_dir = harness.tmpdir().join("config dir");
    fs::create_dir_all(&config_dir)?;
    let config_path = config_dir.join("rmux acceptance café.conf");
    fs::write(
        &config_path,
        "set-option -g status off\nset-environment -g RMUX_ACCEPTANCE_MATRIX ok\n",
    )?;

    harness.success_in(
        &config_dir,
        [
            OsStr::new("source-file"),
            OsStr::new("rmux acceptance café.conf"),
        ],
    )?;

    let status = harness.stdout(["show-options", "-gqv", "status"])?;
    assert_eq!(
        status.trim(),
        "off",
        "source-file did not apply status option"
    );

    let env = harness.stdout(["show-environment", "-g", "RMUX_ACCEPTANCE_MATRIX"])?;
    assert_eq!(
        env.trim(),
        "RMUX_ACCEPTANCE_MATRIX=ok",
        "source-file did not apply global environment option"
    );

    let sessions = harness.stdout(["list-sessions", "-F", "#{session_name}"])?;
    assert!(
        sessions.lines().any(|line| line == session),
        "list-sessions did not report created session; output: {sessions:?}"
    );

    Ok(())
}

#[test]
fn runtime_command_aliases_override_direct_builtin_commands() -> Result<(), Box<dyn Error>> {
    let harness = AcceptanceHarness::new("direct-runtime-command-alias")?;
    harness.success(["new-session", "-d", "-s", "alpha"])?;
    harness.success([
        "set-option",
        "-g",
        "command-alias[20]",
        "list-sessions=display-message -p canonical-override",
    ])?;
    harness.success([
        "set-option",
        "-g",
        "command-alias[21]",
        "ls=display-message -p short-override",
    ])?;

    assert_eq!(harness.stdout(["list-sessions"])?, "canonical-override\n");
    assert_eq!(harness.stdout(["ls"])?, "short-override\n");
    assert_eq!(
        harness.stdout([
            "list-sessions",
            ";",
            "display-message",
            "-p",
            "unalias",
            ";",
            "ls",
        ])?,
        "canonical-override\nunalias\nshort-override\n"
    );

    let source = harness.tmpdir().join("runtime-command-alias.conf");
    fs::write(&source, "list-sessions\nls\n")?;
    assert_eq!(
        harness.stdout([OsStr::new("source-file"), source.as_os_str()])?,
        "canonical-override\nshort-override\n"
    );

    let literal = "space ; dollar $HOME slash\\ quote' double\"";
    harness.success([
        "set-option",
        "-g",
        "command-alias[20]",
        "list-sessions=display-message -p --",
    ])?;
    assert_eq!(
        harness.stdout(["list-sessions", literal])?,
        format!("{literal}\n")
    );
    assert_eq!(harness.stdout(["list-sessions", "semi\\;"])?, "semi;\n");

    let multiline = "one\ntwo\rthree\tfour";
    harness.success([
        "set-option",
        "-s",
        "command-alias",
        "list-sessions=display-message -p \"one\\ntwo\\rthree\\tfour\"",
    ])?;
    assert_eq!(harness.stdout(["list-sessions"])?, format!("{multiline}\n"));

    let server_value = "line1\nline2\rline3\t$slash\\quote\"`";
    harness.success(["set-environment", "-g", "ALIAS_VISIBLE", server_value])?;
    harness.success([
        "set-option",
        "-s",
        "command-alias",
        "list-sessions=display-message -p \"$ALIAS_VISIBLE\"",
    ])?;
    assert_eq!(
        harness.stdout(["list-sessions"])?,
        format!("{server_value}\n")
    );
    fs::write(&source, "list-sessions\n")?;
    assert_eq!(
        harness.stdout([OsStr::new("source-file"), source.as_os_str()])?,
        format!("{server_value}\n")
    );

    let hidden_value = "hidden\nvalue\\with$dollar";
    harness.success(["set-environment", "-gh", "ALIAS_HIDDEN", hidden_value])?;
    harness.success([
        "set-option",
        "-s",
        "command-alias",
        "list-sessions=display-message -p \"$ALIAS_HIDDEN\"",
    ])?;
    assert_eq!(
        harness.stdout(["list-sessions"])?,
        format!("{hidden_value}\n")
    );
    assert_eq!(
        harness.stdout([OsStr::new("source-file"), source.as_os_str()])?,
        format!("{hidden_value}\n")
    );

    let display_only_value = "a\"\\b";
    harness.success([
        "set-environment",
        "-g",
        "ALIAS_DISPLAY_ONLY",
        display_only_value,
    ])?;
    harness.success([
        "set-option",
        "-s",
        "command-alias",
        "list-sessions=display-message -p \"$ALIAS_DISPLAY_ONLY\"",
    ])?;
    assert_eq!(
        harness.stdout(["list-sessions"])?,
        format!("{display_only_value}\n")
    );

    harness.success([
        "set-option",
        "-s",
        "command-alias",
        "list-sessions=if-shell -F 1 { display-message -p nested-alias }",
    ])?;
    assert_eq!(harness.stdout(["list-sessions"])?, "nested-alias\n");

    harness.success([
        "set-option",
        "-s",
        "command-alias",
        "find-sessions=display-message -p extension-override",
    ])?;
    assert_eq!(harness.stdout(["find-sessions"])?, "extension-override\n");

    Ok(())
}

#[test]
fn cold_start_config_alias_applies_to_first_builtin_product_divergence(
) -> Result<(), Box<dyn Error>> {
    let harness = AcceptanceHarness::new("cold-start-first-command-alias")?;
    let config = harness.tmpdir().join("cold-start-alias.conf");
    fs::write(
        &config,
        "set-option -s command-alias[20] 'new-session=COLD_ALIAS=ok new-session -d -s aliased ; display-message -p \"$COLD_ALIAS\"'\n",
    )?;

    let output = harness.run([
        OsStr::new("-f"),
        config.as_os_str(),
        OsStr::new("new-session"),
    ])?;

    assert_success(&output)?;
    assert_eq!(String::from_utf8(output.stdout)?, "ok\n");
    assert!(output.stderr.is_empty());
    assert_eq!(
        harness.stdout(["list-sessions", "-F", "#{session_name}"])?,
        "aliased\n"
    );
    assert_eq!(
        harness.stdout(["show-environment", "-g", "COLD_ALIAS"])?,
        "COLD_ALIAS=ok\n"
    );
    Ok(())
}

#[test]
fn cold_start_config_alias_respects_no_start_server() -> Result<(), Box<dyn Error>> {
    let harness = AcceptanceHarness::new("cold-start-alias-no-server")?;
    let config = harness.tmpdir().join("cold-start-alias.conf");
    fs::write(
        &config,
        "set-option -s command-alias[20] 'new-session=new-session -d -s forbidden'\n",
    )?;

    let output = harness.run([
        OsStr::new("-N"),
        OsStr::new("-f"),
        config.as_os_str(),
        OsStr::new("new-session"),
    ])?;

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains("no server running") || stderr.contains("error connecting"),
        "stderr={stderr:?}",
    );
    assert!(!harness.run(["list-sessions"])?.status.success());
    Ok(())
}

#[test]
fn cold_start_config_alias_applies_to_later_command_group() -> Result<(), Box<dyn Error>> {
    let harness = AcceptanceHarness::new("cold-start-later-command-alias")?;
    let config = harness.tmpdir().join("cold-start-alias.conf");
    fs::write(
        &config,
        "set-option -s command-alias[20] 'probe=display-message -p OK'\n",
    )?;

    let output = harness.run([
        OsStr::new("-f"),
        config.as_os_str(),
        OsStr::new("new-session"),
        OsStr::new("-d"),
        OsStr::new("-s"),
        OsStr::new("hold"),
        OsStr::new(";"),
        OsStr::new("probe"),
    ])?;

    assert_success(&output)?;
    assert_eq!(String::from_utf8(output.stdout)?, "OK\n");
    assert!(output.stderr.is_empty());
    harness.success(["has-session", "-t", "hold"])?;
    Ok(())
}

#[test]
fn cold_start_assignment_before_builtin_preserves_later_config_alias_product_divergence(
) -> Result<(), Box<dyn Error>> {
    let harness = AcceptanceHarness::new("cold-start-assignment-later-alias")?;
    let config = harness.tmpdir().join("cold-start-alias.conf");
    fs::write(
        &config,
        "set-option -s command-alias[20] 'probe=display-message -p \"$FOO\"'\n",
    )?;

    let output = harness.run([
        OsStr::new("-f"),
        config.as_os_str(),
        OsStr::new("FOO=x"),
        OsStr::new("new-session"),
        OsStr::new("-d"),
        OsStr::new("-s"),
        OsStr::new("hold"),
        OsStr::new(";"),
        OsStr::new("probe"),
    ])?;

    assert_success(&output)?;
    assert_eq!(String::from_utf8(output.stdout)?, "x\n");
    assert!(output.stderr.is_empty());
    assert_eq!(
        harness.stdout(["show-environment", "-g", "FOO"])?,
        "FOO=x\n"
    );
    harness.success(["has-session", "-t", "hold"])?;
    Ok(())
}

#[test]
fn cold_start_later_alias_respects_no_start_server() -> Result<(), Box<dyn Error>> {
    let harness = AcceptanceHarness::new("cold-start-later-alias-no-server")?;
    let config = harness.tmpdir().join("cold-start-alias.conf");
    fs::write(
        &config,
        "set-option -s command-alias[20] 'probe=display-message -p forbidden'\n",
    )?;

    let output = harness.run([
        OsStr::new("-N"),
        OsStr::new("-f"),
        config.as_os_str(),
        OsStr::new("new-session"),
        OsStr::new("-d"),
        OsStr::new("-s"),
        OsStr::new("forbidden"),
        OsStr::new(";"),
        OsStr::new("probe"),
    ])?;

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!harness
        .run(["-N", "has-session", "-t", "forbidden"])?
        .status
        .success());
    Ok(())
}

#[test]
fn cold_start_unknown_later_command_is_not_partially_dispatched() -> Result<(), Box<dyn Error>> {
    let harness = AcceptanceHarness::new("cold-start-unknown-later-command")?;
    let config = harness.tmpdir().join("cold-start.conf");
    fs::write(&config, "set-option -g status off\n")?;

    let output = harness.run([
        OsStr::new("-f"),
        config.as_os_str(),
        OsStr::new("new-session"),
        OsStr::new("-d"),
        OsStr::new("-s"),
        OsStr::new("must-not-exist"),
        OsStr::new(";"),
        OsStr::new("not-a-command"),
    ])?;

    assert!(!output.status.success());
    assert!(output.stdout.is_empty());
    assert!(!harness
        .run(["-N", "has-session", "-t", "must-not-exist"])?
        .status
        .success());
    Ok(())
}

#[test]
fn runtime_command_aliases_are_not_expanded_twice() -> Result<(), Box<dyn Error>> {
    let harness = AcceptanceHarness::new("runtime-command-alias-single-expansion")?;
    harness.success(["new-session", "-d", "-s", "alpha"])?;
    harness.success([
        "set-option",
        "-s",
        "command-alias",
        "foo=if-shell -F 1 \"display-message -p foo\"",
    ])?;
    harness.success([
        "set-option",
        "-sa",
        "command-alias",
        "if-shell=display-message -p second",
    ])?;

    assert_eq!(harness.stdout(["foo"])?, "foo\n");
    Ok(())
}

#[test]
fn runtime_alias_parse_time_assignments_persist_without_set_environment_hooks(
) -> Result<(), Box<dyn Error>> {
    let harness = AcceptanceHarness::new("runtime-alias-assignments")?;
    harness.success(["new-session", "-d", "-s", "alpha"])?;
    harness.success(["set-environment", "-gu", "ASSIGNMENT_HOOK"])?;
    harness.success([
        "set-hook",
        "-g",
        "after-set-environment",
        "set-environment -g ASSIGNMENT_HOOK fired",
    ])?;
    harness.success([
        "set-option",
        "-s",
        "command-alias",
        "zz=FOO=bar display-message -p \"$FOO\"",
    ])?;

    assert_eq!(harness.stdout(["zz"])?, "bar\n");
    assert_eq!(
        harness.stdout(["show-environment", "-g", "FOO"])?,
        "FOO=bar\n"
    );
    assert!(!harness
        .run(["show-environment", "-g", "ASSIGNMENT_HOOK"])?
        .status
        .success());
    Ok(())
}

#[test]
fn invalid_runtime_alias_does_not_apply_parse_time_assignments_product_divergence(
) -> Result<(), Box<dyn Error>> {
    let harness = AcceptanceHarness::new("runtime-alias-invalid-assignment")?;
    harness.success(["new-session", "-d", "-s", "alpha"])?;
    harness.success(["set-environment", "-gu", "INVALID_ALIAS_VALUE"])?;
    harness.success([
        "set-option",
        "-s",
        "command-alias",
        "bad=INVALID_ALIAS_VALUE=written new-window -Q",
    ])?;

    assert!(!harness.run(["bad"])?.status.success());
    assert!(!harness
        .run(["show-environment", "-g", "INVALID_ALIAS_VALUE"])?
        .status
        .success());
    Ok(())
}

#[test]
fn runtime_alias_preparse_has_no_show_options_hook_side_effects() -> Result<(), Box<dyn Error>> {
    let harness = AcceptanceHarness::new("runtime-alias-hook-isolation")?;
    harness.success(["new-session", "-d", "-s", "alpha"])?;
    harness.success(["set-environment", "-gu", "ALIAS_HOOK"])?;
    harness.success([
        "set-hook",
        "-g",
        "after-show-options",
        "set-environment -g ALIAS_HOOK fired",
    ])?;

    harness.success(["list-sessions"])?;
    assert!(!harness
        .run(["show-environment", "-g", "ALIAS_HOOK"])?
        .status
        .success());

    harness.success(["show-options", "-g", "status"])?;
    assert_eq!(
        harness.stdout(["show-environment", "-g", "ALIAS_HOOK"])?,
        "ALIAS_HOOK=fired\n"
    );
    Ok(())
}

#[test]
fn runtime_preparse_preserves_stateful_and_extension_queue_order() -> Result<(), Box<dyn Error>> {
    let harness = AcceptanceHarness::new("runtime-preparse-queue-order")?;
    harness.success([
        "new-session",
        "-d",
        "-s",
        "created",
        ";",
        "new-window",
        "-d",
        "-t",
        "created",
        "-n",
        "second",
    ])?;
    assert_eq!(
        harness
            .stdout(["list-windows", "-t", "created", "-F", "#{window_name}"])?
            .lines()
            .count(),
        2
    );

    harness.success([
        "set-option",
        "-s",
        "command-alias",
        "ls=display-message -p alias-segment",
    ])?;
    assert_eq!(
        harness.stdout([
            "ls",
            ";",
            "find-sessions",
            "--name",
            "does-not-exist",
            ";",
            "ls",
        ])?,
        "alias-segment\nalias-segment\n"
    );
    Ok(())
}

#[test]
fn runtime_command_aliases_use_one_ordered_snapshot() -> Result<(), Box<dyn Error>> {
    let harness = AcceptanceHarness::new("runtime-command-alias-snapshot")?;
    harness.success(["new-session", "-d", "-s", "alpha"])?;
    harness.success([
        "set-option",
        "-s",
        "command-alias",
        "list-sessions=display-message -p old",
    ])?;

    assert_eq!(
        harness.stdout([
            "set-option",
            "-s",
            "command-alias",
            "list-sessions=display-message -p new",
            ";",
            "list-sessions",
        ])?,
        "old\n"
    );
    harness.success([
        "set-option",
        "-s",
        "command-alias",
        "list-sessions=display-message -p old",
    ])?;
    assert_eq!(
        harness.stdout([
            "set-option",
            "-s",
            "command-alias",
            "",
            ";",
            "list-sessions",
        ])?,
        "old\n"
    );

    let disabled_default = harness.run(["split-pane", "-d", "-t", "alpha:0.0"])?;
    assert!(!disabled_default.status.success());
    assert_eq!(
        String::from_utf8(disabled_default.stderr)?,
        "unknown command: split-pane\n"
    );
    assert_eq!(
        harness
            .stdout(["list-panes", "-t", "alpha", "-F", "#{pane_index}"])?
            .lines()
            .count(),
        1
    );

    harness.success([
        "set-option",
        "-s",
        "command-alias",
        "dup=display-message -p first",
    ])?;
    harness.success([
        "set-option",
        "-sa",
        "command-alias",
        "dup=display-message -p second",
    ])?;
    assert_eq!(harness.stdout(["dup"])?, "first\n");

    Ok(())
}

#[test]
fn runtime_command_aliases_keep_client_transitions_typed() -> Result<(), Box<dyn Error>> {
    let harness = AcceptanceHarness::new("runtime-command-alias-client-transitions")?;
    harness.success(["new-session", "-d", "-s", "alpha"])?;
    harness.success(["set-environment", "-g", "ALIAS_TARGET", "alpha"])?;
    harness.success([
        "set-option",
        "-s",
        "command-alias",
        "list-sessions=attach-session -t \"$ALIAS_TARGET\"",
    ])?;

    let attach = harness.run(["list-sessions"])?;
    assert!(!attach.status.success());
    assert_eq!(
        String::from_utf8(attach.stderr)?,
        "open terminal failed: not a terminal\n"
    );

    harness.success([
        "set-option",
        "-s",
        "command-alias",
        "kill-server=display-message -p alias-survives",
    ])?;
    assert_eq!(harness.stdout(["kill-server"])?, "alias-survives\n");
    harness.success(["has-session", "-t", "alpha"])?;

    harness.success(["set-environment", "-g", "ALIAS_COMMAND", "kill-server"])?;
    harness.success([
        "set-option",
        "-s",
        "command-alias",
        "list-sessions=$ALIAS_COMMAND",
    ])?;
    harness.success(["list-sessions"])?;
    assert!(!harness
        .run(["has-session", "-t", "alpha"])?
        .status
        .success());

    Ok(())
}

#[test]
fn control_mode_leaves_runtime_alias_expansion_to_the_server() -> Result<(), Box<dyn Error>> {
    let harness = AcceptanceHarness::new("runtime-command-alias-control")?;
    harness.success(["new-session", "-d", "-s", "alpha"])?;
    harness.success(["set-option", "-s", "command-alias", ""])?;

    let control = harness.run(["-C", "split-pane", "-d", "-t", "alpha:0.0"])?;
    assert_eq!(control.status.code(), Some(1));
    assert!(control.stderr.is_empty());
    let control_stdout = String::from_utf8(control.stdout)?;
    assert!(control_stdout.contains("unknown command: split-pane"));
    assert!(control_stdout.contains("%exit"));
    assert_eq!(
        harness
            .stdout(["list-panes", "-t", "alpha", "-F", "#{pane_index}"])?
            .lines()
            .count(),
        1
    );

    Ok(())
}

#[test]
fn control_mode_validates_flags_and_keeps_literal_newlines_in_one_command(
) -> Result<(), Box<dyn Error>> {
    let harness = AcceptanceHarness::new("runtime-command-alias-control-framing")?;
    harness.success(["new-session", "-d", "-s", "alpha"])?;

    let invalid = harness.run(["-C", "list-sessions", "-Z"])?;
    assert_eq!(invalid.status.code(), Some(1));
    assert!(invalid.stderr.is_empty());
    let invalid_stdout = String::from_utf8(invalid.stdout)?;
    assert!(
        invalid_stdout.contains("unknown flag -Z"),
        "{invalid_stdout:?}"
    );
    assert!(invalid_stdout.contains("%exit"), "{invalid_stdout:?}");

    let invalid_control_control = harness.run(["-CC", "list-sessions", "-Z"])?;
    assert_eq!(invalid_control_control.status.code(), Some(1));
    assert!(invalid_control_control.stderr.is_empty());
    let expected_control_control = format!(
        "{CONTROL_CONTROL_START}command list-sessions: unknown flag -Z\n%exit\n{CONTROL_CONTROL_END}"
    );
    assert_eq!(
        invalid_control_control.stdout,
        expected_control_control.as_bytes(),
        "control-control prevalidation errors must be exactly DCS framed"
    );

    let lone_dash = harness.run(["-C", "list-sessions", "-"])?;
    assert_eq!(lone_dash.status.code(), Some(1));
    assert!(lone_dash.stderr.is_empty());
    let lone_dash_stdout = String::from_utf8(lone_dash.stdout)?;
    assert!(lone_dash_stdout.contains("%exit"), "{lone_dash_stdout:?}");
    assert!(
        !lone_dash_stdout.contains("unknown flag -"),
        "{lone_dash_stdout:?}"
    );

    let invalid_terminal = harness.run(["-C", "kill-server", "-Z"])?;
    assert_eq!(invalid_terminal.status.code(), Some(1));
    assert!(invalid_terminal.stderr.is_empty());
    let invalid_terminal_stdout = String::from_utf8(invalid_terminal.stdout)?;
    assert!(
        invalid_terminal_stdout.contains("command kill-server: unknown flag -Z"),
        "{invalid_terminal_stdout:?}"
    );
    assert!(
        invalid_terminal_stdout.contains("%exit"),
        "{invalid_terminal_stdout:?}"
    );
    harness.success(["has-session", "-t", "alpha"])?;

    let injected = harness.run(["-C", "list-sessions", "--bad\n%exit"])?;
    assert_eq!(injected.status.code(), Some(1));
    assert!(injected.stderr.is_empty());
    let injected_stdout = String::from_utf8(injected.stdout)?;
    assert_eq!(
        injected_stdout
            .lines()
            .filter(|line| *line == "%exit")
            .count(),
        1,
        "{injected_stdout:?}"
    );
    assert!(
        injected_stdout.contains("--bad\\n%exit"),
        "{injected_stdout:?}"
    );

    let injected_control_control = harness.run(["-CC", "list-sessions", "--bad\n%exit"])?;
    assert_eq!(injected_control_control.status.code(), Some(1));
    assert!(injected_control_control.stderr.is_empty());
    let injected_control_control_stdout = String::from_utf8(injected_control_control.stdout)?;
    let inner = injected_control_control_stdout
        .strip_prefix(CONTROL_CONTROL_START)
        .and_then(|value| value.strip_suffix(CONTROL_CONTROL_END))
        .ok_or("control-control injection diagnostic lost its exact DCS envelope")?;
    assert_eq!(
        inner
            .lines()
            .filter(|line| line.starts_with('%'))
            .collect::<Vec<_>>(),
        vec!["%exit"],
        "{injected_control_control_stdout:?}"
    );
    assert!(
        inner.contains("--bad\\n%exit"),
        "{injected_control_control_stdout:?}"
    );

    let injected_value = harness.run([
        "-C",
        "set-hook",
        "-g",
        "bad\n%exit",
        "display-message -p ignored",
    ])?;
    assert_eq!(injected_value.status.code(), Some(1));
    assert!(injected_value.stderr.is_empty());
    let injected_value_stdout = String::from_utf8(injected_value.stdout)?;
    assert_eq!(
        injected_value_stdout
            .lines()
            .filter(|line| line.starts_with('%'))
            .collect::<Vec<_>>(),
        vec!["%exit"],
        "{injected_value_stdout:?}"
    );

    let injected_command = harness.run(["-C", "bad\n%exit"])?;
    assert_eq!(injected_command.status.code(), Some(1));
    assert!(injected_command.stderr.is_empty());
    let injected_command_stdout = String::from_utf8(injected_command.stdout)?;
    assert_eq!(
        injected_command_stdout
            .lines()
            .filter(|line| line.starts_with('%'))
            .collect::<Vec<_>>(),
        vec!["%exit"],
        "{injected_command_stdout:?}"
    );

    let injected_command_control_control = harness.run(["-CC", "bad\n%exit"])?;
    assert_eq!(injected_command_control_control.status.code(), Some(1));
    assert!(injected_command_control_control.stderr.is_empty());
    let injected_command_control_control_stdout =
        String::from_utf8(injected_command_control_control.stdout)?;
    let inner = injected_command_control_control_stdout
        .strip_prefix(CONTROL_CONTROL_START)
        .and_then(|value| value.strip_suffix(CONTROL_CONTROL_END))
        .ok_or("control-control command diagnostic lost its exact DCS envelope")?;
    assert_eq!(
        inner
            .lines()
            .filter(|line| line.starts_with('%'))
            .collect::<Vec<_>>(),
        vec!["%exit"],
        "{injected_command_control_control_stdout:?}"
    );

    let literal = harness.run(["-C", "display-message", "-p", "literal\nlist-sessions"])?;
    assert_eq!(literal.status.code(), Some(0));
    assert!(literal.stderr.is_empty());
    let literal_stdout = String::from_utf8(literal.stdout)?;
    assert_eq!(
        literal_stdout.matches("%begin").count(),
        1,
        "{literal_stdout:?}"
    );
    assert_eq!(
        literal_stdout.matches("%end").count(),
        1,
        "{literal_stdout:?}"
    );

    harness.success([
        "set-option",
        "-s",
        "command-alias",
        "zz=CONTROL_ALIAS_VALUE=ok display-message -p \"$CONTROL_ALIAS_VALUE\"",
    ])?;
    let aliased = harness.run(["-C", "zz"])?;
    assert_eq!(aliased.status.code(), Some(0));
    assert!(aliased.stderr.is_empty());
    let aliased_stdout = String::from_utf8(aliased.stdout)?;
    assert_eq!(
        aliased_stdout.matches("%begin").count(),
        1,
        "{aliased_stdout:?}"
    );
    assert_eq!(
        aliased_stdout.matches("%end").count(),
        1,
        "{aliased_stdout:?}"
    );
    assert!(
        aliased_stdout.lines().any(|line| line == "ok"),
        "{aliased_stdout:?}"
    );
    assert_eq!(
        harness.stdout(["show-environment", "-g", "CONTROL_ALIAS_VALUE"])?,
        "CONTROL_ALIAS_VALUE=ok\n"
    );
    Ok(())
}

#[test]
fn kill_server_is_terminal_for_the_cli_command_queue() -> Result<(), Box<dyn Error>> {
    for existing_server in [false, true] {
        let harness = AcceptanceHarness::new(if existing_server {
            "kill-server-terminal-existing"
        } else {
            "kill-server-terminal-absent"
        })?;
        if existing_server {
            harness.success(["new-session", "-d", "-s", "seed"])?;
        }

        harness.success([
            "kill-server",
            ";",
            "new-session",
            "-d",
            "-s",
            "must-not-exist",
        ])?;
        let probe = harness.run(["has-session", "-t", "must-not-exist"])?;
        assert!(
            !probe.status.success(),
            "commands after kill-server must not recreate the daemon"
        );
    }
    Ok(())
}

#[test]
fn detached_window_spawns_without_c_use_non_attached_caller_cwd() -> Result<(), Box<dyn Error>> {
    let harness = AcceptanceHarness::new("window-caller-cwd")?;
    let session_cwd = harness.tmpdir().join("session-cwd");
    let caller_cwd = harness.tmpdir().join("caller-cwd");
    fs::create_dir_all(&session_cwd)?;
    fs::create_dir_all(&caller_cwd)?;
    let session_cwd = fs::canonicalize(session_cwd)?;
    let caller_cwd = fs::canonicalize(caller_cwd)?;

    harness.success_in(&session_cwd, ["new-session", "-d", "-s", "cwd-parity"])?;

    // Simple forms exercise the tiny client path.
    harness.success_in(
        &caller_cwd,
        ["new-window", "-d", "-t", "cwd-parity", "-n", "tiny-new"],
    )?;
    harness.success_in(&caller_cwd, ["split-window", "-d", "-t", "cwd-parity:0.0"])?;

    // Environment flags force the full CLI path while preserving the same
    // no-`-c` semantics.
    harness.success_in(
        &caller_cwd,
        [
            "new-window",
            "-d",
            "-t",
            "cwd-parity",
            "-n",
            "full-new",
            "-e",
            "RMUX_CWD_PATH=full-new",
        ],
    )?;
    harness.success_in(
        &caller_cwd,
        [
            "split-window",
            "-d",
            "-t",
            "cwd-parity:0.0",
            "-e",
            "RMUX_CWD_PATH=full-split",
        ],
    )?;

    // Sourced commands carry the detached caller cwd through the queue.
    let source = harness.tmpdir().join("window-cwd.conf");
    fs::write(
        &source,
        "new-window -d -t cwd-parity -n source-new\nsplit-window -d -t cwd-parity:0.0\n",
    )?;
    harness.success_in(&caller_cwd, [OsStr::new("source-file"), source.as_os_str()])?;

    let panes = harness.stdout(["list-panes", "-a", "-F", "#{pane_current_path}"])?;
    let paths = panes.lines().map(normalized_path).collect::<Vec<_>>();
    let expected_session = normalized_path(&session_cwd.to_string_lossy());
    let expected_caller = normalized_path(&caller_cwd.to_string_lossy());
    assert_eq!(
        paths
            .iter()
            .filter(|path| **path == expected_session)
            .count(),
        1,
        "only the initial pane should keep the session cwd; paths={paths:?}"
    );
    assert_eq!(
        paths
            .iter()
            .filter(|path| **path == expected_caller)
            .count(),
        6,
        "tiny, full, and source-file new/split paths should use the caller cwd; paths={paths:?}"
    );
    assert_eq!(paths.len(), 7, "unexpected pane count; paths={paths:?}");

    Ok(())
}

fn normalized_path(path: &str) -> String {
    let path = path.strip_prefix(r"\\?\").unwrap_or(path);
    let path = path.replace('\\', "/");
    #[cfg(windows)]
    let path = path.to_ascii_lowercase();
    path.trim_end_matches('/').to_owned()
}

struct AcceptanceHarness {
    label: String,
    tmpdir: PathBuf,
}

impl AcceptanceHarness {
    fn new(label: &str) -> Result<Self, Box<dyn Error>> {
        let unique = unique_id(label);
        let tmpdir = std::env::temp_dir().join(&unique);
        let _ = fs::remove_dir_all(&tmpdir);
        fs::create_dir_all(&tmpdir)?;
        let harness = Self {
            label: unique,
            tmpdir,
        };
        let _ = harness.run(["kill-server"]);
        Ok(harness)
    }

    fn tmpdir(&self) -> &Path {
        &self.tmpdir
    }

    fn success<I, S>(&self, args: I) -> Result<(), Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.run(args)?;
        assert_success(&output)
    }

    fn success_in<I, S>(&self, cwd: &Path, args: I) -> Result<(), Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.run_in(cwd, args)?;
        assert_success(&output)
    }

    fn stdout<I, S>(&self, args: I) -> Result<String, Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let output = self.run(args)?;
        assert_success(&output)?;
        Ok(String::from_utf8(output.stdout)?)
    }

    fn run<I, S>(&self, args: I) -> Result<Output, Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        self.run_in(Path::new("."), args)
    }

    fn run_in<I, S>(&self, cwd: &Path, args: I) -> Result<Output, Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<OsStr>,
    {
        let mut command = Command::new(rmux_binary());
        command
            .current_dir(cwd)
            .arg("-L")
            .arg(&self.label)
            .args(args);
        Ok(command.output()?)
    }

    fn wait_for_capture_contains(&self, target: &str, needle: &str) -> Result<(), Box<dyn Error>> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut last = String::new();
        while Instant::now() < deadline {
            last = self.stdout(["capture-pane", "-p", "-t", target])?;
            if capture_contains_terminal_text(&last, needle) {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(100));
        }
        Err(format!(
            "capture-pane for target {target} did not contain {needle:?}; last capture: {last:?}"
        )
        .into())
    }
}

fn capture_contains_terminal_text(capture: &str, needle: &str) -> bool {
    if capture.contains(needle) {
        return true;
    }

    // `capture-pane -p` exposes physical terminal rows.  On Windows a long
    // shell prompt in a split pane can soft-wrap inside text typed with
    // `send-keys`, even though the pane contains the requested bytes in order.
    // Keep the oracle strict about character order, but ignore row breaks that
    // are an artifact of terminal capture rather than process output.
    let unwrapped: String = capture
        .chars()
        .filter(|ch| !matches!(ch, '\r' | '\n'))
        .collect();
    unwrapped.contains(needle)
}

impl Drop for AcceptanceHarness {
    fn drop(&mut self) {
        let _ = self.run(["kill-server"]);
        let _ = fs::remove_dir_all(&self.tmpdir);
    }
}

fn rmux_binary() -> &'static Path {
    Path::new(env!("CARGO_BIN_EXE_rmux"))
}

fn assert_success(output: &Output) -> Result<(), Box<dyn Error>> {
    if output.status.success() {
        return Ok(());
    }
    Err(format!(
        "rmux command failed with status {}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .into())
}

fn unique_id(label: &str) -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("system clock before epoch")
        .as_nanos();
    format!("rmux-{label}-{}-{nanos}", std::process::id())
        .chars()
        .map(|ch| {
            if ch.is_ascii_alphanumeric() || ch == '-' {
                ch
            } else {
                '-'
            }
        })
        .collect()
}

#[test]
fn capture_contains_terminal_text_accepts_soft_wrapped_needles() {
    assert!(capture_contains_terminal_text(
        "prompt>rmux_acceptance_marker\n_1234\n",
        "rmux_acceptance_marker_1234"
    ));
    assert!(!capture_contains_terminal_text(
        "prompt>rmux_acceptance_marker\n_wrong\n",
        "rmux_acceptance_marker_1234"
    ));
}
