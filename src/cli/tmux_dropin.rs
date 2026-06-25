use std::ffi::{OsStr, OsString};
use std::io::{self, ErrorKind, Write};
use std::path::Path;

use super::ExitFailure;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum DropinInvocation {
    DoctorTmuxDropin,
    SetupTmuxShim,
}

pub(super) fn parse_invocation(
    arguments: &[OsString],
) -> Result<Option<DropinInvocation>, ExitFailure> {
    let Some(command_index) = split_top_level_prefix(arguments) else {
        return Ok(None);
    };
    let Some(command) = arguments
        .get(command_index)
        .and_then(|value| value.to_str())
    else {
        return Ok(None);
    };

    match command {
        "doctor" => parse_doctor(&arguments[command_index + 1..]).map(Some),
        "setup" => parse_setup(&arguments[command_index + 1..]).map(Some),
        _ => Ok(None),
    }
}

pub(super) fn run(
    invocation: DropinInvocation,
    argv0: Option<&OsString>,
) -> Result<i32, ExitFailure> {
    match invocation {
        DropinInvocation::DoctorTmuxDropin => run_doctor(argv0),
        DropinInvocation::SetupTmuxShim => run_setup_tmux_shim(),
    }
}

fn parse_doctor(arguments: &[OsString]) -> Result<DropinInvocation, ExitFailure> {
    if arguments
        .first()
        .and_then(|argument| argument.to_str())
        .is_some_and(|argument| argument == "--help")
    {
        return Err(ExitFailure::new_stdout(0, "usage: rmux doctor tmux-dropin"));
    }
    match single_subcommand(arguments, "doctor", "tmux-dropin")? {
        "tmux-dropin" => Ok(DropinInvocation::DoctorTmuxDropin),
        other => Err(ExitFailure::new(
            1,
            format!("rmux doctor: unknown check '{other}'"),
        )),
    }
}

fn parse_setup(arguments: &[OsString]) -> Result<DropinInvocation, ExitFailure> {
    if arguments
        .first()
        .and_then(|argument| argument.to_str())
        .is_some_and(|argument| argument == "--help")
    {
        return Err(ExitFailure::new_stdout(0, "usage: rmux setup tmux-shim"));
    }
    match single_subcommand(arguments, "setup", "tmux-shim")? {
        "tmux-shim" => Ok(DropinInvocation::SetupTmuxShim),
        other => Err(ExitFailure::new(
            1,
            format!("rmux setup: unknown action '{other}'"),
        )),
    }
}

fn single_subcommand<'a>(
    arguments: &'a [OsString],
    command: &str,
    expected: &str,
) -> Result<&'a str, ExitFailure> {
    let Some(subcommand) = arguments.first().and_then(|value| value.to_str()) else {
        return Err(ExitFailure::new(
            1,
            format!("rmux {command}: expected {expected}"),
        ));
    };
    if arguments.len() != 1 {
        return Err(ExitFailure::new(
            1,
            format!("rmux {command}: expected exactly one argument"),
        ));
    }
    Ok(subcommand)
}

fn split_top_level_prefix(arguments: &[OsString]) -> Option<usize> {
    let mut index = 0;

    while let Some(argument) = arguments.get(index) {
        let value = argument.to_str()?;
        if value == "--" {
            return Some(index + 1);
        }
        if !value.starts_with('-') || value == "-" {
            return Some(index);
        }

        match value {
            "-2" | "-D" | "-N" | "-l" | "-u" => {}
            "-C" | "-v" => {}
            "-c" | "-f" | "-L" | "-S" | "-T" => {
                index += 1;
            }
            _ if value.starts_with("-L") && value.len() > 2 => {}
            _ if value.starts_with("-S") && value.len() > 2 => {}
            _ if value.starts_with("-c") && value.len() > 2 => {}
            _ if value.starts_with("-f") && value.len() > 2 => {}
            _ if value.starts_with("-T") && value.len() > 2 => {}
            _ if is_short_flag_cluster(value, "2CDNluv") => {}
            _ => return Some(index),
        }

        index += 1;
    }

    None
}

fn is_short_flag_cluster(value: &str, allowed: &str) -> bool {
    value.len() > 2
        && value.starts_with('-')
        && !value.starts_with("--")
        && value.chars().skip(1).all(|flag| allowed.contains(flag))
}

fn run_doctor(argv0: Option<&OsString>) -> Result<i32, ExitFailure> {
    let argv0_name = argv0
        .and_then(|value| Path::new(value).file_name())
        .and_then(OsStr::to_str)
        .unwrap_or("rmux");
    let shim_detected = Path::new(argv0_name)
        .file_stem()
        .and_then(OsStr::to_str)
        .is_some_and(|stem| stem == "tmux");
    let shim = if shim_detected {
        "detected"
    } else {
        "not detected"
    };

    let mut output = String::new();
    output.push_str("rmux tmux-dropin doctor\n");
    output.push_str(&format!("shim:        {shim}   (argv[0]={argv0_name})\n"));
    if !shim_detected {
        output.push_str("suggested:   ln -s $(command -v rmux) ~/.local/bin/tmux\n");
        output.push_str("setup:       rmux setup tmux-shim\n");
    }
    write_stdout(&output, "doctor")
}

#[cfg(unix)]
fn run_setup_tmux_shim() -> Result<i32, ExitFailure> {
    use std::fs;
    use std::os::unix::fs::symlink;
    use std::path::PathBuf;

    let home = std::env::var_os("HOME")
        .filter(|value| !value.is_empty())
        .ok_or_else(|| ExitFailure::new(1, "rmux setup tmux-shim: HOME is not set"))?;
    let bin_dir = PathBuf::from(home).join(".local").join("bin");
    fs::create_dir_all(&bin_dir).map_err(|error| {
        ExitFailure::new(
            1,
            format!(
                "rmux setup tmux-shim: failed to create '{}': {error}",
                bin_dir.display()
            ),
        )
    })?;

    let target = std::env::current_exe().map_err(|error| {
        ExitFailure::new(
            1,
            format!("rmux setup tmux-shim: failed to resolve current rmux binary: {error}"),
        )
    })?;
    let shim = bin_dir.join("tmux");
    match fs::symlink_metadata(&shim) {
        Ok(metadata) if metadata.file_type().is_symlink() && symlink_points_to(&shim, &target) => {
            write_stdout(
                &format!("exists:      {} -> {}\n", shim.display(), target.display()),
                "setup tmux-shim",
            )
        }
        Ok(_) => Err(ExitFailure::new(
            1,
            format!(
                "rmux setup tmux-shim: '{}' already exists; refusing to overwrite",
                shim.display()
            ),
        )),
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            symlink(&target, &shim).map_err(|error| {
                ExitFailure::new(
                    1,
                    format!(
                        "rmux setup tmux-shim: failed to create '{}': {error}",
                        shim.display()
                    ),
                )
            })?;
            write_stdout(
                &format!(
                    "created:     {} -> {}\nnext:        ensure ~/.local/bin is before tmux in PATH\n",
                    shim.display(),
                    target.display()
                ),
                "setup tmux-shim",
            )
        }
        Err(error) => Err(ExitFailure::new(
            1,
            format!(
                "rmux setup tmux-shim: failed to inspect '{}': {error}",
                shim.display()
            ),
        )),
    }
}

#[cfg(unix)]
fn symlink_points_to(shim: &Path, target: &Path) -> bool {
    let Ok(link_target) = std::fs::read_link(shim) else {
        return false;
    };
    let resolved = if link_target.is_absolute() {
        link_target
    } else {
        shim.parent()
            .unwrap_or_else(|| Path::new("."))
            .join(link_target)
    };
    paths_resolve_to_same_file(&resolved, target)
}

#[cfg(unix)]
fn paths_resolve_to_same_file(left: &Path, right: &Path) -> bool {
    let Ok(left) = std::fs::canonicalize(left) else {
        return false;
    };
    let Ok(right) = std::fs::canonicalize(right) else {
        return false;
    };
    left == right
}

#[cfg(not(unix))]
fn run_setup_tmux_shim() -> Result<i32, ExitFailure> {
    Err(ExitFailure::new(
        1,
        "rmux setup tmux-shim is only supported on Unix-like systems",
    ))
}

fn write_stdout(output: &str, context: &str) -> Result<i32, ExitFailure> {
    match io::stdout().lock().write_all(output.as_bytes()) {
        Ok(()) => Ok(0),
        Err(error) if error.kind() == ErrorKind::BrokenPipe => Ok(0),
        Err(error) => Err(ExitFailure::new(
            1,
            format!("failed to write {context} output: {error}"),
        )),
    }
}

#[cfg(test)]
mod tests {
    use super::{parse_invocation, DropinInvocation};
    use std::ffi::OsString;

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn parses_doctor_after_top_level_socket_flags() {
        let invocation = parse_invocation(&args(&["-Ldemo", "doctor", "tmux-dropin"]))
            .expect("parse succeeds")
            .expect("drop-in invocation");

        assert_eq!(invocation, DropinInvocation::DoctorTmuxDropin);
    }

    #[test]
    fn parses_doctor_after_glued_start_directory_flag() {
        let invocation = parse_invocation(&args(&["-c/tmp", "doctor", "tmux-dropin"]))
            .expect("parse succeeds")
            .expect("drop-in invocation");

        assert_eq!(invocation, DropinInvocation::DoctorTmuxDropin);
    }

    #[test]
    fn parses_setup_tmux_shim() {
        let invocation = parse_invocation(&args(&["setup", "tmux-shim"]))
            .expect("parse succeeds")
            .expect("drop-in invocation");

        assert_eq!(invocation, DropinInvocation::SetupTmuxShim);
    }

    #[test]
    fn ignores_other_commands() {
        assert!(parse_invocation(&args(&["list-sessions"]))
            .expect("parse succeeds")
            .is_none());
    }
}
