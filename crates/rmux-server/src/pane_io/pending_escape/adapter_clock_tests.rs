//! Four component witnesses from the independently reviewed M03 contract.
//! Retained buffers are explicit fixtures; this does not exercise a decoder.
use std::time::Duration;

use tokio::runtime::{Builder, Handle, RuntimeFlavor};
use tokio::time::Instant;

use super::super::{
    pending_escape_kind, PendingEscapeFlush, PendingEscapeKind, PendingEscapeProvenance,
    PendingInputDispatch,
};

const ESCAPE_TIME: Duration = Duration::from_millis(50);

struct Case {
    name: &'static str,
    episode: &'static str,
    origin: Instant,
    timer: PendingEscapeFlush,
    pending: Vec<u8>,
}

#[derive(Debug)]
struct Observation {
    now: Duration,
    deadline: Option<Duration>,
    pending: Vec<u8>,
    provenance: Option<PendingEscapeProvenance>,
    streaming_len: Option<usize>,
    kind: Option<PendingEscapeKind>,
    due: Option<bool>,
}

struct Expected {
    at_ms: u64,
    deadline_ms: Option<u64>,
    pending: &'static [u8],
    provenance: Option<PendingEscapeProvenance>,
    streaming_len: Option<usize>,
    kind: Option<PendingEscapeKind>,
    due: Option<bool>,
}

impl Expected {
    fn empty(at_ms: u64) -> Self {
        Self {
            at_ms,
            deadline_ms: None,
            pending: b"",
            provenance: None,
            streaming_len: None,
            kind: None,
            due: None,
        }
    }

    fn ambiguous(at_ms: u64, deadline_ms: Option<u64>, pending: &'static [u8]) -> Self {
        Self {
            at_ms,
            deadline_ms,
            pending,
            provenance: Some(PendingEscapeProvenance::AmbiguousKeyboard),
            streaming_len: None,
            kind: Some(PendingEscapeKind::Ambiguous),
            due: None,
        }
    }

    fn streaming(at_ms: u64, deadline_ms: u64, pending: &'static [u8]) -> Self {
        Self {
            at_ms,
            deadline_ms: Some(deadline_ms),
            pending,
            provenance: Some(PendingEscapeProvenance::StreamingControl),
            streaming_len: Some(pending.len()),
            kind: Some(PendingEscapeKind::Streaming),
            due: None,
        }
    }
}

impl Case {
    fn new(name: &'static str, episode: &'static str) -> Self {
        assert_eq!(
            Handle::current().runtime_flavor(),
            RuntimeFlavor::CurrentThread
        );
        Self {
            name,
            episode,
            origin: Instant::now(),
            timer: PendingEscapeFlush::default(),
            pending: Vec::new(),
        }
    }

    fn now(&self) -> Duration {
        Instant::now()
            .checked_duration_since(self.origin)
            .expect("clock must not precede this case's origin")
    }

    fn assert_clock(&self, at_ms: u64) {
        assert_eq!(self.now(), Duration::from_millis(at_ms));
    }

    async fn set_clock(&self, at_ms: u64) {
        let before = self.now();
        let target = Duration::from_millis(at_ms);
        let delta = target.checked_sub(before).expect("backward clock rejected");
        if !delta.is_zero() {
            // A no-op advance cannot pass the exact comparison below.
            tokio::time::advance(delta).await;
        }
        let after = self.now();
        println!(
            "M03_CLOCK {}",
            serde_json::json!({
                "case": self.name, "episode": self.episode,
                "runtime_id": Handle::current().id().to_string(),
                "target_ns": target.as_nanos(),
                "before_ns": before.as_nanos(), "after_ns": after.as_nanos(),
            })
        );
        assert_eq!(
            after, target,
            "advance must change the actual Tokio clock exactly"
        );
    }

    fn observe(&mut self, phase: &str, at_ms: u64, before: &[u8], appended: &[u8], after: &[u8]) {
        self.assert_clock(at_ms);
        assert_eq!(self.pending, before, "fixture continuity");
        let dispatch = PendingInputDispatch::capture(&self.pending, appended);
        // Only the retained byte buffer is staged. Never assign timer fields.
        self.pending.clear();
        self.pending.extend_from_slice(after);
        self.timer.observe_input_dispatch(dispatch, &self.pending);
        self.assert_clock(at_ms);
        self.snapshot(phase, false);
    }

    fn sync(&mut self, at_ms: u64) {
        self.assert_clock(at_ms);
        self.timer.sync(&self.pending, ESCAPE_TIME);
        self.assert_clock(at_ms);
    }

    fn snapshot(&self, phase: &str, poll_due: bool) -> Observation {
        let runtime = Handle::current();
        assert_eq!(runtime.runtime_flavor(), RuntimeFlavor::CurrentThread);
        // This function has no expected-state argument or table access.
        let observed = Observation {
            now: self.now(),
            deadline: self.timer.deadline().map(|deadline| {
                deadline
                    .checked_duration_since(self.origin)
                    .expect("deadline must not precede the case origin")
            }),
            pending: self.pending.clone(),
            provenance: self.timer.provenance,
            streaming_len: self.timer.streaming_len,
            kind: pending_escape_kind(&self.pending),
            due: poll_due.then(|| crate::pane_io::pending_escape_deadline_due(&self.timer)),
        };
        assert_eq!(self.now(), observed.now, "snapshot must not move time");
        println!(
            "M03_OBSERVATION {}",
            serde_json::json!({
                "case": self.name, "episode": self.episode, "phase": phase,
                "runtime_id": runtime.id().to_string(),
                "runtime_flavor": format!("{:?}", runtime.runtime_flavor()),
                "origin": format!("{:?}", self.origin),
                "now_ns": observed.now.as_nanos(),
                "deadline_ns": observed.deadline.map(|value| value.as_nanos()),
                "pending_hex": observed.pending.iter().map(|byte| format!("{byte:02x}")).collect::<String>(),
                "provenance": observed.provenance.map(|value| format!("{value:?}")),
                "streaming_len": observed.streaming_len,
                "kind": observed.kind.map(|value| format!("{value:?}")),
                "due": observed.due,
            })
        );
        observed
    }

    fn check(&self, phase: &str, expected: Expected) {
        let observed = self.snapshot(phase, false);
        Self::compare(observed, expected);
    }

    fn check_due(&self, phase: &str, expected: Expected) {
        let observed = self.snapshot(phase, true);
        Self::compare(observed, expected);
    }

    fn compare(observed: Observation, expected: Expected) {
        assert_eq!(observed.now, Duration::from_millis(expected.at_ms));
        assert_eq!(
            observed.deadline,
            expected.deadline_ms.map(Duration::from_millis)
        );
        assert_eq!(observed.pending, expected.pending);
        assert_eq!(observed.provenance, expected.provenance);
        assert_eq!(observed.streaming_len, expected.streaming_len);
        assert_eq!(observed.kind, expected.kind);
        assert_eq!(observed.due, expected.due);
    }
}

#[test]
fn l11_m03_clock_zero_and_reset() {
    let first_runtime = Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap();
    let first_id = first_runtime.handle().id();
    first_runtime.block_on(async {
        let mut case = Case::new("l11_m03_clock_zero_and_reset", "A");
        case.check("fresh", Expected::empty(0));
        case.observe("arm.after_observe", 0, b"", b"\x1b[", b"\x1b[");
        case.sync(0);
        case.check("armed", Expected::ambiguous(0, Some(50), b"\x1b["));
        case.set_clock(10).await;
        case.timer.clear();
        case.pending.clear();
        case.check("cleared", Expected::empty(10));
    });
    drop(first_runtime);

    let second_runtime = Builder::new_current_thread()
        .enable_time()
        .start_paused(true)
        .build()
        .unwrap();
    assert_ne!(
        first_id,
        second_runtime.handle().id(),
        "independent runtime"
    );
    second_runtime.block_on(async {
        let case = Case::new("l11_m03_clock_zero_and_reset", "B");
        case.check("fresh", Expected::empty(0));
    });
    drop(second_runtime);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn l11_m03_keyboard_growth_keeps_origin() {
    let mut case = Case::new("l11_m03_keyboard_growth_keeps_origin", "only");
    case.check("fresh", Expected::empty(0));
    case.observe("initial.after_observe", 0, b"", b"\x1b[", b"\x1b[");
    case.sync(0);
    case.check("initial", Expected::ambiguous(0, Some(50), b"\x1b["));
    case.set_clock(10).await;
    case.observe("growth.after_observe", 10, b"\x1b[", b"12;", b"\x1b[12;");
    case.sync(10);
    case.check("growth", Expected::ambiguous(10, Some(50), b"\x1b[12;"));
    case.set_clock(49).await;
    case.sync(49);
    case.check("unchanged", Expected::ambiguous(49, Some(50), b"\x1b[12;"));
    let mut due_before_deadline = Expected::ambiguous(49, Some(50), b"\x1b[12;");
    due_before_deadline.due = Some(false);
    case.check_due("due49", due_before_deadline);
    case.set_clock(50).await;
    let mut due_at_deadline = Expected::ambiguous(50, Some(50), b"\x1b[12;");
    due_at_deadline.due = Some(true);
    case.check_due("due50", due_at_deadline);
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn l11_m03_stream_promotes_and_rearms_only_on_growth() {
    let mut case = Case::new("l11_m03_stream_promotes_and_rearms_only_on_growth", "only");
    case.check("fresh", Expected::empty(0));
    case.observe("initial.after_observe", 0, b"", b"\x1b]", b"\x1b]");
    case.sync(0);
    case.check("initial", Expected::ambiguous(0, Some(50), b"\x1b]"));
    case.set_clock(10).await;
    case.observe("promotion.after_observe", 10, b"\x1b]", b"52;", b"\x1b]52;");
    case.sync(10);
    case.check("promotion", Expected::streaming(10, 8010, b"\x1b]52;"));
    case.set_clock(100).await;
    case.observe(
        "growth.after_observe",
        100,
        b"\x1b]52;",
        b"c;AA",
        b"\x1b]52;c;AA",
    );
    case.sync(100);
    case.check("growth", Expected::streaming(100, 8100, b"\x1b]52;c;AA"));
    case.set_clock(200).await;
    case.sync(200);
    case.check("unchanged", Expected::streaming(200, 8100, b"\x1b]52;c;AA"));
}

#[tokio::test(flavor = "current_thread", start_paused = true)]
async fn l11_m03_consumed_stream_starts_suffix_lifetime() {
    let mut case = Case::new("l11_m03_consumed_stream_starts_suffix_lifetime", "only");
    case.check("fresh", Expected::empty(0));
    case.observe(
        "stream.after_observe",
        0,
        b"",
        b"\x1b]52;c;YQ==",
        b"\x1b]52;c;YQ==",
    );
    case.sync(0);
    case.check("stream", Expected::streaming(0, 8000, b"\x1b]52;c;YQ=="));
    case.set_clock(20).await;
    case.observe(
        "suffix.after_observe",
        20,
        b"\x1b]52;c;YQ==",
        b"\x1b\\\x1b",
        b"\x1b",
    );
    // G-M03-OBSERVE: the real observe call must clear the old stream deadline
    // before sync. A no-op observe would leave8000/StreamingControl/Some(11).
    case.check("G-M03-OBSERVE", Expected::ambiguous(20, None, b"\x1b"));
    case.sync(20);
    case.check("suffix", Expected::ambiguous(20, Some(70), b"\x1b"));
}
