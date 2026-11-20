#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(target_os = "macos")]
mod macos_grant;
#[cfg(unix)]
mod unix_exit;
#[cfg(unix)]
mod unix_fds;
#[cfg(unix)]
mod unix_io;
#[cfg(windows)]
mod windows;

#[cfg(target_os = "linux")]
pub(crate) use linux::*;
#[cfg(target_os = "macos")]
pub(crate) use macos::*;
#[cfg(unix)]
pub(crate) use unix_exit::child_has_exited;
#[cfg(unix)]
pub(crate) use unix_fds::mark_child_fds_close_on_exec;
#[cfg(unix)]
pub(crate) use unix_io::PtyInputQueue;
#[cfg(windows)]
pub(crate) use windows::*;
