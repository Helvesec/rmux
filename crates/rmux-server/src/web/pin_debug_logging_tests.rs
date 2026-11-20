use std::sync::{Arc, Mutex};
use tracing::{span, Event, Metadata, Subscriber};

use super::{tests::request, WebSharePairingCodes};
use crate::web::WebShareRegistry;

#[test]
fn diagnostic_logs_redact_pins_and_preserve_registry_events() {
    let mut request = request();
    request.operator_pin = Some("193847".to_owned());
    request.spectator_pin = Some("826591".to_owned());
    let codes = WebSharePairingCodes::for_request(&request).expect("pairing codes");
    let logs = Arc::new(Mutex::new(String::new()));
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("test runtime");
    // Keep two dispatches registered so parallel callsite registration cannot
    // cache only the default subscriber of a neighboring test thread.
    let _callsite_guard = tracing::Dispatch::new(tracing::subscriber::NoSubscriber::new());
    tracing::subscriber::with_default(CapturedLogs(logs.clone()), || {
        tracing::error!(pairing_codes = ?codes, "pairing_debug_canary");
        runtime.block_on(async {
            let registry = WebShareRegistry::default();
            registry.mark_listener_available();
            let created = registry.create(request).expect("share creates");
            let token = created
                .operator_url
                .as_deref()
                .unwrap()
                .split("t=")
                .nth(1)
                .expect("operator token");
            let error = registry
                .connect(token, Some("826591"))
                .await
                .expect_err("spectator PIN cannot authenticate operator");
            assert!(!error.to_string().contains("826591"));
            let access = registry
                .connect(token, Some("193847"))
                .await
                .expect("operator authenticates");
            tracing::error!(?access, "access_debug_canary");
            assert_eq!(
                access.operator_visible_spectator_pairing_code(),
                Some("826591")
            );
        });
    });
    let logs = logs.lock().unwrap();
    for event in [
        "pairing_debug_canary",
        "access_debug_canary",
        "web_share_created",
        "web_share_auth_backoff",
        "web_share_access_granted",
    ] {
        assert!(
            logs.contains(event),
            "diagnostic event must actually be captured"
        );
    }
    for pin in ["193847", "826591"] {
        assert!(
            !logs.contains(pin),
            "PIN must not leak into diagnostic logs"
        );
    }
}

struct CapturedLogs(Arc<Mutex<String>>);

impl Subscriber for CapturedLogs {
    fn enabled(&self, _: &Metadata<'_>) -> bool {
        true
    }
    fn new_span(&self, _: &span::Attributes<'_>) -> span::Id {
        span::Id::from_u64(1)
    }
    fn record(&self, _: &span::Id, _: &span::Record<'_>) {}
    fn record_follows_from(&self, _: &span::Id, _: &span::Id) {}
    fn event(&self, event: &Event<'_>) {
        struct Fields<'a>(&'a mut String);
        impl tracing::field::Visit for Fields<'_> {
            fn record_debug(&mut self, _: &tracing::field::Field, value: &dyn std::fmt::Debug) {
                use std::fmt::Write;
                write!(self.0, "{value:?}").unwrap();
            }
        }
        let mut logs = self.0.lock().unwrap();
        logs.push_str(event.metadata().name());
        event.record(&mut Fields(&mut logs));
        logs.push('\n');
    }
    fn enter(&self, _: &span::Id) {}
    fn exit(&self, _: &span::Id) {}
}
