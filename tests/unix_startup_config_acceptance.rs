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

struct StartupHarness {
    label: String,
    tmpdir: PathBuf,
    home: PathBuf,
    xdg: PathBuf,
}

impl StartupHarness {
    fn new(label: &str) -> Result<Self, Box<dyn Error>> {
        let unique = unique_id(label);
        let tmpdir = PathBuf::from("/tmp").join(&unique);
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
    Path::new(env!("CARGO_BIN_EXE_rmux"))
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
