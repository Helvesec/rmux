use super::support::*;
use std::io::{self, Read};
use std::process::ChildStdout;

// Exercise the real stdout pipe: the reader is deliberately absent during the
// burst. Concatenate decoded pane bytes rather than comparing PTY chunk sizes.
#[test]
fn tmux_compat_control_pause_resume_retains_backpressure_output() -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("control-pause-resume-backpressure")?;
    let Some(tmux) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    super::control_criteria::PAUSE_RESUME.report(&tmux);
    let payload = (0..16384)
        .map(|n| format!("{n:06}|{}\n", "x".repeat(120)))
        .collect::<String>();
    fs::write(harness.tmpdir().join("payload"), &payload)?;
    let mut observed = Vec::new();
    for oracle in [true, false] {
        let label = if oracle { "tmux" } else { "rmux" };
        let root = harness.tmpdir().join(label);
        fs::create_dir_all(&root)?;
        let script = format!(
            "stty -onlcr; for n in 1 2 3 4 5; do while [ ! -f {0}/go$n ]; do sleep 0.02; done; cat {1}/payload; touch {0}/done$n; done; sleep 60",
            shell_quote(&root.to_string_lossy()),
            shell_quote(&harness.tmpdir().to_string_lossy()),
        );
        let argv = ["new-session", "-d", "-s", "alpha", &script];
        let create = if oracle {
            let output = Command::new(&tmux)
                .args(["-f", "/dev/null", "-S"])
                .arg(harness.tmux_socket_path())
                .args(argv)
                .output()?;
            assert!(output.status.success(), "tmux create: {output:?}");
            None
        } else {
            Some(harness.run_rmux(&argv)?)
        };
        if let Some(create) = create {
            assert_eq!(
                create.status_code,
                Some(0),
                "{label}: {}",
                create.stderr_string()
            );
        }
        // A fast peer keeps the PTY readable while this client's stdout stalls.
        // Otherwise tmux can throttle the sole reader before an aged block is
        // queued, so pipe capacity alone cannot establish automatic pause.
        let mut fast_command = if oracle {
            tmux_control_mode_command(&harness, &tmux, &[], &[])?
        } else {
            rmux_control_mode_command(&harness, &[], &[])?
        };
        fast_command.args(["attach-session", "-t", "alpha"]);
        let mut fast_client = LiveControlClient::spawn_capturing(fast_command)?;
        fast_client.wait_for_notification(0, Duration::from_secs(15), label, |line| {
            line.starts_with("%session-changed ")
        })?;
        let mut command = if oracle {
            tmux_control_mode_command(&harness, &tmux, &[], &[])?
        } else {
            rmux_control_mode_command(&harness, &[], &[])?
        };
        command.args(["attach-session", "-t", "alpha"]);
        let mut client = PipeControlClient::spawn(command)?;
        client.until("%session-changed", label)?;
        client.send("refresh-client -f pause-after=10\n")?;
        client.until("%end", label)?;
        let start = client.bytes.len();
        fs::write(root.join("go1"), "")?;
        // More than pipe capacity, less than the measured pause-after period.
        thread::sleep(Duration::from_millis(1200));
        client.until_file(&root.join("done1"), label)?;
        client.send("display-message -p BURST-DRAINED\n")?;
        client.until("BURST-DRAINED\n%end", label)?;
        let burst = decoded_output(&client.bytes[start..])?;
        assert!(
            burst == payload.as_bytes(),
            "{label} lost or reordered bytes: {} of {}",
            burst.len(),
            payload.len()
        );
        assert!(!client.bytes[start..].windows(6).any(|w| w == b"%pause"));

        let start = client.bytes.len();
        client.send("refresh-client -A '%0:pause'\n")?;
        client.until("%end", label)?;
        fs::write(root.join("go2"), "")?;
        client.until_file(&root.join("done2"), label)?;
        client.send("display-message -p PAUSED-DRAINED\n")?;
        client.until("PAUSED-DRAINED\n%end", label)?;
        assert!(
            decoded_output(&client.bytes[start..])?.is_empty(),
            "{label} output while paused"
        );
        client.send("refresh-client -A '%0:continue'\n")?;
        client.until("%end", label)?;
        fs::write(root.join("go3"), "")?;
        client.until_file(&root.join("done3"), label)?;
        client.send("display-message -p RESUMED-DRAINED\n")?;
        client.until("RESUMED-DRAINED\n%end", label)?;
        assert!(
            decoded_output(&client.bytes[start..])? == payload.as_bytes(),
            "{label} resume bytes"
        );
        let notifications = String::from_utf8_lossy(&client.bytes[start..])
            .lines()
            .filter(|l| l.starts_with("%pause ") || l.starts_with("%continue "))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        eprintln!(
            "{label} pause/backpressure: {} bytes, {notifications:?}",
            burst.len()
        );
        client.send("refresh-client -f pause-after=1\n")?;
        client.until("%end", label)?;
        let auto_start = client.bytes.len();
        fs::write(root.join("go4"), "")?;
        let ready = Instant::now() + Duration::from_secs(15);
        while !root.join("done4").exists() && Instant::now() < ready {
            thread::sleep(Duration::from_millis(10));
        }
        assert!(
            root.join("done4").exists(),
            "{label} fast peer did not keep the producer readable"
        );
        // tmux 3.7b pauses aged pending output when the real pipe is stalled.
        thread::sleep(Duration::from_secs(8));
        client.until("%pause %0", label)?;
        client.until_file(&root.join("done4"), label)?;
        // The subsequent burst tests resumption, separately from the one-second
        // pause deadline; a loaded oracle must not pause that burst a second time.
        client.send("refresh-client -f pause-after=10\n")?;
        client.until("%end", label)?;
        client.send("refresh-client -A '%0:continue'\n")?;
        client.until("%end", label)?;
        let resumed_start = client.bytes.len();
        fs::write(root.join("go5"), "")?;
        client.until_file(&root.join("done5"), label)?;
        client.send("display-message -p AUTO-RESUMED\n")?;
        client.until("AUTO-RESUMED\n%end", label)?;
        assert!(
            decoded_output(&client.bytes[resumed_start..])? == payload.as_bytes(),
            "{label} auto resume bytes"
        );
        let auto_notifications = String::from_utf8_lossy(&client.bytes[auto_start..])
            .lines()
            .filter(|l| l.starts_with("%pause ") || l.starts_with("%continue "))
            .map(str::to_owned)
            .collect::<Vec<_>>();
        assert_eq!(
            auto_notifications, notifications,
            "{label} automatic transitions"
        );
        assert_control_frames(&client.bytes)?;
        fast_client.assert_running(label)?;
        observed.push((notifications, auto_notifications));
    }
    assert_eq!(observed[0].0, ["%pause %0", "%continue %0"]);
    assert_eq!(observed[1], observed[0]);
    Ok(())
}

fn assert_control_frames(bytes: &[u8]) -> Result<(), Box<dyn Error>> {
    let mut pending = None;
    for line in std::str::from_utf8(bytes)?.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        match fields.first().copied() {
            Some("%begin") => {
                assert_eq!(fields.len(), 4);
                assert!(
                    pending.replace(fields[1..].to_vec()).is_none(),
                    "nested frame"
                );
            }
            Some("%end") => assert_eq!(pending.take().as_deref(), Some(&fields[1..])),
            Some("%error") => panic!("control command failed: {line}"),
            _ => {}
        }
    }
    assert!(pending.is_none(), "unterminated frame");
    Ok(())
}

fn decoded_output(bytes: &[u8]) -> Result<Vec<u8>, Box<dyn Error>> {
    let text = std::str::from_utf8(bytes)?;
    let mut result = Vec::new();
    for line in text.lines() {
        let data = if let Some(rest) = line.strip_prefix("%extended-output ") {
            rest.split_once(" : ").ok_or("invalid extended output")?.1
        } else if let Some(rest) = line.strip_prefix("%output ") {
            rest.split_once(' ').ok_or("invalid output")?.1
        } else {
            continue;
        };
        let mut bytes = data.bytes();
        while let Some(byte) = bytes.next() {
            if byte == b'\\' {
                let digits = [bytes.next(), bytes.next(), bytes.next()];
                let mut value = 0;
                for digit in digits {
                    let digit = digit.ok_or("truncated output escape")?;
                    if !(b'0'..=b'7').contains(&digit) {
                        return Err("invalid output escape".into());
                    }
                    value = (value << 3) | (digit - b'0');
                }
                result.push(value);
            } else {
                result.push(byte);
            }
        }
    }
    Ok(result)
}

struct PipeControlClient {
    child: Child,
    stdout: ChildStdout,
    bytes: Vec<u8>,
}

impl PipeControlClient {
    fn spawn(mut command: Command) -> Result<Self, Box<dyn Error>> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let stdout = child.stdout.take().ok_or("missing stdout")?;
        let fd = stdout.as_raw_fd();
        if unsafe { libc::fcntl(fd, libc::F_SETFL, libc::O_NONBLOCK) } == -1 {
            return Err(io::Error::last_os_error().into());
        }
        Ok(Self {
            child,
            stdout,
            bytes: Vec::new(),
        })
    }

    fn send(&mut self, command: &str) -> Result<(), Box<dyn Error>> {
        self.child
            .stdin
            .as_mut()
            .ok_or("missing stdin")?
            .write_all(command.as_bytes())?;
        Ok(())
    }

    fn drain(&mut self) -> Result<(), Box<dyn Error>> {
        let mut buf = [0; 65536];
        loop {
            match self.stdout.read(&mut buf) {
                Ok(0) => return Err("control client closed stdout".into()),
                Ok(n) => self.bytes.extend_from_slice(&buf[..n]),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock => return Ok(()),
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => return Err(e.into()),
            }
        }
    }

    fn until(&mut self, needle: &str, label: &str) -> Result<(), Box<dyn Error>> {
        let start = self.bytes.len();
        let deadline = Instant::now() + Duration::from_secs(15);
        while Instant::now() < deadline {
            self.drain()?;
            if String::from_utf8_lossy(&self.bytes[start..]).contains(needle) {
                return Ok(());
            }
            thread::sleep(Duration::from_millis(10));
        }
        Err(format!(
            "{label}: missing {needle:?}: {} bytes, tail {:?}",
            self.bytes.len() - start,
            String::from_utf8_lossy(&self.bytes[self.bytes.len().saturating_sub(1024)..])
        )
        .into())
    }

    fn until_file(&mut self, path: &Path, label: &str) -> Result<(), Box<dyn Error>> {
        let deadline = Instant::now() + Duration::from_secs(15);
        while !path.exists() && Instant::now() < deadline {
            self.drain()?;
            thread::sleep(Duration::from_millis(10));
        }
        if !path.exists() {
            return Err(format!("{label}: producer stalled").into());
        }
        Ok(())
    }
}

impl Drop for PipeControlClient {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}
