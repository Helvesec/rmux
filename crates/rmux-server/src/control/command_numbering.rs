#[derive(Debug, Clone, Copy)]
pub(super) struct ControlCommandFrame {
    pub(super) number: u64,
    pub(super) guard_flag: u8,
    pub(super) origin: ControlCommandOrigin,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ControlCommandOrigin {
    Initial { completes_batch: bool },
    Stdin,
}

impl ControlCommandOrigin {
    pub(super) const fn is_initial(self) -> bool {
        matches!(self, Self::Initial { .. })
    }
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
        let origin = match self.initial_frames_remaining {
            0 => ControlCommandOrigin::Stdin,
            1 => {
                self.initial_frames_remaining = 0;
                ControlCommandOrigin::Initial {
                    completes_batch: true,
                }
            }
            _ => {
                self.initial_frames_remaining -= 1;
                ControlCommandOrigin::Initial {
                    completes_batch: false,
                }
            }
        };
        let guard_flag = match origin {
            ControlCommandOrigin::Initial { .. } => 0,
            ControlCommandOrigin::Stdin => 1,
        };
        ControlCommandFrame {
            number,
            guard_flag,
            origin,
        }
    }

    /// Ends the initial batch before its last command and returns how many
    /// initial commands will now never start.
    pub(super) fn cancel_remaining_initial_commands(&mut self) -> usize {
        std::mem::take(&mut self.initial_frames_remaining)
    }

    pub(super) fn next_synchronous_child_number(&mut self) -> u64 {
        let number = self.next_number;
        self.next_number = self.next_number.saturating_add(1);
        number
    }
}

#[cfg(test)]
mod tests {
    use super::{ControlCommandNumbering, ControlCommandOrigin};

    #[test]
    fn initial_batch_boundary_is_explicit_and_stdin_frames_follow_it() {
        let mut numbering = ControlCommandNumbering::with_initial_commands(2);

        let first = numbering.next_frame();
        assert_eq!(first.number, 1);
        assert_eq!(first.guard_flag, 0);
        assert_eq!(
            first.origin,
            ControlCommandOrigin::Initial {
                completes_batch: false
            }
        );

        let last = numbering.next_frame();
        assert_eq!(last.number, 2);
        assert_eq!(last.guard_flag, 0);
        assert_eq!(
            last.origin,
            ControlCommandOrigin::Initial {
                completes_batch: true
            }
        );

        let stdin = numbering.next_frame();
        assert_eq!(stdin.number, 3);
        assert_eq!(stdin.guard_flag, 1);
        assert_eq!(stdin.origin, ControlCommandOrigin::Stdin);
    }

    #[test]
    fn cancelled_initial_batch_numbers_the_next_frame_as_stdin() {
        let mut numbering = ControlCommandNumbering::with_initial_commands(3);

        let first = numbering.next_frame();
        assert!(first.origin.is_initial());
        assert_eq!(numbering.cancel_remaining_initial_commands(), 2);
        assert_eq!(numbering.cancel_remaining_initial_commands(), 0);

        let stdin = numbering.next_frame();
        assert_eq!(stdin.number, 2);
        assert_eq!(stdin.guard_flag, 1);
        assert_eq!(stdin.origin, ControlCommandOrigin::Stdin);
    }

    #[test]
    fn synthetic_initial_ack_places_the_first_real_command_on_stdin() {
        let mut numbering = ControlCommandNumbering::after_initial_ack();

        let stdin = numbering.next_frame();
        assert_eq!(stdin.number, 2);
        assert_eq!(stdin.guard_flag, 1);
        assert_eq!(stdin.origin, ControlCommandOrigin::Stdin);
    }
}
