//! Frozen compiler witnesses for public dependencies, including feature adapters.

pub fn lease_state(
    session: &rmux_sdk::OwnedSession,
) -> Option<tokio::sync::watch::Receiver<rmux_sdk::LeaseState>> {
    session.lease_state_receiver()
}

pub fn identity_serde() {
    fn serde_bounds<T: serde::Serialize + for<'de> serde::Deserialize<'de>>() {}
    serde_bounds::<rmux_sdk::SessionName>();
    serde_bounds::<rmux_sdk::PaneId>();
}

#[cfg(feature = "regex")]
pub fn regex_query(value: regex::Regex) -> rmux_sdk::LocatorText {
    value.into()
}

#[cfg(feature = "crossterm")]
pub fn keyboard(
    value: crossterm::event::KeyEvent,
) -> Result<rmux_sdk::KeyEvent, rmux_sdk::KeyConversionError> {
    let sdk = rmux_sdk::KeyEvent::try_from(value)?;
    let _: crossterm::event::KeyEvent = sdk.into();
    Ok(sdk)
}
