//! A real pane child that persists the bytes its application actually read.
//!
//! Every other bracketed-paste suite in this crate observes
//! `RawPaneInputProbe`, which short-circuits
//! `prepare_pane_input_write_with_encoding` to `PaneInputSink::CapturedForTest`
//! *before* the starting-pane queue and *before* the Windows passthrough/legacy
//! sink selection. Those probes stay green when the legacy console sink is
//! bypassed, so they cannot close the `AttachInput::bytes -> ... -> real child
//! application bytes` obligation.
//!
//! A slot is a directory the harness and the child exchange files through:
//!
//! | File       | Written by | Meaning                                        |
//! |------------|------------|------------------------------------------------|
//! | `ready`    | child      | terminal setup is complete; read is starting        |
//! | `out.part` | child      | the capture so far, grown as bytes arrive       |
//! | `out`      | child      | the complete application-side capture           |
//! | `error`    | child      | a step failed, with the reason that step gives  |
//! | `stop`     | harness    | the child may leave its keep-alive park         |
//! | `done`     | child      | the child left its park, so teardown is over    |
//!
//! Only the child's own files are evidence that a child ran. `stop` is written
//! by the harness, so reading it back would make teardown believe in a child
//! for every slot it ever touched.
//!
//! The four Unix clock-paste cases opt into a VEOF-terminated capture (see
//! `unix_eof`): `out` then contains every byte before the controlled EOF, even
//! if the count is wrong. The default count-bounded children create it from an
//! `out.part` that already holds exactly the expected byte count, and any
//! failure keeps `out.part` and writes `error` instead. A reported `error`
//! therefore outranks everything the harness might otherwise infer from a
//! file's presence.
//!
//! `out.part` is what makes a mutation red readable. A mutation that routes a
//! bracketed paste back through the delimiter-consuming legacy path leaves the
//! child twelve bytes short forever; without a growing partial file the only
//! observable would be a timeout indistinguishable from reader starvation.
//! With one, the failure names the exact bytes that arrived and the first
//! offset at which they diverge.
//!
//! The default child reads from a *raw* terminal: a cooked console or PTY line
//! discipline treats the paste's leading `ESC` as an editing command and
//! rewrites CR/LF, so the captured bytes would say nothing about the sink. The
//! controlled-EOF variant retains raw byte mappings and disables editing while
//! enabling only canonical framing for its reserved VEOF. The
//! crate is `#![forbid(unsafe_code)]`, so the child cannot be this test binary
//! re-executed — the Windows console-mode change needs FFI.
//!
//! Unix therefore uses `/bin/sh`, which [`super`] already exists to build, and
//! Windows uses a small pinned Rust program: its read boundary is the whole
//! point of the R1 diagnostic, so it is the historical
//! `stdin().lock().read(&mut [0_u8; 4096])` itself rather than an emulation of
//! it. See [`byte_observer`] and [`windows_byte_child`].

use std::collections::hash_map::RandomState;
use std::fs;
use std::hash::{BuildHasher, Hasher};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::time::{Duration, Instant};

/// The receiver-owning attach guard both final-sink suites send input through.
mod attached_client;
/// The historical standard-input byte boundary the Windows child reads through,
/// compiled both into this binary and into that child.
mod byte_observer;
/// What the production transcript observed when a proof gave up waiting for a
/// child's announcement. Shared, because both final-sink siblings wait for it.
pub(crate) mod pane_observation;
/// The Unix child's capture protocol. Generated on every platform so its shape
/// is checked wherever this crate's tests run, executed only on Unix.
mod unix_child;
#[cfg(unix)]
mod unix_eof;
#[cfg(unix)]
mod unix_eof_witness;
#[cfg(windows)]
mod windows_byte_child;

pub(crate) use attached_client::AttachedFinalSinkClient;
pub(crate) use pane_observation::{describe_missing_bracketed_mode, observe_pane_output};

const READY_FILE: &str = "ready";
const OUT_FILE: &str = "out";
const OUT_PARTIAL_FILE: &str = "out.part";
const ERROR_FILE: &str = "error";
const STOP_FILE: &str = "stop";
const DONE_FILE: &str = "done";

/// Every artifact a child or the harness may leave in a slot. Finding one at
/// construction time means the directory belongs to an earlier run.
const SLOT_ARTIFACTS: [&str; 6] = [
    READY_FILE,
    OUT_PARTIAL_FILE,
    OUT_FILE,
    ERROR_FILE,
    STOP_FILE,
    DONE_FILE,
];

/// The subset only a child writes, which is therefore the only file evidence
/// that a child ran.
///
/// `stop` is excluded deliberately: teardown writes it itself, so counting it
/// would be the harness taking its own signal as proof of a process.
const CHILD_ARTIFACTS: [&str; 5] = [
    READY_FILE,
    OUT_PARTIAL_FILE,
    OUT_FILE,
    ERROR_FILE,
    DONE_FILE,
];

/// How long the child stays alive after capturing, so the harness can still
/// resolve it as a live synchronized destination while it asserts.
const CHILD_PARK_SECONDS: u64 = 120;
/// Generous: a pane child starts a process and a terminal before it can read.
const CAPTURE_TIMEOUT: Duration = Duration::from_secs(60);
/// Once at least one byte has arrived, a gap this long with no further byte
/// means the sink has stopped delivering.
///
/// This never decides a pass. A complete capture is reported by the child
/// through `out`, so the success oracle is always the byte count, never elapsed
/// time. The gap only bounds how long an already-failing capture is waited on,
/// which is what lets a delimiter-consuming mutation report its short body
/// promptly instead of after the full timeout.
const CAPTURE_IDLE_GAP: Duration = Duration::from_secs(8);
/// Teardown is bounded: a child that will not leave its park must not hang the
/// suite. The wait returns what it observed, and a slot whose teardown was not
/// acknowledged is kept, with its path and reason printed, instead of removed.
const TEARDOWN_TIMEOUT: Duration = Duration::from_secs(2);
const POLL_INTERVAL: Duration = Duration::from_millis(25);
/// Escaped byte dumps stay readable; elision is always stated, never silent.
const MAX_ESCAPED_BYTES: usize = 512;

pub(crate) struct FinalSinkSlot {
    directory: PathBuf,
    expected: Vec<u8>,
    bracket_aware: bool,
    #[cfg(unix)]
    capture_until_eof: bool,
    capture_timeout: Duration,
    capture_idle_gap: Duration,
    teardown_timeout: Duration,
    /// Whether the command carrying this slot's child was ever submitted.
    ///
    /// Shared rather than owned because the call sites that submit it hold the
    /// slot by reference, and it is the fact teardown cannot recover from the
    /// filesystem: a child that dies before writing anything leaves a slot
    /// indistinguishable from one that was never launched.
    launch_attempted: AtomicBool,
}

impl FinalSinkSlot {
    /// `expected` is the exact byte sequence the child must read. It must be
    /// valid UTF-8: the Windows child converts the console's UTF-16 to UTF-8
    /// itself, and arbitrary non-UTF-8 bytes are a separate policy that this
    /// harness deliberately does not exercise.
    pub(crate) fn new(label: &str, expected: &[u8], bracket_aware: bool) -> Self {
        Self::create(fresh_slot_directory(label), expected, bracket_aware)
            .unwrap_or_else(|failure| panic!("{failure}"))
    }

    /// Fallible so the stale-slot regression can assert the exact rejection.
    fn create(directory: PathBuf, expected: &[u8], bracket_aware: bool) -> Result<Self, String> {
        assert!(
            std::str::from_utf8(expected).is_ok(),
            "the final-sink harness only covers the valid-UTF-8 #92 path"
        );
        // Exclusive, not `create_dir_all`: a slot must never inherit another
        // child's `ready` or `out`. Readiness and output are file existence
        // checks, so an adopted directory would let an abandoned child satisfy
        // this run before its own child captured anything.
        if let Err(error) = fs::create_dir(&directory) {
            if error.kind() == std::io::ErrorKind::AlreadyExists {
                let stale = SLOT_ARTIFACTS
                    .iter()
                    .filter(|artifact| directory.join(artifact).exists())
                    .copied()
                    .collect::<Vec<_>>();
                return Err(format!(
                    "final-sink slot {} already exists and cannot be adopted; \
                     evidence left by an earlier child: {stale:?}",
                    directory.display()
                ));
            }
            return Err(format!(
                "final-sink slot {} could not be created: {error}",
                directory.display()
            ));
        }
        Ok(Self {
            directory,
            expected: expected.to_vec(),
            bracket_aware,
            #[cfg(unix)]
            capture_until_eof: false,
            capture_timeout: CAPTURE_TIMEOUT,
            capture_idle_gap: CAPTURE_IDLE_GAP,
            teardown_timeout: TEARDOWN_TIMEOUT,
            launch_attempted: AtomicBool::new(false),
        })
    }

    /// Records that the command carrying this slot's child was submitted.
    ///
    /// Called immediately *before* submission rather than after it returns:
    /// from the moment the request is in flight a process may exist, and a
    /// teardown that ran against this slot must not read it as never launched.
    fn record_launch_attempt(&self) {
        self.launch_attempted.store(true, Ordering::SeqCst);
    }

    fn launch_was_attempted(&self) -> bool {
        self.launch_attempted.load(Ordering::SeqCst)
    }

    /// The child-written files present in this slot.
    fn child_artifacts(&self) -> Vec<&'static str> {
        CHILD_ARTIFACTS
            .iter()
            .filter(|artifact| self.directory.join(artifact).exists())
            .copied()
            .collect()
    }

    /// Shortens the diagnostic bounds so a harness regression can reach them
    /// without waiting out the real ones.
    fn with_capture_bounds(mut self, timeout: Duration, idle_gap: Duration) -> Self {
        self.capture_timeout = timeout;
        self.capture_idle_gap = idle_gap;
        self
    }

    /// Same, for the teardown bound.
    fn with_teardown_timeout(mut self, timeout: Duration) -> Self {
        self.teardown_timeout = timeout;
        self
    }

    fn path(&self, file: &str) -> String {
        self.directory.join(file).display().to_string()
    }

    pub(crate) fn wait_until_ready(&self) {
        self.try_wait_until_ready()
            .unwrap_or_else(|failure| panic!("{failure}"));
    }

    fn try_wait_until_ready(&self) -> Result<(), String> {
        let deadline = Instant::now() + self.capture_timeout;
        loop {
            if self.directory.join(READY_FILE).is_file() {
                return Ok(());
            }
            if let Some(error) = self.child_error() {
                return Err(
                    self.describe("the child failed before signalling readiness", Some(error))
                );
            }
            if Instant::now() >= deadline {
                return Err(self.describe(
                    &format!(
                        "the child never signalled readiness within {:?}",
                        self.capture_timeout
                    ),
                    None,
                ));
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// Asserts that the child read exactly the bytes this slot was built for.
    ///
    /// Every final-sink proof compares against its own `expected`, so routing
    /// the comparison through the slot gives every red — short capture, wrong
    /// delimiters, timeout — the same unambiguous representation.
    pub(crate) fn assert_application_bytes(&self, context: &str) {
        match self.try_application_bytes() {
            Ok(received) if received == self.expected => {}
            Ok(received) => panic!(
                "{}",
                self.describe_against(
                    &format!("{context}: the child read the wrong bytes"),
                    &received,
                    None
                )
            ),
            Err(failure) => panic!("{context}: {failure}"),
        }
    }

    /// Blocks until the child persisted its complete capture, or until the sink
    /// demonstrably stopped delivering, and reports exact bytes either way.
    fn try_application_bytes(&self) -> Result<Vec<u8>, String> {
        let deadline = Instant::now() + self.capture_timeout;
        let mut partial_len = self.partial_bytes().len();
        let mut partial_grew_at = Instant::now();
        loop {
            // A reported failure outranks a published capture. Both children
            // publish `out` only after a successful read (count or EOF), so the two are
            // mutually exclusive; checking `out` first is what let a script
            // that renamed a short partial be read as a completed publication
            // and reported as merely the wrong bytes with `child error: none`.
            if let Some(error) = self.child_error() {
                return Err(self.describe(
                    "the child failed before persisting its capture",
                    Some(error),
                ));
            }
            if self.directory.join(OUT_FILE).is_file() {
                return fs::read(self.directory.join(OUT_FILE)).map_err(|error| {
                    self.describe(
                        &format!("the child's capture could not be read: {error}"),
                        None,
                    )
                });
            }
            let now = Instant::now();
            let current = self.partial_bytes().len();
            if current != partial_len {
                partial_len = current;
                partial_grew_at = now;
            }
            if partial_len > 0 && now.duration_since(partial_grew_at) >= self.capture_idle_gap {
                return Err(self.describe(
                    &format!(
                        "the child received no further byte for {:?} while its capture was still short",
                        self.capture_idle_gap
                    ),
                    None,
                ));
            }
            if now >= deadline {
                return Err(self.describe(
                    &format!(
                        "the child never persisted its capture within {:?}",
                        self.capture_timeout
                    ),
                    None,
                ));
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// The bytes the child has received so far. `dd` on Unix and the Windows
    /// child both grow `out.part` as input arrives, so this is the real
    /// application-side prefix, not a reconstruction.
    fn partial_bytes(&self) -> Vec<u8> {
        fs::read(self.directory.join(OUT_PARTIAL_FILE)).unwrap_or_default()
    }

    fn child_error(&self) -> Option<String> {
        fs::read_to_string(self.directory.join(ERROR_FILE)).ok()
    }

    fn describe(&self, headline: &str, child_error: Option<String>) -> String {
        let received = self.partial_bytes();
        self.describe_against(headline, &received, child_error)
    }

    /// One representation of every boundary a final-sink failure can hit, so a
    /// red names the delimiter or body difference rather than only a deadline.
    fn describe_against(
        &self,
        headline: &str,
        received: &[u8],
        child_error: Option<String>,
    ) -> String {
        let mut report = format!(
            "final-sink capture failed: {headline}\n  \
             slot: {}\n  \
             expected {} bytes: {}\n  \
             received {} bytes: {}",
            self.directory.display(),
            self.expected.len(),
            escape_bytes(&self.expected),
            received.len(),
            escape_bytes(received),
        );
        match first_difference(&self.expected, received) {
            Some(offset) => report.push_str(&format!(
                "\n  first difference at byte {offset}: expected {}, received {}",
                describe_byte(self.expected.get(offset).copied()),
                describe_byte(received.get(offset).copied()),
            )),
            None if received.len() < self.expected.len() => report.push_str(&format!(
                "\n  the received bytes are an exact prefix; {} byte(s) never arrived",
                self.expected.len() - received.len()
            )),
            None if received.len() > self.expected.len() => report.push_str(&format!(
                "\n  the expected bytes are an exact prefix; {} extra byte(s) arrived",
                received.len() - self.expected.len()
            )),
            None => report.push_str("\n  the bytes match"),
        }
        report.push_str(&format!(
            "\n  child error: {}\n  ready: {}",
            child_error.as_deref().unwrap_or("none"),
            self.directory.join(READY_FILE).is_file(),
        ));
        report
    }

    /// Runs the checked `/bin/sh` capture protocol. `dd bs=1` writes each byte
    /// to `out.part` as it arrives, so a capture that never completes still
    /// exposes exactly what reached the child.
    #[cfg(unix)]
    pub(crate) fn pane_command(&self) -> Vec<String> {
        if self.capture_until_eof {
            unix_eof::pane_command(self)
        } else {
            unix_child::pane_command(self)
        }
    }

    /// Runs the pinned Rust child, whose read boundary is the historical
    /// `stdin().lock().read(&mut [0_u8; 4096])`.
    ///
    /// The slot files, expected byte count, awareness and park duration are
    /// arguments rather than an interpolated script, so nothing about the
    /// child's source varies between runs — which is what lets the Windows 10
    /// A/B rebuild exactly this observer.
    #[cfg(windows)]
    pub(crate) fn pane_command(&self) -> Vec<String> {
        let program = windows_byte_child::child_program().unwrap_or_else(|failure| {
            panic!("the final-sink Windows child is unavailable: {failure}")
        });
        vec![
            program.display().to_string(),
            self.path(READY_FILE),
            self.path(OUT_PARTIAL_FILE),
            self.path(OUT_FILE),
            self.path(ERROR_FILE),
            self.path(STOP_FILE),
            self.path(DONE_FILE),
            self.expected.len().to_string(),
            if self.bracket_aware {
                "aware"
            } else {
                "unaware"
            }
            .to_owned(),
            CHILD_PARK_SECONDS.to_string(),
        ]
    }
}

/// What bounded teardown actually observed.
///
/// Teardown used to return nothing, so an expired wait was indistinguishable
/// from an acknowledgement and the slot was removed either way — including the
/// slot that was the only evidence the child never left its park.
#[derive(Debug, Clone, PartialEq, Eq)]
enum TeardownOutcome {
    /// No command was ever submitted and the slot holds nothing a child wrote,
    /// so there is no process to ask and nothing to acknowledge.
    NeverLaunched,
    /// The child wrote `done`: it observably left its park.
    Acknowledged,
    /// `stop` could not be written, so the child was never asked to leave.
    StopNotSignalled { reason: String },
    /// The wait expired with no acknowledgement. Carries why teardown believed
    /// a child existed, so a preserved slot names the evidence it was kept for.
    TimedOut {
        waited: Duration,
        launch_attempted: bool,
        child_artifacts: Vec<&'static str>,
        child_error: Option<String>,
    },
}

impl TeardownOutcome {
    /// Only a slot no child ever inhabited, or one whose child observably left
    /// its park, may be removed.
    fn permits_removal(&self) -> bool {
        matches!(self, Self::NeverLaunched | Self::Acknowledged)
    }
}

impl std::fmt::Display for TeardownOutcome {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::NeverLaunched => formatter
                .write_str("no child was ever launched for this slot and it left nothing behind"),
            Self::Acknowledged => formatter.write_str("the child acknowledged teardown"),
            Self::StopNotSignalled { reason } => write!(
                formatter,
                "`stop` could not be written, so the child was never asked to leave its park: \
                 {reason}"
            ),
            Self::TimedOut {
                waited,
                launch_attempted,
                child_artifacts,
                child_error,
            } => write!(
                formatter,
                "the child never acknowledged teardown within {waited:?}; \
                 launch attempted: {launch_attempted}; \
                 child artifacts: {child_artifacts:?}; \
                 child error: {}",
                child_error.as_deref().unwrap_or("none")
            ),
        }
    }
}

impl Drop for FinalSinkSlot {
    fn drop(&mut self) {
        let outcome = self.teardown();
        if std::thread::panicking() {
            self.preserve(
                "the failing assertion's diagnosis needs these artifacts",
                &outcome,
            );
            return;
        }
        if !outcome.permits_removal() {
            self.preserve("teardown was not acknowledged", &outcome);
            return;
        }
        if let Err(error) = fs::remove_dir_all(&self.directory) {
            self.preserve(&format!("the slot could not be removed: {error}"), &outcome);
        }
    }
}

impl FinalSinkSlot {
    /// Signals `stop`, then waits, bounded, for the child to acknowledge that
    /// it left its park.
    ///
    /// The question is whether a child exists, and readiness was the wrong way
    /// to ask it. Both children establish raw mode *before* signalling `ready`,
    /// so a setup failure writes `error`, parks, and writes `done` only once
    /// `stop` appears — a live child with no `ready` at all. Teardown used to
    /// map every missing `ready` to "never started" and let the slot be
    /// removed, which discarded that `error` and the directory proving it.
    ///
    /// So the launch attempt is recorded where the command is submitted, and
    /// the child's own files are read as evidence that something ran. Only a
    /// slot that was never launched *and* holds nothing a child wrote skips the
    /// wait; everything else is owed an acknowledgement. Slot paths are unique
    /// per instance, so a child that outlives this wait can still never satisfy
    /// a later run; the wait exists so teardown is *observed* rather than
    /// assumed, and so its failure is reported together with the slot that
    /// proves it.
    ///
    /// Terminating and reaping the child *process* is deliberately not done
    /// here. That is owned by `PaneTerminal::terminate_with_bounded_grace`,
    /// which signals the process tree and waits for its exit; in tests it is
    /// reached through `Drop for RequestHandler` → `shutdown_terminals_for_test`
    /// → `PaneTerminalStore::remove_session`. A slot that also managed the
    /// process would be a second, competing owner of that lifecycle.
    fn teardown(&self) -> TeardownOutcome {
        // An acknowledgement already on disk answers everything, whether or not
        // the child ever reached `ready`, and there is nobody left to signal.
        if self.directory.join(DONE_FILE).is_file() {
            return TeardownOutcome::Acknowledged;
        }
        let launch_attempted = self.launch_was_attempted();
        let child_artifacts = self.child_artifacts();
        if !launch_attempted && child_artifacts.is_empty() {
            return TeardownOutcome::NeverLaunched;
        }
        if let Err(error) = fs::write(self.directory.join(STOP_FILE), b"1") {
            return TeardownOutcome::StopNotSignalled {
                reason: error.to_string(),
            };
        }
        let deadline = Instant::now() + self.teardown_timeout;
        loop {
            if self.directory.join(DONE_FILE).is_file() {
                return TeardownOutcome::Acknowledged;
            }
            if Instant::now() >= deadline {
                return TeardownOutcome::TimedOut {
                    waited: self.teardown_timeout,
                    launch_attempted,
                    child_artifacts,
                    child_error: self.child_error(),
                };
            }
            std::thread::sleep(POLL_INTERVAL);
        }
    }

    /// Keeps a slot and names both why it was kept and what teardown saw.
    fn preserve(&self, reason: &str, outcome: &TeardownOutcome) {
        eprintln!(
            "final-sink slot preserved for diagnosis: {}\n  reason: {reason}\n  teardown: {outcome}",
            self.directory.display()
        );
    }
}

/// A slot path no earlier run can reproduce.
///
/// The process id and a process-local counter are not enough: after an
/// interrupted run the operating system can reuse the id while the counter
/// restarts at zero, so a later invocation with the same label would land on an
/// abandoned directory. `RandomState` is seeded by the standard library from
/// the operating system, so the token differs between processes that share an
/// id.
fn fresh_slot_directory(label: &str) -> PathBuf {
    static NEXT: AtomicU32 = AtomicU32::new(0);
    let mut hasher = RandomState::new().build_hasher();
    hasher.write_u32(std::process::id());
    hasher.write_u32(NEXT.fetch_add(1, Ordering::Relaxed));
    std::env::temp_dir().join(format!(
        "rmux-final-sink-{}-{:016x}-{label}",
        std::process::id(),
        hasher.finish()
    ))
}

/// The first offset at which the two sequences disagree, or `None` when one is
/// a prefix of the other.
fn first_difference(expected: &[u8], received: &[u8]) -> Option<usize> {
    expected
        .iter()
        .zip(received)
        .position(|(expected, received)| expected != received)
}

fn describe_byte(byte: Option<u8>) -> String {
    byte.map_or_else(|| "nothing".to_owned(), |byte| format!("0x{byte:02x}"))
}

/// Renders bytes so `ESC[200~` and a raw `0x02` are both legible. Elision is
/// always announced with its exact size.
fn escape_bytes(bytes: &[u8]) -> String {
    let head = bytes.len().min(MAX_ESCAPED_BYTES);
    let mut escaped = String::with_capacity(head + 16);
    for &byte in &bytes[..head] {
        match byte {
            b'\\' => escaped.push_str(r"\\"),
            0x20..=0x7e => escaped.push(byte as char),
            _ => escaped.push_str(&format!("\\x{byte:02x}")),
        }
    }
    if bytes.len() > head {
        escaped.push_str(&format!("... [{} more byte(s)]", bytes.len() - head));
    }
    escaped
}

/// Creates a detached session whose only pane is a real final-sink child.
///
/// Lives here rather than in either test module because the active-path proofs
/// sit under the private `handler::tests` tree and the deferred proof sits under
/// `handler::session_support`; neither can see the other.
pub(crate) async fn create_final_sink_session(
    handler: &crate::handler::RequestHandler,
    session: &rmux_proto::SessionName,
    slot: &FinalSinkSlot,
) {
    let command = slot.pane_command();
    // Recorded before the request is submitted: from here on a child may exist,
    // and its slot must never be torn down as one that was never launched.
    slot.record_launch_attempt();
    let created = handler
        .handle(rmux_proto::Request::NewSessionExt(Box::new(
            rmux_proto::NewSessionExtRequest {
                session_name: Some(session.clone()),
                // The child script carries absolute slot paths, so it needs
                // neither a working directory nor environment plumbing.
                working_directory: None,
                detached: true,
                size: Some(rmux_proto::TerminalSize { cols: 80, rows: 24 }),
                environment: None,
                group_target: None,
                attach_if_exists: false,
                detach_other_clients: false,
                kill_other_clients: false,
                flags: None,
                window_name: None,
                print_session_info: false,
                print_format: None,
                command: Some(command),
                process_command: None,
                client_environment: None,
                skip_environment_update: false,
            },
        )))
        .await;
    assert!(
        matches!(created, rmux_proto::Response::NewSession(_)),
        "unexpected new-session response: {created:?}"
    );
}

/// Adds a second final-sink pane to window 0 and returns its target.
pub(crate) async fn split_final_sink_pane(
    handler: &crate::handler::RequestHandler,
    session: &rmux_proto::SessionName,
    slot: &FinalSinkSlot,
) -> rmux_proto::PaneTarget {
    let command = slot.pane_command();
    // Same obligation as the session call site above.
    slot.record_launch_attempt();
    let split = handler
        .handle(rmux_proto::Request::SplitWindowExt(Box::new(
            rmux_proto::SplitWindowExtRequest {
                target: rmux_proto::SplitWindowTarget::Pane(rmux_proto::PaneTarget::new(
                    session.clone(),
                    0,
                )),
                direction: rmux_proto::SplitDirection::Horizontal,
                before: false,
                environment: None,
                command: Some(command),
                process_command: None,
                start_directory: None,
                keep_alive_on_exit: None,
                detached: false,
                size: None,
                preserve_zoom: false,
                full_size: false,
                stdin_payload: None,
            },
        )))
        .await;
    let rmux_proto::Response::SplitWindow(split) = split else {
        panic!("expected split-window response: {split:?}");
    };
    split.pane
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Stages the acknowledgement a real child would have written.
    ///
    /// The capture-diagnostic cases below fabricate slot files with no child
    /// behind them. That fabrication is exactly what teardown now reads as a
    /// started child, so without this every one of their slots would be
    /// correctly preserved as unacknowledged.
    fn stage_acknowledgement(slot: &FinalSinkSlot) {
        fs::write(slot.directory.join(DONE_FILE), b"1").expect("stage the acknowledgement");
    }

    /// A slot must not adopt a directory an earlier child left behind: it would
    /// accept that child's `ready` and `out` as this run's evidence.
    #[test]
    fn a_slot_left_by_an_earlier_child_is_never_adopted() {
        let directory = fresh_slot_directory("stale-rejection");
        fs::create_dir(&directory).expect("stage the abandoned slot");
        fs::write(directory.join(READY_FILE), b"1").expect("stage stale readiness");
        fs::write(directory.join(OUT_FILE), b"stale").expect("stage stale capture");

        let Err(failure) = FinalSinkSlot::create(directory.clone(), b"fresh", true) else {
            panic!("an existing slot must be rejected, never adopted");
        };

        assert!(
            failure.contains("already exists"),
            "unexpected rejection: {failure}"
        );
        assert!(
            failure.contains(READY_FILE) && failure.contains(OUT_FILE),
            "the rejection must name the stale evidence: {failure}"
        );
        assert_eq!(
            fs::read(directory.join(OUT_FILE)).expect("stale capture survives"),
            b"stale",
            "rejecting a slot must not destroy the evidence in it"
        );
        fs::remove_dir_all(&directory).expect("clean up the staged slot");
    }

    /// A delimiter-consuming sink leaves the child short forever. The failure
    /// must name the bytes that did arrive, not only that time ran out.
    #[test]
    fn a_short_capture_reports_the_exact_bytes_the_child_received() {
        let expected = b"\x1b[200~body\x1b[201~";
        let slot = FinalSinkSlot::new("short-capture", expected, true)
            .with_capture_bounds(Duration::from_secs(30), Duration::from_millis(200));
        // Exactly what the legacy console path leaves behind: the body with
        // both six-byte delimiters consumed.
        fs::write(slot.directory.join(OUT_PARTIAL_FILE), b"body").expect("stage the partial");
        stage_acknowledgement(&slot);

        let failure = slot
            .try_application_bytes()
            .expect_err("a capture that stays short must fail");

        assert!(
            failure.contains("no further byte for 200ms"),
            "the idle boundary must be named: {failure}"
        );
        assert!(
            failure.contains(r"expected 16 bytes: \x1b[200~body\x1b[201~"),
            "the expected bytes must be shown: {failure}"
        );
        assert!(
            failure.contains("received 4 bytes: body"),
            "the received bytes must be shown: {failure}"
        );
        assert!(
            failure.contains("first difference at byte 0: expected 0x1b, received 0x62"),
            "the divergence must be located: {failure}"
        );
    }

    /// A capture that is merely truncated is a different diagnosis from one
    /// whose bytes disagree, and must read as such.
    #[test]
    fn a_truncated_capture_is_reported_as_an_exact_prefix() {
        let slot = FinalSinkSlot::new("truncated-capture", b"abcdef", false)
            .with_capture_bounds(Duration::from_secs(30), Duration::from_millis(200));
        fs::write(slot.directory.join(OUT_PARTIAL_FILE), b"abc").expect("stage the partial");
        stage_acknowledgement(&slot);

        let failure = slot
            .try_application_bytes()
            .expect_err("a capture that stays short must fail");

        assert!(
            failure.contains("the received bytes are an exact prefix; 3 byte(s) never arrived"),
            "a truncation must not be reported as a mismatch: {failure}"
        );
    }

    /// A child that reported a failure must be diagnosed by that reason, not by
    /// whichever slot file the harness happens to look at first. A script that
    /// renamed a short partial used to be read as a completed publication and
    /// reported as merely the wrong bytes with `child error: none`.
    #[test]
    fn a_reported_child_failure_outranks_a_published_capture() {
        let slot = FinalSinkSlot::new("error-precedence", b"abcdef", false);
        fs::write(slot.directory.join(OUT_PARTIAL_FILE), b"abc").expect("stage the partial");
        fs::write(slot.directory.join(OUT_FILE), b"abc").expect("stage a published capture");
        fs::write(
            slot.directory.join(ERROR_FILE),
            b"standard input ended after 3 of 6 bytes",
        )
        .expect("stage the child's own reason");
        stage_acknowledgement(&slot);

        let failure = slot
            .try_application_bytes()
            .expect_err("a reported failure must not be read as a completed capture");

        assert!(
            failure.contains("the child failed before persisting its capture"),
            "the reported failure must be the diagnosis: {failure}"
        );
        assert!(
            failure.contains("child error: standard input ended after 3 of 6 bytes"),
            "the child's own reason must be carried through: {failure}"
        );
    }

    /// The F3 case: a child that reached readiness and then never left its
    /// park. The expired wait must be observable, and the slot that proves it
    /// must survive — it used to be deleted exactly like an acknowledged one.
    #[test]
    fn a_teardown_that_is_never_acknowledged_times_out_and_keeps_the_slot() {
        let slot = FinalSinkSlot::new("teardown-timeout", b"abc", true)
            .with_teardown_timeout(Duration::from_millis(150));
        fs::write(slot.directory.join(READY_FILE), b"1").expect("stage readiness");
        let directory = slot.directory.clone();

        let outcome = slot.teardown();

        assert_eq!(
            outcome,
            TeardownOutcome::TimedOut {
                waited: Duration::from_millis(150),
                launch_attempted: false,
                child_artifacts: vec![READY_FILE],
                child_error: None,
            },
            "an expired wait must be distinguishable from an acknowledgement"
        );
        assert!(
            !outcome.permits_removal(),
            "an unacknowledged teardown must not authorise removal"
        );
        assert!(
            outcome
                .to_string()
                .contains("never acknowledged teardown within 150ms"),
            "the timeout must be reportable: {outcome}"
        );
        assert!(
            directory.join(STOP_FILE).is_file(),
            "the child must still have been asked to leave its park"
        );

        drop(slot);

        assert!(
            directory.is_dir(),
            "the slot of a child that never acknowledged teardown is the evidence"
        );
        fs::remove_dir_all(&directory).expect("clean up the preserved slot");
    }

    /// The converse: an acknowledged teardown is what authorises removal.
    #[test]
    fn an_acknowledged_teardown_removes_the_slot() {
        let slot = FinalSinkSlot::new("teardown-acknowledged", b"abc", true)
            .with_teardown_timeout(Duration::from_millis(150));
        fs::write(slot.directory.join(READY_FILE), b"1").expect("stage readiness");
        fs::write(slot.directory.join(DONE_FILE), b"1").expect("stage the acknowledgement");
        let directory = slot.directory.clone();

        let outcome = slot.teardown();

        assert_eq!(outcome, TeardownOutcome::Acknowledged);
        assert!(outcome.permits_removal());

        drop(slot);

        assert!(!directory.exists(), "an acknowledged slot is removed");
    }

    /// The F3 finding itself. Both children establish raw mode *before* they
    /// signal readiness, so a setup failure writes `error`, parks, and writes
    /// `done` only once `stop` appears. Teardown used to read the missing
    /// `ready` as "never started" and let the slot be removed — discarding the
    /// `error` that was the only account of what went wrong.
    #[test]
    fn a_setup_failure_without_readiness_is_waited_for_and_keeps_its_slot() {
        let slot = FinalSinkSlot::new("teardown-error-no-ready", b"abc", true)
            .with_teardown_timeout(Duration::from_millis(150));
        fs::write(
            slot.directory.join(ERROR_FILE),
            b"GetConsoleMode failed: standard input is not a console",
        )
        .expect("stage the child's setup failure");
        let directory = slot.directory.clone();

        let outcome = slot.teardown();

        assert_eq!(
            outcome,
            TeardownOutcome::TimedOut {
                waited: Duration::from_millis(150),
                launch_attempted: false,
                child_artifacts: vec![ERROR_FILE],
                child_error: Some(
                    "GetConsoleMode failed: standard input is not a console".to_owned()
                ),
            },
            "a child that reported a setup failure must never be read as never launched"
        );
        assert!(
            !outcome.permits_removal(),
            "an unacknowledged setup failure must not authorise removal"
        );
        assert!(
            outcome.to_string().contains(
                "child artifacts: [\"error\"]; \
                 child error: GetConsoleMode failed: standard input is not a console"
            ),
            "the evidence and the child's own reason must reach the report: {outcome}"
        );
        assert!(
            directory.join(STOP_FILE).is_file(),
            "the child must still have been asked to leave its park"
        );

        drop(slot);

        assert!(
            directory.is_dir(),
            "the slot carrying the setup failure is the evidence"
        );
        fs::remove_dir_all(&directory).expect("clean up the preserved slot");
    }

    /// The same child once it has acknowledged: `error` with no `ready` is
    /// still a complete teardown when `done` is there, and needs no `stop`
    /// because the park is already over.
    #[test]
    fn a_setup_failure_that_acknowledged_teardown_releases_its_slot() {
        let slot = FinalSinkSlot::new("teardown-error-acknowledged", b"abc", true)
            .with_teardown_timeout(Duration::from_millis(150));
        fs::write(
            slot.directory.join(ERROR_FILE),
            b"readiness could not be signalled",
        )
        .expect("stage the child's setup failure");
        fs::write(slot.directory.join(DONE_FILE), b"1").expect("stage the acknowledgement");
        let directory = slot.directory.clone();

        let outcome = slot.teardown();

        assert_eq!(outcome, TeardownOutcome::Acknowledged);
        assert!(outcome.permits_removal());
        assert!(
            !directory.join(STOP_FILE).exists(),
            "a child that already left its park is not asked again"
        );

        drop(slot);

        assert!(!directory.exists(), "an acknowledged slot is removed");
    }

    /// A launch that produced no artifact at all is still a launch: the command
    /// was submitted, so a process may exist and its acknowledgement is owed.
    /// The filesystem cannot show this, which is why the attempt is recorded.
    #[test]
    fn a_recorded_launch_with_no_artifacts_is_waited_for_and_keeps_its_slot() {
        let slot = FinalSinkSlot::new("teardown-launched-silent", b"abc", true)
            .with_teardown_timeout(Duration::from_millis(150));
        slot.record_launch_attempt();
        let directory = slot.directory.clone();

        let outcome = slot.teardown();

        assert_eq!(
            outcome,
            TeardownOutcome::TimedOut {
                waited: Duration::from_millis(150),
                launch_attempted: true,
                child_artifacts: Vec::new(),
                child_error: None,
            },
            "a submitted command must not be read as never launched"
        );
        assert!(!outcome.permits_removal());
        assert!(
            outcome.to_string().contains("launch attempted: true"),
            "the report must say a launch was attempted: {outcome}"
        );
        assert!(directory.join(STOP_FILE).is_file());

        drop(slot);

        assert!(
            directory.is_dir(),
            "a launched child that never acknowledged keeps its slot"
        );
        fs::remove_dir_all(&directory).expect("clean up the preserved slot");
    }

    /// The genuine fast path: nothing was ever submitted and no child wrote
    /// anything, so there is nobody to ask and nothing to wait for. `stop` is
    /// not written either — there is no park to end.
    #[test]
    fn a_slot_whose_child_was_never_launched_is_removed_without_waiting() {
        let slot = FinalSinkSlot::new("teardown-never-launched", b"abc", false)
            .with_teardown_timeout(Duration::from_secs(30));
        let directory = slot.directory.clone();

        let started = Instant::now();
        let outcome = slot.teardown();

        assert_eq!(outcome, TeardownOutcome::NeverLaunched);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "teardown must not wait for a child that was never launched"
        );
        assert!(
            !directory.join(STOP_FILE).exists(),
            "a child that was never launched is not asked to leave a park"
        );
        assert!(outcome.permits_removal());

        drop(slot);

        assert!(!directory.exists(), "a never-launched slot is removed");
    }

    /// `stop` is the harness's own signal. Counting it as evidence that a child
    /// ran would make every slot teardown ever touched look inhabited, so a
    /// slot holding nothing but `stop` is still never-launched.
    #[test]
    fn the_harness_written_stop_is_never_read_as_child_evidence() {
        let slot = FinalSinkSlot::new("teardown-stop-not-evidence", b"abc", false)
            .with_teardown_timeout(Duration::from_secs(30));
        fs::write(slot.directory.join(STOP_FILE), b"1").expect("stage a harness signal");
        let directory = slot.directory.clone();

        let started = Instant::now();
        let outcome = slot.teardown();

        assert_eq!(outcome, TeardownOutcome::NeverLaunched);
        assert!(
            started.elapsed() < Duration::from_secs(5),
            "the harness's own signal must not make teardown wait"
        );
        assert!(outcome.permits_removal());

        drop(slot);

        assert!(!directory.exists());
    }

    /// A child that could not be asked to leave its park tells us nothing about
    /// whether it left, so its slot is kept and the reason is reported.
    #[test]
    fn a_stop_that_could_not_be_written_is_reported_and_keeps_the_slot() {
        let slot = FinalSinkSlot::new("teardown-stop-unwritable", b"abc", true)
            .with_teardown_timeout(Duration::from_millis(150));
        fs::write(slot.directory.join(ERROR_FILE), b"the child failed early")
            .expect("stage child evidence");
        // A directory at `stop` cannot be replaced by a file write on either
        // platform, which is the failure teardown has to survive.
        fs::create_dir(slot.directory.join(STOP_FILE)).expect("block the stop signal");
        let directory = slot.directory.clone();

        let outcome = slot.teardown();

        let TeardownOutcome::StopNotSignalled { reason } = &outcome else {
            panic!("a `stop` that could not be written must be reported: {outcome:?}");
        };
        assert!(!reason.is_empty(), "the reason must be carried through");
        assert!(
            !outcome.permits_removal(),
            "a child that was never asked to leave must not authorise removal"
        );

        drop(slot);

        assert!(
            directory.is_dir(),
            "a child that was never asked to leave keeps its slot"
        );
        fs::remove_dir_all(&directory).expect("clean up the preserved slot");
    }

    #[test]
    fn escaped_bytes_stay_legible_and_announce_every_elision() {
        assert_eq!(escape_bytes(b"\x1b[200~a\\b\x02"), r"\x1b[200~a\\b\x02");
        let long = vec![b'a'; MAX_ESCAPED_BYTES + 3];
        assert!(escape_bytes(&long).ends_with("... [3 more byte(s)]"));
    }
}
