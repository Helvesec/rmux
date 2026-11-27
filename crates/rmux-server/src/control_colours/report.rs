//! Bounded tmux 3.7b `cmd_refresh_report` / `tty_keys_colours` grammar.
mod x11;

/// The same colour grammar for application and outer-terminal facts.
pub(crate) fn parse_rgb(value: &str) -> Option<[u8; 3]> {
    x11::parse(value)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ColourReport {
    pub(crate) pane: u32,
    pub(super) slot: usize,
    pub(super) rgb: [u8; 3],
}

impl ColourReport {
    pub(crate) const fn rgb(self) -> [u8; 3] {
        self.rgb
    }

    pub(crate) fn parse(value: &str) -> Option<Self> {
        // C-string termination also bounds the framing scan: a NUL before
        // BEL/ST cannot turn an incomplete report into a complete one.
        let value = value.split('\0').next()?;
        let (pane, report) = value.strip_prefix('%')?.split_once(':')?;
        let mut pane = pane.as_bytes();
        // Like %u, consume a decimal prefix. Checked arithmetic deliberately
        // rejects overflow instead of letting an external ID alias another pane.
        let pane = x11::unsigned(&mut pane, 10, usize::MAX)?;
        let report = report.as_bytes().strip_prefix(b"\x1b]1")?;
        let slot = match report.first()? {
            b'0' => 0,
            b'1' => 1,
            _ => return None,
        };
        let body = report.get(1..)?.strip_prefix(b";")?;
        // tty_keys_colours has char tmp[128], including the terminating NUL.
        // Its ST scan counts the ESC as a byte of colour text.
        let mut end = None;
        for index in 0..body.len().min(127) {
            if body[index] == 7 {
                end = Some(index);
                break;
            }
            if index > 0 && body[index - 1..=index] == *b"\x1b\\" {
                end = Some(index - 1);
                break;
            }
        }
        let body = std::str::from_utf8(&body[..end?]).ok()?;
        Some(Self {
            pane,
            slot,
            rgb: x11::parse(body)?,
        })
    }
}
