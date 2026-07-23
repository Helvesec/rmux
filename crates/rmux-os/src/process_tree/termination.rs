use std::io;
use std::time::Duration;

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
}
