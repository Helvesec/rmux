/// Server policy for application requests to change xterm key reporting.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub enum ExtendedKeyPolicy {
    /// Ignore application requests to enable extended reporting.
    Off,
    /// Let the application select or disable extended reporting.
    #[default]
    On,
    /// Restore mode one whenever the application disables reporting.
    Always,
}

impl ExtendedKeyPolicy {
    /// Resolves the server option, whose default is off.
    #[must_use]
    pub fn from_option(value: Option<&str>) -> Self {
        match value {
            Some("always") => Self::Always,
            Some("on") => Self::On,
            _ => Self::Off,
        }
    }
}
