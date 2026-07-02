use std::error::Error;
use std::fs;
#[cfg(unix)]
use std::os::unix::fs::symlink;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static UNIQUE_TEST_ID: AtomicUsize = AtomicUsize::new(0);
const REPO_SKILL: &str = include_str!("../resources/claude/skills/rmux/SKILL.md");

#[test]
fn claude_install_skill_writes_user_level_skill() -> Result<(), Box<dyn Error>> {
    let root = unique_test_dir("install-skill");
    let home = root.join("home");
    fs::create_dir_all(&home)?;

    let output = Command::new(env!("CARGO_BIN_EXE_rmux"))
        .arg("claude")
        .arg("install-skill")
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()?;

    assert!(
        output.status.success(),
        "rmux claude install-skill failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let skill = home
        .join(".claude")
        .join("skills")
        .join("rmux")
        .join("SKILL.md");
    let content = fs::read_to_string(&skill)?;
    assert_eq!(content, REPO_SKILL);

    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(
        stdout.contains(&skill.display().to_string()),
        "stdout should mention installed skill path, got {stdout:?}"
    );
    assert!(
        stdout.contains("resources/claude/skills/rmux/SKILL.md"),
        "stdout should mention repository skill source, got {stdout:?}"
    );

    let second = Command::new(env!("CARGO_BIN_EXE_rmux"))
        .arg("claude")
        .arg("install-skill")
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()?;
    assert!(
        second.status.success(),
        "second install should be idempotent\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&second.stdout),
        String::from_utf8_lossy(&second.stderr)
    );
    assert!(
        String::from_utf8_lossy(&second.stdout).contains("exists:"),
        "second install should report existing content"
    );
    assert!(
        second.stderr.is_empty(),
        "second install should not write stderr: {}",
        String::from_utf8_lossy(&second.stderr)
    );

    let _ = fs::remove_dir_all(root);
    Ok(())
}

#[test]
fn claude_install_skill_backs_up_existing_custom_skill() -> Result<(), Box<dyn Error>> {
    let root = unique_test_dir("install-skill-backup");
    let home = root.join("home");
    let skill = home
        .join(".claude")
        .join("skills")
        .join("rmux")
        .join("SKILL.md");
    fs::create_dir_all(skill.parent().expect("skill parent"))?;
    fs::write(&skill, "custom user skill\n")?;

    let output = Command::new(env!("CARGO_BIN_EXE_rmux"))
        .arg("claude")
        .arg("install-skill")
        .env("HOME", &home)
        .env("USERPROFILE", &home)
        .output()?;

    assert!(
        output.status.success(),
        "custom skill update failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(&skill)?, REPO_SKILL);
    let backup = skill.with_file_name("SKILL.md.rmux-backup");
    assert_eq!(fs::read_to_string(&backup)?, "custom user skill\n");
    let stdout = String::from_utf8_lossy(&output.stdout);
    assert!(stdout.contains("updated:"), "stdout: {stdout:?}");
    assert!(stdout.contains("backup:"), "stdout: {stdout:?}");

    let _ = fs::remove_dir_all(root);
    Ok(())
}

#[cfg(unix)]
#[test]
fn claude_install_skill_refuses_to_overwrite_symlink() -> Result<(), Box<dyn Error>> {
    let root = unique_test_dir("install-skill-symlink");
    let home = root.join("home");
    let skill = home
        .join(".claude")
        .join("skills")
        .join("rmux")
        .join("SKILL.md");
    fs::create_dir_all(skill.parent().expect("skill parent"))?;
    let target = root.join("custom-skill.md");
    fs::write(&target, "custom target\n")?;
    symlink(&target, &skill)?;

    let output = Command::new(env!("CARGO_BIN_EXE_rmux"))
        .arg("claude")
        .arg("install-skill")
        .env("HOME", &home)
        .output()?;

    assert!(
        !output.status.success(),
        "symlink install should fail\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    assert_eq!(fs::read_to_string(&target)?, "custom target\n");
    assert!(fs::symlink_metadata(&skill)?.file_type().is_symlink());
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("refusing to overwrite"),
        "stderr should explain symlink refusal: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let _ = fs::remove_dir_all(root);
    Ok(())
}

fn unique_test_dir(label: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "rmux-claude-skill-{label}-{}-{}",
        std::process::id(),
        UNIQUE_TEST_ID.fetch_add(1, Ordering::Relaxed)
    ))
}
