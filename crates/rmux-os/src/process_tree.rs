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

#[cfg(unix)]
use rustix::process::Pid;
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
use rustix::process::{waitid, WaitId, WaitIdOptions};
#[cfg(windows)]
use windows_sys::Win32::System::Threading::{CREATE_NO_WINDOW, CREATE_SUSPENDED};

#[cfg(windows)]
use crate::process::ProcessJob;

#[cfg(unix)]
mod terminal;
#[cfg(unix)]
pub use terminal::ForegroundTerminalGuard;
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

    /// Spawns `command` with descendant isolation and the requested Windows
    /// console-window behavior.
    pub fn spawn_with_console_window(
        command: &mut Command,
        console_window: ConsoleWindowBehavior,
    ) -> io::Result<Self> {
        #[cfg(not(windows))]
        let _ = console_window;

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
            let job = match ProcessJob::for_child(&child) {
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

    /// Reports whether the direct child has exited without making its Unix
    /// process-group identifier reusable before a possible tree signal.
    pub fn has_exited(&mut self) -> io::Result<bool> {
        if !self.armed {
            return Ok(true);
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
            let pid = Pid::from_raw(self.process_group)
                .ok_or_else(|| io::Error::other("child process id is zero"))?;
            Ok(waitid(
                WaitId::Pid(pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            )?
            .is_some_and(|status| status.exited() || status.killed() || status.dumped()))
        }

        #[cfg(any(
            not(unix),
            all(
                unix,
                any(
                    target_os = "cygwin",
                    target_os = "horizon",
                    target_os = "openbsd",
                    target_os = "redox",
                    target_os = "wasi"
                )
            )
        ))]
        {
            self.child.try_wait().map(|status| status.is_some())
        }
    }

    /// Reports whether the direct Unix child is stopped without consuming the
    /// status that remains owned by [`Self::wait`].
    #[cfg(unix)]
    pub fn has_stopped(&mut self) -> io::Result<bool> {
        if !self.armed {
            return Ok(false);
        }

        #[cfg(not(any(
            target_os = "cygwin",
            target_os = "horizon",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "wasi"
        )))]
        {
            let pid = Pid::from_raw(self.process_group)
                .ok_or_else(|| io::Error::other("child process id is zero"))?;
            Ok(waitid(
                WaitId::Pid(pid),
                WaitIdOptions::STOPPED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
            )?
            .is_some_and(|status| status.stopped()))
        }

        #[cfg(any(
            target_os = "cygwin",
            target_os = "horizon",
            target_os = "openbsd",
            target_os = "redox",
            target_os = "wasi"
        ))]
        {
            Ok(false)
        }
    }

    /// Forwards a raw Unix signal to the complete child process group.
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
    pub fn forward_signal(&mut self, signal: i32) -> io::Result<()> {
        if !self.armed {
            return Ok(());
        }
        let result = unsafe {
            // SAFETY: `process_group` is the positive PID of the child group
            // leader created by this value. A negative PID addresses exactly
            // that process group, and the caller supplies an OS signal value.
            libc::kill(-self.process_group, signal)
        };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH)
            || (error.raw_os_error() == Some(libc::EPERM) && self.has_exited()?)
        {
            Ok(())
        } else {
            Err(error)
        }
    }

    /// Forwards a raw Unix signal to the direct child on targets that cannot
    /// inspect an exited group leader without reaping it.
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
    pub fn forward_signal(&mut self, signal: i32) -> io::Result<()> {
        if !self.armed {
            return Ok(());
        }
        let result = unsafe {
            // SAFETY: `process_group` is also the direct child PID. Limiting
            // the signal to that PID avoids a recycled-group race on targets
            // without waitid(WNOWAIT).
            libc::kill(self.process_group, signal)
        };
        if result == 0 {
            return Ok(());
        }
        let error = io::Error::last_os_error();
        if error.raw_os_error() == Some(libc::ESRCH) {
            Ok(())
        } else {
            Err(error)
        }
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
            let pid = Pid::from_raw(self.process_group)
                .ok_or_else(|| io::Error::other("child process id is zero"))?;
            let _ = waitid(
                WaitId::Pid(pid),
                WaitIdOptions::EXITED | WaitIdOptions::NOWAIT,
            )?;
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
