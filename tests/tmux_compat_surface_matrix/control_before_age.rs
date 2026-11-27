use super::support::*;
use std::io::{self, Read};
use std::sync::atomic::{AtomicBool, Ordering};

const PAYLOAD_BYTES: usize = 8 * 1024 * 1024;
const DEADLINE: Duration = Duration::from_millis(8500);

// D54: exceed both the source journal and the per-client pending budget while
// reading continuously below pause-after=10. The oracle supplies the contract.
#[test]
fn tmux_compat_control_slow_reader_is_lossless_before_pause_age() -> Result<(), Box<dyn Error>> {
    let harness = TmuxCompatHarness::new("control-lossless-before-age")?;
    let Some(tmux) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    super::control_criteria::BACKPRESSURE.report(&tmux);
    let mut payload = Vec::with_capacity(PAYLOAD_BYTES);
    for sequence in 0..PAYLOAD_BYTES / 4096 {
        let prefix = format!("{sequence:08x}:");
        payload.extend_from_slice(prefix.as_bytes());
        payload.resize(
            payload.len() + 4095 - prefix.len(),
            b'A' + (sequence % 26) as u8,
        );
        payload.push(b'\n');
    }
    fs::write(harness.tmpdir().join("payload"), &payload)?;
    for oracle in [true, false] {
        let label = if oracle { "tmux" } else { "rmux" };
        let root = harness.tmpdir().join(label);
        fs::create_dir_all(&root)?;
        let script = format!(
            "stty -onlcr; while [ ! -f {0}/go ]; do sleep 0.02; done; cat {1}/payload; touch {0}/done; sleep 60",
            shell_quote(&root.to_string_lossy()),
            shell_quote(&harness.tmpdir().to_string_lossy()),
        );
        let argv = ["new-session", "-d", "-s", "alpha", &script];
        if oracle {
            let create = Command::new(&tmux)
                .args(["-f", "/dev/null", "-S"])
                .arg(harness.tmux_socket_path())
                .args(argv)
                .output()?;
            assert!(create.status.success(), "tmux create: {create:?}");
        } else {
            assert_eq!(harness.run_rmux(&argv)?.status_code, Some(0));
        }
        let mut clients = Vec::new();
        for role in ["fast", "slow"] {
            let mut command = if oracle {
                tmux_control_mode_command(&harness, &tmux, &[], &[])?
            } else {
                rmux_control_mode_command(&harness, &[], &[])?
            };
            command.args(["attach-session", "-t", "alpha"]);
            let mut client = PacedClient::spawn(command)?;
            client.until(
                b"%session-changed ",
                Instant::now() + Duration::from_secs(15),
            )?;
            client.send(&format!(
                "refresh-client -f pause-after=10\ndisplay-message -p {role}-ARMED\n"
            ))?;
            client.until(
                format!("{role}-ARMED\n%end ").as_bytes(),
                Instant::now() + Duration::from_secs(15),
            )?;
            assert!(decode(&client.snapshot())?.is_empty());
            clients.push(client);
        }
        clients[1].paced.store(true, Ordering::Release);
        let start = Instant::now();
        let deadline = start + DEADLINE;
        fs::write(root.join("go"), "")?;
        while Instant::now() < deadline {
            if clients[1].has(b"%pause ")
                || (root.join("done").exists() && clients.iter().all(|c| c.len() >= PAYLOAD_BYTES))
            {
                break;
            }
            thread::sleep(Duration::from_millis(2));
        }
        // A reply follows all published output, including an in-flight write.
        for client in &mut clients {
            client.send("display-message -p D54-COMPLETE\n")?;
            client.until(b"D54-COMPLETE\n%end ", deadline)?;
        }
        for (role, client) in ["fast", "slow"].into_iter().zip(&mut clients) {
            client.paced.store(false, Ordering::Release);
            let bytes = client.snapshot();
            let decoded = decode(&bytes)?;
            eprintln!(
                "D54 {label}/{role}: {} bytes in {:?}",
                decoded.len(),
                start.elapsed()
            );
            assert!(
                decoded == payload,
                "{label}/{role}: {} of {} bytes",
                decoded.len(),
                payload.len()
            );
            assert!(!client.has(b"%pause "), "{label}/{role}: premature pause");
            assert!(!client.has(b"%exit"), "{label}/{role}: early exit");
            assert!(
                client.child.try_wait()?.is_none(),
                "{label}/{role}: client ended"
            );
            assert_frames(&bytes)?;
        }
        assert!(
            root.join("done").exists(),
            "{label}: producer did not finish"
        );
        assert!(start.elapsed() < DEADLINE, "{label}: exceeded deadline");
    }
    Ok(())
}

// tmux 3.7b: a stopped reader is paused at age 10s. Producer completion
// and lossless fast-peer delivery must precede resuming that reader. They
// are not a 32 MiB / 12 s throughput requirement (including under host load).
#[test]
fn tmux_compat_control_pause_age_wakes_producer_during_blocked_flush() -> Result<(), Box<dyn Error>>
{
    let harness = TmuxCompatHarness::new("control-age-wakeup")?;
    let Some(tmux) = frozen_tmux_or_skip(&harness)? else {
        return Ok(());
    };
    let payload = (0..8192)
        .flat_map(|sequence| {
            let mut bytes = format!("{sequence:08x}:").into_bytes();
            bytes.resize(4095, b'A' + (sequence % 26) as u8);
            bytes.push(b'\n');
            bytes
        })
        .collect::<Vec<_>>();
    fs::write(harness.tmpdir().join("age-payload"), &payload)?;
    for oracle in [true, false] {
        let label = if oracle { "tmux" } else { "rmux" };
        let root = harness.tmpdir().join(label);
        fs::create_dir_all(&root)?;
        let script = format!(
            "stty -onlcr; while [ ! -f {0}/go ]; do sleep 0.02; done; cat {1}/age-payload; touch {0}/done; sleep 60",
            shell_quote(&root.to_string_lossy()), shell_quote(&harness.tmpdir().to_string_lossy()),
        );
        let argv = ["new-session", "-d", "-s", "alpha", &script];
        if oracle {
            assert!(Command::new(&tmux)
                .args(["-f", "/dev/null", "-S"])
                .arg(harness.tmux_socket_path())
                .args(argv)
                .status()?
                .success());
        } else {
            assert_eq!(harness.run_rmux(&argv)?.status_code, Some(0));
        }
        let mut clients = Vec::new();
        for role in ["fast", "slow"] {
            let mut command = if oracle {
                tmux_control_mode_command(&harness, &tmux, &[], &[])?
            } else {
                rmux_control_mode_command(&harness, &[], &[])?
            };
            command.args(["attach-session", "-t", "alpha"]);
            let mut client = PacedClient::spawn(command)?;
            let deadline = Instant::now() + Duration::from_secs(15);
            client.until(b"%session-changed ", deadline)?;
            client.send(&format!(
                "refresh-client -f pause-after=10\ndisplay-message -p {role}-ARMED\n"
            ))?;
            client.until(format!("{role}-ARMED\n%end ").as_bytes(), deadline)?;
            clients.push(client);
        }
        clients[1].stopped.store(true, Ordering::Release);
        let start = Instant::now();
        fs::write(root.join("go"), "")?;
        // Keep the pause age unchanged. The watchdog is below the transport's
        // 30 s write timeout; it bounds this probe, not the producer's throughput.
        // A paced producer also reproduces the old 12 s assertion on tmux 3.7b.
        let deadline = start + Duration::from_secs(25);
        while start.elapsed() < Duration::from_secs(12) || !root.join("done").exists() {
            assert!(
                Instant::now() < deadline,
                "{label}: producer incomplete while slow reader stopped; \
                 diagnose throughput and pause separately (fast wire bytes={})",
                clients[0].len()
            );
            thread::sleep(Duration::from_millis(10));
        }
        let producer_elapsed = start.elapsed();
        // The producer's file marker may precede the PTY's final publication.
        // Wait for the complete payload, ignoring only an in-flight wire line.
        let fast_bytes = loop {
            let snapshot = clients[0].snapshot();
            let complete = snapshot
                .iter()
                .rposition(|&b| b == b'\n')
                .map_or(0, |i| i + 1);
            let bytes = decode(&snapshot[..complete])?;
            if bytes.len() >= payload.len() {
                break bytes;
            }
            assert!(
                Instant::now() < deadline,
                "{label}: fast peer received {} of {} bytes while slow reader stopped",
                bytes.len(),
                payload.len()
            );
            thread::sleep(Duration::from_millis(10));
        };
        assert_eq!(fast_bytes, payload, "{label}: fast peer output");
        eprintln!(
            "AGE {label}: producer observed at {producer_elapsed:?}, fast={} at {:?}, slow still stopped",
            fast_bytes.len(),
            start.elapsed()
        );
        clients[1].stopped.store(false, Ordering::Release);
        let deadline = Instant::now() + Duration::from_secs(5);
        for client in &mut clients {
            client.send("display-message -p AGE-COMPLETE\n")?;
            client.until(b"AGE-COMPLETE\n%end ", deadline)?;
            assert!(client.child.try_wait()?.is_none(), "{label}: client ended");
            assert!(!client.has(b"%exit"), "{label}: early exit");
            assert_frames(&client.snapshot())?;
        }
        assert!(!clients[0].has(b"%pause "), "{label}: fast peer paused");
        assert!(clients[1].has(b"%pause "), "{label}: slow peer not paused");
    }
    Ok(())
}

struct PacedClient {
    child: Child,
    bytes: Arc<Mutex<Vec<u8>>>,
    paced: Arc<AtomicBool>,
    stopped: Arc<AtomicBool>,
    reader: Option<thread::JoinHandle<io::Result<()>>>,
}

impl PacedClient {
    fn spawn(mut command: Command) -> io::Result<Self> {
        let mut child = command
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::inherit())
            .spawn()?;
        let mut stdout = child
            .stdout
            .take()
            .ok_or_else(|| io::Error::other("missing stdout"))?;
        let bytes = Arc::new(Mutex::new(Vec::new()));
        let paced = Arc::new(AtomicBool::new(false));
        let sink = bytes.clone();
        let pace = paced.clone();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = stopped.clone();
        let reader = thread::spawn(move || {
            let mut buffer = [0; 16384];
            loop {
                while stop.load(Ordering::Acquire) {
                    thread::sleep(Duration::from_millis(2));
                }
                let count = stdout.read(&mut buffer)?;
                if count == 0 {
                    return Ok(());
                }
                sink.lock()
                    .expect("control bytes")
                    .extend_from_slice(&buffer[..count]);
                if pace.load(Ordering::Acquire) {
                    // Darwin's smaller pipe reads need shorter pacing to keep
                    // the same 8 MiB / 8.5 s contract below the pause age.
                    thread::sleep(Duration::from_millis(if cfg!(target_os = "macos") {
                        1
                    } else {
                        5
                    }));
                }
            }
        });
        Ok(Self {
            child,
            bytes,
            paced,
            stopped,
            reader: Some(reader),
        })
    }

    fn send(&mut self, text: &str) -> io::Result<()> {
        let stdin = self
            .child
            .stdin
            .as_mut()
            .ok_or_else(|| io::Error::other("missing stdin"))?;
        stdin.write_all(text.as_bytes())?;
        stdin.flush()
    }

    fn len(&self) -> usize {
        self.bytes.lock().expect("control bytes").len()
    }

    fn snapshot(&self) -> Vec<u8> {
        self.bytes.lock().expect("control bytes").clone()
    }

    fn has(&self, needle: &[u8]) -> bool {
        let bytes = self.bytes.lock().expect("control bytes");
        bytes[bytes.len().saturating_sub(65536)..]
            .windows(needle.len())
            .any(|w| w == needle)
    }

    fn until(&mut self, needle: &[u8], deadline: Instant) -> Result<(), Box<dyn Error>> {
        while Instant::now() < deadline {
            if self.has(needle) {
                return Ok(());
            }
            if let Some(status) = self.child.try_wait()? {
                return Err(format!("control client ended: {status}").into());
            }
            thread::sleep(Duration::from_millis(2));
        }
        Err(format!(
            "missing {:?}: {} bytes",
            String::from_utf8_lossy(needle),
            self.len()
        )
        .into())
    }
}

impl Drop for PacedClient {
    fn drop(&mut self) {
        self.paced.store(false, Ordering::Release);
        self.stopped.store(false, Ordering::Release);
        let _ = self.child.kill();
        let _ = self.child.wait();
        if let Some(reader) = self.reader.take() {
            let _ = reader.join();
        }
    }
}

fn decode(bytes: &[u8]) -> Result<Vec<u8>, Box<dyn Error>> {
    let mut output = Vec::new();
    for line in std::str::from_utf8(bytes)?.lines() {
        let content = if let Some(rest) = line.strip_prefix("%extended-output ") {
            rest.split_once(" : ").ok_or("invalid extended output")?.1
        } else if let Some(rest) = line.strip_prefix("%output ") {
            rest.split_once(' ').ok_or("invalid output")?.1
        } else {
            continue;
        };
        let mut data = content.bytes();
        while let Some(byte) = data.next() {
            if byte != b'\\' {
                output.push(byte);
                continue;
            }
            let mut value = 0;
            for _ in 0..3 {
                let digit = data.next().ok_or("truncated output escape")?;
                if !(b'0'..=b'7').contains(&digit) {
                    return Err("invalid output escape".into());
                }
                value = (value << 3) | (digit - b'0');
            }
            output.push(value);
        }
    }
    Ok(output)
}

fn assert_frames(bytes: &[u8]) -> Result<(), Box<dyn Error>> {
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
