use std::os::fd::BorrowedFd;

use rmux_os::process;

use super::RuntimeFormatContext;

impl RuntimeFormatContext<'_> {
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

    pub(super) fn pane_current_path(&self) -> Option<String> {
        let pid = self.pane_foreground_pid()?;
        process::current_path(pid).or_else(|| self.pane_screen_path())
    }

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

fn process_foreground_pid(fd: BorrowedFd<'_>) -> Option<u32> {
    process::unix::foreground_pid(fd)
}
