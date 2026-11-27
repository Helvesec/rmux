use std::ffi::OsStr;
use std::fs::File;
use std::io::Write;
use std::os::unix::process::ExitStatusExt;
use std::path::Path;
use std::process::ExitStatus;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::{ChildCommand, ProcessId, PtyChild, RmuxShell, ShellChild, SpawnedPty};
use crate::{PtyMaster, PtyPair, PtySlave, Result, Signal, TerminalSize};

/// A shell child that never touches the operating system: it reports the
/// host process, records every signal, and exits once signaled.
#[derive(Debug)]
struct ScriptedChild {
    exit: ExitStatus,
    killed: Arc<AtomicBool>,
    signals: Arc<Mutex<Vec<Signal>>>,
    _terminal: PtySlave,
}

impl ShellChild for ScriptedChild {
    fn pid(&self) -> ProcessId {
        ProcessId::new(std::process::id()).expect("host pid")
    }

    fn wait(&mut self) -> Result<ExitStatus> {
        Ok(self.exit)
    }

    fn try_wait(&mut self) -> Result<Option<ExitStatus>> {
        Ok(self.killed.load(Ordering::SeqCst).then_some(self.exit))
    }

    fn kill(&self, signal: Signal) -> Result<()> {
        self.signals.lock().expect("signal log").push(signal);
        self.killed.store(true, Ordering::SeqCst);
        Ok(())
    }

    fn kill_session_leader(&self, signal: Signal) -> Result<()> {
        self.kill(signal)
    }

    fn continue_if_stopped(&self) -> Result<bool> {
        Ok(false)
    }
}

#[derive(Debug)]
struct ScriptedShell {
    output: &'static [u8],
    exit: ExitStatus,
    seen: Arc<Mutex<Option<ChildCommand>>>,
    signals: Arc<Mutex<Vec<Signal>>>,
}

impl RmuxShell for ScriptedShell {
    fn spawn(&self, command: ChildCommand) -> Result<SpawnedPty> {
        let size = command.initial_size().unwrap_or(TerminalSize::new(80, 24));
        *self.seen.lock().expect("spawn log") = Some(command);

        let (master, slave) = PtyPair::open_with_size(size)?.into_split();
        let mut terminal = File::from(slave.try_clone()?.into_owned_fd());
        terminal.write_all(self.output)?;
        drop(terminal);

        Ok(SpawnedPty::new(
            master,
            PtyChild::new(ScriptedChild {
                exit: self.exit,
                killed: Arc::new(AtomicBool::new(false)),
                signals: Arc::clone(&self.signals),
                _terminal: slave,
            }),
        ))
    }
}

fn read_from_master(master: &PtyMaster, len: usize) -> Vec<u8> {
    let io = master.io();
    let mut buffer = vec![0_u8; len];
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut offset = 0;
    while offset < buffer.len() {
        match io.try_read(&mut buffer[offset..]) {
            Ok(0) => panic!("pty master reached end of file after {offset} bytes"),
            Ok(read) => offset += read,
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::Interrupted
                ) =>
            {
                assert!(Instant::now() < deadline, "timed out reading pty master");
                std::thread::sleep(Duration::from_millis(5));
            }
            Err(error) => panic!("pty master read failed: {error}"),
        }
    }
    buffer
}

#[test]
fn custom_shell_backend_drives_pty_child_through_the_trait() {
    let seen = Arc::new(Mutex::new(None));
    let signals = Arc::new(Mutex::new(Vec::new()));
    let shell: Arc<dyn RmuxShell> = Arc::new(ScriptedShell {
        output: b"scripted\n",
        exit: ExitStatusExt::from_raw(3 << 8),
        seen: Arc::clone(&seen),
        signals: Arc::clone(&signals),
    });

    let mut spawned = shell
        .spawn(
            ChildCommand::new("/opt/brush")
                .arg0("-brush")
                .args(["-c", "true"])
                .env("RMUX_TEST", "1")
                .current_dir("/tmp")
                .clear_env()
                .size(TerminalSize::new(80, 24)),
        )
        .expect("scripted shell spawns");

    let recorded = seen
        .lock()
        .expect("spawn log")
        .take()
        .expect("spawn request");
    assert_eq!(recorded.program(), Path::new("/opt/brush"));
    assert_eq!(recorded.argv0(), Some(OsStr::new("-brush")));
    assert_eq!(recorded.arguments(), [OsStr::new("-c"), OsStr::new("true")]);
    assert_eq!(
        recorded.environment(),
        [(
            OsStr::new("RMUX_TEST").to_owned(),
            OsStr::new("1").to_owned()
        )]
    );
    assert!(recorded.clears_env());
    assert_eq!(recorded.working_dir(), Some(Path::new("/tmp")));
    assert_eq!(recorded.initial_size(), Some(TerminalSize::new(80, 24)));

    assert_eq!(
        spawned.master().size().expect("master size"),
        TerminalSize::new(80, 24)
    );
    assert_eq!(read_from_master(spawned.master(), 10), b"scripted\r\n");

    let child = spawned.child_mut();
    assert_eq!(child.pid().as_u32(), std::process::id());
    assert_eq!(child.try_wait().expect("try_wait"), None);

    child.interrupt().expect("interrupt");
    child.terminate_forcefully().expect("terminate");
    assert_eq!(
        *signals.lock().expect("signal log"),
        [Signal::INT, Signal::KILL]
    );
    assert_eq!(
        child.try_wait().expect("try_wait").and_then(|s| s.code()),
        Some(3)
    );
}
