use super::support::*;
use std::io::Read;
use std::process::ExitStatus;
use std::thread::JoinHandle;

type Collector = JoinHandle<std::io::Result<()>>;

pub(super) struct ControlPeer {
    child: Child,
    stdout: Arc<Mutex<Vec<u8>>>,
    stderr: Arc<Mutex<Vec<u8>>>,
    collectors: Vec<Collector>,
}

impl ControlPeer {
    pub fn spawn(mut command: Command) -> Result<Self, Box<dyn Error>> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()?;
        let stdout = Arc::new(Mutex::new(Vec::new()));
        let stderr = Arc::new(Mutex::new(Vec::new()));
        let collectors = vec![
            collect(
                child.stdout.take().ok_or("stdout missing")?,
                Arc::clone(&stdout),
            ),
            collect(
                child.stderr.take().ok_or("stderr missing")?,
                Arc::clone(&stderr),
            ),
        ];
        Ok(Self {
            child,
            stdout,
            stderr,
            collectors,
        })
    }

    pub fn send(&mut self, commands: &str) -> Result<(), Box<dyn Error>> {
        let stdin = self.child.stdin.as_mut().ok_or("stdin closed")?;
        stdin.write_all(commands.as_bytes())?;
        stdin.flush()?;
        Ok(())
    }

    pub fn snapshot(&self) -> String {
        String::from_utf8_lossy(&self.stdout.lock().expect("stdout lock")).into_owned()
    }

    pub fn wait(
        &mut self,
        timeout: Duration,
        predicate: impl Fn(&str) -> bool,
    ) -> Result<String, Box<dyn Error>> {
        let deadline = Instant::now() + timeout;
        loop {
            let output = self.snapshot();
            if predicate(&output) {
                return Ok(output);
            }
            if self.child.try_wait()?.is_some() || Instant::now() >= deadline {
                return Err(format!("control observable did not arrive: {output}").into());
            }
            thread::sleep(Duration::from_millis(5));
        }
    }

    /// Closes stdin and returns stdout of a session that ends with status 0
    /// and an empty stderr. Scenarios call it for tmux 3.7b and rmux alike,
    /// so the oracle is held to the same outcome as the candidate.
    pub fn finish(self, timeout: Duration) -> Result<String, Box<dyn Error>> {
        let output = self.finish_output(timeout)?;
        assert_eq!(output.status_code, Some(0), "{}", output.stdout);
        assert!(output.stderr.is_empty(), "{}", output.stderr);
        Ok(output.stdout)
    }

    /// Closes stdin and returns the raw outcome, exit status and stderr
    /// included, without judging them: tmux 3.7b exits 1 after a failed
    /// command, so a scenario compares them with [`assert_same_exit`].
    pub fn finish_output(mut self, timeout: Duration) -> Result<ControlModeOutput, Box<dyn Error>> {
        drop(self.child.stdin.take());
        let deadline = Instant::now() + timeout;
        let status: ExitStatus = loop {
            if let Some(status) = self.child.try_wait()? {
                break status;
            }
            if Instant::now() >= deadline {
                return Err(format!("control EOF timed out: {}", self.snapshot()).into());
            }
            thread::sleep(Duration::from_millis(5));
        };
        while !self.collectors.iter().all(JoinHandle::is_finished) {
            if Instant::now() >= deadline {
                return Err("control pipe remained open after process exit".into());
            }
            thread::sleep(Duration::from_millis(5));
        }
        for collector in self.collectors.drain(..) {
            collector
                .join()
                .map_err(|_| "control collector panicked")??;
        }
        Ok(ControlModeOutput {
            status_code: status.code(),
            stdout: self.snapshot(),
            stderr: String::from_utf8_lossy(&self.stderr.lock().expect("stderr lock")).into_owned(),
        })
    }
}

/// Holds rmux to the exit status and stderr that tmux 3.7b gave for the same
/// control session, instead of a fixed status.
pub(super) fn assert_same_exit(tmux: &ControlModeOutput, rmux: &ControlModeOutput) {
    assert_eq!(
        rmux.status_code, tmux.status_code,
        "exit status: tmux={tmux:?} rmux={rmux:?}"
    );
    assert_eq!(
        rmux.stderr, tmux.stderr,
        "stderr: tmux={tmux:?} rmux={rmux:?}"
    );
}

impl Drop for ControlPeer {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

fn collect(mut stream: impl Read + Send + 'static, sink: Arc<Mutex<Vec<u8>>>) -> Collector {
    thread::spawn(move || {
        let mut buffer = [0; 8192];
        loop {
            let count = stream.read(&mut buffer)?;
            if count == 0 {
                return Ok(());
            }
            sink.lock()
                .expect("control output lock")
                .extend_from_slice(&buffer[..count]);
        }
    })
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Frame {
    pub flag: u64,
    pub terminal: String,
    pub payload: Vec<String>,
}

// Normalize only wall time and the global starting command number. Guard pairs
// must match all three fields exactly, and numbers must remain strictly ordered.
pub(super) fn frames(output: &str) -> Vec<Frame> {
    let mut pending = None;
    let mut previous_number = 0;
    let mut result = Vec::new();
    let mut payload = Vec::new();
    for line in output.lines() {
        let fields = line.split_whitespace().collect::<Vec<_>>();
        match fields.first().copied() {
            Some("%begin" | "%end" | "%error") => {
                assert_eq!(fields.len(), 4, "{line}");
                let tuple = fields[1..]
                    .iter()
                    .map(|v| v.parse::<u64>().unwrap())
                    .collect::<Vec<_>>();
                if fields[0] == "%begin" {
                    assert!(tuple[1] > previous_number, "{output}");
                    previous_number = tuple[1];
                    assert!(pending.replace(tuple).is_none(), "nested frame: {output}");
                } else {
                    assert_eq!(pending.take().as_ref(), Some(&tuple), "{output}");
                    result.push(Frame {
                        flag: tuple[2],
                        terminal: fields[0].to_owned(),
                        payload: std::mem::take(&mut payload),
                    });
                }
            }
            _ if pending.is_some() => payload.push(line.to_owned()),
            _ => {}
        }
    }
    assert!(pending.is_none(), "incomplete frame: {output}");
    result
}

pub(super) fn command(
    harness: &TmuxCompatHarness,
    tmux: &Path,
    oracle: bool,
    argv: &[&str],
) -> Result<Command, Box<dyn Error>> {
    let mut command = if oracle {
        tmux_control_mode_command(harness, tmux, &[], &[])?
    } else {
        rmux_control_mode_command(harness, &[], &[])?
    };
    command.args(argv);
    Ok(command)
}
