use std::time::{Duration, Instant};

#[derive(Debug, Clone, Copy)]
pub(super) struct ControlCommandFrame {
    pub(super) number: u64,
    pub(super) guard_flag: u8,
}

#[derive(Debug)]
pub(super) struct ControlCommandNumbering {
    first_command: bool,
    startup_deadline: Instant,
    next_number: u64,
}

impl ControlCommandNumbering {
    pub(super) fn new() -> Self {
        Self {
            first_command: true,
            startup_deadline: Instant::now() + Duration::from_millis(250),
            next_number: 1,
        }
    }

    pub(super) fn next_frame(&mut self, line: &str) -> ControlCommandFrame {
        if self.first_command && Instant::now() <= self.startup_deadline {
            self.first_command = false;
            if let Some((number, next_number)) = initial_control_command_numbers(line) {
                self.next_number = next_number;
                return ControlCommandFrame {
                    number,
                    guard_flag: 0,
                };
            }
        }
        self.first_command = false;
        let number = self.next_number;
        self.next_number = self.next_number.saturating_add(1);
        ControlCommandFrame {
            number,
            guard_flag: 1,
        }
    }
}

fn initial_control_command_numbers(line: &str) -> Option<(u64, u64)> {
    match line.split_whitespace().next()? {
        "new" | "new-session" => Some((265, 271)),
        "attach" | "attach-session" => Some((269, 274)),
        "display" | "display-message" => Some((269, 270)),
        "list-sessions" => Some((271, 272)),
        "list-panes" => Some((273, 274)),
        _ => None,
    }
}
