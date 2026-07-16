use std::env;
use std::ffi::OsString;
#[cfg(not(unix))]
use std::io::{self, IsTerminal, Write};
use std::path::{Path, PathBuf};
use std::process::Command;
#[cfg(not(unix))]
use std::process::Stdio;
#[cfg(not(unix))]
use std::thread;

#[cfg(unix)]
use std::os::unix::process::CommandExt;

const FULL_HELPER_OVERRIDE_ENV: &str = "RMUX_FULL_BINARY_PATH";
const PUBLIC_BINARY_OVERRIDE_ENV: &str = "RMUX_INTERNAL_PUBLIC_BINARY_PATH";
#[cfg(windows)]
const INTERNAL_CLIENT_SHELL_ENV: &str = "RMUX_INTERNAL_CLIENT_SHELL";
#[cfg(windows)]
const TMUX_COMPAT_OVERRIDE_ENV: &str = "RMUX_INTERNAL_INVOKED_AS_TMUX";

pub(super) fn exec_full_helper(args: &[OsString]) -> Result<i32, String> {
    let helper = full_helper_path()?;
    let mut command = Command::new(&helper);
    command.args(args.iter().skip(1));
    if let Ok(current) = env::current_exe() {
        command.env(PUBLIC_BINARY_OVERRIDE_ENV, current);
    }
    #[cfg(windows)]
    set_windows_client_shell_handoff(&mut command, super::parse::invoking_client_shell());
    #[cfg(windows)]
    if invoked_as_tmux(args) {
        command.env(TMUX_COMPAT_OVERRIDE_ENV, "1");
    }

    #[cfg(unix)]
    {
        if let Some(argv0) = args.first() {
            command.arg0(argv0);
        }
        let error = command.exec();
        Err(format!(
            "failed to exec private rmux helper '{}': {error}",
            helper.display()
        ))
    }

    #[cfg(not(unix))]
    {
        let output_pipes = helper_output_pipes();
        if output_pipes.any() {
            return run_full_helper_with_piped_output(command, output_pipes);
        }
        let status = command.status().map_err(|error| {
            format!(
                "failed to run private rmux helper '{}': {error}",
                helper.display()
            )
        })?;
        Ok(status.code().unwrap_or(1))
    }
}

#[cfg(not(unix))]
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct HelperOutputPipes {
    stdout: bool,
    stderr: bool,
}

#[cfg(not(unix))]
impl HelperOutputPipes {
    fn any(self) -> bool {
        self.stdout || self.stderr
    }
}

#[cfg(windows)]
fn helper_output_pipes() -> HelperOutputPipes {
    use std::os::windows::io::AsRawHandle;

    let stdout = io::stdout();
    let stderr = io::stderr();
    HelperOutputPipes {
        stdout: helper_stream_should_be_piped(
            stdout.is_terminal(),
            crate::windows_terminal::handle_is_msys_pty(stdout.as_raw_handle()),
        ),
        stderr: helper_stream_should_be_piped(
            stderr.is_terminal(),
            crate::windows_terminal::handle_is_msys_pty(stderr.as_raw_handle()),
        ),
    }
}

#[cfg(all(not(unix), not(windows)))]
fn helper_output_pipes() -> HelperOutputPipes {
    HelperOutputPipes {
        stdout: !io::stdout().is_terminal(),
        stderr: !io::stderr().is_terminal(),
    }
}

#[cfg(not(unix))]
fn helper_stream_should_be_piped(is_terminal: bool, is_msys_pty: bool) -> bool {
    !is_terminal && !is_msys_pty
}

#[cfg(not(unix))]
fn run_full_helper_with_piped_output(
    mut command: Command,
    output_pipes: HelperOutputPipes,
) -> Result<i32, String> {
    if output_pipes.stdout {
        command.stdout(Stdio::piped());
    }
    if output_pipes.stderr {
        command.stderr(Stdio::piped());
    }
    let mut child = command.spawn().map_err(|error| {
        format!(
            "failed to run private rmux helper '{}': {error}",
            command.get_program().to_string_lossy()
        )
    })?;
    let stdout = child.stdout.take();
    let stderr = child.stderr.take();

    let stdout_thread = thread::spawn(move || {
        if let Some(mut stdout) = stdout {
            copy_helper_output(&mut stdout, io::stdout())
        } else {
            Ok(())
        }
    });
    let stderr_thread = thread::spawn(move || {
        if let Some(mut stderr) = stderr {
            copy_helper_output(&mut stderr, io::stderr())
        } else {
            Ok(())
        }
    });

    let status = child.wait().map_err(|error| error.to_string())?;
    join_output_thread(stdout_thread)?;
    join_output_thread(stderr_thread)?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(not(unix))]
fn copy_helper_output<R, W>(reader: &mut R, writer: W) -> io::Result<()>
where
    R: io::Read,
    W: Write,
{
    let mut writer = writer;
    match io::copy(reader, &mut writer) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => Ok(()),
        Err(error) => Err(error),
    }
}

#[cfg(not(unix))]
fn join_output_thread(thread: thread::JoinHandle<io::Result<()>>) -> Result<(), String> {
    thread
        .join()
        .map_err(|_| "private rmux helper output thread panicked".to_owned())?
        .map_err(|error| error.to_string())
}

#[cfg(windows)]
fn set_windows_client_shell_handoff(command: &mut Command, shell: Option<String>) {
    command.env_remove(INTERNAL_CLIENT_SHELL_ENV);
    if let Some(shell) = shell.filter(|shell| !shell.is_empty()) {
        command.env(INTERNAL_CLIENT_SHELL_ENV, shell);
    }
}

#[cfg(windows)]
fn invoked_as_tmux(args: &[OsString]) -> bool {
    args.first()
        .and_then(|arg| Path::new(arg).file_stem())
        .and_then(|stem| stem.to_str())
        .is_some_and(|stem| stem.eq_ignore_ascii_case("tmux"))
}

pub(super) fn full_helper_path() -> Result<PathBuf, String> {
    if let Some(path) = env::var_os(FULL_HELPER_OVERRIDE_ENV) {
        let path = PathBuf::from(path);
        if path.is_file() {
            return Ok(path);
        }
    }

    let current = env::current_exe().map_err(|error| error.to_string())?;
    let resolved = std::fs::canonicalize(&current).ok();
    if let Some(path) = helper_from_executable_paths(&current, resolved.as_deref()) {
        return Ok(path);
    }

    Err("private rmux helper not found under libexec/rmux; rebuild or reinstall rmux".to_owned())
}

#[cfg(not(windows))]
pub(super) fn daemon_helper_path() -> Result<PathBuf, String> {
    if let Ok(current) = env::current_exe() {
        if let Some(path) = daemon_from_executable_path(&current) {
            return Ok(path);
        }
    }

    full_helper_path()
}

fn helper_from_executable_path(current: &Path) -> Option<PathBuf> {
    full_helper_candidates(current)
        .into_iter()
        .find(|candidate| candidate.is_file() && candidate.as_path() != current)
}

fn helper_from_executable_paths(current: &Path, resolved: Option<&Path>) -> Option<PathBuf> {
    helper_from_executable_path(current).or_else(|| resolved.and_then(helper_from_executable_path))
}

fn full_helper_candidates(current_exe: &Path) -> Vec<PathBuf> {
    let Some(parent) = current_exe.parent() else {
        return Vec::new();
    };
    vec![
        parent.join("libexec").join("rmux").join(helper_file_name()),
        parent
            .join("..")
            .join("libexec")
            .join("rmux")
            .join(helper_file_name()),
    ]
}

#[cfg(not(windows))]
fn daemon_from_executable_path(current: &Path) -> Option<PathBuf> {
    daemon_helper_candidates(current)
        .into_iter()
        .find(|candidate| candidate.is_file() && candidate.as_path() != current)
}

#[cfg(not(windows))]
fn daemon_helper_candidates(current_exe: &Path) -> Vec<PathBuf> {
    let Some(parent) = current_exe.parent() else {
        return Vec::new();
    };
    let daemon_name = daemon_file_name();
    vec![
        parent.join(&daemon_name),
        parent.join("..").join("bin").join(&daemon_name),
        parent.join("..").join("..").join("bin").join(&daemon_name),
    ]
}

fn helper_file_name() -> OsString {
    let mut name = OsString::from("rmux");
    if !env::consts::EXE_SUFFIX.is_empty() {
        name.push(env::consts::EXE_SUFFIX);
    }
    name
}

#[cfg(not(windows))]
fn daemon_file_name() -> OsString {
    let mut name = OsString::from("rmux-daemon");
    if !env::consts::EXE_SUFFIX.is_empty() {
        name.push(env::consts::EXE_SUFFIX);
    }
    name
}

#[cfg(test)]
mod tests {
    use std::env;
    use std::path::PathBuf;
    use std::time::{SystemTime, UNIX_EPOCH};

    #[cfg(not(windows))]
    use super::{daemon_file_name, daemon_from_executable_path};
    use super::{helper_file_name, helper_from_executable_paths};
    #[cfg(windows)]
    use super::{
        helper_stream_should_be_piped, set_windows_client_shell_handoff, INTERNAL_CLIENT_SHELL_ENV,
    };

    fn temp_root(name: &str) -> PathBuf {
        let timestamp = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time")
            .as_nanos();
        env::temp_dir().join(format!(
            "rmux-tiny-helper-{name}-{}-{timestamp}",
            std::process::id()
        ))
    }

    #[test]
    fn full_helper_falls_back_to_resolved_executable_layout() {
        let root = temp_root("resolved");
        let links = root.join("links");
        let install = root.join("package");
        let libexec = install.join("libexec").join("rmux");
        std::fs::create_dir_all(&links).expect("create links");
        std::fs::create_dir_all(&libexec).expect("create libexec");

        let alias = links.join(helper_file_name());
        let public = install.join(helper_file_name());
        let full = libexec.join(helper_file_name());
        std::fs::write(&alias, b"alias").expect("write alias");
        std::fs::write(&public, b"public").expect("write public");
        std::fs::write(&full, b"full").expect("write full");

        assert_eq!(
            helper_from_executable_paths(&alias, Some(&public)),
            Some(full)
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn full_helper_prefers_current_executable_layout_before_resolved_layout() {
        let root = temp_root("current-first");
        let links_libexec = root.join("links").join("libexec").join("rmux");
        let install_libexec = root.join("package").join("libexec").join("rmux");
        std::fs::create_dir_all(&links_libexec).expect("create links libexec");
        std::fs::create_dir_all(&install_libexec).expect("create install libexec");

        let alias = root.join("links").join(helper_file_name());
        let public = root.join("package").join(helper_file_name());
        let links_full = links_libexec.join(helper_file_name());
        let install_full = install_libexec.join(helper_file_name());
        std::fs::write(&alias, b"alias").expect("write alias");
        std::fs::write(&public, b"public").expect("write public");
        std::fs::write(&links_full, b"links full").expect("write links full");
        std::fs::write(&install_full, b"install full").expect("write install full");

        assert_eq!(
            helper_from_executable_paths(&alias, Some(&public)),
            Some(links_full)
        );

        let _ = std::fs::remove_dir_all(&root);
    }

    #[cfg(windows)]
    #[test]
    fn windows_full_helper_shell_handoff_replaces_stale_internal_hint() {
        let mut command = std::process::Command::new("rmux.exe");
        command.env(INTERNAL_CLIENT_SHELL_ENV, "stale.exe");

        set_windows_client_shell_handoff(&mut command, Some("pwsh.exe".to_owned()));

        let handoff = command
            .get_envs()
            .find(|(name, _)| name.eq_ignore_ascii_case(INTERNAL_CLIENT_SHELL_ENV))
            .and_then(|(_, value)| value)
            .expect("client shell handoff env");
        assert_eq!(handoff, std::ffi::OsStr::new("pwsh.exe"));
    }

    #[cfg(windows)]
    #[test]
    fn windows_full_helper_shell_handoff_removes_stale_hint_without_parent_shell() {
        let mut command = std::process::Command::new("rmux.exe");
        command.env(INTERNAL_CLIENT_SHELL_ENV, "stale.exe");

        set_windows_client_shell_handoff(&mut command, None);

        let handoff = command
            .get_envs()
            .find(|(name, _)| name.eq_ignore_ascii_case(INTERNAL_CLIENT_SHELL_ENV))
            .expect("client shell handoff env override");
        assert!(handoff.1.is_none());
    }

    #[cfg(windows)]
    #[test]
    fn windows_full_helper_preserves_msys_terminal_handles() {
        assert!(!helper_stream_should_be_piped(false, true));
        assert!(!helper_stream_should_be_piped(true, false));
        assert!(helper_stream_should_be_piped(false, false));
    }

    #[cfg(not(windows))]
    #[test]
    fn daemon_candidate_prefers_packaged_bin_sibling() {
        let root = temp_root("daemon");
        let bin = root.join("bin");
        let libexec = root.join("libexec").join("rmux");
        std::fs::create_dir_all(&bin).expect("create bin");
        std::fs::create_dir_all(&libexec).expect("create libexec");
        let public = bin.join(helper_file_name());
        let daemon = bin.join(daemon_file_name());
        let full = libexec.join(helper_file_name());
        std::fs::write(&public, b"public").expect("write public");
        std::fs::write(&daemon, b"daemon").expect("write daemon");
        std::fs::write(&full, b"full").expect("write full");

        assert_eq!(daemon_from_executable_path(&public), Some(daemon));

        let _ = std::fs::remove_dir_all(&root);
    }
}
