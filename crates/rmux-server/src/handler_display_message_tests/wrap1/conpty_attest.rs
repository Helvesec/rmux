//! Bounded, journey-scoped ConPTY evidence; Windows only.

/// Collects the ConPTY records this journey attests from, under a bound.
///
/// `api.rs` and `pty.rs` emit on `rmux::conpty`, and the per-child attestation
/// (the console that child ended up on, read after its companion startup)
/// on `rmux::conpty::attest`. This crate depends on `tracing` but on no
/// collector, so in a test binary those records reach nobody; this is that
/// collector and nothing else.
///
/// It is BOUNDED in count and in volume, and every way it can fail to be a
/// faithful record is a fact it carries: an installation that was refused, a
/// lock that was poisoned, or a bound that was reached all make the attestation
/// INVALID. None of them is allowed to look like success.
#[derive(Debug, Default)]
pub(super) struct ConptyCollector {
    lines: std::sync::Mutex<Vec<(String, String)>>,
    dropped: std::sync::atomic::AtomicUsize,
    bytes: std::sync::atomic::AtomicUsize,
    poisoned: std::sync::atomic::AtomicBool,
}

/// At most this many records, and at most this many bytes of them.
const COLLECTOR_MAX_RECORDS: usize = 512;
const COLLECTOR_MAX_BYTES: usize = 256 * 1024;

#[derive(Debug, Clone)]
pub(super) struct ConptyAttestation {
    installed: bool,
    pub(super) records: Vec<(String, String)>,
    dropped: usize,
    bytes: usize,
    poisoned: bool,
}

impl ConptyAttestation {
    /// Why this attestation cannot be trusted, when it cannot.
    pub(super) fn invalid_because(&self) -> Option<String> {
        if !self.installed {
            return Some("the collector was refused installation".to_owned());
        }
        if self.poisoned {
            return Some("the collector's lock was poisoned".to_owned());
        }
        if self.dropped > 0 {
            return Some(format!(
                "the collector reached its bound and dropped {} record(s), keeping {} bytes",
                self.dropped, self.bytes
            ));
        }
        None
    }

    /// The per-child attestations, as `(pid, rendered fields)`.
    pub(super) fn per_child(&self) -> Vec<(u32, String)> {
        self.records
            .iter()
            .filter(|(target, _)| target == "rmux::conpty::attest")
            .filter_map(|(_, rendered)| {
                let pid = rendered
                    .split_whitespace()
                    .find_map(|field| field.strip_prefix("pid="))?
                    .parse()
                    .ok()?;
                Some((pid, rendered.clone()))
            })
            .collect()
    }
}

struct ConptyVisitor<'a>(&'a mut String);

impl ConptyVisitor<'_> {
    fn push(&mut self, name: &str, value: &str) {
        if !self.0.is_empty() {
            self.0.push(' ');
        }
        self.0.push_str(name);
        self.0.push('=');
        self.0.push_str(value);
    }
}

impl tracing::field::Visit for ConptyVisitor<'_> {
    fn record_debug(&mut self, field: &tracing::field::Field, value: &dyn std::fmt::Debug) {
        self.push(field.name(), &format!("{value:?}"));
    }

    fn record_u64(&mut self, field: &tracing::field::Field, value: u64) {
        self.push(field.name(), &value.to_string());
    }

    fn record_bool(&mut self, field: &tracing::field::Field, value: bool) {
        self.push(field.name(), &value.to_string());
    }

    fn record_str(&mut self, field: &tracing::field::Field, value: &str) {
        self.push(field.name(), value);
    }
}

/// The collector as a subscriber; the trait is implemented on this local type,
/// since neither `Arc` nor `Subscriber` belongs to this crate.
struct ConptySubscriber(std::sync::Arc<ConptyCollector>);

impl tracing::Subscriber for ConptySubscriber {
    fn enabled(&self, metadata: &tracing::Metadata<'_>) -> bool {
        metadata.target().starts_with("rmux::conpty")
    }

    fn new_span(&self, _: &tracing::span::Attributes<'_>) -> tracing::span::Id {
        tracing::span::Id::from_u64(1)
    }

    fn record(&self, _: &tracing::span::Id, _: &tracing::span::Record<'_>) {}

    fn record_follows_from(&self, _: &tracing::span::Id, _: &tracing::span::Id) {}

    fn event(&self, event: &tracing::Event<'_>) {
        use std::sync::atomic::Ordering;
        let mut rendered = String::new();
        event.record(&mut ConptyVisitor(&mut rendered));
        let target = event.metadata().target().to_owned();
        let weight = target.len() + rendered.len();
        let Ok(mut lines) = self.0.lines.lock() else {
            self.0.poisoned.store(true, Ordering::SeqCst);
            return;
        };
        if lines.len() >= COLLECTOR_MAX_RECORDS
            || self.0.bytes.load(Ordering::SeqCst) + weight > COLLECTOR_MAX_BYTES
        {
            // Dropped, and said so: a bound that is reached invalidates the
            // attestation rather than truncating it quietly.
            self.0.dropped.fetch_add(1, Ordering::SeqCst);
            return;
        }
        self.0.bytes.fetch_add(weight, Ordering::SeqCst);
        lines.push((target, rendered));
    }

    fn enter(&self, _: &tracing::span::Id) {}

    fn exit(&self, _: &tracing::span::Id) {}
}

/// Runs one journey under a fresh scoped dispatch. Foreign test threads and
/// later journeys cannot consume its budget or reuse its failure state.
pub(super) async fn collect<F, Fut>(run: F)
where
    F: FnOnce(std::sync::Arc<ConptyCollector>, bool) -> Fut,
    Fut: std::future::Future<Output = ()>,
{
    use tracing::instrument::WithSubscriber;
    let collector = std::sync::Arc::new(ConptyCollector::default());
    let dispatch = tracing::Dispatch::new(ConptySubscriber(std::sync::Arc::clone(&collector)));
    async move {
        let installed = tracing::dispatcher::get_default(|active| active.is::<ConptySubscriber>());
        run(collector, installed).await;
    }
    .with_subscriber(dispatch)
    .await;
}

#[tokio::test]
async fn collector_isolates_foreign_conpty_traffic() {
    collect(|collector, installed| async move {
        // Same target and callsite for both writers: ownership must come
        // from the dispatch, never from dropping a required event kind.
        fn emit(pid: u32) {
            tracing::debug!(target: "rmux::conpty::attest", pid, "collector isolation witness");
        }
        emit(7);
        std::thread::spawn(|| {
            for _ in 0..513 {
                emit(99);
            }
        })
        .join()
        .expect("foreign producer finishes");
        let attestation = conpty_attestation(&collector, installed);
        assert_eq!(attestation.invalid_because(), None, "{attestation:?}");
        assert_eq!(attestation.per_child().len(), 1, "{attestation:?}");
        assert_eq!(attestation.per_child()[0].0, 7);
    })
    .await;
}

/// Reads the collector without disturbing it.
pub(super) fn conpty_attestation(
    collector: &ConptyCollector,
    installed: bool,
) -> ConptyAttestation {
    use std::sync::atomic::Ordering;
    let (records, poisoned) = match collector.lines.lock() {
        Ok(lines) => (lines.clone(), false),
        Err(_) => (Vec::new(), true),
    };
    ConptyAttestation {
        installed,
        records,
        dropped: collector.dropped.load(Ordering::SeqCst),
        bytes: collector.bytes.load(Ordering::SeqCst),
        poisoned: poisoned || collector.poisoned.load(Ordering::SeqCst),
    }
}

#[tokio::test]
async fn collector_keeps_owned_saturation_invalid() {
    collect(|collector, installed| async move {
        for _ in 0..513 {
            tracing::debug!(target: "rmux::conpty", "owned record");
        }
        let attestation = conpty_attestation(&collector, installed);
        assert_eq!(attestation.records.len(), 512);
        assert_eq!(attestation.dropped, 1);
        assert!(attestation
            .invalid_because()
            .unwrap()
            .contains("dropped 1 record(s)"));
    })
    .await;
}

#[tokio::test]
async fn collector_keeps_owned_byte_overflow_invalid() {
    collect(|collector, installed| async move {
        let oversized = "x".repeat(256 * 1024);
        tracing::debug!(target: "rmux::conpty", payload = oversized.as_str());
        let attestation = conpty_attestation(&collector, installed);
        assert!(attestation.records.is_empty());
        assert_eq!(attestation.bytes, 0);
        assert_eq!(attestation.dropped, 1);
        assert!(attestation.invalid_because().is_some());
    })
    .await;
}

#[tokio::test]
async fn collector_keeps_poison_invalid_and_next_journey_fresh() {
    collect(|collector, installed| async move {
        let other = std::sync::Arc::clone(&collector);
        assert!(std::thread::spawn(move || {
            let _lock = other.lines.lock().unwrap();
            panic!("intentional collector poison");
        })
        .join()
        .is_err());
        tracing::debug!(target: "rmux::conpty", "owned event after poison");
        let attestation = conpty_attestation(&collector, installed);
        assert!(attestation.poisoned);
        assert_eq!(
            attestation.invalid_because().as_deref(),
            Some("the collector's lock was poisoned")
        );
    })
    .await;
    collect(|collector, installed| async move {
        let attestation = conpty_attestation(&collector, installed);
        assert_eq!(attestation.invalid_because(), None);
        assert!(attestation.records.is_empty());
        assert_eq!(attestation.dropped, 0);
        assert_eq!(attestation.bytes, 0);
    })
    .await;
}

#[test]
fn collector_keeps_refused_installation_invalid() {
    let collector = ConptyCollector::default();
    let attestation = conpty_attestation(&collector, false);
    assert_eq!(
        attestation.invalid_because().as_deref(),
        Some("the collector was refused installation")
    );
}

#[tokio::test]
async fn collector_attests_both_real_panes_across_deferred_startup() {
    for inherited_runtime in [true, false] {
        collect(|collector, installed| async move {
            let name = if inherited_runtime {
                "wrap1-owned-runtime"
            } else {
                "wrap1-owned-fallback"
            };
            let nonce = format!("n{}", std::process::id());
            let handler = if inherited_runtime {
                super::RequestHandler::new()
            } else {
                // Constructed outside Tokio: exercises spawn_deferred_initial_pane's
                // fallback route as well as the saved server-runtime route.
                std::thread::spawn(super::RequestHandler::new)
                    .join()
                    .unwrap()
            };
            assert_eq!(handler.server_task_runtime().is_some(), inherited_runtime);
            let (handler, _session) =
                super::commanded_pair_with_handler(handler, name, &nonce).await;
            let identities = super::identities(&handler, name).await;
            let mut wanted: Vec<u32> = identities
                .lines()
                .map(|line| line.split('|').nth(1).unwrap().parse().unwrap())
                .collect();
            wanted.sort_unstable();
            assert_eq!(wanted.len(), 2);
            assert!(wanted.iter().all(|pid| *pid != 0));
            let attestation = conpty_attestation(&collector, installed);
            assert_eq!(attestation.invalid_because(), None, "{attestation:?}");
            let mut actual: Vec<u32> = attestation
                .per_child()
                .into_iter()
                .map(|(pid, _)| pid)
                .collect();
            actual.sort_unstable();
            assert_eq!(
                actual, wanted,
                "both native startup paths retain their own complete evidence: {attestation:?}"
            );
        })
        .await;
    }
}

#[tokio::test]
async fn collector_isolates_foreign_future_between_polls() {
    let ready = tokio::sync::Notify::new();
    let finished = tokio::sync::Notify::new();
    let (ready, finished) = (&ready, &finished);
    tokio::join!(
        collect(|collector, installed| async move {
            tracing::debug!(target: "rmux::conpty::attest", pid = 7u32, "owned future");
            ready.notify_one();
            finished.notified().await;
            let attestation = conpty_attestation(&collector, installed);
            assert_eq!(attestation.invalid_because(), None, "{attestation:?}");
            assert_eq!(attestation.per_child().len(), 1, "{attestation:?}");
            assert_eq!(attestation.per_child()[0].0, 7);
        }),
        async {
            ready.notified().await;
            for _ in 0..513 {
                tracing::debug!(target: "rmux::conpty::attest", pid = 99u32, "foreign future");
            }
            finished.notify_one();
        }
    );
}
