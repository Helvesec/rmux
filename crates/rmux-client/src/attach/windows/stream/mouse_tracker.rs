#[derive(Debug, Default)]
pub(super) struct WindowsConsoleMouseTracker {
    enabled: bool,
    normal_tracking: bool,
    button_tracking: bool,
    any_tracking: bool,
    tail: Vec<u8>,
}

impl WindowsConsoleMouseTracker {
    pub(super) fn observe(&mut self, bytes: &[u8]) -> Option<bool> {
        const TAIL_LEN: usize = 7;

        if bytes.is_empty() {
            return None;
        }

        let mut combined = Vec::with_capacity(self.tail.len() + bytes.len());
        combined.extend_from_slice(&self.tail);
        combined.extend_from_slice(bytes);

        let mut observed = None;
        for index in 0..combined.len() {
            let tail = &combined[index..];
            if tail.starts_with(b"\x1b[?1000h") {
                self.normal_tracking = true;
                observed = Some(self.mouse_input_enabled());
            } else if tail.starts_with(b"\x1b[?1000l") {
                self.normal_tracking = false;
                observed = Some(self.mouse_input_enabled());
            } else if tail.starts_with(b"\x1b[?1002h") {
                self.button_tracking = true;
                observed = Some(self.mouse_input_enabled());
            } else if tail.starts_with(b"\x1b[?1002l") {
                self.button_tracking = false;
                observed = Some(self.mouse_input_enabled());
            } else if tail.starts_with(b"\x1b[?1003h") {
                self.any_tracking = true;
                observed = Some(self.mouse_input_enabled());
            } else if tail.starts_with(b"\x1b[?1003l") {
                self.any_tracking = false;
                observed = Some(self.mouse_input_enabled());
            } else if tail.starts_with(b"\x1b[?1006h") {
                // SGR mouse encoding is independent from DECSET 1000/1002/1003 tracking.
            } else if tail.starts_with(b"\x1b[?1006l") {
                // Disabling SGR encoding must not disable Windows console mouse input.
            }
        }

        self.tail.clear();
        self.tail
            .extend_from_slice(&combined[combined.len().saturating_sub(TAIL_LEN)..]);

        let enabled = observed?;
        if self.enabled == enabled {
            return None;
        }
        self.enabled = enabled;
        Some(enabled)
    }

    const fn mouse_input_enabled(&self) -> bool {
        self.normal_tracking || self.button_tracking || self.any_tracking
    }
}
