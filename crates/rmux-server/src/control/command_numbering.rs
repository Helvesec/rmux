#[derive(Debug, Clone, Copy)]
pub(super) struct ControlCommandFrame {
    pub(super) number: u64,
    pub(super) guard_flag: u8,
}

#[derive(Debug)]
pub(super) struct ControlCommandNumbering {
    next_number: u64,
    initial_frames_remaining: usize,
}

impl ControlCommandNumbering {
    pub(super) fn after_initial_ack() -> Self {
        Self {
            next_number: 2,
            initial_frames_remaining: 0,
        }
    }

    pub(super) fn with_initial_commands(command_count: usize) -> Self {
        Self {
            next_number: 1,
            initial_frames_remaining: command_count,
        }
    }

    pub(super) fn next_frame(&mut self) -> ControlCommandFrame {
        let number = self.next_number;
        self.next_number = self.next_number.saturating_add(1);
        let guard_flag = if self.initial_frames_remaining == 0 {
            1
        } else {
            self.initial_frames_remaining = self.initial_frames_remaining.saturating_sub(1);
            0
        };
        ControlCommandFrame { number, guard_flag }
    }
}
