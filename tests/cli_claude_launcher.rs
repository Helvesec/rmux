#![cfg(unix)]

use std::error::Error;
use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static UNIQUE_TEST_ID: AtomicUsize = AtomicUsize::new(0);

#[test]
fn claude_launcher_prepends_private_tmux_shim_and_routes_args() -> Result<(), Box<dyn Error>> {
    let root = unique_test_dir("launcher");
    let home = root.join("home");
    let bin = root.join("bin");
    fs::create_dir_all(&home)?;
    fs::create_dir_all(&bin)?;

    let output_path = root.join("claude-output.txt");
    write_fake_claude(&bin.join("claude"))?;

    let status = Command::new(env!("CARGO_BIN_EXE_rmux"))
        .arg("claude")
        .arg("--dangerously-skip-permissions")
        .arg("--teammate-mode")
        .arg("in-process")
        .env("HOME", &home)
        .env("PATH", &bin)
        .env("RMUX_CLAUDE_DIRECT", "1")
        .env("RMUX_CLAUDE_TEST_OUT", &output_path)
        .status()?;
    assert!(status.success(), "rmux claude exited with {status}");

    let output = fs::read_to_string(&output_path)?;
    assert!(
        output.contains(
            "args=[--teammate-mode][tmux][--dangerously-skip-permissions][--teammate-mode][in-process]\n"
        ),
        "unexpected fake claude argv: {output}"
    );
    assert!(
        output.contains("teams=1\n"),
        "agent teams env was not enabled: {output}"
    );

    let shim = home
        .join(".local")
        .join("share")
        .join("rmux")
        .join("claude-tmux-shim")
        .join("tmux");
    assert!(
        output.contains(&format!("tmux_path={}\n", shim.display())),
        "private shim was not first in PATH: {output}"
    );
    assert!(
        output.contains("tmux_version=tmux 3.4\n"),
        "private shim did not expose tmux-compatible version: {output}"
    );

    let _ = fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn claude_launcher_honors_disable_tmux_shim_env() -> Result<(), Box<dyn Error>> {
    let root = unique_test_dir("launcher-disable-shim");
    let home = root.join("home");
    let bin = root.join("bin");
    fs::create_dir_all(&home)?;
    fs::create_dir_all(&bin)?;

    let output_path = root.join("claude-output.txt");
    write_fake_claude(&bin.join("claude"))?;
    write_fake_tmux(&bin.join("tmux"), "external tmux 9.9")?;

    let status = Command::new(env!("CARGO_BIN_EXE_rmux"))
        .arg("claude")
        .arg("--probe")
        .env("HOME", &home)
        .env("PATH", &bin)
        .env("RMUX_CLAUDE_DIRECT", "1")
        .env("RMUX_DISABLE_TMUX_SHIM", "1")
        .env("RMUX_CLAUDE_TEST_OUT", &output_path)
        .status()?;
    assert!(status.success(), "rmux claude exited with {status}");

    let output = fs::read_to_string(&output_path)?;
    assert!(
        output.contains("args=[--teammate-mode][tmux][--probe]\n"),
        "unexpected fake claude argv: {output}"
    );
    assert!(
        output.contains(&format!("tmux_path={}\n", bin.join("tmux").display())),
        "disabled shim should leave caller PATH first: {output}"
    );
    assert!(
        output.contains("tmux_version=external tmux 9.9\n"),
        "disabled shim should not run rmux's private shim: {output}"
    );
    assert!(
        !home
            .join(".local")
            .join("share")
            .join("rmux")
            .join("claude-tmux-shim")
            .join("tmux")
            .exists(),
        "disabled shim should not create a private tmux shim"
    );

    let _ = fs::remove_dir_all(root);
    Ok(())
}

fn write_fake_claude(path: &Path) -> Result<(), Box<dyn Error>> {
    fs::write(
        path,
        "#!/bin/sh\n\
         {\n\
         printf 'args='\n\
         for arg in \"$@\"; do printf '[%s]' \"$arg\"; done\n\
         printf '\\n'\n\
         printf 'teams=%s\\n' \"$CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS\"\n\
         printf 'tmux_path=%s\\n' \"$(command -v tmux)\"\n\
         printf 'tmux_version=%s\\n' \"$(tmux -V)\"\n\
         } > \"$RMUX_CLAUDE_TEST_OUT\"\n",
    )?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn write_fake_tmux(path: &Path, version: &str) -> Result<(), Box<dyn Error>> {
    fs::write(
        path,
        format!(
            "#!/bin/sh\nif [ \"$1\" = \"-V\" ]; then printf '{}\\n'; exit 0; fi\nprintf 'fake tmux command\\n'\n",
            version
        ),
    )?;
    let mut permissions = fs::metadata(path)?.permissions();
    permissions.set_mode(0o755);
    fs::set_permissions(path, permissions)?;
    Ok(())
}

fn unique_test_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "rmux-claude-{label}-{}-{}",
        std::process::id(),
        UNIQUE_TEST_ID.fetch_add(1, Ordering::Relaxed)
    ))
}
