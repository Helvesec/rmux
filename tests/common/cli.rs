use std::error::Error;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Output, Stdio};
use std::time::{Duration, Instant};

use rmux_client::INTERNAL_DAEMON_FLAG;

use crate::common::{
    default_socket_path_in, shutdown_rmux_server, terminate_child, unique_tmpdir, wait_for_socket,
    write_hidden_launcher, AutoStartCleanup, BINARY_OVERRIDE_ENV, BINARY_OVERRIDE_TEST_OPT_IN_ENV,
};

pub(crate) struct CliHarness {
    tmpdir: PathBuf,
    socket_path: PathBuf,
    launcher_path: PathBuf,
    pid_path: PathBuf,
}

impl CliHarness {
    pub(crate) fn new(label: &str) -> Result<Self, Box<dyn Error>> {
        let tmpdir = unique_tmpdir(label);
        fs::create_dir_all(&tmpdir)?;
        let socket_path = default_socket_path_in(&tmpdir)?;
        let launcher_path = tmpdir.join("rmux-launcher.sh");
        let pid_path = tmpdir.join("rmux.pid");

        Ok(Self {
            tmpdir,
            socket_path,
            launcher_path,
            pid_path,
        })
    }

    pub(crate) fn run(&self, args: &[&str]) -> Result<Output, Box<dyn Error>> {
        self.run_with(args, |_| {})
    }

    pub(crate) fn run_with<F>(&self, args: &[&str], configure: F) -> Result<Output, Box<dyn Error>>
    where
        F: FnOnce(&mut Command),
    {
        let _lock = acquire_cli_command_lock()?;
        let mut command = self.base_command();
        command.args(args);
        command.stdin(Stdio::null());
        configure(&mut command);
        Ok(command.output()?)
    }

    pub(crate) fn start_hidden_daemon(&self) -> Result<DaemonGuard, Box<dyn Error>> {
        let _lock = acquire_cli_command_lock()?;
        let mut child = self
            .base_command()
            .arg(INTERNAL_DAEMON_FLAG)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .spawn()?;

        wait_for_socket(&self.socket_path, &mut child)?;
        Ok(DaemonGuard { child })
    }

    pub(crate) fn auto_start_cleanup(&self) -> Result<AutoStartCleanup, Box<dyn Error>> {
        write_hidden_launcher(&self.launcher_path, &self.pid_path)?;
        Ok(AutoStartCleanup::new(
            self.socket_path.clone(),
            self.pid_path.clone(),
        ))
    }

    pub(crate) fn base_command(&self) -> Command {
        let mut command = Command::new(env!("CARGO_BIN_EXE_rmux"));
        command.env("RMUX_TMPDIR", &self.tmpdir);
        command.env("HOME", self.tmpdir.join("home"));
        command.env("XDG_CONFIG_HOME", self.tmpdir.join("xdg"));
        command.env(BINARY_OVERRIDE_TEST_OPT_IN_ENV, "1");
        command.env_remove(BINARY_OVERRIDE_ENV);
        command.env_remove("RMUX");
        command
    }

    pub(crate) fn socket_path(&self) -> &Path {
        &self.socket_path
    }

    pub(crate) fn pid_path(&self) -> &Path {
        &self.pid_path
    }

    pub(crate) fn launcher_path(&self) -> &Path {
        &self.launcher_path
    }

    pub(crate) fn tmpdir(&self) -> &Path {
        &self.tmpdir
    }
}

impl Drop for CliHarness {
    fn drop(&mut self) {
        let _ = shutdown_rmux_server(&self.socket_path);
        let _ = fs::remove_file(&self.socket_path);
        let _ = fs::remove_dir_all(&self.tmpdir);
    }
}

pub(crate) struct DaemonGuard {
    child: Child,
}

impl DaemonGuard {
    pub(crate) fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    pub(crate) fn pid(&self) -> u32 {
        self.child.id()
    }
}

impl Drop for DaemonGuard {
    fn drop(&mut self) {
        let _ = terminate_child(&mut self.child);
    }
}

struct CliCommandLock {
    path: PathBuf,
}

impl Drop for CliCommandLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(cli_command_lock_owner_path(&self.path));
        let _ = fs::remove_dir(&self.path);
    }
}

fn acquire_cli_command_lock() -> Result<CliCommandLock, Box<dyn Error>> {
    let path = std::env::temp_dir().join("rmux-cli-command.lock");
    let deadline = Instant::now() + Duration::from_secs(120);

    loop {
        match fs::create_dir(&path) {
            Ok(()) => {
                if let Err(error) = fs::write(
                    cli_command_lock_owner_path(&path),
                    std::process::id().to_string(),
                ) {
                    if error.kind() == io::ErrorKind::NotFound {
                        let _ = fs::remove_dir(&path);
                        continue;
                    }
                    let _ = fs::remove_dir(&path);
                    return Err(format!(
                        "failed to record CLI command lock owner '{}': {error}",
                        path.display()
                    )
                    .into());
                }
                return Ok(CliCommandLock { path });
            }
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                if clear_stale_cli_command_lock(&path)? {
                    continue;
                }
                if Instant::now() >= deadline {
                    return Err(format!(
                        "timed out waiting for CLI command lock '{}'",
                        path.display()
                    )
                    .into());
                }
                std::thread::sleep(Duration::from_millis(10));
            }
            Err(error) => {
                return Err(format!(
                    "failed to acquire CLI command lock '{}': {error}",
                    path.display()
                )
                .into());
            }
        }
    }
}

fn cli_command_lock_owner_path(path: &Path) -> PathBuf {
    path.join("owner.pid")
}

fn clear_stale_cli_command_lock(path: &Path) -> Result<bool, Box<dyn Error>> {
    let owner_path = cli_command_lock_owner_path(path);
    let owner_pid = match fs::read_to_string(&owner_path) {
        Ok(owner_pid) => Some(owner_pid),
        Err(error) if error.kind() == io::ErrorKind::NotFound => None,
        Err(error) => {
            return Err(format!(
                "failed to inspect CLI command lock owner '{}': {error}",
                owner_path.display()
            )
            .into())
        }
    };

    match owner_pid {
        Some(owner_pid) => {
            let owner_pid = owner_pid.trim();
            let parsed = owner_pid.parse::<u32>().ok();
            if let Some(owner_pid) = parsed {
                if process_id_exists(owner_pid) {
                    return Ok(false);
                }
            } else if !lock_dir_is_stale(path)? {
                return Ok(false);
            }

            let _ = fs::remove_file(&owner_path);
            match fs::remove_dir(path) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => Ok(false),
                Err(error) => Err(format!(
                    "failed to clear stale CLI command lock '{}': {error}",
                    path.display()
                )
                .into()),
            }
        }
        None => {
            if !lock_dir_is_stale(path)? {
                return Ok(false);
            }

            match fs::remove_dir(path) {
                Ok(()) => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(true),
                Err(error) if error.kind() == io::ErrorKind::DirectoryNotEmpty => Ok(false),
                Err(error) => Err(format!(
                    "failed to clear stale CLI command lock '{}': {error}",
                    path.display()
                )
                .into()),
            }
        }
    }
}

fn process_id_exists(pid: u32) -> bool {
    let Ok(pid) = libc::pid_t::try_from(pid) else {
        return false;
    };

    // SAFETY: `kill(pid, 0)` does not send a signal; it only asks the kernel
    // whether the process exists and whether this process may signal it.
    if unsafe { libc::kill(pid, 0) } == 0 {
        return true;
    }

    std::io::Error::last_os_error().raw_os_error() == Some(libc::EPERM)
}

fn lock_dir_is_stale(path: &Path) -> Result<bool, Box<dyn Error>> {
    let modified = match fs::metadata(path).and_then(|metadata| metadata.modified()) {
        Ok(modified) => modified,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(true),
        Err(error) => {
            return Err(format!(
                "failed to inspect CLI command lock '{}': {error}",
                path.display()
            )
            .into())
        }
    };
    Ok(modified.elapsed().unwrap_or_default() >= Duration::from_secs(2))
}

#[track_caller]
pub(crate) fn assert_success(output: &Output) {
    assert_eq!(
        output.status.code(),
        Some(0),
        "expected successful command, got status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status,
        stdout(output),
        stderr(output)
    );
    assert!(stdout(output).is_empty(), "stdout should be empty");
    assert!(stderr(output).is_empty(), "stderr should be empty");
}

pub(crate) fn assert_clap_failure(output: &Output) {
    assert_eq!(output.status.code(), Some(1));
    assert!(
        stdout(output).is_empty(),
        "clap errors must not produce stdout"
    );
    assert!(
        !stderr(output).is_empty(),
        "clap errors must produce stderr"
    );
}

pub(crate) fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

pub(crate) fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}
