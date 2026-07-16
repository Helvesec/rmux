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
        DropinInvocation::SetupTmuxShim => run_setup_tmux_shim(argv0),
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
fn run_setup_tmux_shim(argv0: Option<&OsString>) -> Result<i32, ExitFailure> {
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

    let target = setup_tmux_shim_target(argv0)?;
    let shim = bin_dir.join("tmux");
    match fs::symlink_metadata(&shim) {
        Ok(metadata)
            if metadata.file_type().is_symlink()
                && symlink_points_to(&shim, &target.link_target) =>
        {
            write_stdout(
                &format!(
                    "exists:      {} -> {}\n",
                    shim.display(),
                    target.link_target.display()
                ),
                "setup tmux-shim",
            )
        }
        Ok(metadata)
            if metadata.file_type().is_symlink()
                && symlink_is_previous_packaged_rmux(&shim, &target.executable) =>
        {
            replace_symlink(&shim, &target.link_target).map_err(|error| {
                ExitFailure::new(
                    1,
                    format!(
                        "rmux setup tmux-shim: failed to refresh '{}': {error}",
                        shim.display()
                    ),
                )
            })?;
            write_stdout(
                &format!(
                    "updated:     {} -> {}\n",
                    shim.display(),
                    target.link_target.display()
                ),
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
            symlink(&target.link_target, &shim).map_err(|error| {
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
                    target.link_target.display()
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
struct SetupTmuxShimTarget {
    link_target: std::path::PathBuf,
    executable: std::path::PathBuf,
}

#[cfg(unix)]
fn setup_tmux_shim_target(argv0: Option<&OsString>) -> Result<SetupTmuxShimTarget, ExitFailure> {
    const PUBLIC_BINARY_OVERRIDE_ENV: &str = "RMUX_INTERNAL_PUBLIC_BINARY_PATH";

    let current = std::env::current_exe().map_err(|error| {
        ExitFailure::new(
            1,
            format!("rmux setup tmux-shim: failed to resolve current rmux binary: {error}"),
        )
    })?;
    let public = std::env::var_os(PUBLIC_BINARY_OVERRIDE_ENV)
        .map(std::path::PathBuf::from)
        .and_then(|path| absolute_without_resolving_links(&path))
        .filter(|path| path.is_file())
        .unwrap_or(current);
    let executable = std::fs::canonicalize(&public).unwrap_or_else(|_| public.clone());
    let link_target = stable_rmux_invocation_path(argv0, &public).unwrap_or(public);
    Ok(SetupTmuxShimTarget {
        link_target,
        executable,
    })
}

#[cfg(unix)]
fn stable_rmux_invocation_path(
    argv0: Option<&OsString>,
    public_binary: &Path,
) -> Option<std::path::PathBuf> {
    let invoked = Path::new(argv0?);
    if invoked.file_stem().and_then(OsStr::to_str) != Some("rmux") {
        return None;
    }

    if invoked.components().count() > 1 {
        let candidate = absolute_without_resolving_links(invoked)?;
        return paths_resolve_to_same_file(&candidate, public_binary).then_some(candidate);
    }

    let path = std::env::var_os("PATH")?;
    std::env::split_paths(&path)
        .map(|directory| directory.join(invoked))
        .filter_map(|candidate| absolute_without_resolving_links(&candidate))
        .find(|candidate| paths_resolve_to_same_file(candidate, public_binary))
}

#[cfg(unix)]
fn absolute_without_resolving_links(path: &Path) -> Option<std::path::PathBuf> {
    if path.is_absolute() {
        Some(path.to_path_buf())
    } else {
        std::env::current_dir().ok().map(|cwd| cwd.join(path))
    }
}

#[cfg(unix)]
fn symlink_is_previous_packaged_rmux(shim: &Path, current_executable: &Path) -> bool {
    let Ok(target) = std::fs::read_link(shim) else {
        return false;
    };
    let target = if target.is_absolute() {
        target
    } else {
        shim.parent().unwrap_or_else(|| Path::new(".")).join(target)
    };
    same_packaged_rmux_lineage(&target, current_executable)
}

#[cfg(unix)]
#[derive(Clone, Copy)]
enum PackagedRmuxRoot<'a> {
    Homebrew(&'a Path),
    Nix(&'a Path),
}

#[cfg(unix)]
fn same_packaged_rmux_lineage(left: &Path, right: &Path) -> bool {
    let roots = match (packaged_rmux_root(left), packaged_rmux_root(right)) {
        (Some(PackagedRmuxRoot::Homebrew(left)), Some(PackagedRmuxRoot::Homebrew(right)))
        | (Some(PackagedRmuxRoot::Nix(left)), Some(PackagedRmuxRoot::Nix(right))) => {
            Some((left, right))
        }
        _ => None,
    };
    match roots {
        Some((left, right)) => left == right || paths_resolve_to_same_file(left, right),
        None => false,
    }
}

#[cfg(unix)]
fn packaged_rmux_root(binary: &Path) -> Option<PackagedRmuxRoot<'_>> {
    if binary.file_name()? != OsStr::new("rmux") {
        return None;
    }
    let bin = binary.parent()?;
    if bin.file_name()? != OsStr::new("bin") {
        return None;
    }
    let package = bin.parent()?;

    if let Some(formula) = package.parent() {
        if formula.file_name() == Some(OsStr::new("rmux"))
            && formula.parent()?.file_name() == Some(OsStr::new("Cellar"))
        {
            return Some(PackagedRmuxRoot::Homebrew(formula));
        }
    }

    if !nix_derivation_is_rmux(package.file_name()?) {
        return None;
    }
    let store = package.parent()?;
    let nix = store.parent()?;
    (store.file_name()? == OsStr::new("store") && nix.file_name()? == OsStr::new("nix"))
        .then_some(PackagedRmuxRoot::Nix(store))
}

#[cfg(unix)]
fn nix_derivation_is_rmux(name: &OsStr) -> bool {
    let Some(name) = name.to_str() else {
        return false;
    };
    let Some((hash, package)) = name.split_once('-') else {
        return false;
    };
    hash.len() == 32
        && hash.bytes().all(|byte| {
            matches!(
                byte,
                b'0'..=b'9'
                    | b'a'..=b'd'
                    | b'f'..=b'n'
                    | b'p'..=b's'
                    | b'v'..=b'z'
            )
        })
        && (package == "rmux" || package.starts_with("rmux-"))
}

#[cfg(unix)]
fn replace_symlink(shim: &Path, target: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::symlink;

    let previous = std::fs::read_link(shim)?;
    let parent = shim.parent().unwrap_or_else(|| Path::new("."));
    let mut temporary = None;
    for attempt in 0..16 {
        let candidate = parent.join(format!(
            ".tmux.rmux-update-{}-{attempt}",
            std::process::id()
        ));
        match symlink(target, &candidate) {
            Ok(()) => {
                temporary = Some(candidate);
                break;
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
            Err(error) => return Err(error),
        }
    }
    let temporary = temporary.ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::AlreadyExists,
            "no temporary tmux shim path was available",
        )
    })?;

    if !std::fs::read_link(shim).is_ok_and(|current| current == previous) {
        let _ = std::fs::remove_file(&temporary);
        return Err(io::Error::new(
            io::ErrorKind::WouldBlock,
            "tmux shim changed while it was being refreshed",
        ));
    }
    if let Err(error) = std::fs::rename(&temporary, shim) {
        let _ = std::fs::remove_file(&temporary);
        return Err(error);
    }
    Ok(())
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
fn run_setup_tmux_shim(_argv0: Option<&OsString>) -> Result<i32, ExitFailure> {
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
    #[cfg(unix)]
    use super::same_packaged_rmux_lineage;
    use super::{parse_invocation, DropinInvocation};
    use std::ffi::OsString;
    #[cfg(unix)]
    use std::path::Path;

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

    #[cfg(unix)]
    #[test]
    fn recognizes_versions_of_the_same_homebrew_formula() {
        assert!(same_packaged_rmux_lineage(
            Path::new("/opt/homebrew/Cellar/rmux/0.8.0/bin/rmux"),
            Path::new("/opt/homebrew/Cellar/rmux/0.9.0/bin/rmux")
        ));
        assert!(!same_packaged_rmux_lineage(
            Path::new("/tmp/attacker/rmux"),
            Path::new("/opt/homebrew/Cellar/rmux/0.9.0/bin/rmux")
        ));
    }

    #[cfg(unix)]
    #[test]
    fn recognizes_versions_from_the_same_nix_store() {
        assert!(same_packaged_rmux_lineage(
            Path::new("/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-rmux-0.8.0/bin/rmux"),
            Path::new("/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-rmux-0.9.0/bin/rmux")
        ));
        assert!(!same_packaged_rmux_lineage(
            Path::new("/tmp/attacker/rmux"),
            Path::new("/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-rmux-0.9.0/bin/rmux")
        ));
        assert!(!same_packaged_rmux_lineage(
            Path::new("/tmp/fake/nix/store/aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa-rmux-0.8.0/bin/rmux"),
            Path::new("/nix/store/bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb-rmux-0.9.0/bin/rmux")
        ));
    }
}
