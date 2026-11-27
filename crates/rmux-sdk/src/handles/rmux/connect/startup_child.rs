//! A Unix startup owns its child until the endpoint has been published.
use std::io;
use std::process::Child;

pub(super) struct PendingDaemon {
    child: Option<Child>,
}

impl PendingDaemon {
    pub(super) fn new(child: Child) -> Self {
        Self { child: Some(child) }
    }

    pub(super) fn detach(mut self) -> Option<Child> {
        self.child.take()
    }

    pub(super) fn stop(&mut self) -> io::Result<()> {
        if let Some(child) = self.child.as_mut() {
            if child.try_wait()?.is_none() {
                // This exact, still-owned child has not completed startup.
                // Collect its native end before returning the startup error.
                child.kill()?;
                child.wait()?;
            }
        }
        self.child.take();
        Ok(())
    }
}

impl Drop for PendingDaemon {
    fn drop(&mut self) {
        // Cancellation must retain the same ownership as an ordinary error.
        let _ = self.stop();
    }
}

#[cfg(test)]
mod tests {
    use super::PendingDaemon;
    use std::process::Command;

    #[test]
    fn published_daemon_is_released_without_termination() {
        let child = Command::new("/bin/sleep").arg("0.2").spawn().unwrap();
        let mut child = PendingDaemon::new(child).detach().unwrap();
        assert!(child.wait().unwrap().success());
    }

    #[test]
    fn already_ended_startup_child_is_reaped_before_error_returns() {
        let mut child = Command::new("/usr/bin/true").spawn().unwrap();
        child.wait().unwrap();
        let mut pending = PendingDaemon::new(child);
        pending.stop().unwrap();
        pending.stop().unwrap();
        assert!(pending.child.is_none());
    }
}
