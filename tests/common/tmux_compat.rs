use std::error::Error;
use std::ffi::OsString;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use crate::common::{assert_socket_directory_empty, CliHarness};

pub(crate) const FROZEN_TMUX_ENV: &str = "RMUX_FROZEN_TMUX";
pub(crate) const DEFAULT_FROZEN_TMUX_PATH: &str =
    "/opt/rmux/reference/tmux-frozen/31d77e29b6c9fbb07d032018da78db3a8a38d979/tmux";
pub(crate) const SYSTEM_TMUX_PATH: &str = "/usr/bin/tmux";
const FROZEN_TMUX_SOURCE_SHA: &str = "31d77e29b6c9fbb07d032018da78db3a8a38d979";
const TMUX_REFERENCE_ROOT: &str = "/opt/rmux/reference/tmux";
pub(crate) const FROZEN_TMUX_REFERENCE_REL_PATH: &str =
    "tests/reference/tmux_compat/frozen_reference.yaml";
pub(crate) const DEFAULT_TMUX_COMPAT_TERM: &str = "xterm-256color";
pub(crate) const PTY_SERIALIZATION_NOTE: &str =
    "PTY-heavy tmux compatibility cases must use an explicit focused serialization guard instead of requiring --test-threads=1 globally.";
pub(crate) const TMUX_COMPAT_PREREQUISITES_NOTE: &str =
    "Overlay and prompt-history compatibility uses deterministic attached-client and PTY-aware fixtures with registered attaches, 80x24 terminal sizes, TERM=xterm-256color normalization, explicit LC_ALL/LC_CTYPE=C.UTF-8 for width-sensitive cases, the frozen tmux authority record, and a working man executable for reproduction.";

pub(crate) struct TmuxCompatHarness {
    rmux: CliHarness,
    tmux_socket_dir: PathBuf,
    tmux_socket_path: PathBuf,
}

impl TmuxCompatHarness {
    pub(crate) fn new(label: &str) -> Result<Self, Box<dyn Error>> {
        let rmux = CliHarness::new(label)?;
        let tmux_socket_dir = rmux.tmpdir().join("tmux-sockets");
        fs::create_dir_all(&tmux_socket_dir)?;
        let tmux_socket_path = tmux_socket_dir.join("tmux.sock");

        Ok(Self {
            rmux,
            tmux_socket_dir,
            tmux_socket_path,
        })
    }

    pub(crate) fn run_rmux(&self, argv: &[&str]) -> Result<CapturedCommand, Box<dyn Error>> {
        self.run_rmux_with(argv, &TmuxCompatRunConfig::default())
    }

    pub(crate) fn run_pair(
        &self,
        tmux_binary: &Path,
        argv: &[&str],
    ) -> Result<TmuxCompatRun, Box<dyn Error>> {
        self.run_pair_with(tmux_binary, argv, TmuxCompatRunConfig::default())
    }

    pub(crate) fn run_pair_with(
        &self,
        tmux_binary: &Path,
        argv: &[&str],
        config: TmuxCompatRunConfig,
    ) -> Result<TmuxCompatRun, Box<dyn Error>> {
        Ok(TmuxCompatRun {
            tmux: self.run_tmux_with(tmux_binary, argv, &config)?,
            rmux: self.run_rmux_with(argv, &config)?,
        })
    }

    pub(crate) fn assert_socket_dirs_clean(&self) -> Result<(), Box<dyn Error>> {
        assert_socket_directory_empty(self.rmux.socket_path())?;
        assert_directory_empty(&self.tmux_socket_dir)?;
        Ok(())
    }

    pub(crate) fn rmux_socket_path(&self) -> &Path {
        self.rmux.socket_path()
    }

    pub(crate) fn tmux_socket_path(&self) -> &Path {
        &self.tmux_socket_path
    }

    pub(crate) fn rmux_socket_dir(&self) -> &Path {
        self.rmux
            .socket_path()
            .parent()
            .expect("rmux socket path always has a parent")
    }

    pub(crate) fn tmux_socket_dir(&self) -> &Path {
        &self.tmux_socket_dir
    }

    pub(crate) fn tmpdir(&self) -> &Path {
        self.rmux.tmpdir()
    }

    pub(crate) fn run_rmux_with(
        &self,
        argv: &[&str],
        config: &TmuxCompatRunConfig,
    ) -> Result<CapturedCommand, Box<dyn Error>> {
        let mut command = self.rmux.base_command();
        command.args(argv);
        let environment_overrides = config.environment.overrides(self.rmux.tmpdir());
        config.environment.apply(&mut command, self.rmux.tmpdir());
        run_bounded(
            command,
            CommandCapture {
                program: "rmux",
                program_path: PathBuf::from(env!("CARGO_BIN_EXE_rmux")),
                requested_argv: requested_argv(argv),
                effective_argv: requested_argv(argv),
                environment_overrides,
                socket_dir: self.rmux_socket_dir().to_path_buf(),
                timeout: config.timeout,
            },
        )
    }

    fn run_tmux_with(
        &self,
        tmux_binary: &Path,
        argv: &[&str],
        config: &TmuxCompatRunConfig,
    ) -> Result<CapturedCommand, Box<dyn Error>> {
        let mut command = Command::new(tmux_binary);
        command.arg("-S").arg(&self.tmux_socket_path).args(argv);
        let environment_overrides = config.environment.overrides(self.rmux.tmpdir());
        config.environment.apply(&mut command, self.rmux.tmpdir());
        let mut effective_argv = vec![
            OsString::from("-S"),
            self.tmux_socket_path.as_os_str().to_owned(),
        ];
        effective_argv.extend(requested_argv(argv));
        run_bounded(
            command,
            CommandCapture {
                program: "tmux",
                program_path: tmux_binary.to_path_buf(),
                requested_argv: requested_argv(argv),
                effective_argv,
                environment_overrides,
                socket_dir: self.tmux_socket_dir.clone(),
                timeout: config.timeout,
            },
        )
    }
}

impl Drop for TmuxCompatHarness {
    fn drop(&mut self) {
        shutdown_tmux_server(&self.tmux_socket_path);
        let _ = fs::remove_file(&self.tmux_socket_path);
        let _ = fs::remove_dir(&self.tmux_socket_dir);
    }
}

#[derive(Clone)]
pub(crate) struct TmuxCompatRunConfig {
    timeout: Duration,
    environment: TmuxCompatEnvironment,
}

impl TmuxCompatRunConfig {
    pub(crate) fn with_timeout(mut self, timeout: Duration) -> Self {
        self.timeout = timeout;
        self
    }

    pub(crate) fn with_tmpdir(mut self, tmpdir: impl Into<PathBuf>) -> Self {
        self.environment.tmpdir = Some(tmpdir.into());
        self
    }

    pub(crate) fn with_tmux(mut self, tmux: impl Into<OsString>) -> Self {
        self.environment.tmux = Some(Some(tmux.into()));
        self
    }

    pub(crate) fn without_tmux(mut self) -> Self {
        self.environment.tmux = Some(None);
        self
    }

    pub(crate) fn with_term(mut self, term: impl Into<OsString>) -> Self {
        self.environment.term = Some(term.into());
        self
    }

    pub(crate) fn with_env(
        mut self,
        name: impl Into<OsString>,
        value: impl Into<OsString>,
    ) -> Self {
        self.environment
            .extra
            .push((name.into(), Some(value.into())));
        self
    }

    pub(crate) fn without_env(mut self, name: impl Into<OsString>) -> Self {
        self.environment.extra.push((name.into(), None));
        self
    }
}

impl Default for TmuxCompatRunConfig {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(5),
            environment: TmuxCompatEnvironment::default(),
        }
    }
}

#[derive(Clone)]
struct TmuxCompatEnvironment {
    tmpdir: Option<PathBuf>,
    tmux: Option<Option<OsString>>,
    term: Option<OsString>,
    extra: Vec<(OsString, Option<OsString>)>,
}

impl TmuxCompatEnvironment {
    fn overrides(&self, default_tmpdir: &Path) -> EnvironmentOverrides {
        let tmpdir = self.tmpdir.as_deref().unwrap_or(default_tmpdir);
        let mut overrides = vec![
            (
                OsString::from("TMPDIR"),
                Some(tmpdir.as_os_str().to_owned()),
            ),
            (
                OsString::from("RMUX_TMPDIR"),
                Some(tmpdir.as_os_str().to_owned()),
            ),
            (
                OsString::from("TMUX_TMPDIR"),
                Some(tmpdir.as_os_str().to_owned()),
            ),
        ];

        match self.tmux.as_ref() {
            Some(Some(value)) => overrides.push((OsString::from("TMUX"), Some(value.clone()))),
            Some(None) | None => overrides.push((OsString::from("TMUX"), None)),
        }

        if let Some(term) = self.term.as_ref() {
            overrides.push((OsString::from("TERM"), Some(term.clone())));
        }

        overrides.extend(self.extra.iter().cloned());
        overrides
    }

    fn apply(&self, command: &mut Command, default_tmpdir: &Path) {
        command.stdin(Stdio::null());
        command.stdout(Stdio::piped());
        command.stderr(Stdio::piped());
        for (name, value) in self.overrides(default_tmpdir) {
            match value {
                Some(value) => {
                    command.env(name, value);
                }
                None => {
                    command.env_remove(name);
                }
            }
        }
    }
}

impl Default for TmuxCompatEnvironment {
    fn default() -> Self {
        Self {
            tmpdir: None,
            tmux: Some(None),
            term: Some(OsString::from(DEFAULT_TMUX_COMPAT_TERM)),
            extra: Vec::new(),
        }
    }
}

pub(crate) struct TmuxCompatRun {
    pub(crate) tmux: CapturedCommand,
    pub(crate) rmux: CapturedCommand,
}

pub(crate) type EnvironmentOverrides = Vec<(OsString, Option<OsString>)>;

pub(crate) struct CapturedCommand {
    pub(crate) program: &'static str,
    pub(crate) program_path: PathBuf,
    pub(crate) requested_argv: Vec<OsString>,
    pub(crate) effective_argv: Vec<OsString>,
    pub(crate) environment_overrides: EnvironmentOverrides,
    pub(crate) socket_dir: PathBuf,
    pub(crate) timeout: Duration,
    pub(crate) stdout: Vec<u8>,
    pub(crate) stderr: Vec<u8>,
    pub(crate) status_code: Option<i32>,
    pub(crate) timed_out: bool,
}

impl CapturedCommand {
    pub(crate) fn stdout_string(&self) -> String {
        String::from_utf8_lossy(&self.stdout).into_owned()
    }

    pub(crate) fn stderr_string(&self) -> String {
        String::from_utf8_lossy(&self.stderr).into_owned()
    }
}

pub(crate) enum FrozenTmuxBinary {
    Available(PathBuf),
    Unavailable {
        checked_path: PathBuf,
        reason: String,
    },
}

impl FrozenTmuxBinary {
    pub(crate) fn discover() -> Self {
        let checked_path = std::env::var_os(FROZEN_TMUX_ENV)
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from(DEFAULT_FROZEN_TMUX_PATH));

        Self::discover_at(checked_path)
    }

    pub(crate) fn discover_at(checked_path: PathBuf) -> Self {
        if is_system_tmux_path(&checked_path) {
            return Self::Unavailable {
                checked_path,
                reason: format!(
                    "{SYSTEM_TMUX_PATH} is the host tmux, not the frozen reference build"
                ),
            };
        }

        let recorded = match RecordedFrozenTmuxBinary::load() {
            Ok(recorded) => recorded,
            Err(reason) => {
                return Self::Unavailable {
                    checked_path,
                    reason,
                };
            }
        };

        if recorded.source_sha != FROZEN_TMUX_SOURCE_SHA {
            return Self::Unavailable {
                checked_path,
                reason: format!(
                    "frozen tmux reference records source SHA {} instead of {FROZEN_TMUX_SOURCE_SHA}",
                    recorded.source_sha
                ),
            };
        }
        if recorded
            .build_directory_path
            .starts_with(Path::new(TMUX_REFERENCE_ROOT))
        {
            return Self::Unavailable {
                checked_path,
                reason: format!(
                    "frozen tmux reference records an in-tree build directory '{}'",
                    recorded.build_directory_path.display()
                ),
            };
        }
        if recorded.result != "available" {
            return Self::Unavailable {
                checked_path,
                reason: format!(
                    "frozen tmux reference does not record a trusted binary (result: {})",
                    recorded.result
                ),
            };
        }

        let Some(recorded_binary_path) = recorded.resulting_binary_path else {
            return Self::Unavailable {
                checked_path,
                reason: "frozen tmux reference is missing the recorded binary path".to_owned(),
            };
        };
        if !same_path(&checked_path, &recorded_binary_path) {
            return Self::Unavailable {
                checked_path,
                reason: format!(
                    "candidate does not match the recorded frozen tmux binary '{}'",
                    recorded_binary_path.display()
                ),
            };
        };

        let Some(recorded_sha256) = recorded.binary_sha256 else {
            return Self::Unavailable {
                checked_path,
                reason: "frozen tmux reference is missing the binary sha256".to_owned(),
            };
        };

        match executable_metadata(&checked_path) {
            Ok(()) => match sha256sum(&checked_path) {
                Ok(actual_sha256) if actual_sha256 == recorded_sha256 => Self::Available(checked_path),
                Ok(actual_sha256) => Self::Unavailable {
                    checked_path,
                    reason: format!(
                        "candidate sha256 {actual_sha256} does not match frozen tmux reference {recorded_sha256}"
                    ),
                },
                Err(reason) => Self::Unavailable { checked_path, reason },
            },
            Err(reason) => Self::Unavailable {
                checked_path,
                reason,
            },
        }
    }
}

fn is_system_tmux_path(path: &Path) -> bool {
    if path == Path::new(SYSTEM_TMUX_PATH) {
        return true;
    }

    let Ok(system_tmux) = fs::canonicalize(SYSTEM_TMUX_PATH) else {
        return false;
    };
    fs::canonicalize(path).is_ok_and(|candidate| candidate == system_tmux)
}

fn run_bounded(
    mut command: Command,
    capture: CommandCapture,
) -> Result<CapturedCommand, Box<dyn Error>> {
    let mut child = command.spawn()?;
    let deadline = Instant::now() + capture.timeout;
    let mut timed_out = false;

    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            if let Err(error) = child.kill()
                && error.kind() != io::ErrorKind::InvalidInput {
                    return Err(error.into());
                }
            timed_out = true;
            break;
        }
        std::thread::sleep(Duration::from_millis(10));
    }

    let output = child.wait_with_output()?;
    Ok(CapturedCommand {
        program: capture.program,
        program_path: capture.program_path,
        requested_argv: capture.requested_argv,
        effective_argv: capture.effective_argv,
        environment_overrides: capture.environment_overrides,
        socket_dir: capture.socket_dir,
        timeout: capture.timeout,
        stdout: output.stdout,
        stderr: output.stderr,
        status_code: output.status.code(),
        timed_out,
    })
}

struct CommandCapture {
    program: &'static str,
    program_path: PathBuf,
    requested_argv: Vec<OsString>,
    effective_argv: Vec<OsString>,
    environment_overrides: EnvironmentOverrides,
    socket_dir: PathBuf,
    timeout: Duration,
}

fn requested_argv(argv: &[&str]) -> Vec<OsString> {
    argv.iter().map(OsString::from).collect()
}

fn shutdown_tmux_server(socket_path: &Path) {
    if !socket_path.exists() {
        return;
    }

    let FrozenTmuxBinary::Available(tmux_binary) = FrozenTmuxBinary::discover() else {
        return;
    };

    let _ = Command::new(tmux_binary)
        .env_remove("TMUX")
        .arg("-S")
        .arg(socket_path)
        .arg("kill-server")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status();
}

fn executable_metadata(path: &Path) -> Result<(), String> {
    let metadata = fs::metadata(path)
        .map_err(|error| format!("{} is unavailable: {error}", path.display()))?;
    if !metadata.is_file() {
        return Err(format!("{} is not a regular file", path.display()));
    }

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;

        if metadata.permissions().mode() & 0o111 == 0 {
            return Err(format!("{} is not executable", path.display()));
        }
    }

    Ok(())
}

#[derive(Debug)]
struct RecordedFrozenTmuxBinary {
    source_sha: String,
    build_directory_path: PathBuf,
    resulting_binary_path: Option<PathBuf>,
    binary_sha256: Option<String>,
    result: String,
}

impl RecordedFrozenTmuxBinary {
    fn load() -> Result<Self, String> {
        let reference_path =
            PathBuf::from(env!("CARGO_MANIFEST_DIR")).join(FROZEN_TMUX_REFERENCE_REL_PATH);
        let contents = fs::read_to_string(&reference_path).map_err(|error| {
            format!(
                "failed to read frozen tmux reference '{}': {error}",
                reference_path.display()
            )
        })?;
        let section = yaml_section(
            &contents,
            "frozen_tmux_binary_acquisition:",
            "baseline_test_floor:",
        )
        .map_err(|error| {
            format!(
                "failed to parse frozen tmux reference '{}': {error}",
                reference_path.display()
            )
        })?;

        Ok(Self {
            source_sha: yaml_value(section, "source_sha")?,
            build_directory_path: PathBuf::from(yaml_value(section, "build_directory_path")?),
            resulting_binary_path: yaml_optional_value(section, "resulting_binary_path")?
                .map(PathBuf::from),
            binary_sha256: yaml_optional_value(section, "binary_sha256")?,
            result: yaml_value(section, "result")?,
        })
    }
}

fn yaml_section<'a>(contents: &'a str, start: &str, end: &str) -> Result<&'a str, String> {
    contents
        .split_once(start)
        .and_then(|(_, tail)| tail.split_once(end).map(|(section, _)| section))
        .ok_or_else(|| format!("missing yaml section {start}"))
}

fn yaml_value(section: &str, key: &str) -> Result<String, String> {
    yaml_optional_value(section, key)?.ok_or_else(|| format!("missing yaml key {key}"))
}

fn yaml_optional_value(section: &str, key: &str) -> Result<Option<String>, String> {
    let prefix = format!("  {key}: ");
    let raw = match section.lines().find_map(|line| line.strip_prefix(&prefix)) {
        Some(raw) => raw.trim(),
        None => return Ok(None),
    };
    if raw == "null" {
        return Ok(None);
    }
    Ok(Some(raw.trim_matches('"').to_owned()))
}

fn same_path(left: &Path, right: &Path) -> bool {
    match (fs::canonicalize(left), fs::canonicalize(right)) {
        (Ok(left), Ok(right)) => left == right,
        _ => left == right,
    }
}

fn sha256sum(path: &Path) -> Result<String, String> {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .map_err(|error| format!("failed to hash {} with sha256sum: {error}", path.display()))?;
    if !output.status.success() {
        return Err(format!(
            "sha256sum failed for {} with status {:?}",
            path.display(),
            output.status.code()
        ));
    }
    let stdout = String::from_utf8_lossy(&output.stdout);
    stdout
        .split_whitespace()
        .next()
        .map(ToOwned::to_owned)
        .ok_or_else(|| format!("sha256sum did not return a digest for {}", path.display()))
}

fn assert_directory_empty(path: &Path) -> Result<(), Box<dyn Error>> {
    match fs::read_dir(path) {
        Ok(entries) => {
            let entries = entries
                .map(|entry| entry.map(|entry| entry.file_name()))
                .collect::<Result<Vec<_>, io::Error>>()?;
            assert!(
                entries.is_empty(),
                "expected '{}' to be empty, found {entries:?}",
                path.display()
            );
            Ok(())
        }
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error.into()),
    }
}
