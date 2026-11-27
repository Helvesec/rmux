//! Compile and run with only --extern rmux_sdk from a locked root build.
use std::error::Error;
use rmux_sdk::{protocol_diagnostic, unsupported_feature_id, CollectError, DiagnosticSeverity,
    PaneId, ProtocolError, RmuxError, SessionName};

fn bounds<T: Error + Send + Sync + 'static>() {}

fn main() {
    bounds::<RmuxError>();
    bounds::<ProtocolError>();
    bounds::<CollectError>();
    let error = RmuxError::protocol(ProtocolError::new("caller diagnostic"));
    let RmuxError::Protocol { source, .. } = &error else { panic!("protocol") };
    assert_eq!(source.to_string(), "caller diagnostic");
    assert_eq!(source.invalid_target(), None);
    assert_eq!(source.session_not_found(), None);
    assert_eq!(source.duplicate_session(), None);
    assert_eq!(source.server_message(), None);
    assert!(error.source().unwrap().is::<ProtocolError>());
    assert!(error.source().unwrap().source().is_none());
    let diagnostic = protocol_diagnostic(&error);
    assert_eq!(diagnostic.severity(), DiagnosticSeverity::Error);
    assert_eq!(diagnostic.message(), "rmux protocol error: caller diagnostic");
    assert_eq!(diagnostic.hint(), error.hint());
    let unsupported = RmuxError::unsupported("capability.future", "upgrade");
    assert_eq!(unsupported_feature_id(&unsupported).as_deref(), Some("capability.future"));
    assert_eq!(rmux_sdk::diagnostics::protocol_diagnostic(&unsupported).hint(), Some("upgrade"));
    let missing = RmuxError::pane_not_found(SessionName::new("alpha").unwrap(), PaneId::new(7));
    assert_eq!(protocol_diagnostic(&missing).message(), "pane id %7 was not found in session alpha");
    assert_eq!(protocol_diagnostic(&missing).hint(), None);
    let aggregate = RmuxError::collect(CollectError::new(vec![error, unsupported, missing]));
    assert!(aggregate.source().unwrap().is::<CollectError>());
    assert_eq!(protocol_diagnostic(&aggregate).message(), aggregate.to_string());
}
