mod common_cross;

use std::error::Error;
use std::ffi::OsStr;
use std::fs;

use common_cross::{assert_success, CrossPlatformHarness};

#[test]
fn direct_cli_accepts_clustered_scripting_flags() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("direct-scripting-clusters")?;
    harness.success(["new-session", "-d", "-s", "anchor"])?;

    harness.success([
        "bind-key",
        "-nr",
        "-N",
        "direct-note",
        "C-a",
        "display-message",
        "-p",
        "direct-cluster",
    ])?;
    assert_binding_contains(&harness, "C-a", "direct-cluster")?;

    harness.success(["set-buffer", "-b", "direct-audit", "head"])?;
    harness.success(["set-buffer", "-aw", "-b", "direct-audit", "tail"])?;
    assert_eq!(
        harness.stdout(["show-buffer", "-b", "direct-audit"])?,
        "headtail"
    );

    harness.success(["set-environment", "-g", "DIRECT_CLUSTER_ENV", "keep"])?;
    harness.success(["set-environment", "-gu", "DIRECT_CLUSTER_ENV"])?;
    assert_environment_missing(&harness, "DIRECT_CLUSTER_ENV")
}

#[test]
fn source_file_accepts_clustered_scripting_flags_and_parse_only_stays_inert(
) -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("source-bind-key-cluster")?;
    harness.success(["new-session", "-d", "-s", "anchor"])?;
    harness.success(["set-buffer", "-b", "source-audit", "head"])?;
    harness.success(["set-environment", "-g", "SOURCE_CLUSTER_ENV", "keep"])?;
    let config = harness.tmpdir().join("clustered-bind-key.conf");
    fs::write(
        &config,
        "bind-key -nrTroot -N 'source note' C-b display-message -p source-cluster\n\
         set-buffer -aw -b source-audit tail\n\
         set-environment -gu SOURCE_CLUSTER_ENV\n",
    )?;

    harness.success([OsStr::new("source-file"), config.as_os_str()])?;
    assert_binding_contains(&harness, "C-b", "source-cluster")?;
    assert_eq!(
        harness.stdout(["show-buffer", "-b", "source-audit"])?,
        "headtail"
    );
    assert_environment_missing(&harness, "SOURCE_CLUSTER_ENV")?;

    harness.success(["set-buffer", "-b", "source-audit", "stable"])?;
    harness.success(["set-environment", "-g", "SOURCE_CLUSTER_ENV", "stable"])?;
    let parse_only = harness.tmpdir().join("clustered-bind-key-parse-only.conf");
    fs::write(
        &parse_only,
        "bind-key -nr C-c display-message -p parse-only-cluster\n\
         set-buffer -aw -b source-audit ignored\n\
         set-environment -gu SOURCE_CLUSTER_ENV\n",
    )?;
    harness.success([
        OsStr::new("source-file"),
        OsStr::new("-n"),
        parse_only.as_os_str(),
    ])?;
    assert_binding_absent(&harness, "C-c", "parse-only-cluster")?;
    assert_eq!(
        harness.stdout(["show-buffer", "-b", "source-audit"])?,
        "stable"
    );
    assert_environment(&harness, "SOURCE_CLUSTER_ENV=stable")
}

#[test]
fn startup_config_accepts_clustered_scripting_flags() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("startup-bind-key-cluster")?;
    let config = harness.tmpdir().join("clustered-bind-key-startup.conf");
    fs::write(
        &config,
        "bind-key -nr -T root C-d display-message -p startup-cluster\n\
         set-buffer -b startup-audit head\n\
         set-buffer -aw -b startup-audit tail\n\
         set-environment -g STARTUP_CLUSTER_ENV keep\n\
         set-environment -gu STARTUP_CLUSTER_ENV\n",
    )?;

    harness.success([
        OsStr::new("-f"),
        config.as_os_str(),
        OsStr::new("new-session"),
        OsStr::new("-d"),
        OsStr::new("-s"),
        OsStr::new("requested"),
    ])?;
    assert_binding_contains(&harness, "C-d", "startup-cluster")?;
    assert_eq!(
        harness.stdout(["show-buffer", "-b", "startup-audit"])?,
        "headtail"
    );
    assert_environment_missing(&harness, "STARTUP_CLUSTER_ENV")
}

#[test]
fn option_like_values_after_positionals_survive_all_command_entry_paths(
) -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("option-like-positional-values")?;
    harness.success(["new-session", "-d", "-s", "anchor"])?;

    harness.success(["set-option", "-g", "@direct", "-tfoo"])?;
    assert_option(&harness, "@direct", "-tfoo")?;

    let source = harness.tmpdir().join("source.conf");
    fs::write(&source, "set-option -g @source -tfoo\n")?;
    harness.success([OsStr::new("source-file"), source.as_os_str()])?;
    assert_option(&harness, "@source", "-tfoo")?;

    let first_path = harness.tmpdir().join("first.conf");
    let option_like_path = harness.tmpdir().join("-tfoo");
    fs::write(&first_path, "set-option -g @first-path loaded\n")?;
    fs::write(
        &option_like_path,
        "set-option -g @option-like-path loaded\n",
    )?;
    let mut source_paths = harness.command(["source-file", "first.conf", "-tfoo"]);
    source_paths.current_dir(harness.tmpdir());
    assert_success(&source_paths.output()?)?;
    assert_option(&harness, "@first-path", "loaded")?;
    assert_option(&harness, "@option-like-path", "loaded")?;

    harness.success([
        "set-option",
        "-s",
        "command-alias",
        "literal=set-option -g @alias",
    ])?;
    harness.success(["literal", "-tfoo"])?;
    assert_option(&harness, "@alias", "-tfoo")?;

    harness.success(["-C", "set-option", "-g", "@control", "-tfoo"])?;
    assert_option(&harness, "@control", "-tfoo")?;

    let startup = CrossPlatformHarness::new("startup-option-like-positional")?;
    let startup_config = startup.tmpdir().join("startup.conf");
    fs::write(&startup_config, "set-option -g @startup -tfoo\n")?;
    startup.success([
        OsStr::new("-f"),
        startup_config.as_os_str(),
        OsStr::new("new-session"),
        OsStr::new("-d"),
        OsStr::new("-s"),
        OsStr::new("startup"),
    ])?;
    assert_option(&startup, "@startup", "-tfoo")
}

#[test]
fn source_file_parse_only_accepts_remaining_compact_flag_families() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("source-remaining-compact-flags")?;
    harness.success(["new-session", "-d", "-s", "anchor"])?;
    let config = harness.tmpdir().join("remaining-compact-flags.conf");
    fs::write(
        &config,
        "detach-client -aP\n\
         refresh-client -lS\n\
         switch-client -Er\n\
         list-clients -rF client\n\
         server-access -lr\n\
         display-panes -bN\n\
         last-pane -de\n",
    )?;

    harness.success([
        OsStr::new("source-file"),
        OsStr::new("-n"),
        config.as_os_str(),
    ])?;
    Ok(())
}

#[test]
fn startup_config_keeps_commands_around_compact_client_flags() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("startup-compact-client-flags")?;
    let config = harness.tmpdir().join("compact-client-startup.conf");
    fs::write(
        &config,
        "set-environment -g COMPACT_CLIENT_BEFORE ok\n\
         list-clients -rF client\n\
         set-environment -g COMPACT_CLIENT_AFTER ok\n",
    )?;

    harness.success([
        OsStr::new("-f"),
        config.as_os_str(),
        OsStr::new("new-session"),
        OsStr::new("-d"),
        OsStr::new("-s"),
        OsStr::new("requested"),
    ])?;
    assert_environment(&harness, "COMPACT_CLIENT_BEFORE=ok")?;
    assert_environment(&harness, "COMPACT_CLIENT_AFTER=ok")
}

#[test]
fn if_shell_accepts_clustered_scripting_flags() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("if-shell-bind-key-cluster")?;
    harness.success(["new-session", "-d", "-s", "anchor"])?;
    harness.success(["set-buffer", "-b", "if-shell-audit", "head"])?;
    harness.success(["set-environment", "-g", "IF_SHELL_CLUSTER_ENV", "keep"])?;

    harness.success([
        "if-shell",
        "-F",
        "1",
        "bind-key -nrTroot C-e display-message -p if-shell-cluster",
    ])?;
    assert_binding_contains(&harness, "C-e", "if-shell-cluster")?;

    harness.success([
        "if-shell",
        "-F",
        "1",
        "set-buffer -aw -b if-shell-audit tail",
    ])?;
    assert_eq!(
        harness.stdout(["show-buffer", "-b", "if-shell-audit"])?,
        "headtail"
    );

    harness.success([
        "if-shell",
        "-F",
        "1",
        "set-environment -gu IF_SHELL_CLUSTER_ENV",
    ])?;
    assert_environment_missing(&harness, "IF_SHELL_CLUSTER_ENV")
}

#[test]
fn source_file_accepts_clustered_new_session_flags() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("source-new-session-cluster")?;
    harness.success(["new-session", "-d", "-s", "anchor"])?;
    let config = harness.tmpdir().join("clustered-new-session.conf");
    fs::write(&config, "new-session -dP -s sourced\n")?;

    harness.success([OsStr::new("source-file"), config.as_os_str()])?;
    harness.success(["has-session", "-t", "sourced"])?;
    Ok(())
}

#[test]
fn startup_config_accepts_clustered_new_session_flags() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("startup-new-session-cluster")?;
    let config = harness.tmpdir().join("clustered-new-session.conf");
    fs::write(&config, "new-session -dP -s configured\n")?;

    harness.success([
        OsStr::new("-f"),
        config.as_os_str(),
        OsStr::new("new-session"),
        OsStr::new("-d"),
        OsStr::new("-s"),
        OsStr::new("requested"),
    ])?;
    harness.success(["has-session", "-t", "configured"])?;
    harness.success(["has-session", "-t", "requested"])?;
    Ok(())
}

#[test]
fn source_file_accepts_clustered_swap_window_flags() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("source-swap-window-cluster")?;
    create_swap_window_fixture(&harness, "source")?;
    let config = harness.tmpdir().join("clustered-swap-window.conf");
    fs::write(&config, "swap-window -ds source:0 -tsource:1\n")?;

    harness.success([OsStr::new("source-file"), config.as_os_str()])?;
    assert_swap_window_order(&harness, "source", "0:one\n1:zero\n")
}

#[test]
fn if_shell_accepts_clustered_swap_window_flags() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("if-shell-swap-window-cluster")?;
    create_swap_window_fixture(&harness, "ifshell")?;

    harness.success([
        "if-shell",
        "-F",
        "1",
        "swap-window -dt ifshell:1 -sifshell:0",
    ])?;
    assert_swap_window_order(&harness, "ifshell", "0:one\n1:zero\n")
}

#[test]
fn command_queue_preserves_swap_window_value_flag_boundaries() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("queue-swap-window-cluster")?;
    create_swap_window_fixture(&harness, "queued")?;
    harness.success(["new-window", "-d", "-t", "queued:2", "-n", "d"])?;

    harness.success(["run-shell", "-C", "swap-window -sd -t queued:1"])?;
    assert_swap_window_order(&harness, "queued", "0:zero\n1:d\n2:one\n")
}

#[test]
fn startup_config_accepts_clustered_swap_window_flags() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("startup-swap-window-cluster")?;
    let config = harness.tmpdir().join("clustered-swap-window.conf");
    fs::write(
        &config,
        "new-session -d -s configured -n zero\n\
         new-window -d -t configured:1 -n one\n\
         swap-window -ds configured:0 -tconfigured:1\n",
    )?;

    harness.success([
        OsStr::new("-f"),
        config.as_os_str(),
        OsStr::new("new-session"),
        OsStr::new("-d"),
        OsStr::new("-s"),
        OsStr::new("requested"),
    ])?;
    assert_swap_window_order(&harness, "configured", "0:one\n1:zero\n")
}

#[test]
fn source_file_accepts_lf_crlf_unicode_paths_and_parse_only_has_no_side_effects(
) -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("source-config-matrix")?;
    harness.success(["new-session", "-d", "-s", "cfg"])?;

    let config_dir = harness.tmpdir().join("config corpus café");
    fs::create_dir_all(&config_dir)?;

    let lf_config = config_dir.join("line endings lf.conf");
    fs::write(
        &lf_config,
        "set-option -g status off\n\
         set-option -g @rmux-config-line-ending lf\n\
         set-environment -g RMUX_CONFIG_MATRIX lf\n\
         if-shell -F '1' 'set-option -g @rmux-if-shell yes' 'set-option -g @rmux-if-shell no'\n",
    )?;
    harness.success([OsStr::new("source-file"), lf_config.as_os_str()])?;
    assert_option(&harness, "status", "off")?;
    assert_option(&harness, "@rmux-config-line-ending", "lf")?;
    assert_option(&harness, "@rmux-if-shell", "yes")?;
    assert_environment(&harness, "RMUX_CONFIG_MATRIX=lf")?;

    let crlf_config = config_dir.join("line endings crlf.conf");
    fs::write(
        &crlf_config,
        "set-option -g status on\r\n\
         set-option -g @rmux-config-line-ending crlf\r\n\
         set-environment -g RMUX_CONFIG_MATRIX crlf\r\n",
    )?;
    harness.success([OsStr::new("source-file"), crlf_config.as_os_str()])?;
    assert_option(&harness, "status", "on")?;
    assert_option(&harness, "@rmux-config-line-ending", "crlf")?;
    assert_environment(&harness, "RMUX_CONFIG_MATRIX=crlf")?;

    let parse_only_config = config_dir.join("parse only.conf");
    fs::write(
        &parse_only_config,
        "set-option -g status off\n\
         set-option -g @rmux-config-line-ending parse-only\n\
         set-environment -g RMUX_CONFIG_MATRIX parse-only\n",
    )?;
    harness.success([
        OsStr::new("source-file"),
        OsStr::new("-n"),
        OsStr::new("-v"),
        parse_only_config.as_os_str(),
    ])?;

    assert_option(&harness, "status", "on")?;
    assert_option(&harness, "@rmux-config-line-ending", "crlf")?;
    assert_environment(&harness, "RMUX_CONFIG_MATRIX=crlf")?;

    Ok(())
}

#[test]
fn source_file_missing_quiet_include_is_recoverable_and_silent() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("source-config-quiet")?;
    harness.success(["new-session", "-d", "-s", "cfg"])?;

    let config = harness.tmpdir().join("optional-local.conf");
    fs::write(
        &config,
        "source-file -q definitely-missing-local.conf\n\
         set-option -g @rmux-quiet-include-after yes\n",
    )?;

    harness.success([OsStr::new("source-file"), config.as_os_str()])?;
    assert_option(&harness, "@rmux-quiet-include-after", "yes")?;

    let messages = harness.stdout(["show-messages"])?;
    assert!(
        !messages.contains("config error") && !messages.contains("definitely-missing-local.conf"),
        "quiet missing include produced noisy diagnostics: {messages:?}"
    );

    Ok(())
}

#[test]
fn source_file_unquoted_hash_format_comments_match_tmux_boolean_toggle(
) -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("source-config-hash-comment")?;
    harness.success(["new-session", "-d", "-s", "cfg"])?;
    harness.success(["set-option", "-g", "extended-keys", "off"])?;
    assert_option(&harness, "extended-keys", "off")?;

    let config = harness.tmpdir().join("gpakosz-extended-keys-min.conf");
    fs::write(
        &config,
        "%if #{>=:#{version},3.2}\n\
         set-option -g extended-keys #{?#{||:#{m/ri:mintty|iTerm,#{TERM_PROGRAM}},#{!=:#{XTERM_VERSION},}},on,off}\n\
         %endif\n",
    )?;

    harness.success([OsStr::new("source-file"), config.as_os_str()])?;
    assert_option(&harness, "extended-keys", "on")?;

    Ok(())
}

#[test]
fn source_file_set_option_scope_and_scalar_append_match_tmux37() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("source-config-options-tmux37")?;
    harness.success(["new-session", "-d", "-s", "cfg"])?;

    let config = harness.tmpdir().join("options-tmux37.conf");
    fs::write(
        &config,
        "set-option -sw -t cfg status off\n\
         set-option -ga history-limit 5\n\
         set-option -g set-clipboard external\n\
         set-option -g set-clipboard\n",
    )?;

    harness.success([OsStr::new("source-file"), config.as_os_str()])?;
    let window_status = harness.stdout(["show-options", "-wv", "-t", "cfg", "status"])?;
    assert_eq!(window_status.trim(), "off");
    assert_option(&harness, "history-limit", "5")?;
    assert_option(&harness, "set-clipboard", "off")?;

    Ok(())
}

#[test]
#[cfg(unix)]
fn source_file_uses_current_targets_for_tmux_commands_without_t() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("source-config-implicit-targets")?;
    harness.success([
        "new-session",
        "-d",
        "-s",
        "cfg",
        "sh",
        "-c",
        "printf hi; sleep 30",
    ])?;

    let execute_config = harness.tmpdir().join("implicit-execute.conf");
    fs::write(
        &execute_config,
        "capture-pane -p\n\
         list-windows -F '#{window_index}'\n\
         pipe-pane -o cat\n\
         resize-window -A\n\
         set-option -g @rmux-implicit-target-after yes\n",
    )?;
    let output = harness.run([OsStr::new("source-file"), execute_config.as_os_str()])?;
    assert_success(&output)?;
    assert_option(&harness, "@rmux-implicit-target-after", "yes")?;

    let parse_only_config = harness.tmpdir().join("implicit-bindings.conf");
    fs::write(
        &parse_only_config,
        "bind-key C-u capture-pane -J\n\
         bind-key C-w list-windows\n\
         bind-key C-p pipe-pane cat\n\
         bind-key C-k kill-session\n\
         bind-key C-r resize-window -A\n",
    )?;
    harness.success([
        OsStr::new("source-file"),
        OsStr::new("-n"),
        OsStr::new("-v"),
        parse_only_config.as_os_str(),
    ])?;

    Ok(())
}

#[test]
#[cfg(windows)]
fn source_file_uses_current_targets_for_windows_safe_tmux_commands_without_t(
) -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("source-config-implicit-targets-win")?;
    harness.success([
        "new-session",
        "-d",
        "-s",
        "cfg",
        "cmd.exe",
        "/Q",
        "/K",
        "echo hi",
    ])?;

    let execute_config = harness.tmpdir().join("implicit-execute.conf");
    fs::write(
        &execute_config,
        "capture-pane -p\n\
         list-windows -F '#{window_index}'\n\
         resize-window -A\n\
         set-option -g @rmux-implicit-target-after yes\n",
    )?;
    let output = harness.run([OsStr::new("source-file"), execute_config.as_os_str()])?;
    assert_success(&output)?;
    assert_option(&harness, "@rmux-implicit-target-after", "yes")?;

    let parse_only_config = harness.tmpdir().join("implicit-bindings.conf");
    fs::write(
        &parse_only_config,
        "bind-key C-u capture-pane -J\n\
         bind-key C-w list-windows\n\
         bind-key C-p pipe-pane cat\n\
         bind-key C-k kill-session\n\
         bind-key C-r resize-window -A\n",
    )?;
    harness.success([
        OsStr::new("source-file"),
        OsStr::new("-n"),
        OsStr::new("-v"),
        parse_only_config.as_os_str(),
    ])?;

    Ok(())
}

#[test]
#[cfg(windows)]
fn source_file_expands_tilde_from_userprofile_when_home_is_absent() -> Result<(), Box<dyn Error>> {
    let harness = CrossPlatformHarness::new("source-config-win-userprofile")?;
    let user_profile = harness.tmpdir().join("windows-profile");
    fs::create_dir_all(&user_profile)?;

    let mut start = harness.command(["new-session", "-d", "-s", "cfg"]);
    start.env_remove("HOME");
    start.env("USERPROFILE", &user_profile);
    let output = start.output()?;
    assert_success(&output)?;

    let config = harness.tmpdir().join("windows-userprofile-tilde.conf");
    fs::write(
        &config,
        "set-environment -g RMUX_WINDOWS_TILDE ~/config-marker\n",
    )?;
    harness.success([OsStr::new("source-file"), config.as_os_str()])?;

    let expected = format!(
        "RMUX_WINDOWS_TILDE={}/config-marker",
        user_profile.display()
    );
    assert_environment(&harness, &expected)
}

#[test]
#[cfg(windows)]
fn startup_config_unquoted_windows_powershell_default_shell_starts_powershell(
) -> Result<(), Box<dyn Error>> {
    let powershell = r"C:\Windows\System32\WindowsPowerShell\v1.0\powershell.exe";
    if !std::path::Path::new(powershell).is_file() {
        eprintln!("skipping WindowsPowerShell default-shell probe: {powershell} missing");
        return Ok(());
    }

    let harness = CrossPlatformHarness::new("source-config-win-powershell")?;
    let config = harness
        .tmpdir()
        .join("windows-powershell-default-shell.conf");
    fs::write(&config, format!("set -g default-shell {powershell}\r\n"))?;

    harness.success([
        OsStr::new("-f"),
        config.as_os_str(),
        OsStr::new("new-session"),
        OsStr::new("-d"),
        OsStr::new("-s"),
        OsStr::new("psdefault"),
    ])?;
    std::thread::sleep(std::time::Duration::from_millis(1_800));
    harness.success(["has-session", "-t", "psdefault"])?;
    assert_option(&harness, "default-shell", powershell)?;
    harness.success([
        "send-keys",
        "-t",
        "psdefault:0.0",
        "Write-Output ('RMUX_' + 'PS51_READY')",
        "Enter",
    ])?;
    wait_for_capture_contains(
        &harness,
        "psdefault:0.0",
        "RMUX_PS51_READY",
        std::time::Duration::from_secs(8),
    )?;

    Ok(())
}

#[test]
#[cfg(windows)]
fn windows_source_startup_and_queued_commands_preserve_windows_paths() -> Result<(), Box<dyn Error>>
{
    let source_harness = CrossPlatformHarness::new("source-config-win-unc")?;
    source_harness.success(["new-session", "-d", "-s", "cfg"])?;
    let source = source_harness.tmpdir().join("windows-unc-source.conf");
    fs::write(
        &source,
        r#"set-environment -g RMUX_UNC_SOURCE "\\server\share"
if-shell -F 1 { set-environment -g RMUX_UNC_QUEUE "\\server\share" }
set-environment -g RMUX_DEVICE_SOURCE "\\?\C:\rmux\config"
set-environment -g RMUX_RELATIVE_SOURCE ".\scripts\tool.ps1"
if-shell -F 1 { set-environment -g RMUX_RELATIVE_QUEUE "..\scripts\tool.ps1" }
set-environment -g RMUX_EMBEDDED_SOURCE "--script=.\scripts\tool.ps1"
"#,
    )?;
    source_harness.success([OsStr::new("source-file"), source.as_os_str()])?;
    assert_environment(&source_harness, r"RMUX_UNC_SOURCE=\\server\share")?;
    assert_environment(&source_harness, r"RMUX_UNC_QUEUE=\\server\share")?;
    assert_environment(&source_harness, r"RMUX_DEVICE_SOURCE=\\?\C:\rmux\config")?;
    assert_environment(&source_harness, r"RMUX_RELATIVE_SOURCE=.\scripts\tool.ps1")?;
    assert_environment(&source_harness, r"RMUX_RELATIVE_QUEUE=..\scripts\tool.ps1")?;
    assert_environment(
        &source_harness,
        r"RMUX_EMBEDDED_SOURCE=--script=.\scripts\tool.ps1",
    )?;

    let startup_harness = CrossPlatformHarness::new("startup-config-win-unc")?;
    let startup = startup_harness.tmpdir().join("windows-unc-startup.conf");
    fs::write(
        &startup,
        r#"set-environment -g RMUX_UNC_STARTUP "\\server\share"
set-environment -g RMUX_RELATIVE_STARTUP ".\scripts\tool.ps1"
"#,
    )?;
    startup_harness.success([
        OsStr::new("-f"),
        startup.as_os_str(),
        OsStr::new("new-session"),
        OsStr::new("-d"),
        OsStr::new("-s"),
        OsStr::new("startup"),
    ])?;
    assert_environment(&startup_harness, r"RMUX_UNC_STARTUP=\\server\share")?;
    assert_environment(
        &startup_harness,
        r"RMUX_RELATIVE_STARTUP=.\scripts\tool.ps1",
    )?;

    startup_harness.success([
        "set-environment",
        "-g",
        "RMUX_RELATIVE_DIRECT",
        r".\scripts\tool.ps1",
    ])?;
    assert_environment(&startup_harness, r"RMUX_RELATIVE_DIRECT=.\scripts\tool.ps1")?;

    Ok(())
}

fn assert_option(
    harness: &CrossPlatformHarness,
    option_name: &str,
    expected: &str,
) -> Result<(), Box<dyn Error>> {
    let actual = harness.stdout(["show-options", "-gqv", option_name])?;
    assert_eq!(actual.trim(), expected, "unexpected option {option_name}");
    Ok(())
}

fn create_swap_window_fixture(
    harness: &CrossPlatformHarness,
    session: &str,
) -> Result<(), Box<dyn Error>> {
    harness.success(["new-session", "-d", "-s", session, "-n", "zero"])?;
    let target = format!("{session}:1");
    harness.success(["new-window", "-d", "-t", target.as_str(), "-n", "one"])
}

fn assert_swap_window_order(
    harness: &CrossPlatformHarness,
    session: &str,
    expected: &str,
) -> Result<(), Box<dyn Error>> {
    let actual = harness.stdout([
        "list-windows",
        "-t",
        session,
        "-F",
        "#{window_index}:#{window_name}",
    ])?;
    assert_eq!(actual, expected);
    Ok(())
}

fn assert_environment(
    harness: &CrossPlatformHarness,
    expected_line: &str,
) -> Result<(), Box<dyn Error>> {
    let name = expected_line
        .split_once('=')
        .map(|(name, _)| name)
        .ok_or("expected NAME=value environment assertion")?;
    let output = harness.run(["show-environment", "-g", name])?;
    assert_success(&output)?;
    assert_eq!(String::from_utf8(output.stdout)?.trim(), expected_line);
    Ok(())
}

fn assert_environment_missing(
    harness: &CrossPlatformHarness,
    name: &str,
) -> Result<(), Box<dyn Error>> {
    let output = harness.run(["show-environment", "-g", name])?;
    assert!(
        !output.status.success(),
        "environment variable {name} unexpectedly remained: {:?}",
        String::from_utf8_lossy(&output.stdout)
    );
    Ok(())
}

fn assert_binding_contains(
    harness: &CrossPlatformHarness,
    key: &str,
    expected: &str,
) -> Result<(), Box<dyn Error>> {
    let output = harness.stdout(["list-keys", "-T", "root"])?;
    assert!(
        output.contains(expected),
        "binding {key} did not contain {expected:?}: {output:?}"
    );
    Ok(())
}

fn assert_binding_absent(
    harness: &CrossPlatformHarness,
    key: &str,
    unexpected: &str,
) -> Result<(), Box<dyn Error>> {
    let output = harness.stdout(["list-keys", "-T", "root"])?;
    assert!(
        !output.contains(unexpected),
        "parse-only unexpectedly installed binding {key}: {output:?}"
    );
    Ok(())
}

#[cfg(windows)]
fn wait_for_capture_contains(
    harness: &CrossPlatformHarness,
    target: &str,
    needle: &str,
    timeout: std::time::Duration,
) -> Result<String, Box<dyn Error>> {
    let deadline = std::time::Instant::now() + timeout;
    let mut last_capture = String::new();

    while std::time::Instant::now() < deadline {
        last_capture = harness.stdout(["capture-pane", "-p", "-t", target])?;
        if last_capture.contains(needle) {
            return Ok(last_capture);
        }
        std::thread::sleep(std::time::Duration::from_millis(200));
    }

    Err(
        format!("timed out waiting for {needle:?} in {target}; last capture:\n{last_capture}")
            .into(),
    )
}
