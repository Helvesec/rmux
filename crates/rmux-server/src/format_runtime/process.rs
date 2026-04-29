#[cfg(unix)]
use std::os::fd::BorrowedFd;

use rmux_os::process;

use super::RuntimeFormatContext;

impl RuntimeFormatContext<'_> {
    #[cfg(unix)]
    pub(super) fn pane_foreground_pid(&self) -> Option<u32> {
        let session_name = self.session_name()?;
        let window_index = self.window_index?;
        let pane = self.pane?;
        let state = self.state?;
        state
            .pane_master_fd(session_name, window_index, pane.index())
            .ok()
            .and_then(process_foreground_pid)
            .or_else(|| {
                state
                    .pane_pid_in_window(session_name, window_index, pane.index())
                    .ok()
            })
    }

    #[cfg(windows)]
    pub(super) fn pane_current_path(&self) -> Option<String> {
        let process_cwd = || {
            let state = self.state?;
            let session_name = self.session_name()?;
            let window_index = self.window_index?;
            let pane = self.pane?;
            state
                .pane_pid_in_window(session_name, window_index, pane.index())
                .ok()
                .and_then(process::current_path)
        };
        let profile_cwd = || {
            let state = self.state?;
            let session_name = self.session_name()?;
            let window_index = self.window_index?;
            let pane = self.pane?;
            state
                .pane_profile_in_window(session_name, window_index, pane.index())
                .ok()
                .map(|profile| profile.cwd().to_string_lossy().into_owned())
        };
        self.pane_screen_path()
            .or_else(process_cwd)
            .or_else(profile_cwd)
            .or_else(|| self.environment_value_by_name("PWD"))
            .or_else(|| self.environment_value_by_name("USERPROFILE"))
    }

    #[cfg(unix)]
    pub(super) fn pane_current_path(&self) -> Option<String> {
        let pid = self.pane_foreground_pid()?;
        process::current_path(pid).or_else(|| self.pane_screen_path())
    }

    #[cfg(windows)]
    pub(super) fn pane_current_command(&self) -> Option<String> {
        let state = self.state?;
        let session_name = self.session_name()?;
        let window_index = self.window_index?;
        let pane = self.pane?;
        state
            .pane_runtime_window_name_in_window(session_name, window_index, pane.index())
            .ok()
            .flatten()
            .or_else(|| {
                state
                    .pane_pid_in_window(session_name, window_index, pane.index())
                    .ok()
                    .and_then(process::command_name)
            })
            .or_else(|| {
                state
                    .pane_profile_in_window(session_name, window_index, pane.index())
                    .ok()
                    .and_then(|profile| {
                        profile
                            .shell()
                            .file_name()
                            .and_then(|name| name.to_str())
                            .map(str::to_owned)
                    })
            })
    }

    #[cfg(unix)]
    pub(super) fn pane_current_command(&self) -> Option<String> {
        let state = self.state?;
        let pid = self.pane_foreground_pid()?;
        process::command_name(pid).or_else(|| {
            let session_name = self.session_name()?;
            let window_index = self.window_index?;
            let pane = self.pane?;
            state
                .pane_profile_in_window(session_name, window_index, pane.index())
                .ok()
                .and_then(|profile| {
                    profile
                        .shell()
                        .file_name()
                        .and_then(|name| name.to_str())
                        .map(str::to_owned)
                })
        })
    }
}

#[cfg(unix)]
fn process_foreground_pid(fd: BorrowedFd<'_>) -> Option<u32> {
    process::unix::foreground_pid(fd)
}
