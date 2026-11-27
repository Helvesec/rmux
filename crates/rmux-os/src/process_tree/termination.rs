use std::io;
use std::time::{Duration, Instant};

use super::ProcessTreeController;

impl ProcessTreeController {
    /// Force-terminates the isolated tree and waits up to `timeout` for its
    /// processes to stop running.
    ///
    /// Returns `true` when the platform observes no live tree member.
    /// Unix targets without process-group enumeration still receive the
    /// termination signal but return `false` because quiescence is unknowable.
    pub fn terminate_and_wait(&self, timeout: Duration) -> io::Result<bool> {
        #[cfg(all(
            unix,
            not(any(
                target_os = "cygwin",
                target_os = "horizon",
                target_os = "openbsd",
                target_os = "redox",
                target_os = "wasi"
            ))
        ))]
        {
            self.process_group.terminate_and_wait(timeout)
        }

        #[cfg(all(
            unix,
            any(
                target_os = "cygwin",
                target_os = "horizon",
                target_os = "openbsd",
                target_os = "redox",
                target_os = "wasi"
            )
        ))]
        {
            let _ = timeout;
            self.terminate()?;
            Ok(false)
        }

        #[cfg(windows)]
        {
            super::windows::terminate_job_and_wait(&self.job, timeout)
        }

        #[cfg(not(any(unix, windows)))]
        {
            let _ = timeout;
            self.terminate()?;
            Ok(true)
        }
    }

    /// Force-terminates the tree during a shutdown that shares `deadline`
    /// across several trees, and reports whether shutdown may treat it as
    /// settled.
    ///
    /// Linux observes descendant quiescence until `deadline` through
    /// [`Self::terminate_and_wait`]; an observation error reports `false`.
    /// Other platforms deliver the force-termination request without
    /// observing descendants and report `true`.
    pub fn terminate_for_shutdown(&self, deadline: Instant) -> bool {
        #[cfg(target_os = "linux")]
        {
            self.terminate_and_wait(deadline.saturating_duration_since(Instant::now()))
                .unwrap_or(false)
        }

        #[cfg(not(target_os = "linux"))]
        {
            let _ = deadline;
            let _ = self.terminate();
            true
        }
    }
}
