use rmux_sdk::{DiagnosticSeverity, ProtocolError, RmuxError, SessionName, SessionNameError};
use std::error::Error;

fn sdk_new(raw: &str) -> rmux_sdk::Result<SessionName> {
    Ok(SessionName::new(raw)?)
}

fn sdk_parse(raw: &str) -> rmux_sdk::Result<SessionName> {
    Ok(raw.parse()?)
}

fn sdk_borrowed(raw: &str) -> rmux_sdk::Result<SessionName> {
    Ok(SessionName::try_from(raw)?)
}

fn sdk_owned(raw: &str) -> rmux_sdk::Result<SessionName> {
    Ok(SessionName::try_from(raw.to_owned())?)
}

fn assert_empty_diagnostic(error: RmuxError) {
    const MESSAGE: &str = "invalid session name: session names must be non-empty";
    const HINT: &str =
        "check the request and daemon state, then retry after correcting the request";
    let RmuxError::Protocol { ref source, .. } = error else {
        panic!("expected SDK protocol diagnostic: {error:?}");
    };
    assert_eq!(source.to_string(), MESSAGE);
    assert_eq!(format!("{source:?}"), "EmptySessionName");
    assert_eq!(source.duplicate_session(), None);
    assert_eq!(source.session_not_found(), None);
    assert_eq!(source.invalid_target(), None);
    assert_eq!(source.server_message(), None);
    let source = error.source().unwrap();
    assert!(source.is::<ProtocolError>());
    assert!(!source.is::<SessionNameError>());
    assert!(source.source().is_none());
    assert_eq!(error.feature(), None);
    assert_eq!(error.hint(), Some(HINT));
    assert_eq!(
        error.to_string(),
        format!("rmux protocol error: {MESSAGE}\nhint: {HINT}")
    );
    let diagnostic = rmux_sdk::protocol_diagnostic(&error);
    assert_eq!(diagnostic.severity(), DiagnosticSeverity::Error);
    assert_eq!(diagnostic.feature(), None);
    assert_eq!(
        diagnostic.message(),
        format!("rmux protocol error: {MESSAGE}")
    );
    assert_eq!(diagnostic.hint(), Some(HINT));
}

#[test]
fn sdk_question_mark_preserves_each_constructor_diagnostic() {
    for validate in [sdk_new, sdk_parse, sdk_borrowed, sdk_owned] {
        assert_empty_diagnostic(validate("").unwrap_err());
        assert_eq!(validate("alpha").unwrap().as_str(), "alpha");
    }
}

#[test]
fn sdk_semantic_conversions_remain_available() {
    assert_empty_diagnostic(RmuxError::from(SessionNameError));
    assert_empty_diagnostic(SessionNameError.into());
    #[expect(
        clippy::unnecessary_fallible_conversions,
        reason = "Exercise the supported SDK TryInto contract explicitly."
    )]
    let tried: Result<RmuxError, std::convert::Infallible> = SessionNameError.try_into();
    assert_empty_diagnostic(tried.unwrap());
    assert_empty_diagnostic(SessionName::new("").map_err(RmuxError::from).unwrap_err());
}

#[test]
fn sdk_normalization_and_old_alias_do_not_reparse_canonical_values() {
    for validate in [sdk_new, sdk_parse, sdk_borrowed, sdk_owned] {
        let name = validate("a\\n:b.c").unwrap();
        assert_eq!(name.as_str(), "a\\\\n_b_c");
        let old_path: rmux_sdk::types::SessionName = name;
        let root: SessionName = old_path;
        let target = rmux_sdk::PaneRef::new(root.clone(), 2, 3);
        assert_eq!(target.session_name, root);
        assert_eq!(target.session_name.as_str(), "a\\\\n_b_c");
    }
    let _: rmux_sdk::types::SessionNameError = SessionNameError;
}
