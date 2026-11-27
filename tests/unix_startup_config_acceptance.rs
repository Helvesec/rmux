#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
fn startup_fallback_symlink_applies_recoverable_tmux_config_to_end() -> Result<(), Box<dyn Error>> {
    let harness = StartupHarness::new("startup-config")?;
    let real_config_dir = harness.tmpdir().join("real");
    let xdg_tmux_dir = harness.xdg().join("tmux");
    fs::create_dir_all(&real_config_dir)?;
    fs::create_dir_all(&xdg_tmux_dir)?;
    let real_config = real_config_dir.join("tmux.conf");
    fs::write(
        &real_config,
        "set -q -g status-utf8 on\n\
         setw -q -g utf8 on\n\
         source-file -q \"$HOME/.tmux.conf.local\"\n\
         set -g @rmux-startup-loaded yes\n",
    )?;
    std::os::unix::fs::symlink(&real_config, xdg_tmux_dir.join("tmux.conf"))?;

    harness.success(["new-session", "-d", "-s", "s"])?;

    let marker = harness.stdout(["show-options", "-gqv", "@rmux-startup-loaded"])?;
    assert_eq!(
        marker.trim(),
        "yes",
        "startup fallback did not apply commands after recoverable config lines"
    );

    let messages = harness.stdout(["show-messages"])?;
    assert!(
        !messages.contains("config error"),
        "recoverable quiet startup config emitted noisy config errors: {messages:?}"
    );

    Ok(())
}

#[test]
fn startup_fallback_quiets_optional_gpakosz_local_source() -> Result<(), Box<dyn Error>> {
    let harness = StartupHarness::new("startup-gpakosz-local")?;
    let real_config_dir = harness.tmpdir().join("real");
    let xdg_tmux_dir = harness.xdg().join("tmux");
    fs::create_dir_all(&real_config_dir)?;
    fs::create_dir_all(&xdg_tmux_dir)?;
    let real_config = real_config_dir.join("tmux.conf");
    let missing_local = xdg_tmux_dir.join("tmux.conf.local");
    fs::write(
        &real_config,
        format!(
            "set-environment -g TMUX_PROGRAM '{}'\n\
             set-environment -g TMUX_CONF_LOCAL '{}'\n\
             set-environment -g TERM_PROGRAM iTerm.app\n\
             set -g extended-keys #{{?#{{||:#{{m/ri:mintty|iTerm,#{{TERM_PROGRAM}}}},#{{!=:#{{XTERM_VERSION}},}}}},on,off}}\n\
             run '\"$TMUX_PROGRAM\" -S #{{socket_path}} source \"$TMUX_CONF_LOCAL\"'\n\
             set -g @rmux-gpakosz-after-local yes\n",
            shell_single_quote(rmux_binary()),
            shell_single_quote(&missing_local)
        ),
    )?;
    std::os::unix::fs::symlink(&real_config, xdg_tmux_dir.join("tmux.conf"))?;

    harness.success(["new-session", "-d", "-s", "s"])?;

    let extended_keys = harness.stdout(["show-options", "-gqv", "extended-keys"])?;
    assert_eq!(
        extended_keys.trim(),
        "on",
        "gpakosz-style extended-keys expression was not applied"
    );
    let marker = harness.stdout(["show-options", "-gqv", "@rmux-gpakosz-after-local"])?;
    assert_eq!(
        marker.trim(),
        "yes",
        "startup fallback did not continue after the optional local source"
    );
    let messages = harness.stdout(["show-messages"])?;
    assert!(
        !messages.contains("tmux.conf.local") && !messages.contains("No such file or directory"),
        "optional gpakosz local source emitted noisy startup messages: {messages:?}"
    );

    Ok(())
}

#[test]
fn startup_fallback_deduplicates_home_and_xdg_symlinked_tmux_conf() -> Result<(), Box<dyn Error>> {
    let harness = StartupHarness::new("startup-gpakosz-dedupe")?;
    let xdg_tmux_dir = harness.xdg().join("tmux");
    fs::create_dir_all(&xdg_tmux_dir)?;
    let home_config = harness.home().join(".tmux.conf");
    let marker = harness.tmpdir().join("fallback-load-count.txt");
    fs::write(
        &home_config,
        format!(
            "set-environment -g TERM_PROGRAM iTerm.app\n\
             set -g extended-keys #{{?#{{||:#{{m/ri:mintty|iTerm,#{{TERM_PROGRAM}}}},#{{!=:#{{XTERM_VERSION}},}}}},on,off}}\n\
             run-shell \"printf x >> {}\"\n",
            shell_single_quote(&marker)
        ),
    )?;
    std::os::unix::fs::symlink(&home_config, xdg_tmux_dir.join("tmux.conf"))?;

    harness.success(["new-session", "-d", "-s", "s"])?;

    let extended_keys = harness.stdout(["show-options", "-gqv", "extended-keys"])?;
    assert_eq!(
        extended_keys.trim(),
        "on",
        "deduped gpakosz-style config did not apply extended-keys"
    );
    let load_count = fs::read_to_string(&marker)?;
    assert_eq!(
        load_count, "x",
        "startup fallback must source a home/XDG symlinked tmux config exactly once"
    );
    let messages = harness.stdout(["show-messages"])?;
    assert!(
        !messages.contains("unmatched }") && !messages.contains("config error"),
        "duplicate fallback load leaked startup parse/config errors: {messages:?}"
    );

    Ok(())
}

#[test]
fn startup_fallback_deduplicates_home_and_xdg_hardlinked_tmux_conf() -> Result<(), Box<dyn Error>> {
    let harness = StartupHarness::new("startup-gpakosz-hardlink-dedupe")?;
    let xdg_tmux_dir = harness.xdg().join("tmux");
    fs::create_dir_all(&xdg_tmux_dir)?;
    let home_config = harness.home().join(".tmux.conf");
    let xdg_config = xdg_tmux_dir.join("tmux.conf");
    let marker = harness.tmpdir().join("fallback-hardlink-load-count.txt");
    fs::write(
        &home_config,
        format!(
            "set-environment -g TERM_PROGRAM iTerm.app\n\
             set -g extended-keys #{{?#{{||:#{{m/ri:mintty|iTerm,#{{TERM_PROGRAM}}}},#{{!=:#{{XTERM_VERSION}},}}}},on,off}}\n\
             run-shell \"printf x >> {}\"\n",
            shell_single_quote(&marker)
        ),
    )?;
    fs::hard_link(&home_config, &xdg_config)?;

    harness.success(["new-session", "-d", "-s", "s"])?;

    let extended_keys = harness.stdout(["show-options", "-gqv", "extended-keys"])?;
    assert_eq!(
        extended_keys.trim(),
        "on",
        "hardlink-deduped gpakosz-style config did not apply extended-keys"
    );
    let load_count = fs::read_to_string(&marker)?;
    assert_eq!(
        load_count, "x",
        "startup fallback must source a home/XDG hardlinked tmux config exactly once"
    );
    let messages = harness.stdout(["show-messages"])?;
    assert!(
        !messages.contains("unmatched }") && !messages.contains("config error"),
        "duplicate hardlink fallback load leaked startup parse/config errors: {messages:?}"
    );

    Ok(())
}

#[test]
fn startup_direct_group_continues_after_renaming_current_session() -> Result<(), Box<dyn Error>> {
    let harness = StartupHarness::new("startup-direct-rename-context")?;
    let config = harness.tmpdir().join("startup-direct-rename.conf");
    fs::write(
        &config,
        "new-session -d -s alpha \"sleep 30\"\n\
         rename-session -t alpha beta ; new-window -d -n after-direct-rename \
         \"sleep 30\" ; set-option -g @tail U01\n",
    )?;

    harness.success([
        "-f".into(),
        config.into_os_string(),
        "new-session".into(),
        "-d".into(),
        "-s".into(),
        "keeper".into(),
        "sleep 30".into(),
    ])?;

    assert_startup_rename_continuation(&harness, "after-direct-rename", "U01")
}

#[test]
fn startup_non_nested_group_continues_after_renaming_current_session() -> Result<(), Box<dyn Error>>
{
    let harness = StartupHarness::new("startup-non-nested-rename-context")?;
    let config = harness.tmpdir().join("startup-non-nested-rename.conf");
    fs::write(
        &config,
        "new-session -d -s alpha \"sleep 30\"\n\
         rename-session -t alpha beta\n\
         display-message -p \"#{session_id}|#{session_name}\"\n\
         new-window -d -n after-non-nested-rename \"sleep 30\" ; \
         set-option -g @tail U03\n",
    )?;

    harness.success([
        "-f".into(),
        config.into_os_string(),
        "new-session".into(),
        "-d".into(),
        "-s".into(),
        "keeper".into(),
        "sleep 30".into(),
    ])?;

    assert_startup_rename_continuation(&harness, "after-non-nested-rename", "U03")
}

#[test]
fn startup_group_follows_successful_new_session_after_rename() -> Result<(), Box<dyn Error>> {
    let harness = StartupHarness::new("startup-new-session-transition-group")?;
    let config = harness
        .tmpdir()
        .join("startup-new-session-transition-group.conf");
    fs::write(
        &config,
        "new-session -d -s alpha \"sleep 30\"\n\
         rename-session -t alpha beta ; \
         new-session -d -s newcomer \"sleep 30\" ; \
         new-window -d -n transition-first \"sleep 30\" ; \
         new-window -d -n transition-repeat \"sleep 30\"\n",
    )?;

    start_with_config_and_keeper(&harness, &config)?;
    assert_successful_new_session_transition(&harness)
}

#[test]
fn startup_lines_follow_successful_new_session_after_rename() -> Result<(), Box<dyn Error>> {
    let harness = StartupHarness::new("startup-new-session-transition-lines")?;
    let config = harness
        .tmpdir()
        .join("startup-new-session-transition-lines.conf");
    fs::write(
        &config,
        "new-session -d -s alpha \"sleep 30\"\n\
         rename-session -t alpha beta\n\
         new-session -d -s newcomer \"sleep 30\"\n\
         new-window -d -n transition-first \"sleep 30\"\n\
         new-window -d -n transition-repeat \"sleep 30\"\n",
    )?;

    start_with_config_and_keeper(&harness, &config)?;
    assert_successful_new_session_transition(&harness)
}

#[test]
fn startup_run_shell_follows_successful_new_session_after_rename() -> Result<(), Box<dyn Error>> {
    let harness = StartupHarness::new("startup-new-session-transition-run-shell")?;
    let config = harness
        .tmpdir()
        .join("startup-new-session-transition-run-shell.conf");
    fs::write(
        &config,
        "new-session -d -s alpha \"sleep 30\"\n\
         run-shell -C \"rename-session -t alpha beta ; \
         new-session -d -s newcomer sleep 30 ; \
         new-window -d -n transition-first sleep 30 ; \
         new-window -d -n transition-repeat sleep 30\"\n",
    )?;

    start_with_config_and_keeper(&harness, &config)?;
    assert_successful_new_session_transition(&harness)
}

#[test]
fn startup_group_attach_if_exists_reuse_keeps_renamed_identity() -> Result<(), Box<dyn Error>> {
    let harness = StartupHarness::new("startup-existing-session-reuse-group")?;
    let config = harness
        .tmpdir()
        .join("startup-existing-session-reuse-group.conf");
    fs::write(
        &config,
        "new-session -d -s occupied \"sleep 30\"\n\
         new-session -d -s alpha \"sleep 30\"\n\
         rename-session -t alpha beta ; \
         new-session -A -s occupied ; \
         new-window -d -n reuse-first \"sleep 30\" ; \
         new-window -d -n reuse-repeat \"sleep 30\"\n",
    )?;

    start_with_config_and_keeper(&harness, &config)?;
    assert_existing_session_reuse_keeps_renamed_identity(&harness)
}

#[test]
fn startup_lines_detached_attach_if_exists_reuse_keeps_renamed_identity(
) -> Result<(), Box<dyn Error>> {
    let harness = StartupHarness::new("startup-existing-session-reuse-lines")?;
    let config = harness
        .tmpdir()
        .join("startup-existing-session-reuse-lines.conf");
    fs::write(
        &config,
        "new-session -d -s occupied \"sleep 30\"\n\
         new-session -d -s alpha \"sleep 30\"\n\
         rename-session -t alpha beta\n\
         new-session -Ad -s occupied\n\
         new-window -d -n reuse-first \"sleep 30\"\n\
         new-window -d -n reuse-repeat \"sleep 30\"\n",
    )?;

    start_with_config_and_keeper(&harness, &config)?;
    assert_existing_session_reuse_keeps_renamed_identity(&harness)
}

#[test]
fn startup_run_shell_detached_attach_if_exists_reuse_keeps_renamed_identity(
) -> Result<(), Box<dyn Error>> {
    let harness = StartupHarness::new("startup-existing-session-reuse-run-shell")?;
    let config = harness
        .tmpdir()
        .join("startup-existing-session-reuse-run-shell.conf");
    fs::write(
        &config,
        "new-session -d -s occupied \"sleep 30\"\n\
         new-session -d -s alpha \"sleep 30\"\n\
         run-shell -C \"rename-session -t alpha beta ; \
         new-session -Ad -s occupied ; \
         new-window -d -n reuse-first sleep 30 ; \
         new-window -d -n reuse-repeat sleep 30\"\n",
    )?;

    start_with_config_and_keeper(&harness, &config)?;
    assert_existing_session_reuse_keeps_renamed_identity(&harness)
}

#[test]
fn startup_nested_source_keeps_renamed_identity_after_new_session() -> Result<(), Box<dyn Error>> {
    let harness = StartupHarness::new("startup-nested-source-transition-boundary")?;
    let nested = harness.tmpdir().join("nested-transition.conf");
    fs::write(
        &nested,
        "rename-session -t alpha beta ; \
         new-session -d -s newcomer \"sleep 30\" ; \
         new-window -d -n transition-first \"sleep 30\" ; \
         new-window -d -n transition-repeat \"sleep 30\"\n",
    )?;
    let config = harness.tmpdir().join("startup-nested-source.conf");
    fs::write(
        &config,
        format!(
            "new-session -d -s alpha \"sleep 30\"\n\
             source-file '{}'\n",
            shell_single_quote(&nested)
        ),
    )?;

    start_with_config_and_keeper(&harness, &config)?;
    assert_source_keeps_renamed_identity(&harness, true)
}

#[test]
fn ordinary_source_keeps_renamed_identity_after_new_session() -> Result<(), Box<dyn Error>> {
    let harness = StartupHarness::new("ordinary-source-transition-boundary")?;
    harness.success(["new-session", "-d", "-s", "alpha", "sleep 30"])?;
    let config = harness.tmpdir().join("ordinary-source-transition.conf");
    fs::write(
        &config,
        "rename-session -t alpha beta ; \
         new-session -d -s newcomer \"sleep 30\" ; \
         new-window -d -n transition-first \"sleep 30\" ; \
         new-window -d -n transition-repeat \"sleep 30\"\n",
    )?;

    harness.success(["source-file".into(), config.into_os_string()])?;
    assert_source_keeps_renamed_identity(&harness, false)
}

#[test]
fn failed_startup_new_session_does_not_supersede_rename_marker() -> Result<(), Box<dyn Error>> {
    let harness = StartupHarness::new("startup-failed-new-session-transition")?;
    let config = harness
        .tmpdir()
        .join("startup-failed-new-session-transition.conf");
    fs::write(
        &config,
        "new-session -d -s alpha \"sleep 30\"\n\
         rename-session -t alpha beta\n\
         new-session -d -s beta \"sleep 30\"\n\
         new-window -d -n after-failed-transition \"sleep 30\"\n",
    )?;

    start_with_config_and_keeper(&harness, &config)?;
    let beta_windows = harness.stdout(["list-windows", "-t", "beta", "-F", "#{window_name}"])?;
    assert_eq!(
        beta_windows
            .lines()
            .filter(|name| *name == "after-failed-transition")
            .count(),
        1,
        "a failed new-session must leave the rename marker current: {beta_windows:?}"
    );
    Ok(())
}

fn start_with_config_and_keeper(
    harness: &StartupHarness,
    config: &Path,
) -> Result<(), Box<dyn Error>> {
    harness.success([
        "-f".into(),
        config.as_os_str().to_owned(),
        "new-session".into(),
        "-d".into(),
        "-s".into(),
        "keeper".into(),
        "sleep 30".into(),
    ])
}

fn assert_successful_new_session_transition(
    harness: &StartupHarness,
) -> Result<(), Box<dyn Error>> {
    let sessions = harness.stdout([
        "list-sessions",
        "-F",
        "#{session_id}|#{session_name}|#{session_windows}",
    ])?;
    assert!(
        sessions.lines().any(|line| line == "$0|beta|1"),
        "the renamed source identity must remain unchanged: {sessions:?}"
    );
    assert!(
        sessions.lines().any(|line| line == "$1|newcomer|3"),
        "the successful new-session must become current for both following windows: {sessions:?}"
    );
    assert!(
        sessions.lines().any(|line| line == "$2|keeper|1"),
        "startup validation must not consume runtime allocators: {sessions:?}"
    );
    for window in ["transition-first", "transition-repeat"] {
        let windows = harness.stdout(["list-windows", "-t", "newcomer", "-F", "#{window_name}"])?;
        assert_eq!(
            windows.lines().filter(|name| *name == window).count(),
            1,
            "{window} must execute once on the explicit new-session transition: {windows:?}"
        );
    }
    Ok(())
}

fn assert_existing_session_reuse_keeps_renamed_identity(
    harness: &StartupHarness,
) -> Result<(), Box<dyn Error>> {
    let sessions = harness.stdout([
        "list-sessions",
        "-F",
        "#{session_id}|#{session_name}|#{session_windows}",
    ])?;
    assert!(
        sessions.lines().any(|line| line == "$0|occupied|1"),
        "reusing an existing session must not receive following windows: {sessions:?}"
    );
    assert!(
        sessions.lines().any(|line| line == "$1|beta|3"),
        "following windows must retain the exact renamed identity: {sessions:?}"
    );
    assert!(
        sessions.lines().any(|line| line == "$2|keeper|1"),
        "startup validation must not consume runtime allocators: {sessions:?}"
    );
    let occupied_windows =
        harness.stdout(["list-windows", "-t", "occupied", "-F", "#{window_name}"])?;
    let renamed_windows = harness.stdout(["list-windows", "-t", "beta", "-F", "#{window_name}"])?;
    for window in ["reuse-first", "reuse-repeat"] {
        assert_eq!(
            occupied_windows
                .lines()
                .filter(|name| *name == window)
                .count(),
            0,
            "{window} must not mutate the reused existing session: {occupied_windows:?}"
        );
        assert_eq!(
            renamed_windows
                .lines()
                .filter(|name| *name == window)
                .count(),
            1,
            "{window} must execute once on the renamed identity: {renamed_windows:?}"
        );
    }
    Ok(())
}

fn assert_source_keeps_renamed_identity(
    harness: &StartupHarness,
    expect_keeper: bool,
) -> Result<(), Box<dyn Error>> {
    let sessions = harness.stdout([
        "list-sessions",
        "-F",
        "#{session_id}|#{session_name}|#{session_windows}",
    ])?;
    assert!(
        sessions.lines().any(|line| line == "$0|beta|3"),
        "source-owned work must remain on the renamed identity: {sessions:?}"
    );
    assert!(
        sessions.lines().any(|line| line == "$1|newcomer|1"),
        "source new-session must still be created exactly once: {sessions:?}"
    );
    assert_eq!(
        sessions.lines().any(|line| line == "$2|keeper|1"),
        expect_keeper,
        "unexpected startup keeper inventory: {sessions:?}"
    );
    Ok(())
}

fn assert_startup_rename_continuation(
    harness: &StartupHarness,
    expected_window: &str,
    expected_tail: &str,
) -> Result<(), Box<dyn Error>> {
    let sessions = harness.stdout([
        "list-sessions",
        "-F",
        "#{session_id}|#{session_name}|#{session_windows}|#{@tail}",
    ])?;
    assert!(
        sessions
            .lines()
            .any(|line| { line == format!("$0|beta|2|{expected_tail}") }),
        "renamed stable session identity or final marker was lost: {sessions:?}"
    );
    assert!(
        sessions
            .lines()
            .any(|line| line == format!("$1|keeper|1|{expected_tail}")),
        "startup command session or global final marker was lost: {sessions:?}"
    );

    let windows = harness.stdout(["list-windows", "-t", "beta", "-F", "#{window_name}"])?;
    assert_eq!(
        windows
            .lines()
            .filter(|window| *window == expected_window)
            .count(),
        1,
        "the post-rename command must execute exactly once: {windows:?}"
    );

    let messages = harness.stdout(["show-messages"])?;
    assert!(
        !messages.contains("requires -t target"),
        "startup queue lost its current target after rename: {messages:?}"
    );
    Ok(())
}

struct StartupHarness {
    label: String,
    tmpdir: PathBuf,
    home: PathBuf,
    xdg: PathBuf,
}

impl StartupHarness {
    fn new(label: &str) -> Result<Self, Box<dyn Error>> {
        let unique = unique_id(label);
        let tmpdir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(&unique);
        let _ = fs::remove_dir_all(&tmpdir);
        let home = tmpdir.join("home");
        let xdg = tmpdir.join("xdg");
        fs::create_dir_all(&home)?;
        fs::create_dir_all(&xdg)?;
        let harness = Self {
            label: unique,
            tmpdir,
            home,
            xdg,
        };
        let _ = harness.run(["kill-server"]);
        Ok(harness)
    }

    fn tmpdir(&self) -> &Path {
        &self.tmpdir
    }

    fn home(&self) -> &Path {
        &self.home
    }

    fn xdg(&self) -> &Path {
        &self.xdg
    }

    fn success<I, S>(&self, args: I) -> Result<(), Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let output = self.run(args)?;
        assert_success(&output)
    }

    fn stdout<I, S>(&self, args: I) -> Result<String, Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        let output = self.run(args)?;
        assert_success(&output)?;
        Ok(String::from_utf8(output.stdout)?)
    }

    fn run<I, S>(&self, args: I) -> Result<Output, Box<dyn Error>>
    where
        I: IntoIterator<Item = S>,
        S: AsRef<std::ffi::OsStr>,
    {
        Ok(Command::new(rmux_binary())
            .env("HOME", &self.home)
            .env("XDG_CONFIG_HOME", &self.xdg)
            .arg("-L")
            .arg(&self.label)
            .args(args)
            .output()?)
    }
}

impl Drop for StartupHarness {
    fn drop(&mut self) {
        let _ = self.run(["kill-server"]);
        let _ = fs::remove_dir_all(&self.tmpdir);
    }
}

fn rmux_binary() -> &'static Path {
    Path::new(rmux_os::build_identity::cargo_binary(env!(
        "CARGO_BIN_EXE_rmux"
    )))
}

fn shell_single_quote(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "'\\''")
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
    format!("rx-{label}-{}-{}", std::process::id(), nanos % 1_000_000)
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
