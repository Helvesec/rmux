use std::env;
use std::ffi::OsString;
use std::path::PathBuf;
use std::process::{self, Command as ProcessCommand};
#[cfg(any(unix, windows))]
use std::time::Duration;

use super::ExitFailure;
#[cfg(unix)]
use std::os::unix::process::CommandExt;

#[cfg(any(unix, windows))]
mod attached;
mod claude_program;
mod runtime_dir;
mod tmux_shim;

#[cfg(all(test, any(unix, windows)))]
use attached::{direct_launch_notice, launch_attached_decision, DirectLaunchNotice};
#[cfg(any(unix, windows))]
use attached::{report_unrequested_direct_launch, run_attached, should_launch_attached};
use claude_program::claude_process_command;
#[cfg(windows)]
use claude_program::windows_git_bash_path;
#[cfg(all(test, windows))]
use claude_program::{resolve_windows_program, WindowsClaudeProgram};
use runtime_dir::write_runner_pid_file;
#[cfg(all(test, windows))]
use runtime_dir::{
    cleanup_stale_windows_claude_temp_dirs_with_root, stale_windows_claude_temp_dir_pid,
};
#[cfg(test)]
use tmux_shim::path_with_shim_first_from;
#[cfg(all(test, unix))]
use tmux_shim::private_tmux_shim_target_for_public_binary;
use tmux_shim::{ensure_private_tmux_shim, path_with_shim_first, PrivateTmuxShim};
#[cfg(all(test, windows))]
use tmux_shim::{install_windows_tmux_shim_in_dir, windows_full_helper_candidates};

const TEAMMATE_MODE_FLAG: &str = "--teammate-mode";
const TEAMMATE_MODE: &str = "tmux";
const AGENT_TEAMS_ENV: &str = "CLAUDE_CODE_EXPERIMENTAL_AGENT_TEAMS";
const DISABLE_TMUX_SHIM_ENV: &str = "RMUX_DISABLE_TMUX_SHIM";
const CLAUDE_MAIN_SOCKET_ENV: &str = "RMUX_INTERNAL_CLAUDE_MAIN_SOCKET";
const CLAUDE_SWARM_SOCKET_ENV: &str = "RMUX_INTERNAL_CLAUDE_SWARM_SOCKET";
#[cfg(windows)]
const CLAUDE_GIT_BASH_PATH_ENV: &str = "CLAUDE_CODE_GIT_BASH_PATH";
#[cfg(any(unix, windows))]
const DIRECT_LAUNCH_ENV: &str = "RMUX_CLAUDE_DIRECT";
#[cfg(any(unix, windows))]
const OUTER_MUX_ENV: &[&str] = &["RMUX", "TMUX", "RMUX_PANE", "TMUX_PANE"];
const INTERNAL_RUNNER_COMMAND: &str = "__rmux-internal-claude-runner";
#[cfg(any(unix, windows))]
const MAIN_SESSION: &str = "rmux-claude";
#[cfg(any(unix, windows))]
const MAIN_WINDOW: &str = "claude";
#[cfg(any(unix, windows))]
const SWARM_SESSION: &str = "claude-swarm";
const SWARM_SOCKET_PREFIX: &str = "claude-swarm-";
#[cfg(any(unix, windows))]
const VIEWER_WAIT_TIMEOUT: Duration = Duration::from_secs(600);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClaudeInvocation {
    args: Vec<OsString>,
}

impl ClaudeInvocation {
    pub(super) fn new(args: Vec<OsString>) -> Self {
        Self { args }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct ClaudeRunnerInvocation {
    pid_file: PathBuf,
    main_socket: String,
    args: Vec<OsString>,
}

pub(super) fn parse_internal_runner(arguments: &[OsString]) -> Option<ClaudeRunnerInvocation> {
    let command = arguments.first()?.to_str()?;
    if command != INTERNAL_RUNNER_COMMAND {
        return None;
    }
    let pid_file = PathBuf::from(arguments.get(1)?);
    let main_socket = arguments.get(2)?.to_str()?.to_owned();
    let separator = arguments.get(3)?.to_str()?;
    if separator != "--" {
        return None;
    }
    Some(ClaudeRunnerInvocation {
        pid_file,
        main_socket,
        args: arguments[4..].to_vec(),
    })
}

pub(super) fn run(invocation: ClaudeInvocation) -> Result<i32, ExitFailure> {
    #[cfg(any(unix, windows))]
    if should_launch_attached() {
        return run_attached(invocation);
    }
    #[cfg(any(unix, windows))]
    report_unrequested_direct_launch();
    run_direct(invocation)
}

pub(super) fn run_internal_runner(invocation: ClaudeRunnerInvocation) -> Result<i32, ExitFailure> {
    write_runner_pid_file(&invocation.pid_file)?;
    let (mut command, _shim) = build_claude_command(invocation.args)?;
    command
        .env(CLAUDE_MAIN_SOCKET_ENV, &invocation.main_socket)
        .env(
            CLAUDE_SWARM_SOCKET_ENV,
            format!("{SWARM_SOCKET_PREFIX}{}", process::id()),
        );
    prepare_claude_leader_environment(&mut command, &invocation.main_socket)?;
    run_claude(command)
}

fn prepare_claude_leader_environment(
    command: &mut ProcessCommand,
    main_socket: &str,
) -> Result<(), ExitFailure> {
    prepare_claude_leader_environment_impl(command, main_socket)
}

#[cfg(windows)]
fn prepare_claude_leader_environment_impl(
    command: &mut ProcessCommand,
    main_socket: &str,
) -> Result<(), ExitFailure> {
    let socket_path = rmux_client::socket_path_for_label(main_socket).map_err(|error| {
        ExitFailure::new(
            1,
            format!("rmux claude: failed to resolve private tmux socket: {error}"),
        )
    })?;
    let pane = env::var_os("RMUX_PANE")
        .or_else(|| env::var_os("TMUX_PANE"))
        .unwrap_or_else(|| OsString::from("%0"));
    command
        .env("TMUX", format!("{},0,0", socket_path.display()))
        .env("TMUX_PANE", pane);
    Ok(())
}

#[cfg(not(windows))]
fn prepare_claude_leader_environment_impl(
    _command: &mut ProcessCommand,
    _main_socket: &str,
) -> Result<(), ExitFailure> {
    Ok(())
}

fn run_direct(invocation: ClaudeInvocation) -> Result<i32, ExitFailure> {
    let (command, _shim) = build_claude_command(invocation.args)?;
    run_claude(command)
}

fn build_claude_command(
    args: Vec<OsString>,
) -> Result<(ProcessCommand, Option<PrivateTmuxShim>), ExitFailure> {
    let mut command = claude_process_command()?;
    command
        .arg(TEAMMATE_MODE_FLAG)
        .arg(TEAMMATE_MODE)
        .args(args)
        .env(AGENT_TEAMS_ENV, "1");
    let shim = if private_tmux_shim_enabled() {
        let shim = ensure_private_tmux_shim()?;
        let path = path_with_shim_first(shim.path())?;
        set_command_path_with_shim(&mut command, path);
        Some(shim)
    } else {
        None
    };
    configure_claude_process_environment(&mut command)?;
    Ok((command, shim))
}

fn private_tmux_shim_enabled() -> bool {
    !env_flag_enabled(DISABLE_TMUX_SHIM_ENV)
}

fn env_flag_enabled(name: &str) -> bool {
    let Ok(value) = env::var(name) else {
        return false;
    };
    let value = value.trim();
    if value.is_empty() {
        return false;
    }
    !matches!(
        value.to_ascii_lowercase().as_str(),
        "0" | "false" | "no" | "off"
    )
}

#[cfg(windows)]
fn set_command_path_with_shim(command: &mut ProcessCommand, path: OsString) {
    // Claude Code resolves tmux via Node/Bun + where.exe. On Windows those
    // paths observe the canonical `Path` key; setting only `PATH` can leave a
    // stale inherited `Path` visible to that resolver.
    command.env("Path", &path).env("PATH", &path);
    ensure_windows_pathext(command);
}

#[cfg(not(windows))]
fn set_command_path_with_shim(command: &mut ProcessCommand, path: OsString) {
    command.env("PATH", path);
}

#[cfg(windows)]
fn ensure_windows_pathext(command: &mut ProcessCommand) {
    let existing = env::var_os("PATHEXT")
        .filter(|value| !value.is_empty())
        .unwrap_or_else(|| OsString::from(".COM;.EXE;.BAT;.CMD;.VBS;.VBE;.JS;.JSE;.WSF;.WSH;.MSC"));
    let has_exe = existing
        .to_string_lossy()
        .split(';')
        .any(|extension| extension.eq_ignore_ascii_case(".EXE"));
    if has_exe {
        command.env("PATHEXT", existing);
        return;
    }
    let mut updated = existing;
    updated.push(";.EXE");
    command.env("PATHEXT", updated);
}

#[cfg(windows)]
fn configure_claude_process_environment(command: &mut ProcessCommand) -> Result<(), ExitFailure> {
    let bash = windows_git_bash_path()?;
    command
        .env("SHELL", &bash)
        .env(CLAUDE_GIT_BASH_PATH_ENV, &bash);
    Ok(())
}

#[cfg(not(windows))]
fn configure_claude_process_environment(_command: &mut ProcessCommand) -> Result<(), ExitFailure> {
    Ok(())
}

#[cfg(unix)]
fn run_claude(mut command: ProcessCommand) -> Result<i32, ExitFailure> {
    let error = command.exec();
    Err(ExitFailure::new(
        1,
        format!("rmux claude: failed to execute claude: {error}"),
    ))
}

#[cfg(not(unix))]
fn run_claude(mut command: ProcessCommand) -> Result<i32, ExitFailure> {
    let status = command.status().map_err(|error| {
        ExitFailure::new(1, format!("rmux claude: failed to execute claude: {error}"))
    })?;
    Ok(status.code().unwrap_or(1))
}

#[cfg(test)]
mod tests {
    #[cfg(windows)]
    use super::{
        cleanup_stale_windows_claude_temp_dirs_with_root, ensure_private_tmux_shim,
        install_windows_tmux_shim_in_dir, resolve_windows_program,
        stale_windows_claude_temp_dir_pid, windows_full_helper_candidates,
    };
    use super::{
        direct_launch_notice, launch_attached_decision, path_with_shim_first_from,
        ClaudeInvocation, DirectLaunchNotice,
    };
    use std::env;
    use std::ffi::OsString;
    #[cfg(any(unix, windows))]
    use std::fs;
    use std::path::{Path, PathBuf};
    #[cfg(windows)]
    use std::sync::Mutex;

    #[cfg(windows)]
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    #[test]
    fn unrequested_direct_launch_notice_distinguishes_human_stderr_from_redirects() {
        assert_eq!(direct_launch_notice(false, false, false, false), None);
        assert_eq!(
            direct_launch_notice(false, false, true, true),
            Some(DirectLaunchNotice::MsysPty)
        );
        assert_eq!(direct_launch_notice(false, false, true, false), None);
        assert_eq!(
            direct_launch_notice(false, false, false, true),
            Some(DirectLaunchNotice::NonTerminal)
        );
        assert_eq!(
            direct_launch_notice(false, true, true, false),
            Some(DirectLaunchNotice::MsysPty)
        );
        assert_eq!(direct_launch_notice(true, true, true, true), None);
    }

    #[test]
    fn attached_launch_rejects_direct_nonterminal_and_msys_inputs() {
        assert!(launch_attached_decision(false, true, false));
        assert!(!launch_attached_decision(true, true, false));
        assert!(!launch_attached_decision(false, false, false));
        assert!(!launch_attached_decision(false, true, true));
    }

    #[cfg(windows)]
    #[test]
    fn msys_pty_pipe_names_are_recognized() {
        // Git Bash / mintty stdin (issue #77): MSYS and Cygwin pty pipes.
        for name in [
            r"\msys-dd50a72ab4668b33-pty0-from-master",
            r"\msys-dd50a72ab4668b33-pty1-to-master",
            r"\cygwin-e022582115c10879-pty4-from-master",
        ] {
            assert!(
                crate::windows_terminal::is_msys_pty_pipe_name(&name.to_ascii_lowercase()),
                "{name} must be detected as an MSYS pty"
            );
        }
        for name in [
            r"\pipe\rmux-daemon",
            r"\msys-dd50a72ab4668b33-shared",
            r"\some-pty-alike",
            "",
        ] {
            assert!(
                !crate::windows_terminal::is_msys_pty_pipe_name(&name.to_ascii_lowercase()),
                "{name} must not be detected as an MSYS pty"
            );
        }
    }

    fn args(values: &[&str]) -> Vec<OsString> {
        values.iter().map(OsString::from).collect()
    }

    #[test]
    fn invocation_preserves_claude_arguments() {
        let invocation = ClaudeInvocation::new(args(&[
            "--dangerously-skip-permissions",
            "--teammate-mode",
            "in-process",
        ]));

        assert_eq!(
            invocation.args,
            args(&[
                "--dangerously-skip-permissions",
                "--teammate-mode",
                "in-process"
            ])
        );
    }

    #[test]
    fn shim_path_precedes_existing_path() {
        let shim = Path::new("rmux-shim");
        let existing =
            env::join_paths([Path::new("usr-bin"), Path::new("bin")]).expect("joined path");
        let path = path_with_shim_first_from(shim, Some(existing)).expect("joined path");
        let mut entries = env::split_paths(&path);
        assert_eq!(entries.next(), Some(shim.to_path_buf()));
        assert_eq!(entries.next(), Some(PathBuf::from("usr-bin")));
        assert_eq!(entries.next(), Some(PathBuf::from("bin")));
    }

    #[cfg(unix)]
    #[test]
    fn unix_private_tmux_shim_prefers_packaged_full_helper() {
        let root = unique_test_dir("unix-full-helper");
        let bin = root.join("bin");
        let libexec = root.join("libexec").join("rmux");
        fs::create_dir_all(&bin).expect("bin dir");
        fs::create_dir_all(&libexec).expect("libexec dir");
        let public = bin.join("rmux");
        let helper = libexec.join("rmux");
        fs::write(&public, b"tiny").expect("public rmux");
        fs::write(&helper, b"full").expect("full helper");

        assert_eq!(
            fs::canonicalize(super::private_tmux_shim_target_for_public_binary(&public))
                .expect("canonical target"),
            fs::canonicalize(helper).expect("canonical helper")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn unix_private_tmux_shim_supports_prefix_lib_full_helper() {
        let root = unique_test_dir("unix-prefix-lib-helper");
        let bin = root.join("bin");
        let libexec = root.join("lib").join("rmux").join("libexec");
        fs::create_dir_all(&bin).expect("bin dir");
        fs::create_dir_all(&libexec).expect("prefix libexec dir");
        let public = bin.join("rmux");
        let helper = libexec.join("rmux");
        fs::write(&public, b"tiny").expect("public rmux");
        fs::write(&helper, b"full").expect("full helper");

        assert_eq!(
            fs::canonicalize(super::private_tmux_shim_target_for_public_binary(&public))
                .expect("canonical target"),
            fs::canonicalize(helper).expect("canonical helper")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn unix_private_tmux_shim_prefers_standard_libexec_layout() {
        let root = unique_test_dir("unix-helper-precedence");
        let bin = root.join("bin");
        let standard = root.join("libexec").join("rmux");
        let alternate = root.join("lib").join("rmux").join("libexec");
        fs::create_dir_all(&bin).expect("bin dir");
        fs::create_dir_all(&standard).expect("standard libexec dir");
        fs::create_dir_all(&alternate).expect("alternate libexec dir");
        let public = bin.join("rmux");
        let standard_helper = standard.join("rmux");
        let alternate_helper = alternate.join("rmux");
        fs::write(&public, b"tiny").expect("public rmux");
        fs::write(&standard_helper, b"standard").expect("standard helper");
        fs::write(&alternate_helper, b"alternate").expect("alternate helper");

        assert_eq!(
            fs::canonicalize(super::private_tmux_shim_target_for_public_binary(&public))
                .expect("canonical target"),
            fs::canonicalize(standard_helper).expect("canonical helper")
        );

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(unix)]
    #[test]
    fn unix_private_tmux_shim_falls_back_to_public_binary_without_helper() {
        let root = unique_test_dir("unix-no-helper");
        let bin = root.join("bin");
        fs::create_dir_all(&bin).expect("bin dir");
        let public = bin.join("rmux");
        fs::write(&public, b"full").expect("public rmux");

        assert_eq!(
            super::private_tmux_shim_target_for_public_binary(&public),
            public
        );

        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn windows_private_tmux_shim_is_persistent() {
        let shim = ensure_private_tmux_shim().expect("persistent shim");
        let dir = shim.path().to_path_buf();
        assert!(dir.ends_with(Path::new("rmux").join("claude-tmux-shim")));
        assert!(dir.join("tmux.exe").is_file());
        drop(shim);
        assert!(dir.exists(), "persistent shim directory should be retained");
    }

    #[cfg(windows)]
    #[test]
    fn windows_private_tmux_shim_uses_temporary_fallback_when_persistent_is_stale() {
        let root = unique_test_dir("claude-shim-fallback");
        let persistent = root.join("persistent");
        fs::create_dir_all(&persistent).expect("persistent dir");
        let source = root.join("rmux.exe");
        fs::write(&source, b"rmux").expect("source binary");
        fs::create_dir(persistent.join("tmux.exe")).expect("stale directory blocks replacement");

        let shim =
            install_windows_tmux_shim_in_dir(&source, persistent.clone()).expect("fallback shim");
        let fallback = shim.path().to_path_buf();

        assert_ne!(fallback, persistent);
        assert!(shim.cleanup_on_drop);
        assert!(fallback.join("tmux.exe").is_file());
        drop(shim);
        assert!(
            !fallback.exists(),
            "temporary fallback shim directory should be cleaned up"
        );
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn windows_claude_resolution_accepts_cmd_from_pathext() {
        let _lock = ENV_LOCK.lock().expect("env lock");
        let root = unique_test_dir("claude-cmd");
        fs::create_dir_all(&root).expect("test dir");
        fs::write(root.join("claude.cmd"), "@echo off\r\n").expect("fake claude");
        let _path = EnvGuard::set("PATH", root.as_os_str());
        let _pathext = EnvGuard::set("PATHEXT", ".CMD;.EXE");

        let program = resolve_windows_program("claude").expect("claude.cmd resolves");

        match program {
            super::WindowsClaudeProgram::Executable(path) => {
                assert!(path
                    .to_string_lossy()
                    .eq_ignore_ascii_case(&root.join("claude.cmd").to_string_lossy()))
            }
            _ => panic!("expected executable resolution"),
        }
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn windows_claude_temp_cleanup_removes_dead_pid_dirs_only() {
        let root = unique_test_dir("claude-cleanup");
        fs::create_dir_all(&root).expect("test root");
        let dead = root.join("rmux-claude-999999-abc-0");
        let live = root.join(format!("rmux-claude-{}-abc-0", std::process::id()));
        fs::create_dir_all(&dead).expect("dead dir");
        fs::create_dir_all(&live).expect("live dir");

        cleanup_stale_windows_claude_temp_dirs_with_root(&root);

        assert!(!dead.exists(), "dead pid temp dir should be removed");
        assert!(live.exists(), "live pid temp dir should be retained");
        let _ = fs::remove_dir_all(root);
    }

    #[cfg(windows)]
    #[test]
    fn stale_windows_claude_temp_dir_pid_parses_known_prefixes() {
        assert_eq!(
            stale_windows_claude_temp_dir_pid("rmux-claude-123-a-0"),
            Some(123)
        );
        assert_eq!(
            stale_windows_claude_temp_dir_pid("rmux-claude-shim-456-a-0"),
            Some(456)
        );
        assert_eq!(stale_windows_claude_temp_dir_pid("rmux-claude-x-a-0"), None);
    }

    #[cfg(windows)]
    #[test]
    fn windows_full_helper_candidates_cover_packaged_layouts() {
        let current = Path::new(r"C:\rmux\bin\rmux.exe");
        let candidates = windows_full_helper_candidates(current);
        assert_eq!(
            candidates[0],
            PathBuf::from(r"C:\rmux\bin\libexec\rmux\rmux.exe")
        );
        assert_eq!(
            candidates[1],
            PathBuf::from(r"C:\rmux\bin\..\libexec\rmux\rmux.exe")
        );
    }

    #[cfg(windows)]
    fn unique_test_dir(label: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "rmux-claude-launcher-{label}-{}",
            std::process::id()
        ))
    }

    #[cfg(unix)]
    fn unique_test_dir(label: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "rmux-claude-launcher-{label}-{}",
            std::process::id()
        ))
    }

    #[cfg(windows)]
    struct EnvGuard {
        name: &'static str,
        old: Option<OsString>,
    }

    #[cfg(windows)]
    impl EnvGuard {
        fn set(name: &'static str, value: impl AsRef<std::ffi::OsStr>) -> Self {
            let old = env::var_os(name);
            // SAFETY: These Windows-only tests serialize their environment mutations with a
            // process-wide mutex, so no other thread observes a concurrent env update here.
            unsafe {
                env::set_var(name, value.as_ref());
            }
            Self { name, old }
        }
    }

    #[cfg(windows)]
    impl Drop for EnvGuard {
        fn drop(&mut self) {
            // SAFETY: See `EnvGuard::set`; the guard is used only while holding the same
            // process-wide test mutex that serializes environment mutation.
            unsafe {
                if let Some(old) = &self.old {
                    env::set_var(self.name, old);
                } else {
                    env::remove_var(self.name);
                }
            }
        }
    }
}
