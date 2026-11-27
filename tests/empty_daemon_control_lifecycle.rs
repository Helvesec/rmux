#![cfg(any(target_os = "linux", target_os = "macos"))]

mod common;

use std::error::Error;
use std::fs;
use std::io::{self, Write};
use std::path::Path;
use std::process::{Output, Stdio};
use std::time::{Duration, Instant};

use common::{assert_success, CliHarness, BINARY_OVERRIDE_ENV};

const _: fn(&'static str) -> &'static str = rmux_os::build_identity::cargo_binary;

const PROCESS_TIMEOUT: Duration = Duration::from_secs(5);
const SHUTDOWN_TIMEOUT: Duration = Duration::from_secs(2);
// The pid is recorded only once the launcher runs. This bounds that wait only: no
// expected daemon lifetime or client exit uses it. An identity build's cold proof
// of a never-seen launcher path is not product startup: it costs 10s or more of
// CPU, so `prove_launcher` completes it before this bound can start.
const GENERATION_OBSERVATION_TIMEOUT: Duration = Duration::from_secs(30);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ProcessGeneration {
    pid: u32,
    start_tick: u64,
}

#[test]
fn failed_startup_client_releases_its_unadmitted_empty_daemon() -> Result<(), Box<dyn Error>> {
    // On failure retain the fixture for inspection: neither a signal nor a
    // kill-server request may manufacture the daemon's natural termination.
    let directory = common::unique_tmpdir("startup-client-eof");
    fs::create_dir_all(&directory)?;
    let socket = common::default_socket_path_in(&directory)?;
    let launcher = directory.join("launcher.sh");
    let pid_path = directory.join("daemon.pid");
    common::write_hidden_launcher(&launcher, &pid_path)?;
    let config = directory.join("slow-startup.conf");
    fs::write(&config, "run-shell '/bin/sleep 25'\n")?;
    prove_launcher(&launcher)?;
    let binary =
        rmux_os::build_identity::resolve(std::ffi::OsStr::new(env!("CARGO_BIN_EXE_rmux")))?;
    let mut child = std::process::Command::new(binary)
        .env("RMUX_TMPDIR", &directory)
        .env("HOME", directory.join("home"))
        .env("XDG_CONFIG_HOME", directory.join("xdg"))
        .env("RMUX_DISABLE_TMUX_FALLBACK", "1")
        .env(common::BINARY_OVERRIDE_TEST_OPT_IN_ENV, "1")
        .env_remove("RMUX")
        .env_remove("TMUX")
        .arg("-S")
        .arg(&socket)
        .arg("-f")
        .arg(&config)
        .args(["-C", "start-server"])
        .env(BINARY_OVERRIDE_ENV, &launcher)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let generation = wait_for_recorded_generation(&pid_path, GENERATION_OBSERVATION_TIMEOUT)?;
    let deadline = Instant::now() + Duration::from_secs(30);
    while child.try_wait()?.is_none() {
        assert!(
            Instant::now() < deadline,
            "startup client did not exit naturally"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    let output = child.wait_with_output()?;
    assert_eq!(output.status.code(), Some(1));
    assert!(
        output.stdout.is_empty(),
        "the control command must not be admitted"
    );
    assert!(
        String::from_utf8_lossy(&output.stderr).contains("still loading startup config"),
        "unexpected startup failure: {:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(
        wait_for_generation_exit(generation, Duration::from_secs(10))?,
        "unadmitted client exited naturally but {generation:?} survived at {}",
        socket.display()
    );
    Ok(())
}

#[test]
fn unadmitted_control_input_cannot_keep_an_auto_started_empty_daemon() -> Result<(), Box<dyn Error>>
{
    // Keep all four stdin populations and the output/exit assertions together:
    // observing the daemon generation must not admit another control command
    // or keep an extra client connected while empty-server cleanup runs.
    let harness = CliHarness::new("control-empty-input")?;
    let _cleanup = harness.auto_start_cleanup()?;
    let cases = [
        ("valid-line", b"list-sessions\n".as_slice()),
        ("invalid-line", b"w13-not-a-command\n".as_slice()),
        ("partial-line", b"list-sessions".as_slice()),
        ("blank", b" \t\r\n".as_slice()),
    ];
    let mut failures = Vec::new();

    for (label, input) in cases {
        let _ = fs::remove_file(harness.pid_path());
        let (output, generation) = run_control_start_server(&harness, input)?;
        let rendered = String::from_utf8_lossy(&output.stdout);
        if output.status.code() != Some(0) {
            failures.push(format!(
                "{label}: status={:?}, stderr={:?}",
                output.status.code(),
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        if !output.stderr.is_empty() {
            failures.push(format!(
                "{label}: stderr={:?}",
                String::from_utf8_lossy(&output.stderr)
            ));
        }
        if rendered.matches("%begin ").count() != 1 {
            failures.push(format!(
                "{label}: the stdin command was admitted, so this run does not isolate the unadmitted-input boundary: {rendered:?}"
            ));
        }
        if !rendered.ends_with("%exit\n") {
            failures.push(format!("{label}: incomplete control output {rendered:?}"));
        }

        if !wait_for_generation_exit(generation, SHUTDOWN_TIMEOUT)? {
            failures.push(format!("{label}: {generation:?} remained alive"));
            let killed = harness.run(&["kill-server"])?;
            assert_success(&killed);
            assert!(
                wait_for_generation_exit(generation, SHUTDOWN_TIMEOUT)?,
                "{label}: cleanup could not stop {generation:?}"
            );
        }
    }

    assert!(
        failures.is_empty(),
        "unadmitted stdin suppressed typed empty-server cleanup: {}",
        failures.join("; ")
    );
    Ok(())
}

#[test]
fn admitted_control_command_finishes_before_typed_empty_cleanup() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("control-empty-admitted")?;
    let _cleanup = harness.auto_start_cleanup()?;
    let arguments = [
        "-C",
        "start-server",
        ";",
        "new-session",
        "-s",
        "alpha",
        "/bin/sleep 20",
        ";",
        "display-message",
        "-p",
        "PID=#{pid}",
    ];
    let input = b"display-message -p ADMITTED\nw13-not-a-command\nkill-session -t alpha\n";

    let (output, generation) = run_control(&harness, &arguments, input)?;

    assert_eq!(
        output.status.code(),
        Some(0),
        "stderr={:?}",
        String::from_utf8_lossy(&output.stderr)
    );
    assert!(output.stderr.is_empty());
    let rendered = String::from_utf8(output.stdout)?;
    assert!(rendered.contains("ADMITTED\n"), "{rendered:?}");
    assert!(
        rendered.lines().any(|line| line.starts_with("%error ")),
        "the admitted invalid command must receive a typed control error: {rendered:?}"
    );
    assert!(rendered.ends_with("%exit\n"), "{rendered:?}");
    assert!(
        wait_for_generation_exit(generation, SHUTDOWN_TIMEOUT)?,
        "the admitted kill must drain before typed cleanup stops {generation:?}"
    );
    Ok(())
}

#[test]
fn recorded_startup_observes_the_live_daemon_generation() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("control-launch-generation")?;
    let _cleanup = harness.auto_start_cleanup()?;
    let arguments = [
        "-C",
        "new-session",
        "-d",
        "-s",
        "alpha",
        "/bin/sleep 20",
        ";",
        "display-message",
        "-p",
        "PID=#{pid}",
    ];
    let (output, generation) = run_control(&harness, &arguments, b"")?;
    assert_eq!(output.status.code(), Some(0));
    assert!(output.stderr.is_empty(), "{:?}", output.stderr);
    let daemon_pid = format!("PID={}", generation.pid);
    assert!(String::from_utf8(output.stdout)?
        .lines()
        .any(|line| line == daemon_pid));
    assert_eq!(
        process_start_tick(generation.pid)?,
        Some(generation.start_tick)
    );
    assert_success(&harness.run(&["kill-server"])?);
    assert!(wait_for_generation_exit(generation, SHUTDOWN_TIMEOUT)?);
    Ok(())
}

#[test]
fn launcher_proof_precedes_the_generation_bound() -> Result<(), Box<dyn Error>> {
    // A cold proof outlasts this check by seconds, so without the prior proof
    // the client that was just spawned cannot have published one yet.
    let Some(target) = option_env!("RMUX_IDENTITY_TARGET") else {
        return Ok(());
    };
    let harness = CliHarness::new("control-launcher-proof")?;
    let _cleanup = harness.auto_start_cleanup()?;
    let observation = StartupObservation::new(&harness)?;
    let mut child = spawn_control(&harness, &observation, &["-C", "start-server"])?;
    let proven = launcher_selection_proven(Path::new(target), harness.launcher_path());
    let _ = child.kill();
    let _ = child.wait();
    assert!(
        proven?,
        "no selection proof existed for {} when the client started",
        harness.launcher_path().display()
    );
    Ok(())
}

fn launcher_selection_proven(target: &Path, launcher: &Path) -> Result<bool, Box<dyn Error>> {
    let binary = format!("\"binary\": \"{}\"", launcher.display());
    for entry in fs::read_dir(target.join("identity-cache"))? {
        let path = entry?.path();
        if path
            .extension()
            .is_some_and(|extension| extension == "json")
        {
            let proof = fs::read_to_string(path)?;
            if proof.contains(&binary) && proof.contains("\"kind\": \"selection\"") {
                return Ok(true);
            }
        }
    }
    Ok(false)
}

fn run_control_start_server(
    harness: &CliHarness,
    input: &[u8],
) -> Result<(Output, ProcessGeneration), Box<dyn Error>> {
    run_control(harness, &["-C", "start-server"], input)
}

fn run_control(
    harness: &CliHarness,
    arguments: &[&str],
    input: &[u8],
) -> Result<(Output, ProcessGeneration), Box<dyn Error>> {
    let mut observation = StartupObservation::new(harness)?;
    let mut child = spawn_control(harness, &observation, arguments)?;
    let mut stdin = Some(child.stdin.take().expect("control stdin is piped"));
    stdin
        .as_mut()
        .expect("control stdin remains open")
        .write_all(input)?;
    stdin
        .as_mut()
        .expect("control stdin remains open")
        .flush()?;
    drop(stdin.take());

    let generation =
        match wait_for_recorded_generation(harness.pid_path(), GENERATION_OBSERVATION_TIMEOUT) {
            Ok(generation) => {
                observation.release()?;
                generation
            }
            Err(error) => {
                drop(observation);
                let _ = child.kill();
                let output = child.wait_with_output()?;
                return Err(format!(
                    "{error}; control status={:?}, stdout={:?}, stderr={:?}",
                    output.status.code(),
                    String::from_utf8_lossy(&output.stdout),
                    String::from_utf8_lossy(&output.stderr),
                )
                .into());
            }
        };
    let deadline = Instant::now() + PROCESS_TIMEOUT;
    loop {
        if child.try_wait()?.is_some() {
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            return Err("timed out waiting for control start-server to exit".into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    drop(stdin.take());
    Ok((child.wait_with_output()?, generation))
}

fn spawn_control(
    harness: &CliHarness,
    observation: &StartupObservation,
    arguments: &[&str],
) -> Result<std::process::Child, Box<dyn Error>> {
    prove_launcher(harness.launcher_path())?;
    Ok(harness
        .base_command()
        .arg("-f")
        .arg(&observation.config)
        .args(arguments)
        .env(BINARY_OVERRIDE_ENV, harness.launcher_path())
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?)
}

/// The client selects the launcher before spawning it, so an unproven path would
/// spend the generation bound on the identity harness instead of on the launcher.
/// Ordinary builds have no verifier and return immediately.
fn prove_launcher(launcher: &Path) -> Result<(), Box<dyn Error>> {
    rmux_os::build_identity::resolve(launcher.as_os_str())?;
    Ok(())
}

/// Observe the actual daemon while startup is pending, before its first control
/// command can finish. Blocking stdout is too late: the Unix server can finish
/// its control transport while stdout is still full. The startup command ends
/// before readiness admits control input; no extra client remains connected.
struct StartupObservation {
    path: std::path::PathBuf,
    config: std::path::PathBuf,
    pipe: fs::File,
    released: bool,
}

impl StartupObservation {
    fn new(harness: &CliHarness) -> Result<Self, Box<dyn Error>> {
        use std::ffi::CString;
        use std::os::unix::ffi::OsStrExt;

        let path = harness.launcher_path().with_extension("observe");
        let fifo_path = CString::new(path.as_os_str().as_bytes())?;
        // SAFETY: fifo_path is a live NUL-terminated path; mkfifo retains no pointer.
        if unsafe { libc::mkfifo(fifo_path.as_ptr(), 0o600) } == -1 {
            return Err(io::Error::last_os_error().into());
        }
        let pipe = fs::OpenOptions::new().read(true).write(true).open(&path)?;
        let observation = Self {
            config: path.with_extension("conf"),
            path,
            pipe,
            released: false,
        };
        let quote = |value: &str| format!("'{}'", value.replace('\'', "'\\''"));
        let command = format!(
            "IFS= read -r observed < {} && [ \"$observed\" = observed ]",
            quote(&observation.path.to_string_lossy()),
        );
        fs::write(
            &observation.config,
            format!("run-shell {}\n", quote(&command)),
        )?;
        Ok(observation)
    }

    fn release(&mut self) -> io::Result<()> {
        self.pipe.write_all(b"observed\n")?;
        self.released = true;
        Ok(())
    }
}

impl Drop for StartupObservation {
    fn drop(&mut self) {
        if !self.released {
            let _ = self.pipe.write_all(b"cancel\n");
        }
        let _ = fs::remove_file(&self.path);
        let _ = fs::remove_file(&self.config);
    }
}

fn wait_for_recorded_generation(
    pid_path: &Path,
    timeout: Duration,
) -> Result<ProcessGeneration, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Ok(pid) = fs::read_to_string(pid_path)
            .and_then(|value| value.trim().parse::<u32>().map_err(std::io::Error::other))
        {
            if let Some(start_tick) = process_start_tick(pid)? {
                return Ok(ProcessGeneration { pid, start_tick });
            }
        }
        if Instant::now() >= deadline {
            return Err(format!(
                "timed out reading a live daemon generation from '{}'",
                pid_path.display()
            )
            .into());
        }
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn wait_for_generation_exit(
    generation: ProcessGeneration,
    timeout: Duration,
) -> Result<bool, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        if process_start_tick(generation.pid)? != Some(generation.start_tick) {
            return Ok(true);
        }
        if Instant::now() >= deadline {
            return Ok(false);
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}

#[cfg(target_os = "linux")]
fn process_start_tick(pid: u32) -> Result<Option<u64>, Box<dyn Error>> {
    let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
        Ok(stat) => stat,
        Err(error)
            if error.kind() == std::io::ErrorKind::NotFound
                || error.raw_os_error() == Some(libc::ESRCH) =>
        {
            return Ok(None);
        }
        Err(error) => return Err(error.into()),
    };
    let fields = stat
        .rsplit_once(") ")
        .ok_or_else(|| format!("malformed /proc/{pid}/stat"))?
        .1
        .split_whitespace()
        .collect::<Vec<_>>();
    let start_tick = fields
        .get(19)
        .ok_or_else(|| format!("missing start tick in /proc/{pid}/stat"))?
        .parse::<u64>()?;
    Ok(Some(start_tick))
}

#[cfg(target_os = "macos")]
fn process_start_tick(pid: u32) -> Result<Option<u64>, Box<dyn Error>> {
    let pid = libc::pid_t::try_from(pid)?;
    let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
    let size = libc::c_int::try_from(std::mem::size_of::<libc::proc_bsdinfo>())?;
    let read = unsafe {
        // SAFETY: `info` is writable for `size` bytes; libproc does not retain it.
        libc::proc_pidinfo(
            pid,
            libc::PROC_PIDTBSDINFO,
            0,
            info.as_mut_ptr().cast(),
            size,
        )
    };
    if read <= 0 {
        let error = std::io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            return Ok(None);
        }
        return Err(error.into());
    }
    if read != size {
        return Err(std::io::Error::new(
            std::io::ErrorKind::InvalidData,
            format!("proc_pidinfo returned {read} of {size} bytes for process {pid}"),
        )
        .into());
    }
    // SAFETY: libproc reported that it initialized the full structure.
    let info = unsafe { info.assume_init() };
    let start_tick = info
        .pbi_start_tvsec
        .checked_mul(1_000_000)
        .and_then(|seconds| seconds.checked_add(info.pbi_start_tvusec))
        .ok_or_else(|| std::io::Error::other("process start time exceeds u64 microseconds"))?;
    Ok(Some(start_tick))
}
