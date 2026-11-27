#[cfg(not(target_os = "linux"))]
use std::io;

/// Native startup-readiness transport, disabled by default.
///
/// This value owns only transport data. Its caller decides when readiness is
/// reached and how signaling errors are handled. Cloning does not signal.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct StartupReadyNotification {
    #[cfg(target_os = "linux")]
    pub(super) ready_fd: Option<std::os::fd::RawFd>,
    #[cfg(windows)]
    ready_event: Option<std::ffi::OsString>,
}

#[cfg(windows)]
impl StartupReadyNotification {
    /// Selects the name of a Win32 startup-readiness event.
    #[must_use]
    pub fn from_event_name(ready_event: std::ffi::OsString) -> Self {
        Self {
            ready_event: Some(ready_event),
        }
    }

    /// Signals the configured transport, or does nothing when disabled.
    pub fn signal(&self) -> io::Result<()> {
        match self.ready_event.as_deref() {
            Some(name) => super::signal_startup_ready_event(name),
            None => Ok(()),
        }
    }
}

#[cfg(not(any(target_os = "linux", windows)))]
impl StartupReadyNotification {
    /// Does nothing on platforms without a startup-readiness transport.
    pub fn signal(&self) -> io::Result<()> {
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn default_and_cloned_default_notifications_are_disabled() {
        let notification = StartupReadyNotification::default();
        notification.signal().unwrap();
        notification.clone().signal().unwrap();
    }
}
