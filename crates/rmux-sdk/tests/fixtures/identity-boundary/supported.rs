//! Compiled and run with only --extern rmux_sdk from the locked workspace build.
use rmux_sdk::{PaneId, SessionId, SessionName, SessionNameError, WindowId};
use std::{error::Error, str::FromStr};

fn validate(raw: &str) -> Result<SessionName, SessionNameError> {
    SessionName::new(raw)
}

fn sdk_question_mark(raw: &str) -> rmux_sdk::Result<SessionName> {
    Ok(SessionName::new(raw)?)
}

fn main() {
    fn bounds<T: Error + Copy + Eq + Send + Sync + 'static>() {}
    bounds::<SessionNameError>();
    let _: <SessionName as FromStr>::Err = SessionNameError;
    let _: <SessionName as TryFrom<&str>>::Error = SessionNameError;
    let _: <SessionName as TryFrom<String>>::Error = SessionNameError;
    let _: rmux_sdk::types::SessionNameError = SessionNameError;
    assert_eq!(validate("").unwrap_err(), SessionNameError);
    assert!(SessionNameError.source().is_none());
    assert_eq!(
        SessionNameError.to_string(),
        "invalid session name: session names must be non-empty"
    );
    let name = validate("a\\n:b.c").unwrap();
    assert_eq!(name.as_str(), "a\\\\n_b_c");
    assert_eq!("a\\n:b.c".parse::<SessionName>().unwrap(), name);
    assert_eq!(SessionName::try_from("a\\n:b.c").unwrap(), name);
    assert_eq!(
        SessionName::try_from(String::from("a\\n:b.c")).unwrap(),
        name
    );
    let target = rmux_sdk::PaneRef::new(name.clone(), 2, 3);
    assert_eq!(target.session_name, name);
    assert_eq!(sdk_question_mark("alpha").unwrap().as_str(), "alpha");
    let error = sdk_question_mark("").unwrap_err();
    assert!(error.source().unwrap().is::<rmux_sdk::ProtocolError>());
    assert!(error.source().unwrap().source().is_none());
    assert_eq!(
        rmux_sdk::protocol_diagnostic(&error).message(),
        "rmux protocol error: invalid session name: session names must be non-empty"
    );
    const SESSION: SessionId = SessionId::new(0);
    const WINDOW: WindowId = WindowId::new(u32::MAX);
    const PANE: PaneId = PaneId::new(u32::MAX);
    assert_eq!(
        (u32::from(SESSION), u32::from(WINDOW), u32::from(PANE)),
        (0, u32::MAX, u32::MAX)
    );
}
