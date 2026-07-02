mod common_cross;

use std::error::Error;
use std::ffi::OsStr;
use std::fs;

use common_cross::{assert_success, CrossPlatformHarness};

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

fn assert_option(
    harness: &CrossPlatformHarness,
    option_name: &str,
    expected: &str,
) -> Result<(), Box<dyn Error>> {
    let actual = harness.stdout(["show-options", "-gqv", option_name])?;
    assert_eq!(actual.trim(), expected, "unexpected option {option_name}");
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
