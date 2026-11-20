//! Child-process trees with platform-native lifetime isolation.

use std::io;
use std::process::{Child, Command, ExitStatus};

#[cfg(unix)]
use std::os::unix::process::CommandExt as _;
#[cfg(windows)]
use std::os::windows::process::CommandExt as _;
#[cfg(any(
    windows,
    all(
        unix,
        not(any(
            target_os = "cygwin",
            target_os = "horizon",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "wasi"
        ))
    )
))]
use std::sync::Arc;

#[cfg(windows)]
use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};

#[cfg(windows)]
use crate::process::ProcessJob;

#[cfg(unix)]
mod terminal;
#[cfg(unix)]
pub use terminal::ForegroundTerminalGuard;
mod group_signals;
mod termination;
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
mod unix_controller;
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
use unix_controller::UnixProcessGroup;
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
mod unix_group_liveness;
#[cfg(windows)]
mod windows;
#[cfg(windows)]
use windows::resume_suspended_process;

/// A child whose descendants are isolated with a process group on Unix targets
/// supporting `waitid(WNOWAIT)`, or with a Job Object on Windows. Other Unix
/// targets retain direct-child cleanup without risking a recycled group ID.
///
/// Dropping a live value terminates and reaps the tree. A successful [`Self::wait`]
/// disarms that cleanup so descendants intentionally left in the background
/// retain the same normal-completion behavior as `std::process::Child`.
pub struct ProcessTreeChild {
    child: Child,
    armed: bool,
    #[cfg(unix)]
    process_group: i32,
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
    process_group_control: Arc<UnixProcessGroup>,
    #[cfg(windows)]
    job: Arc<ProcessJob>,
}

/// Controls whether a spawned process tree may create a console window.
///
/// This setting only changes process creation on Windows. Other platforms
/// ignore it because they do not have the corresponding console-window flag.
#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
pub enum ConsoleWindowBehavior {
    /// Preserve the normal console-window behavior of the child command.
    #[default]
    Inherit,
    /// Suppress creation of a console window for a background helper.
    Suppress,
}

#[derive(Clone, Copy)]
enum JobBreakawayBehavior {
    Disallow,
    AllowExplicit,
}

/// A clonable termination handle for a spawned process tree.
#[derive(Clone)]
pub struct ProcessTreeController {
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
    process_group: Arc<UnixProcessGroup>,
    #[cfg(windows)]
    job: Arc<ProcessJob>,
}

impl ProcessTreeController {
    /// Force-terminates every process still in the isolated tree.
    pub fn terminate(&self) -> io::Result<()> {
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
            self.process_group.terminate()
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
            Ok(())
        }

        #[cfg(windows)]
        {
            self.job.terminate(1)
        }

        #[cfg(not(any(unix, windows)))]
        {
            Ok(())
        }
    }
}

impl ProcessTreeChild {
    /// Spawns `command` with descendant isolation established before user code
    /// can create another process.
    pub fn spawn(command: &mut Command) -> io::Result<Self> {
        Self::spawn_with_console_window(command, ConsoleWindowBehavior::Inherit)
    }

    /// Spawns a process tree while allowing descendants that explicitly ask
    /// Windows for job breakaway to outlive the tree.
    ///
    /// On non-Windows targets this is identical to [`Self::spawn`]. Ordinary
    /// descendants remain isolated and are still terminated with the tree.
    pub fn spawn_allowing_explicit_job_breakaway(command: &mut Command) -> io::Result<Self> {
        Self::spawn_with_options(
            command,
            ConsoleWindowBehavior::Inherit,
            JobBreakawayBehavior::AllowExplicit,
        )
    }

    /// Spawns `command` with descendant isolation and the requested Windows
    /// console-window behavior.
    pub fn spawn_with_console_window(
        command: &mut Command,
        console_window: ConsoleWindowBehavior,
    ) -> io::Result<Self> {
        Self::spawn_with_options(command, console_window, JobBreakawayBehavior::Disallow)
    }

    fn spawn_with_options(
        command: &mut Command,
        console_window: ConsoleWindowBehavior,
        job_breakaway: JobBreakawayBehavior,
    ) -> io::Result<Self> {
        #[cfg(not(windows))]
        let _ = (console_window, job_breakaway);

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
            command.process_group(0);
            let child = command.spawn()?;
            let process_group = i32::try_from(child.id())
                .map_err(|_| io::Error::other("child process id does not fit in i32"))?;
            Ok(Self {
                child,
                armed: true,
                process_group,
                process_group_control: Arc::new(UnixProcessGroup::new(process_group)),
            })
        }

        #[cfg(windows)]
        {
            command.creation_flags(windows_creation_flags(console_window));
            let mut child = command.spawn()?;
            let job = match job_breakaway {
                JobBreakawayBehavior::Disallow => ProcessJob::for_child(&child),
                JobBreakawayBehavior::AllowExplicit => {
                    ProcessJob::for_child_allowing_breakaway(&child)
                }
            };
            let job = match job {
                Ok(job) => Arc::new(job),
                Err(error) => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return Err(error);
                }
            };
            if let Err(error) = resume_suspended_process(child.id()) {
                let _ = job.terminate(1);
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
            Ok(Self {
                child,
                armed: true,
                job,
            })
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
            let child = command.spawn()?;
            let process_group = i32::try_from(child.id())
                .map_err(|_| io::Error::other("child process id does not fit in i32"))?;
            Ok(Self {
                child,
                armed: true,
                process_group,
            })
        }

        #[cfg(not(any(unix, windows)))]
        {
            Ok(Self {
                child: command.spawn()?,
                armed: true,
            })
        }
    }

    /// Returns a clonable handle that can terminate the isolated process tree
    /// while the direct child is owned by another task.
    #[must_use]
    pub fn controller(&self) -> ProcessTreeController {
        ProcessTreeController {
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
            process_group: Arc::clone(&self.process_group_control),
            #[cfg(windows)]
            job: Arc::clone(&self.job),
        }
    }

    /// Returns mutable access to the direct child for transferring configured
    /// standard-I/O handles to an owning runtime task.
    pub fn child_mut(&mut self) -> &mut Child {
        &mut self.child
    }

    /// Force-terminates the direct child and every descendant in its isolated
    /// process tree.
    pub fn terminate(&mut self) -> io::Result<()> {
        if !self.armed {
            return Ok(());
        }

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
            self.forward_signal(libc::SIGKILL)
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
            self.child.kill()
        }

        #[cfg(windows)]
        {
            self.job.terminate(1)
        }

        #[cfg(not(any(unix, windows)))]
        {
            self.child.kill()
        }
    }

    /// Waits for the direct child and disarms tree cleanup after normal
    /// completion.
    pub fn wait(&mut self) -> io::Result<ExitStatus> {
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
        let status = if let Some(status) = self.process_group_control.try_reap(&mut self.child)? {
            status
        } else {
            self.process_group_control.wait_for_leader_exit()?;
            self.process_group_control.reap_exited(&mut self.child)?
        };

        #[cfg(not(all(
            unix,
            not(any(
                target_os = "cygwin",
                target_os = "horizon",
                target_os = "openbsd",
                target_os = "redox",
                target_os = "wasi"
            ))
        )))]
        let status = self.child.wait()?;
        #[cfg(windows)]
        self.job.disarm_kill_on_close()?;
        self.armed = false;
        Ok(status)
    }
}

#[cfg(windows)]
const fn windows_creation_flags(console_window: ConsoleWindowBehavior) -> u32 {
    match console_window {
        ConsoleWindowBehavior::Inherit => CREATE_SUSPENDED,
        ConsoleWindowBehavior::Suppress => CREATE_SUSPENDED | CREATE_NO_WINDOW,
    }
}

#[cfg(all(test, windows))]
mod tests {
    use super::*;

    #[test]
    fn process_tree_creation_flags_preserve_console_window_policy() {
        let inherited = windows_creation_flags(ConsoleWindowBehavior::Inherit);
        assert_eq!(inherited & CREATE_SUSPENDED, CREATE_SUSPENDED);
        assert_eq!(inherited & CREATE_NO_WINDOW, 0);

        let suppressed = windows_creation_flags(ConsoleWindowBehavior::Suppress);
        assert_eq!(suppressed & CREATE_SUSPENDED, CREATE_SUSPENDED);
        assert_eq!(suppressed & CREATE_NO_WINDOW, CREATE_NO_WINDOW);
    }
}

impl Drop for ProcessTreeChild {
    fn drop(&mut self) {
        if !self.armed {
            return;
        }
        let _ = self.terminate();
        let _ = self.child.kill();

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
        if self.wait().is_err() {
            self.process_group_control.disarm();
        }

        #[cfg(not(all(
            unix,
            not(any(
                target_os = "cygwin",
                target_os = "horizon",
                target_os = "openbsd",
                target_os = "redox",
                target_os = "wasi"
            ))
        )))]
        let _ = self.child.wait();
        self.armed = false;
    }
}
