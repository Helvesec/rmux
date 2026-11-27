//! CQUIT1-R9/R10 — the width a pane's own tool really has, asked of the tool.
//!
//! `master.size()` answers from the size this server last stored
//! (`rmux-pty backend/windows/pty.rs:232-237`), so on Windows it is a cache of
//! what was asked for, never what the console holds. What this parcours needs is
//! the other side: the width the real producer sees.
//!
//! So the producer answers it. This is the probe the acquired Windows helper
//! already uses (`runs/integration/cquit1-r1/helper.py:36`), with its
//! measure-before/after protocol (`:77-78`): a sentinel file asks, a JSON file
//! answers, and nothing at all is written to the PTY.
//!
//! R10 closes what that first shape left open. The directory is created
//! exclusively under an identifier that is really unique, every question carries
//! its own nonce, and an answer is accepted only when it is the answer to THAT
//! question: its label and nonce, the API it says it used, dimensions that are
//! real, and no error. The producer also publishes its own identity - pid,
//! parent and birth, through the mechanisms this fleet already qualified - which
//! is pinned once and verified again at the end, so a measurement belongs to the
//! tool it came from and to no other process that might have answered.

use super::{capture, new_session, RequestHandler};
use crate::test_shell::ScratchPath;
#[cfg(not(target_has_atomic = "64"))]
use portable_atomic::AtomicU64;
use rmux_proto::{
    OptionName, PaneTarget, Request, Response, ScopeSelector, SessionName, SetOptionMode,
    SetOptionRequest,
};
use std::path::Path;
#[cfg(target_has_atomic = "64")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;
use tokio::time::{sleep, Duration, Instant};

/// The producer's own program: announce once, publish who it is, then answer
/// questions by file and nothing else.
const PROBE_SOURCE: &str = r#"
import json, os, subprocess, sys, time
from pathlib import Path

directory = Path(sys.argv[1])
marker = sys.argv[2]

if os.name == "nt":
    import ctypes
    from ctypes import wintypes

    kernel32 = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel32.GetStdHandle.restype = wintypes.HANDLE

    class XY(ctypes.Structure):
        _fields_ = [("X", ctypes.c_short), ("Y", ctypes.c_short)]

    class RECT(ctypes.Structure):
        _fields_ = [("Left", ctypes.c_short), ("Top", ctypes.c_short),
                    ("Right", ctypes.c_short), ("Bottom", ctypes.c_short)]

    class INFO(ctypes.Structure):
        _fields_ = [("size", XY), ("cursor", XY), ("attributes", wintypes.WORD),
                    ("window", RECT), ("maximum", XY)]

    class FILETIME(ctypes.Structure):
        _fields_ = [("low", wintypes.DWORD), ("high", wintypes.DWORD)]

    kernel32.GetConsoleScreenBufferInfo.argtypes = [wintypes.HANDLE, ctypes.POINTER(INFO)]
    kernel32.GetProcessTimes.argtypes = [wintypes.HANDLE] + [ctypes.POINTER(FILETIME)] * 4
    kernel32.GetCurrentProcess.restype = wintypes.HANDLE
    handle = kernel32.GetStdHandle(-11)
    API = "GetConsoleScreenBufferInfo"

    def size():
        info = INFO()
        assert kernel32.GetConsoleScreenBufferInfo(handle, ctypes.byref(info)), \
            str(ctypes.get_last_error())
        return {"cols": info.size.X, "rows": info.size.Y,
                "window_cols": info.window.Right - info.window.Left + 1,
                "window_rows": info.window.Bottom - info.window.Top + 1,
                "api": API}

    def birth():
        created, exited, kernel, user = (FILETIME() for _ in range(4))
        assert kernel32.GetProcessTimes(kernel32.GetCurrentProcess(), ctypes.byref(created),
                                        ctypes.byref(exited), ctypes.byref(kernel),
                                        ctypes.byref(user)), str(ctypes.get_last_error())
        return {"value": (created.high << 32) | created.low, "api": "GetProcessTimes"}
else:
    API = "TIOCGWINSZ"

    def size():
        measured = os.get_terminal_size(0)
        return {"cols": measured.columns, "rows": measured.lines,
                "window_cols": measured.columns, "window_rows": measured.lines,
                "api": API}

    def birth():
        try:
            answer = subprocess.run(["/bin/ps", "-o", "lstart=", "-p", str(os.getpid())],
                                    capture_output=True, text=True, timeout=10)
        except subprocess.TimeoutExpired as expired:
            return {"value": "", "api": "ps -o lstart=", "exit": None,
                    "error": repr(expired)}
        return {"value": answer.stdout.strip(), "api": "ps -o lstart=",
                "exit": answer.returncode}


def publish(path, body):
    temporary = path.with_suffix(".tmp")
    temporary.write_text(json.dumps(body))
    os.replace(temporary, path)


publish(directory / "identity.json",
        {"pid": os.getpid(), "ppid": os.getppid(), "birth": birth(), "api": API,
         "at_ns": time.time_ns()})
sys.stdout.write(marker + "\r\n")
sys.stdout.flush()

while not (directory / "finish").exists():
    for request in sorted(directory.glob("*.ask")):
        answer = request.with_suffix(".json")
        if answer.exists():
            continue
        asked = json.loads(request.read_text())
        try:
            # Sounded again, here, through the same functions the identity was
            # published with: an answer carries the process it was measured by,
            # not a sentence written once at startup.
            body = dict(size(), pid=os.getpid(), ppid=os.getppid(), birth=birth(),
                        label=asked["label"], nonce=asked["nonce"], at_ns=time.time_ns())
        except BaseException as error:  # noqa: BLE001 - the failure is the answer
            body = {"pid": os.getpid(), "label": asked.get("label"),
                    "nonce": asked.get("nonce"), "error": repr(error)}
        publish(answer, body)
    time.sleep(0.02)
"#;

/// The API each platform's producer must say it used.
const EXPECTED_API: &str = if cfg!(windows) {
    "GetConsoleScreenBufferInfo"
} else {
    "TIOCGWINSZ"
};

/// How a birth is read on a platform, and therefore what makes one real.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(super) enum BirthPolicy {
    /// `ps -o lstart=` must have exited 0 and printed something.
    Posix,
    /// `GetProcessTimes` must have answered a creation time above zero.
    Windows,
}

impl BirthPolicy {
    const fn expected_api(self) -> &'static str {
        match self {
            Self::Posix => "ps -o lstart=",
            Self::Windows => "GetProcessTimes",
        }
    }

    const fn of_this_platform() -> Self {
        if cfg!(windows) {
            Self::Windows
        } else {
            Self::Posix
        }
    }
}

/// Whether a birth is one at all, before anything is compared with anything.
///
/// The same answer for the identity a producer publishes when it announces
/// itself and for the one it attaches to every measurement: an initial birth
/// that was never valid must not become the standard the rest is measured
/// against. What it checks, in this order: the API is the one this platform
/// reads births with, fixed by code and not by the pin; no error is reported;
/// and the reading really succeeded - `ps` exited 0 with a non-empty value, or
/// `GetProcessTimes` answered a creation time above zero.
pub(super) fn validate_birth(policy: BirthPolicy, birth: &serde_json::Value) -> Result<(), String> {
    if !birth.is_object() {
        return Err(format!("no birth was reported at all: {birth}"));
    }
    if let Some(error) = birth.get("error") {
        return Err(format!("the birth was not read: {error}"));
    }
    let api = birth["api"].as_str().unwrap_or_default();
    if api != policy.expected_api() {
        return Err(format!(
            "the birth names {api:?} where this platform reads {:?}",
            policy.expected_api()
        ));
    }
    match policy {
        BirthPolicy::Posix => {
            match birth["exit"].as_i64() {
                Some(0) => {}
                other => return Err(format!("ps did not answer: exit {other:?}")),
            }
            match birth["value"].as_str() {
                Some(when) if !when.trim().is_empty() => Ok(()),
                other => Err(format!("ps answered an empty birth: {other:?}")),
            }
        }
        BirthPolicy::Windows => match birth["value"].as_u64() {
            Some(created) if created > 0 => Ok(()),
            other => Err(format!(
                "GetProcessTimes answered no creation time: {other:?}"
            )),
        },
    }
}

static FIXTURES: AtomicU64 = AtomicU64::new(0);

/// One pane's own producer, the questions it can be asked, and who it is.
pub(super) struct ProbedPane {
    /// Removed with the pane's answers once the probe is dropped, even when
    /// the test unwinds.
    directory: ScratchPath,
    identity: serde_json::Value,
    pane_pid: u32,
    asked: std::cell::Cell<u32>,
}

fn python() -> &'static str {
    if cfg!(windows) {
        "python"
    } else {
        "/usr/bin/python3"
    }
}

fn quote(path: &Path) -> String {
    format!("\"{}\"", path.display().to_string().replace('\\', "\\\\"))
}

/// A directory this fixture alone owns: exclusive creation under an identifier
/// that cannot repeat - this process, a counter of its own, and the clock.
fn exclusive_directory(name: &str) -> ScratchPath {
    let unique = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .expect("the clock is after the epoch")
        .as_nanos();
    let directory = std::env::temp_dir().join(format!(
        "cquit1-r10-{name}-{}-{}-{unique}",
        std::process::id(),
        FIXTURES.fetch_add(1, Ordering::SeqCst)
    ));
    std::fs::create_dir(&directory).expect("this fixture's directory is created, exclusively");
    ScratchPath::adopt(directory)
}

async fn pin_probe_producer(handler: &RequestHandler, directory: &Path, marker: &str) {
    let script = directory.join("probe.py");
    std::fs::write(&script, PROBE_SOURCE).expect("the probe program is written");
    let command = format!(
        "{} {} {} {}",
        python(),
        quote(&script),
        quote(directory),
        marker
    );
    let response = handler
        .handle(Request::SetOption(SetOptionRequest {
            scope: ScopeSelector::Global,
            option: OptionName::DefaultCommand,
            value: command,
            mode: SetOptionMode::Replace,
        }))
        .await;
    assert!(matches!(response, Response::SetOption(_)), "{response:?}");
}

async fn wait_for_marker(handler: &RequestHandler, name: &str, pane: u32, marker: &str) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if capture(handler, &format!("{name}:0.{pane}"))
            .await
            .contains(marker)
        {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "timed out waiting for the probed pane {name}:0.{pane} to announce {marker}"
        );
        sleep(Duration::from_millis(25)).await;
    }
}

/// The identity the producer published for itself, read once it exists.
async fn read_identity(directory: &Path) -> serde_json::Value {
    let path = directory.join("identity.json");
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Ok(body) = std::fs::read_to_string(&path) {
            if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
                assert!(
                    value["pid"].as_u64().is_some_and(|pid| pid > 0),
                    "the producer's identity carries no pid: {value}"
                );
                assert_eq!(
                    value["api"].as_str(),
                    Some(EXPECTED_API),
                    "the producer names another API than this platform's: {value}"
                );
                // The pin has to be a real reading before it can be a pin: an
                // initial birth that was never read must not become what every
                // answer is then compared against.
                if let Err(reason) =
                    validate_birth(BirthPolicy::of_this_platform(), &value["birth"])
                {
                    panic!("the producer's initial identity is not usable: {reason}; {value}");
                }
                return value;
            }
        }
        assert!(
            Instant::now() < deadline,
            "the producer never published its identity in {}",
            directory.display()
        );
        sleep(Duration::from_millis(20)).await;
    }
}

async fn pane_pid_of(handler: &RequestHandler, target: &PaneTarget) -> u32 {
    handler
        .state
        .lock()
        .await
        .pane_pid_in_window(
            target.session_name(),
            target.window_index(),
            target.pane_index(),
        )
        .expect("the pane has a process")
}

impl ProbedPane {
    async fn adopt(handler: &RequestHandler, directory: ScratchPath, target: &PaneTarget) -> Self {
        let identity = read_identity(&directory).await;
        let pane_pid = pane_pid_of(handler, target).await;
        let producer = identity["pid"].as_u64().expect("a pid");
        let parent = identity["ppid"].as_u64().unwrap_or_default();
        // The pane's process and the producer are the same on Unix, where the
        // pane runs the program itself, and parent and child on Windows, where a
        // shell hosts it. Both are accepted; anything else is not this pane's.
        assert!(
            producer == u64::from(pane_pid) || parent == u64::from(pane_pid),
            "the producer {identity} is neither the pane's process {pane_pid} nor its child"
        );
        Self {
            directory,
            identity,
            pane_pid,
            asked: std::cell::Cell::new(0),
        }
    }

    /// A session whose first pane holds a producer that can be asked its width.
    pub(super) async fn new_session(handler: &RequestHandler, name: &str) -> (SessionName, Self) {
        let directory = exclusive_directory(name);
        let marker = format!("CQ9RDY-{name}-0");
        pin_probe_producer(handler, &directory, &marker).await;
        let session = new_session(handler, name).await;
        let target = PaneTarget::with_window(session.clone(), 0, 0);
        handler
            .wait_for_pane_startup_to_finish_for_test(&target)
            .await;
        wait_for_marker(handler, name, 0, &marker).await;
        let probed = Self::adopt(handler, directory, &target).await;
        (session, probed)
    }

    /// Asks this pane's own producer what its console really measures.
    ///
    /// The question is a file carrying its own nonce and so is the answer, and
    /// only the answer to THAT question is accepted: its label, its nonce, the
    /// API this platform expects, real dimensions and no error. Nothing is
    /// written to the pane's terminal, so a measurement can never be what raises
    /// its flag.
    pub(super) async fn measure(&self, label: &str) -> serde_json::Value {
        let index = self.asked.get() + 1;
        self.asked.set(index);
        let stem = format!("{index:03}-{label}");
        let nonce = format!(
            "{}-{}-{index}",
            std::process::id(),
            self.identity["pid"].as_u64().unwrap_or_default()
        );
        let answer = self.directory.join(format!("{stem}.json"));
        let question = serde_json::json!({"label": stem, "nonce": nonce});
        // Whole into a temporary of this same exclusive directory, closed, then
        // renamed: the producer polls for `*.ask` and would otherwise be able to
        // read a file that has been created but not yet filled.
        let temporary = self
            .directory
            .join(format!("{stem}.ask.tmp{}", std::process::id()));
        std::fs::write(
            &temporary,
            serde_json::to_vec(&question).expect("the question serialises"),
        )
        .expect("the question is written whole");
        std::fs::rename(&temporary, self.directory.join(format!("{stem}.ask")))
            .expect("the question is published atomically");
        let deadline = Instant::now() + Duration::from_secs(30);
        loop {
            if let Ok(body) = std::fs::read_to_string(&answer) {
                if let Ok(value) = serde_json::from_str::<serde_json::Value>(&body) {
                    return self.accept(&stem, &nonce, value);
                }
            }
            assert!(
                Instant::now() < deadline,
                "the producer never answered the measurement {stem}"
            );
            sleep(Duration::from_millis(20)).await;
        }
    }

    /// Everything an answer has to be before it is a measurement.
    fn accept(&self, stem: &str, nonce: &str, value: serde_json::Value) -> serde_json::Value {
        assert!(
            value.get("error").is_none(),
            "the producer answered {stem} with an error: {value}"
        );
        assert_eq!(
            value["label"].as_str(),
            Some(stem),
            "another label: {value}"
        );
        assert_eq!(
            value["nonce"].as_str(),
            Some(nonce),
            "another nonce: {value}"
        );
        assert_eq!(
            value["api"].as_str(),
            Some(EXPECTED_API),
            "another API than this platform's: {value}"
        );
        assert_eq!(
            value["pid"].as_u64(),
            self.identity["pid"].as_u64(),
            "another process than the producer this fixture pinned: {value}"
        );
        for field in ["cols", "rows"] {
            assert!(
                value[field].as_i64().is_some_and(|measured| measured > 0),
                "the {field} this answer carries is not a real dimension: {value}"
            );
        }
        self.accept_identity(stem, &value);
        value
    }

    /// The OS identity this answer was sounded with, and what it has to be.
    ///
    /// Not the receipt compared to itself: every answer carries a birth and a
    /// parent read again through the same qualified functions, and they are
    /// compared with what was pinned when the producer announced itself, and
    /// with the pane this fixture established it belongs to.
    fn accept_identity(&self, stem: &str, value: &serde_json::Value) {
        let birth = &value["birth"];
        if let Err(reason) = validate_birth(BirthPolicy::of_this_platform(), birth) {
            panic!("the identity sounded for {stem} is not usable: {reason}; {value}");
        }
        assert_eq!(
            birth["value"], self.identity["birth"]["value"],
            "the same pid, but not the same process as the one pinned: {value}"
        );
        assert!(
            value["ppid"].as_u64().is_some_and(|parent| parent > 0),
            "this answer carries no parent: {value}"
        );
        let producer = value["pid"].as_u64().expect("a pid");
        let parent = value["ppid"].as_u64().expect("a parent");
        assert!(
            producer == u64::from(self.pane_pid) || parent == u64::from(self.pane_pid),
            "the process that answered is no longer this pane's: {value} against {}",
            self.pane_pid
        );
    }

    /// The producer is still the one this fixture pinned, and still the pane's.
    ///
    /// This is a NEW exchange, with its own label and nonce, so what answers is
    /// an observation made now: its identity is sounded again through the same
    /// OS functions and checked like every other answer. The pane's own process
    /// is read again from the server too, and the parentage has to hold against
    /// that reading, not against the one taken when the producer announced
    /// itself.
    pub(super) async fn verify_identity(&self, handler: &RequestHandler, target: &PaneTarget) {
        let again = self.measure("final-identity").await;
        let pane_pid = pane_pid_of(handler, target).await;
        assert_eq!(pane_pid, self.pane_pid, "the pane changed its process");
        let producer = again["pid"].as_u64().expect("a pid");
        let parent = again["ppid"].as_u64().expect("a parent");
        assert!(
            producer == u64::from(pane_pid) || parent == u64::from(pane_pid),
            "the producer is no longer this pane's: {again} against {pane_pid}"
        );
    }

    /// Lets the producer leave on its own word. Called once nothing else needs
    /// the pane it holds.
    pub(super) fn finish(&self) {
        let _ = std::fs::write(self.directory.join("finish"), b"");
    }
}

#[cfg(test)]
mod tests {
    use super::{validate_birth, BirthPolicy};
    use serde_json::json;

    /// A birth is a pin only once it is a reading.
    ///
    /// The case this test exists for is the one that used to pass: an initial
    /// identity whose `ps` had FAILED - exit 1 - while carrying a date that
    /// looks perfectly valid, followed by a real answer with that same date and
    /// exit 0. Comparing the answer with that pin said they agreed, and the pin
    /// had never been read at all. The validator below is the one both the
    /// adoption and the answers now go through, so neither can accept it.
    ///
    /// Nothing here runs a producer, opens a terminal or asks the system for a
    /// process: these are values, and what is under test is the decision.
    #[test]
    fn a_birth_is_only_a_pin_once_it_is_a_reading() {
        let failed_ps_with_a_valid_looking_date =
            json!({"api": "ps -o lstart=", "value": "Mon Sep 15 20:00:00 2026", "exit": 1});
        let real_answer_with_the_same_date =
            json!({"api": "ps -o lstart=", "value": "Mon Sep 15 20:00:00 2026", "exit": 0});
        assert!(
            validate_birth(BirthPolicy::Posix, &failed_ps_with_a_valid_looking_date).is_err(),
            "a birth whose reading failed cannot become the standard the rest is compared to"
        );
        assert!(
            validate_birth(BirthPolicy::Posix, &real_answer_with_the_same_date).is_ok(),
            "while the same date, really read, is a birth"
        );

        for (accepted, birth) in [
            (
                true,
                json!({"api": "ps -o lstart=", "value": "Tue Sep 16 01:02:03 2026", "exit": 0}),
            ),
            (
                false,
                json!({"api": "GetProcessTimes", "value": "Tue Sep 16 01:02:03 2026", "exit": 0}),
            ),
            (
                false,
                json!({"api": "ps -o lstart=", "value": "x", "exit": 0, "error": "boom"}),
            ),
            (
                false,
                json!({"api": "ps -o lstart=", "value": "Tue Sep 16 01:02:03 2026", "exit": 1}),
            ),
            (
                false,
                json!({"api": "ps -o lstart=", "value": "Tue Sep 16 01:02:03 2026"}),
            ),
            (
                false,
                json!({"api": "ps -o lstart=", "value": "   ", "exit": 0}),
            ),
            (
                false,
                json!({"api": "ps -o lstart=", "value": "", "exit": 0}),
            ),
            (false, json!({"api": "ps -o lstart=", "exit": 0})),
            (
                false,
                json!({"api": "ps -o lstart=", "value": 1_789_000_000_000_u64, "exit": 0}),
            ),
            (false, json!("not an object")),
        ] {
            assert_eq!(
                validate_birth(BirthPolicy::Posix, &birth).is_ok(),
                accepted,
                "posix: {birth}"
            );
        }

        for (accepted, birth) in [
            (
                true,
                json!({"api": "GetProcessTimes", "value": 133_700_000_000_000_000_u64}),
            ),
            (
                false,
                json!({"api": "ps -o lstart=", "value": 133_700_000_000_000_000_u64}),
            ),
            (false, json!({"api": "GetProcessTimes", "value": 0})),
            (
                false,
                json!({"api": "GetProcessTimes", "value": "Tue Sep 16 01:02:03 2026"}),
            ),
            (false, json!({"api": "GetProcessTimes"})),
            (
                false,
                json!({"api": "GetProcessTimes", "value": 1, "error": "E_FAIL"}),
            ),
            (false, json!("not an object")),
        ] {
            assert_eq!(
                validate_birth(BirthPolicy::Windows, &birth).is_ok(),
                accepted,
                "windows: {birth}"
            );
        }

        // The policy this platform uses is the one its own reading names, and
        // both attachment points ask for it the same way.
        assert_eq!(
            BirthPolicy::of_this_platform().expected_api(),
            if cfg!(windows) {
                "GetProcessTimes"
            } else {
                "ps -o lstart="
            }
        );
    }
}
