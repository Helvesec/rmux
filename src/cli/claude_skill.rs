use std::ffi::OsString;
use std::fs::{self, OpenOptions};
use std::io::{self, ErrorKind, Write};
use std::path::{Path, PathBuf};

use super::ExitFailure;

const INSTALL_SKILL_COMMAND: &str = "install-skill";
const SKILL_SOURCE_PATH: &str = "resources/claude/skills/rmux/SKILL.md";
const SKILL_CONTENT: &str = include_str!("../../resources/claude/skills/rmux/SKILL.md");

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ClaudeSkillInvocation {
    InstallSkill,
}

pub(super) fn parse_invocation(
    arguments: &[OsString],
) -> Result<Option<ClaudeSkillInvocation>, ExitFailure> {
    if arguments
        .first()
        .and_then(|value| value.to_str())
        .is_some_and(|value| value == INSTALL_SKILL_COMMAND)
    {
        if arguments.len() != 1 {
            return Err(ExitFailure::new(1, "usage: rmux claude install-skill"));
        }
        return Ok(Some(ClaudeSkillInvocation::InstallSkill));
    }

    Ok(None)
}

pub(super) fn run(invocation: ClaudeSkillInvocation) -> Result<i32, ExitFailure> {
    match invocation {
        ClaudeSkillInvocation::InstallSkill => install_skill(),
    }
}

fn install_skill() -> Result<i32, ExitFailure> {
    let path = claude_skill_path()?;
    let parent = path.parent().ok_or_else(|| {
        ExitFailure::new(
            1,
            format!(
                "rmux claude install-skill: invalid Claude skill path '{}'",
                path.display()
            ),
        )
    })?;
    fs::create_dir_all(parent).map_err(|error| {
        ExitFailure::new(
            1,
            format!(
                "rmux claude install-skill: failed to create '{}': {error}",
                parent.display()
            ),
        )
    })?;

    let status = install_skill_file(&path)?;

    write_stdout(&format_install_status(&path, status))
}

enum InstallSkillStatus {
    Exists,
    Installed,
    Updated { backup: PathBuf },
}

fn install_skill_file(path: &Path) -> Result<InstallSkillStatus, ExitFailure> {
    let metadata = match fs::symlink_metadata(path) {
        Ok(metadata) => Some(metadata),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(ExitFailure::new(
                1,
                format!(
                    "rmux claude install-skill: failed to inspect '{}': {error}",
                    path.display()
                ),
            ));
        }
    };

    let Some(metadata) = metadata else {
        write_skill_atomic(path)?;
        return Ok(InstallSkillStatus::Installed);
    };

    if metadata.file_type().is_symlink() {
        return Err(ExitFailure::new(
            1,
            format!(
                "rmux claude install-skill: '{}' is a symlink; refusing to overwrite it",
                path.display()
            ),
        ));
    }
    if !metadata.is_file() {
        return Err(ExitFailure::new(
            1,
            format!(
                "rmux claude install-skill: '{}' exists and is not a regular file",
                path.display()
            ),
        ));
    }

    let existing = fs::read(path).map_err(|error| {
        ExitFailure::new(
            1,
            format!(
                "rmux claude install-skill: failed to read '{}': {error}",
                path.display()
            ),
        )
    })?;
    if existing == SKILL_CONTENT.as_bytes() {
        return Ok(InstallSkillStatus::Exists);
    }

    let backup = backup_existing_skill(path, &existing)?;
    write_skill_atomic(path)?;
    Ok(InstallSkillStatus::Updated { backup })
}

fn format_install_status(path: &Path, status: InstallSkillStatus) -> String {
    match status {
        InstallSkillStatus::Exists => format!(
            "exists:     {}\nsource:      {SKILL_SOURCE_PATH}\n",
            path.display()
        ),
        InstallSkillStatus::Installed => format!(
            "installed:  {}\nsource:      {SKILL_SOURCE_PATH}\n",
            path.display()
        ),
        InstallSkillStatus::Updated { backup } => format!(
            "updated:    {}\nbackup:     {}\nsource:      {SKILL_SOURCE_PATH}\n",
            path.display(),
            backup.display()
        ),
    }
}

fn backup_existing_skill(path: &Path, existing: &[u8]) -> Result<PathBuf, ExitFailure> {
    for candidate in backup_path_candidates(path) {
        match OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                file.write_all(existing).map_err(|error| {
                    ExitFailure::new(
                        1,
                        format!(
                            "rmux claude install-skill: failed to write backup '{}': {error}",
                            candidate.display()
                        ),
                    )
                })?;
                return Ok(candidate);
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => {
                return Err(ExitFailure::new(
                    1,
                    format!(
                        "rmux claude install-skill: failed to create backup '{}': {error}",
                        candidate.display()
                    ),
                ));
            }
        }
    }

    Err(ExitFailure::new(
        1,
        format!(
            "rmux claude install-skill: failed to choose a backup path for '{}'",
            path.display()
        ),
    ))
}

fn backup_path_candidates(path: &Path) -> impl Iterator<Item = PathBuf> + '_ {
    (0..1000).map(move |index| {
        let file_name = path
            .file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("SKILL.md");
        let suffix = if index == 0 {
            "rmux-backup".to_owned()
        } else {
            format!("rmux-backup.{index}")
        };
        path.with_file_name(format!("{file_name}.{suffix}"))
    })
}

fn write_skill_atomic(path: &Path) -> Result<(), ExitFailure> {
    let temp = temporary_skill_path(path);
    fs::write(&temp, SKILL_CONTENT).map_err(|error| {
        ExitFailure::new(
            1,
            format!(
                "rmux claude install-skill: failed to write temporary skill '{}': {error}",
                temp.display()
            ),
        )
    })?;

    replace_file(&temp, path).map_err(|error| {
        let _ = fs::remove_file(&temp);
        ExitFailure::new(
            1,
            format!(
                "rmux claude install-skill: failed to replace '{}': {error}",
                path.display()
            ),
        )
    })
}

fn temporary_skill_path(path: &Path) -> PathBuf {
    let file_name = path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or("SKILL.md");
    path.with_file_name(format!(".{file_name}.rmux-tmp-{}", std::process::id()))
}

#[cfg(windows)]
fn replace_file(temp: &Path, path: &Path) -> io::Result<()> {
    if path.exists() {
        fs::remove_file(path)?;
    }
    fs::rename(temp, path)
}

#[cfg(not(windows))]
fn replace_file(temp: &Path, path: &Path) -> io::Result<()> {
    fs::rename(temp, path)
}

fn claude_skill_path() -> Result<PathBuf, ExitFailure> {
    user_home().map(|home| {
        home.join(".claude")
            .join("skills")
            .join("rmux")
            .join("SKILL.md")
    })
}

#[cfg(windows)]
fn user_home() -> Result<PathBuf, ExitFailure> {
    std::env::var_os("USERPROFILE")
        .filter(|value| !value.is_empty())
        .or_else(|| std::env::var_os("HOME").filter(|value| !value.is_empty()))
        .map(PathBuf::from)
        .ok_or_else(|| {
            ExitFailure::new(
                1,
                "rmux claude install-skill: USERPROFILE or HOME is not set",
            )
        })
}

#[cfg(not(windows))]
fn user_home() -> Result<PathBuf, ExitFailure> {
    std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| ExitFailure::new(1, "rmux claude install-skill: HOME is not set"))
}

fn write_stdout(output: &str) -> Result<i32, ExitFailure> {
    match io::stdout().lock().write_all(output.as_bytes()) {
        Ok(()) => Ok(0),
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(0),
        Err(error) => Err(ExitFailure::new(
            1,
            format!("failed to write claude skill output: {error}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_invocation, ClaudeSkillInvocation, SKILL_CONTENT};
    use std::ffi::OsString;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_install_skill_subcommand() {
        assert_eq!(
            parse_invocation(&args(&["install-skill"])).expect("parse succeeds"),
            Some(ClaudeSkillInvocation::InstallSkill)
        );
    }

    #[test]
    fn leaves_regular_claude_args_to_launcher() {
        assert_eq!(
            parse_invocation(&args(&["--dangerously-skip-permissions"])).expect("parse succeeds"),
            None
        );
    }

    #[test]
    fn leaves_delimited_install_skill_arg_to_launcher() {
        assert_eq!(
            parse_invocation(&args(&["--", "install-skill"])).expect("parse succeeds"),
            None
        );
    }

    #[test]
    fn rejects_extra_install_skill_args() {
        let error = parse_invocation(&args(&["install-skill", "--force"]))
            .expect_err("extra args should fail");
        assert_eq!(error.message(), "usage: rmux claude install-skill");
    }

    #[test]
    fn bundled_skill_names_rmux_and_documents_project_skill_path() {
        assert!(SKILL_CONTENT.contains("name: rmux"));
        assert!(SKILL_CONTENT.contains("disable-model-invocation: true"));
        assert!(SKILL_CONTENT.contains("resources/claude/skills/rmux/SKILL.md"));
        assert!(SKILL_CONTENT.contains("~/.claude/skills/rmux"));
    }
}
