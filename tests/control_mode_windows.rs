#![cfg(windows)]

use std::error::Error;
use std::fs::{self, File};
use std::io::{Read, Write};
use std::process::{Child, Command, ExitStatus, Output, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rmux_client::{connect, socket_path_for_label, ControlTransition};
use rmux_proto::{ClientTerminalContext, ControlMode, CONTROL_CONTROL_END, CONTROL_CONTROL_START};

#[path = "support/windows_cli_serial.rs"]
mod windows_cli_serial;

const CONTROL_TIMEOUT: Duration = Duration::from_secs(20);

#[test]
fn control_control_mode_uses_tmux_text_protocol() -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("control-control-mode")?;
    let label = unique_label("control-mode-windows")?;
    let _server = ServerGuard::new(label.clone());

    let cmd = cmd_exe();
    assert_command_success(
        rmux_command()
            .args([
                "-L",
                &label,
                "new-session",
                "-d",
                "-s",
                "alpha",
                cmd.as_str(),
                "/d",
                "/q",
            ])
            .stdin(Stdio::null())
            .output()?,
        "create detached session",
    )?;

    let input_path = temp_file_path(&label, "in");
    let output_path = temp_file_path(&label, "out");
    let error_path = temp_file_path(&label, "err");
    fs::write(&input_path, b"list-sessions\nbad-command\ndetach-client\n")?;

    let mut child = rmux_command()
        .args(["-L", &label, "-CC"])
        .stdin(Stdio::from(File::open(&input_path)?))
        .stdout(Stdio::from(File::create(&output_path)?))
        .stderr(Stdio::from(File::create(&error_path)?))
        .spawn()?;
    let status = wait_for_child_exit(&mut child, CONTROL_TIMEOUT)?;
    let rendered = fs::read_to_string(&output_path)?;
    let stderr = fs::read_to_string(&error_path)?;
    let _ = fs::remove_file(&input_path);
    let _ = fs::remove_file(&output_path);
    let _ = fs::remove_file(&error_path);

    assert_eq!(status.code(), Some(0));
    assert!(
        stderr.is_empty(),
        "control-mode stderr should stay empty, got: {stderr:?}"
    );
    assert!(rendered.starts_with(CONTROL_CONTROL_START));
    assert!(rendered.contains("%begin "));
    assert!(rendered.contains("%end "));
    assert!(rendered.contains("%error "));
    assert!(rendered.contains("parse error:"));
    assert!(rendered.contains("bad-command"));
    assert!(rendered.contains("alpha"));
    assert!(rendered.contains("%exit"));
    assert!(rendered.ends_with(CONTROL_CONTROL_END));

    Ok(())
}

#[test]
fn plain_control_mode_exits_after_stdin_eof_without_empty_line() -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("plain-control-mode")?;
    let label = unique_label("plain-control-mode-windows")?;
    let _server = ServerGuard::new(label.clone());

    let cmd = cmd_exe();
    assert_command_success(
        rmux_command()
            .args([
                "-L",
                &label,
                "new-session",
                "-d",
                "-s",
                "alpha",
                cmd.as_str(),
                "/d",
                "/q",
            ])
            .stdin(Stdio::null())
            .output()?,
        "create detached session",
    )?;

    let input_path = temp_file_path(&label, "in");
    let output_path = temp_file_path(&label, "out");
    let error_path = temp_file_path(&label, "err");
    fs::write(&input_path, b"list-sessions\n")?;

    let mut child = rmux_command()
        .args(["-L", &label, "-C"])
        .stdin(Stdio::from(File::open(&input_path)?))
        .stdout(Stdio::from(File::create(&output_path)?))
        .stderr(Stdio::from(File::create(&error_path)?))
        .spawn()?;
    let status = wait_for_child_exit(&mut child, CONTROL_TIMEOUT)?;
    let rendered = fs::read_to_string(&output_path)?;
    let stderr = fs::read_to_string(&error_path)?;
    let _ = fs::remove_file(&input_path);
    let _ = fs::remove_file(&output_path);
    let _ = fs::remove_file(&error_path);

    assert_eq!(status.code(), Some(0));
    assert!(
        stderr.is_empty(),
        "control-mode stderr should stay empty, got: {stderr:?}"
    );
    assert!(!rendered.starts_with(CONTROL_CONTROL_START));
    assert!(rendered.contains("%begin "));
    assert!(rendered.contains("%end "));
    assert!(rendered.contains("alpha"));
    assert!(rendered.contains("%exit"));

    Ok(())
}

#[test]
fn blocking_wait_for_is_cancelled_when_redirected_stdin_reaches_eof() -> Result<(), Box<dyn Error>>
{
    let _serial_guard = windows_cli_serial::acquire("control-wait-for-eof")?;
    let label = unique_label("control-wait-for-eof-windows")?;
    let _server = ServerGuard::new(label.clone());

    assert_command_success(
        rmux_command()
            .args([
                "-L",
                &label,
                "start-server",
                ";",
                "set-option",
                "-g",
                "exit-empty",
                "off",
            ])
            .stdin(Stdio::null())
            .output()?,
        "start control-mode server",
    )?;

    let input_path = temp_file_path(&label, "wait-in");
    let output_path = temp_file_path(&label, "wait-out");
    let error_path = temp_file_path(&label, "wait-err");
    fs::write(&input_path, b"wait-for control-eof-never-signalled\n")?;

    let mut child = rmux_command()
        .args(["-L", &label, "-C"])
        .stdin(Stdio::from(File::open(&input_path)?))
        .stdout(Stdio::from(File::create(&output_path)?))
        .stderr(Stdio::from(File::create(&error_path)?))
        .spawn()?;
    let status = wait_for_child_exit(&mut child, CONTROL_TIMEOUT)?;
    let rendered = fs::read_to_string(&output_path)?;
    let stderr = fs::read_to_string(&error_path)?;
    let _ = fs::remove_file(&input_path);
    let _ = fs::remove_file(&output_path);
    let _ = fs::remove_file(&error_path);

    assert_eq!(status.code(), Some(0));
    assert!(stderr.is_empty(), "unexpected stderr: {stderr:?}");
    assert!(
        rendered.contains("%begin "),
        "missing begin guard: {rendered:?}"
    );
    assert!(
        rendered.contains("%end "),
        "missing end guard: {rendered:?}"
    );
    assert!(
        !rendered.contains("%error "),
        "EOF is a clean cancellation: {rendered:?}"
    );
    assert!(
        rendered.contains("%exit"),
        "missing terminal exit: {rendered:?}"
    );
    Ok(())
}

#[test]
fn control_pipe_eof_after_attach_finishes_admitted_mutation_before_success(
) -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("control-attach-pipe-eof")?;
    let label = unique_label("control-attach-pipe-eof-windows")?;
    let _server = ServerGuard::new(label.clone());
    create_default_detached_session(&label, "alpha")?;

    const ADMITTED_MUTATIONS: usize = 512;
    let buffer_name = "control-eof-proof";
    let output_path = temp_file_path(&label, "out");
    let error_path = temp_file_path(&label, "err");
    let mut child = rmux_command()
        .args(["-L", &label, "-C"])
        .stdin(Stdio::piped())
        .stdout(Stdio::from(File::create(&output_path)?))
        .stderr(Stdio::from(File::create(&error_path)?))
        .spawn()?;
    let mut stdin = child.stdin.take().expect("control stdin is piped");
    writeln!(stdin, "attach-session -t alpha")?;
    for sequence in 0..ADMITTED_MUTATIONS {
        writeln!(stdin, "set-buffer -b {buffer_name} VALUE-{sequence}")?;
    }
    stdin.flush()?;
    drop(stdin);

    let status = wait_for_child_exit(&mut child, CONTROL_TIMEOUT)?;
    let rendered = fs::read_to_string(&output_path)?;
    let stderr = fs::read_to_string(&error_path)?;
    let _ = fs::remove_file(&output_path);
    let _ = fs::remove_file(&error_path);
    assert_eq!(
        status.code(),
        Some(0),
        "stderr={stderr:?}, stdout={rendered:?}"
    );
    assert!(stderr.is_empty(), "unexpected stderr: {stderr:?}");
    let begin_count = control_guard_count(&rendered, "%begin ");
    assert_eq!(
        begin_count,
        ADMITTED_MUTATIONS + 2,
        "ack, attach, and every admitted mutation need begin guards; \
         observed={begin_count}, terminal={:?}",
        rendered.lines().last()
    );
    let end_count = control_guard_count(&rendered, "%end ");
    assert_eq!(
        end_count,
        ADMITTED_MUTATIONS + 2,
        "ack, attach, and every admitted mutation need end guards; \
         observed={end_count}, terminal={:?}",
        rendered.lines().last()
    );
    assert_eq!(control_guard_count(&rendered, "%error "), 0, "{rendered:?}");
    assert_eq!(
        show_buffer(&label, buffer_name)?,
        format!("VALUE-{}", ADMITTED_MUTATIONS - 1),
        "a successful client exit must follow every admitted mutation"
    );
    assert!(
        rendered.ends_with("%exit\n"),
        "terminal record ordering: {rendered:?}"
    );

    Ok(())
}

#[test]
fn raw_control_transport_eof_after_attach_finishes_admitted_mutation() -> Result<(), Box<dyn Error>>
{
    let _serial_guard = windows_cli_serial::acquire("control-attach-raw-eof")?;
    let label = unique_label("control-attach-raw-eof-windows")?;
    let _server = ServerGuard::new(label.clone());
    create_default_detached_session(&label, "alpha")?;

    let connection = connect(&socket_path_for_label(&label)?)?;
    let transition =
        connection.begin_control_mode(ControlMode::Plain, ClientTerminalContext::default())?;
    let mut stream = match transition {
        ControlTransition::Upgraded(upgrade) => upgrade.into_stream(),
        ControlTransition::Rejected(response) => {
            return Err(format!("control upgrade was rejected: {response:?}").into());
        }
    };

    const ADMITTED_MUTATIONS: usize = 512;
    let buffer_name = "control-raw-eof-proof";
    writeln!(stream, "attach-session -t alpha")?;
    for sequence in 0..ADMITTED_MUTATIONS {
        writeln!(stream, "set-buffer -b {buffer_name} VALUE-{sequence}")?;
    }
    stream.flush()?;
    drop(stream);

    wait_for_buffer_after_transport_close(
        &label,
        buffer_name,
        &format!("VALUE-{}", ADMITTED_MUTATIONS - 1),
    )?;

    Ok(())
}

#[test]
fn control_control_stdin_eof_persists_until_transport_dies_then_reaps() -> Result<(), Box<dyn Error>>
{
    let _serial_guard = windows_cli_serial::acquire("control-control-transport-reap")?;
    let label = unique_label("control-control-transport-reap-windows")?;
    let _server = ServerGuard::new(label.clone());
    create_default_detached_session(&label, "alpha")?;

    let mut child = rmux_command()
        .args(["-L", &label, "-CC", "attach-session", "-t", "alpha"])
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    wait_for_attached_client(&label, "alpha", &mut child)?;
    thread::sleep(Duration::from_millis(500));
    assert!(
        child.try_wait()?.is_none(),
        "clean stdin EOF must keep an attached -CC client persistent"
    );

    child.kill()?;
    let _ = child.wait()?;
    for sequence in 0..8 {
        assert_command_success(
            rmux_command()
                .args([
                    "-L",
                    &label,
                    "send-keys",
                    "-t",
                    "alpha:0.0",
                    &format!("echo CONTROL-REAP-{sequence}"),
                    "Enter",
                ])
                .stdin(Stdio::null())
                .output()?,
            "trigger pane output after control transport loss",
        )?;
    }
    wait_for_no_control_clients(&label, "alpha")?;

    Ok(())
}

#[test]
fn control_argv_attach_batch_finishes_before_success() -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("control-attach-argv")?;
    let label = unique_label("control-attach-argv-windows")?;
    let _server = ServerGuard::new(label.clone());
    create_detached_session(&label, "alpha")?;

    let output = rmux_command()
        .args([
            "-L",
            &label,
            "-C",
            "attach-session",
            "-t",
            "alpha",
            ";",
            "set-buffer",
            "-b",
            "control-argv-proof",
            "ARGV",
        ])
        .stdin(Stdio::null())
        .output()?;
    assert_command_success(output.clone(), "run control argv attach batch")?;
    let rendered = String::from_utf8(output.stdout)?;
    let stderr = String::from_utf8(output.stderr)?;

    assert!(stderr.is_empty(), "unexpected stderr: {stderr:?}");
    assert_eq!(show_buffer(&label, "control-argv-proof")?, "ARGV");
    assert_eq!(control_guard_count(&rendered, "%begin "), 2, "{rendered:?}");
    assert_eq!(control_guard_count(&rendered, "%end "), 2, "{rendered:?}");
    assert_eq!(control_guard_count(&rendered, "%error "), 0, "{rendered:?}");
    assert!(rendered.ends_with("%exit\n"), "{rendered:?}");
    Ok(())
}

#[test]
fn control_open_pipe_keeps_follow_on_mutation_guarded_until_eof() -> Result<(), Box<dyn Error>> {
    let _serial_guard = windows_cli_serial::acquire("control-attach-open-pipe")?;
    let label = unique_label("control-attach-open-pipe-windows")?;
    let _server = ServerGuard::new(label.clone());
    create_detached_session(&label, "alpha")?;

    let mut child = rmux_command()
        .args(["-L", &label, "-C"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut stdin = child.stdin.take().expect("control stdin is piped");
    writeln!(stdin, "attach-session -t alpha")?;
    stdin.flush()?;
    wait_for_attached_client(&label, "alpha", &mut child)?;

    writeln!(stdin, "set-buffer -b control-open-pipe-proof OPEN")?;
    stdin.flush()?;
    wait_for_buffer(&label, "control-open-pipe-proof", "OPEN", &mut child)?;
    assert!(
        child.try_wait()?.is_none(),
        "open stdin must keep an attached control client alive"
    );
    drop(stdin);

    let status = wait_for_child_exit(&mut child, CONTROL_TIMEOUT)?;
    let rendered = read_child_pipe(child.stdout.take(), "stdout")?;
    let stderr = read_child_pipe(child.stderr.take(), "stderr")?;
    assert_eq!(status.code(), Some(0), "{stderr:?}");
    assert!(stderr.is_empty(), "unexpected stderr: {stderr:?}");
    assert_eq!(control_guard_count(&rendered, "%begin "), 3, "{rendered:?}");
    assert_eq!(control_guard_count(&rendered, "%end "), 3, "{rendered:?}");
    assert!(rendered.ends_with("%exit\n"), "{rendered:?}");
    Ok(())
}

fn rmux_command() -> Command {
    Command::new(rmux_os::build_identity::cargo_binary(env!(
        "CARGO_BIN_EXE_rmux"
    )))
}

fn unique_label(prefix: &str) -> Result<String, Box<dyn Error>> {
    let now = SystemTime::now().duration_since(UNIX_EPOCH)?.as_millis();
    Ok(format!("{prefix}-{}-{now}", std::process::id()))
}

fn temp_file_path(label: &str, extension: &str) -> std::path::PathBuf {
    std::env::temp_dir().join(format!("{label}.{extension}"))
}

fn assert_command_success(output: Output, context: &str) -> Result<(), Box<dyn Error>> {
    if output.status.success() {
        return Ok(());
    }

    Err(format!(
        "{context} failed with status {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    )
    .into())
}

fn cmd_exe() -> String {
    std::env::var_os("SystemRoot")
        .map(std::path::PathBuf::from)
        .unwrap_or_else(|| std::path::PathBuf::from(r"C:\Windows"))
        .join("System32")
        .join("cmd.exe")
        .to_string_lossy()
        .into_owned()
}

fn create_detached_session(label: &str, session: &str) -> Result<(), Box<dyn Error>> {
    let cmd = cmd_exe();
    assert_command_success(
        rmux_command()
            .args([
                "-L",
                label,
                "new-session",
                "-d",
                "-s",
                session,
                cmd.as_str(),
                "/d",
                "/q",
            ])
            .stdin(Stdio::null())
            .output()?,
        "create detached control test session",
    )
}

fn create_default_detached_session(label: &str, session: &str) -> Result<(), Box<dyn Error>> {
    assert_command_success(
        rmux_command()
            .args(["-L", label, "new-session", "-d", "-s", session])
            .stdin(Stdio::null())
            .output()?,
        "create default detached control test session",
    )
}

fn show_buffer(label: &str, buffer_name: &str) -> Result<String, Box<dyn Error>> {
    let output = rmux_command()
        .args(["-L", label, "show-buffer", "-b", buffer_name])
        .stdin(Stdio::null())
        .output()?;
    assert_command_success(output.clone(), "show control test buffer")?;
    Ok(String::from_utf8(output.stdout)?.trim_end().to_owned())
}

fn wait_for_buffer(
    label: &str,
    buffer_name: &str,
    expected: &str,
    child: &mut Child,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + CONTROL_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            return Err(format!("control client exited before mutation with {status}").into());
        }
        if matches!(
            show_buffer(label, buffer_name).as_deref(),
            Ok(actual) if actual == expected
        ) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err(format!("timed out waiting for buffer {buffer_name:?}={expected:?}").into())
}

fn wait_for_buffer_after_transport_close(
    label: &str,
    buffer_name: &str,
    expected: &str,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + CONTROL_TIMEOUT;
    while Instant::now() < deadline {
        if matches!(
            show_buffer(label, buffer_name).as_deref(),
            Ok(actual) if actual == expected
        ) {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err(format!("timed out waiting after raw control EOF for {buffer_name:?}={expected:?}").into())
}

fn wait_for_attached_client(
    label: &str,
    session: &str,
    child: &mut Child,
) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + CONTROL_TIMEOUT;
    while Instant::now() < deadline {
        if let Some(status) = child.try_wait()? {
            return Err(format!("control client exited before attach with {status}").into());
        }
        let output = rmux_command()
            .args(["-L", label, "list-clients", "-t", session])
            .stdin(Stdio::null())
            .output()?;
        if output.status.success() && !output.stdout.is_empty() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err(format!("timed out waiting for control client to attach to {session:?}").into())
}

fn wait_for_no_control_clients(label: &str, session: &str) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + CONTROL_TIMEOUT;
    let mut last_listing = String::new();
    while Instant::now() < deadline {
        let output = rmux_command()
            .args(["-L", label, "list-clients", "-t", session])
            .stdin(Stdio::null())
            .output()?;
        assert_command_success(output.clone(), "list control clients after transport loss")?;
        last_listing = String::from_utf8(output.stdout)?;
        if last_listing.trim().is_empty() {
            return Ok(());
        }
        thread::sleep(Duration::from_millis(25));
    }
    Err(format!("control client remained registered after transport loss: {last_listing:?}").into())
}

fn read_child_pipe<R>(pipe: Option<R>, name: &str) -> Result<String, Box<dyn Error>>
where
    R: Read,
{
    let mut pipe = pipe.ok_or_else(|| format!("control child {name} is not piped"))?;
    let mut rendered = String::new();
    pipe.read_to_string(&mut rendered)?;
    Ok(rendered)
}

fn control_guard_count(rendered: &str, prefix: &str) -> usize {
    rendered
        .lines()
        .filter(|line| line.starts_with(prefix))
        .count()
}

fn wait_for_child_exit(child: &mut Child, timeout: Duration) -> Result<ExitStatus, Box<dyn Error>> {
    let deadline = Instant::now() + timeout;
    loop {
        if let Some(status) = child.try_wait()? {
            return Ok(status);
        }
        if Instant::now() >= deadline {
            child.kill()?;
            let _ = child.wait();
            return Err("control-mode client did not exit".into());
        }
        thread::sleep(Duration::from_millis(25));
    }
}

struct ServerGuard {
    label: String,
}

impl ServerGuard {
    fn new(label: String) -> Self {
        Self { label }
    }
}

impl Drop for ServerGuard {
    fn drop(&mut self) {
        let _ = rmux_command()
            .args(["-L", &self.label, "kill-server"])
            .stdout(Stdio::null())
            .stderr(Stdio::null())
            .status();
    }
}
