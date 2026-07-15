use std::io;
use std::process::{Child, Command};
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

#[cfg(unix)]
use rustix::process::{kill_process_group, Pid, Signal};
#[cfg(unix)]
use std::os::unix::process::CommandExt as _;

const PIPE_CHILD_POLL_INTERVAL: Duration = Duration::from_millis(250);
#[cfg(test)]
static ACTIVE_PIPE_CHILDREN: AtomicUsize = AtomicUsize::new(0);

#[cfg(test)]
pub(crate) fn active_pipe_child_count_for_test() -> usize {
    ACTIVE_PIPE_CHILDREN.load(Ordering::SeqCst)
}

#[cfg(test)]
pub(super) fn mark_pipe_child_started_for_test() {
    ACTIVE_PIPE_CHILDREN.fetch_add(1, Ordering::SeqCst);
}

#[cfg(not(test))]
pub(super) fn mark_pipe_child_started_for_test() {}

struct ActivePipeChildGuard;

impl Drop for ActivePipeChildGuard {
    fn drop(&mut self) {
        #[cfg(test)]
        ACTIVE_PIPE_CHILDREN.fetch_sub(1, Ordering::SeqCst);
    }
}

pub(super) fn wait_for_pipe_child(
    mut child: Child,
    stop_flag: Arc<AtomicBool>,
    process_group: Arc<PipeChildProcessGroup>,
) -> Child {
    let _active_child = ActivePipeChildGuard;
    loop {
        if stop_flag.load(Ordering::Relaxed) {
            process_group.terminate();
            let _ = child.kill();
            let _ = child.wait();
            return child;
        }
        match process_group.child_exited(&mut child) {
            Ok(true) | Err(_) => return child,
            Ok(false) => thread::sleep(PIPE_CHILD_POLL_INTERVAL),
        }
    }
}

pub(super) struct PipeChildProcessGroup {
    target: ProcessGroupTarget,
    armed: AtomicBool,
    #[cfg(test)]
    termination_count: AtomicUsize,
}

impl PipeChildProcessGroup {
    pub(super) fn from_child(child: &Child) -> io::Result<Self> {
        Ok(Self {
            target: ProcessGroupTarget::from_child(child)?,
            armed: AtomicBool::new(true),
            #[cfg(test)]
            termination_count: AtomicUsize::new(0),
        })
    }

    pub(super) fn child_exited(&self, child: &mut Child) -> io::Result<bool> {
        self.target.child_exited(child)
    }

    pub(super) fn terminate(&self) {
        if !self.armed.swap(false, Ordering::SeqCst) {
            return;
        }
        #[cfg(test)]
        self.termination_count.fetch_add(1, Ordering::SeqCst);
        self.target.terminate();
    }

    #[cfg(test)]
    pub(super) fn is_armed_for_test(&self) -> bool {
        self.armed.load(Ordering::SeqCst)
    }

    #[cfg(test)]
    pub(super) fn termination_count_for_test(&self) -> usize {
        self.termination_count.load(Ordering::SeqCst)
    }
}

#[cfg(unix)]
struct ProcessGroupTarget(Pid);

#[cfg(unix)]
impl ProcessGroupTarget {
    fn from_child(child: &Child) -> io::Result<Self> {
        let raw_pid = i32::try_from(child.id()).map_err(|_| {
            io::Error::new(io::ErrorKind::InvalidData, "pipe-pane child pid is invalid")
        })?;
        let pid = Pid::from_raw(raw_pid).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "pipe-pane child pid is zero")
        })?;
        Ok(Self(pid))
    }

    fn child_exited(&self, child: &mut Child) -> io::Result<bool> {
        child_exited_without_reaping(self.0, child)
    }

    fn terminate(&self) {
        let _ = kill_process_group(self.0, Signal::KILL);
    }
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
fn child_exited_without_reaping(pid: Pid, _child: &mut Child) -> io::Result<bool> {
    use rustix::process::{waitid, WaitId, WaitIdOptions};

    Ok(waitid(
        WaitId::Pid(pid),
        WaitIdOptions::EXITED | WaitIdOptions::NOHANG | WaitIdOptions::NOWAIT,
    )
    .map(|status| status.is_some())?)
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
fn child_exited_without_reaping(_pid: Pid, child: &mut Child) -> io::Result<bool> {
    child.try_wait().map(|status| status.is_some())
}

#[cfg(windows)]
struct ProcessGroupTarget(rmux_os::process::ProcessJob);

#[cfg(windows)]
impl ProcessGroupTarget {
    fn from_child(child: &Child) -> io::Result<Self> {
        rmux_os::process::ProcessJob::for_child(child).map(Self)
    }

    fn child_exited(&self, child: &mut Child) -> io::Result<bool> {
        child.try_wait().map(|status| status.is_some())
    }

    fn terminate(&self) {
        let _ = self.0.terminate(1);
    }
}

#[cfg(not(any(unix, windows)))]
struct ProcessGroupTarget;

#[cfg(not(any(unix, windows)))]
impl ProcessGroupTarget {
    fn from_child(_: &Child) -> io::Result<Self> {
        Ok(Self)
    }

    fn child_exited(&self, child: &mut Child) -> io::Result<bool> {
        child.try_wait().map(|status| status.is_some())
    }

    fn terminate(&self) {}
}

#[cfg(unix)]
pub(super) fn configure_child_process(command: &mut Command) {
    command.process_group(0);
}

#[cfg(not(unix))]
pub(super) fn configure_child_process(_: &mut Command) {}
