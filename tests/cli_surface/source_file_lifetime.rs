use super::*;
use std::os::fd::AsRawFd;
use std::os::unix::process::CommandExt;

#[test]
fn pending_source_input_follows_the_live_pane() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-input-move")?;
    let _daemon = harness.start_hidden_daemon()?;
    for session in ["alpha", "beta"] {
        assert_success(&harness.run(&["new-session", "-ds", session])?);
    }
    assert_success(&harness.run(&["set-option", "-gw", "pane-base-index", "5"])?);
    fs::write(
        harness.tmpdir().join("input.conf"),
        "new-pane -dI -t alpha:0\n",
    )?;
    let mut connection = connect(harness.socket_path())?;
    let offer = rmux_proto::SourceFileInputV1Request::on_demand(
        vec!["source-file".into(), "input.conf".into()],
        Some(harness.tmpdir().to_owned()),
    );
    assert!(matches!(
        connection.source_file_input_v1(offer)?,
        Response::SourceFileReadStdinV1(_)
    ));
    let panes = stdout(&harness.run(&["list-panes", "-t", "alpha:0", "-F", "#{pane_id}"])?);
    let pane = panes.lines().nth(1).expect("created input pane");
    assert_success(&harness.run(&["rename-session", "-t", "alpha", "renamed"])?);
    assert_success(&harness.run(&["move-window", "-s", "renamed:0", "-t", "beta:5"])?);
    assert_success(&harness.run(&["new-session", "-ds", "alpha"])?);
    assert_success(&harness.run(&["new-pane", "-d", "-t", "alpha:0"])?);
    let response = connection.roundtrip(&Request::SourceFileInputDataV1(
        rmux_proto::SourceFileInputDataV1Request {
            data: Ok(b"MOVED".to_vec()),
        },
    ))?;
    assert!(
        matches!(&response, Response::SourceFile(result) if result.stderr().is_empty() && result.exit_status().unwrap_or(0) == 0),
        "{response:?}"
    );
    assert_eq!(
        stdout(&harness.run(&["capture-pane", "-p", "-t", pane])?)
            .lines()
            .next(),
        Some("MOVED")
    );
    Ok(())
}

#[test]
fn redirected_source_counts_only_real_files() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-input-depth")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&["new-session", "-d"])?);
    for i in 1..=50 {
        fs::write(
            harness.tmpdir().join(format!("{i}.conf")),
            format!("source-file {}.conf\n", i + 1),
        )?;
    }
    fs::write(
        harness.tmpdir().join("51.conf"),
        "display-message -p DEPTH_OK\n",
    )?;
    let tiny = std::env::var_os("RMUX_ACCEPTANCE_TINY_BINARY").filter(|path| !path.is_empty());
    for (entry, status) in [("3.conf", 0), ("2.conf", 0), ("1.conf", 1)] {
        let mut client = if let Some(tiny) = &tiny {
            let mut client = Command::new(tiny);
            client
                .arg("-S")
                .arg(harness.socket_path())
                .env("RMUX_TINY_TRACE", "1")
                .env(
                    "RMUX_FULL_BINARY_PATH",
                    harness.base_command().get_program(),
                )
                .env_remove("RMUX")
                .env_remove("TMUX")
                .env_remove("TMUX_PANE");
            client
        } else {
            harness.base_command()
        };
        let output = client
            .current_dir(harness.tmpdir())
            .args(["source-file", entry])
            .stdin(Stdio::null())
            .output()?;
        if tiny.is_some() {
            assert!(
                stderr(&output).contains("rmux tiny: direct: source-file"),
                "{output:?}"
            );
        }
        assert_eq!(output.status.code(), Some(status), "{entry}: {output:?}");
        if status == 0 {
            assert_eq!(stdout(&output), "DEPTH_OK\n");
        } else {
            assert!(
                stderr(&output).contains("too many nested files"),
                "{output:?}"
            );
        }
    }
    Ok(())
}

const DELAYED_IF: &str = "if-shell -b 'while [ ! -f go ]; do sleep 0.02; done'";

#[test]
fn background_if_shell_keeps_redirected_input() -> Result<(), Box<dyn Error>> {
    background_source(DELAYED_IF, false, |_, _| {})
}

#[test]
fn background_run_shell_keeps_redirected_input() -> Result<(), Box<dyn Error>> {
    background_source("run-shell -b -d 0.5 -C", false, |_, _| {})
}

#[test]
fn background_read_during_foreground_execution_does_not_hold_the_caller(
) -> Result<(), Box<dyn Error>> {
    background_source("if-shell -b true", true, |_, _| {})
}

#[test]
fn background_reader_survives_controlling_terminal_session_exit() -> Result<(), Box<dyn Error>> {
    let pty = rmux_pty::PtyPair::open()?;
    let slave = pty.slave().try_clone()?.into_owned_fd();
    background_source(DELAYED_IF, false, |client, _| {
        client.stdout(slave);
        // SAFETY: the child only makes system calls on its installed stdout before exec.
        unsafe {
            client.pre_exec(|| {
                if libc::setsid() == -1 || libc::ioctl(1, libc::TIOCSCTTY as _, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    })
}

#[test]
fn background_reader_closes_inherited_stdin_writer() -> Result<(), Box<dyn Error>> {
    background_source(DELAYED_IF, false, |client, writer| {
        let fd = writer.as_raw_fd();
        // SAFETY: the writer is live at spawn. Only the forked client changes its fd flags.
        unsafe {
            client.pre_exec(move || {
                if libc::fcntl(fd, libc::F_SETFD, 0) == -1 {
                    return Err(io::Error::last_os_error());
                }
                Ok(())
            });
        }
    })
}

fn background_source(
    command: &str,
    hold_foreground: bool,
    configure: impl FnOnce(&mut Command, &io::PipeWriter),
) -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-input-background")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&[
        "new-session",
        "-ds",
        "alpha",
        "-c",
        harness.tmpdir().to_str().unwrap(),
    ])?);
    fs::write(
        harness.tmpdir().join("child.conf"),
        "new-pane -dI\nset-option -g @finished yes\n",
    )?;
    let mut source = format!("{command} 'source-file child.conf'\n");
    if hold_foreground {
        source.push_str("wait-for source-input-release\n");
    }
    fs::write(harness.tmpdir().join("parent.conf"), source)?;
    let (reader, mut writer) = io::pipe()?;
    let mut client = harness.base_command();
    client
        .current_dir(harness.tmpdir())
        .args(["source-file", "parent.conf"])
        .stdin(reader)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    configure(&mut client, &writer);
    let mut child = client.spawn()?;
    drop(client);
    if hold_foreground {
        let deadline = Instant::now() + ATTACH_TIMEOUT;
        while stdout(&harness.run(&["list-panes", "-F", "#{pane_id}"])?)
            .lines()
            .count()
            != 2
        {
            assert!(
                Instant::now() < deadline,
                "background reader did not create its pane"
            );
            thread::sleep(Duration::from_millis(20));
        }
        assert_success(&harness.run(&["wait-for", "-S", "source-input-release"])?);
    }
    wait_for_child_status(&mut child, ATTACH_TIMEOUT)?;
    assert_success(&child.wait_with_output()?);
    fs::write(harness.tmpdir().join("go"), "")?;
    writer.write_all(b"AFTER-EXIT\n")?;
    drop(writer);
    wait_for_background_source(&harness)?;
    let panes = stdout(&harness.run(&["list-panes", "-F", "#{pane_id}"])?);
    let pane = panes.lines().nth(1).expect("background input pane");
    assert_eq!(
        stdout(&harness.run(&["capture-pane", "-p", "-t", pane])?)
            .lines()
            .next(),
        Some("AFTER-EXIT")
    );
    Ok(())
}

#[test]
fn background_branch_without_a_consumer_preserves_stdin() -> Result<(), Box<dyn Error>> {
    let harness = CliHarness::new("source-input-unused")?;
    let _daemon = harness.start_hidden_daemon()?;
    assert_success(&harness.run(&[
        "new-session",
        "-ds",
        "alpha",
        "-c",
        harness.tmpdir().to_str().unwrap(),
    ])?);
    fs::write(harness.tmpdir().join("parent.conf"),
        "if-shell -b 'while [ ! -f go ]; do sleep 0.02; done; exit 1' 'new-pane -dI' 'set-option -g @finished yes'\n")?;
    let input = harness.tmpdir().join("data");
    fs::write(&input, b"unused record\n")?;
    let mut reader = fs::File::open(input)?;
    let mut child = harness
        .base_command()
        .current_dir(harness.tmpdir())
        .args(["source-file", "parent.conf"])
        .stdin(reader.try_clone()?)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    wait_for_child_status(&mut child, ATTACH_TIMEOUT)?;
    assert_success(&child.wait_with_output()?);
    fs::write(harness.tmpdir().join("go"), "")?;
    wait_for_background_source(&harness)?;
    let mut remaining = Vec::new();
    reader.read_to_end(&mut remaining)?;
    assert_eq!(remaining, b"unused record\n");
    assert_eq!(
        stdout(&harness.run(&["list-panes", "-F", "#{pane_id}"])?)
            .lines()
            .count(),
        1
    );
    Ok(())
}

fn wait_for_background_source(harness: &CliHarness) -> Result<(), Box<dyn Error>> {
    let deadline = Instant::now() + ATTACH_TIMEOUT;
    while stdout(&harness.run(&["show-options", "-gqv", "@finished"])?) != "yes\n" {
        assert!(
            Instant::now() < deadline,
            "background source did not finish"
        );
        thread::sleep(Duration::from_millis(20));
    }
    Ok(())
}
