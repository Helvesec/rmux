use super::LEFT_CTRL_PRESSED;

/// A Windows console keyboard event that can be injected into a ConPTY child.
#[derive(Clone, Copy, Debug, Eq, PartialEq)]
pub struct WindowsConsoleKeyEvent {
    pub(super) virtual_key_code: u16,
    pub(super) virtual_scan_code: u16,
    pub(super) unicode_char: u16,
    pub(super) control_key_state: u32,
    pub(super) repeat_count: u16,
}

impl WindowsConsoleKeyEvent {
    /// Creates a key event from the fields of a Windows `KEY_EVENT_RECORD`.
    #[must_use]
    pub const fn new(
        virtual_key_code: u16,
        virtual_scan_code: u16,
        unicode_char: u16,
        control_key_state: u32,
        repeat_count: u16,
    ) -> Self {
        Self {
            virtual_key_code,
            virtual_scan_code,
            unicode_char,
            control_key_state,
            repeat_count,
        }
    }

    /// Creates a Ctrl-C keyboard event.
    #[must_use]
    pub const fn ctrl_c() -> Self {
        Self::new(b'C' as u16, 0x2e, 0x03, LEFT_CTRL_PRESSED, 1)
    }

    /// Creates a Ctrl-D keyboard event.
    #[must_use]
    pub const fn ctrl_d() -> Self {
        Self::new(b'D' as u16, 0x20, 0x04, LEFT_CTRL_PRESSED, 1)
    }

    /// Creates a Ctrl-D event carrying the POSIX EOT byte.
    #[must_use]
    pub const fn ctrl_d_eot() -> Self {
        Self::new(b'D' as u16, 0, 0x04, LEFT_CTRL_PRESSED, 1)
    }

    /// Creates a Ctrl-Z keyboard event.
    #[must_use]
    pub const fn ctrl_z() -> Self {
        Self::new(b'Z' as u16, 0x2c, 0x1a, LEFT_CTRL_PRESSED, 1)
    }

    /// Creates a Ctrl-letter keyboard event for an ASCII letter.
    #[must_use]
    pub const fn ctrl_letter(letter: u8) -> Option<Self> {
        if letter >= b'A' && letter <= b'Z' {
            Some(Self::new(
                letter as u16,
                ctrl_letter_scan_code(letter),
                (letter - b'A' + 1) as u16,
                LEFT_CTRL_PRESSED,
                1,
            ))
        } else {
            None
        }
    }

    /// Returns the same key event with an adjusted repeat count.
    #[must_use]
    pub const fn with_repeat_count(self, repeat_count: u16) -> Self {
        Self {
            repeat_count,
            ..self
        }
    }

    /// Returns the Windows virtual-key code.
    #[must_use]
    pub const fn virtual_key_code(self) -> u16 {
        self.virtual_key_code
    }

    /// Returns the Windows virtual scan code.
    #[must_use]
    pub const fn virtual_scan_code(self) -> u16 {
        self.virtual_scan_code
    }

    /// Returns the UTF-16 character reported by the key event.
    #[must_use]
    pub const fn unicode_char(self) -> u16 {
        self.unicode_char
    }

    /// Returns the Windows control-key-state bitset.
    #[must_use]
    pub const fn control_key_state(self) -> u32 {
        self.control_key_state
    }

    /// Returns the Windows key repeat count.
    #[must_use]
    pub const fn repeat_count(self) -> u16 {
        self.repeat_count
    }
}

const fn ctrl_letter_scan_code(letter: u8) -> u16 {
    match letter {
        b'A' => 0x1e,
        b'B' => 0x30,
        b'C' => 0x2e,
        b'D' => 0x20,
        b'E' => 0x12,
        b'F' => 0x21,
        b'G' => 0x22,
        b'H' => 0x23,
        b'I' => 0x17,
        b'J' => 0x24,
        b'K' => 0x25,
        b'L' => 0x26,
        b'M' => 0x32,
        b'N' => 0x31,
        b'O' => 0x18,
        b'P' => 0x19,
        b'Q' => 0x10,
        b'R' => 0x13,
        b'S' => 0x1f,
        b'T' => 0x14,
        b'U' => 0x16,
        b'V' => 0x2f,
        b'W' => 0x11,
        b'X' => 0x2d,
        b'Y' => 0x15,
        b'Z' => 0x2c,
        _ => 0,
    }
}
