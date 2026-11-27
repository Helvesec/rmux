use rmux_proto::{RmuxError, TerminalSize, WindowId};

use crate::WINDOW_MAXIMUM;

/// A control client's global size, per-window size, or explicit window removal.
/// The wire request remains a string; all public entry paths validate it here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ControlClientSize {
    /// Global content size, in either `WxH` or `W,H` form.
    Global(TerminalSize),
    /// Content size for one canonical window (`@id:WxH`), each axis in 1..=10000.
    Window(WindowId, TerminalSize),
    /// Stop constraining a canonical window (`@id:`).
    RemoveWindow(WindowId),
}

impl ControlClientSize {
    /// Parses the complete argument without accepting signs, suffixes or overflow.
    pub fn parse(value: &str) -> Result<Self, RmuxError> {
        Self::parse_checked(value)
            .ok_or_else(|| RmuxError::Server(format!("invalid refresh-client size '{value}'")))
    }

    fn parse_checked(value: &str) -> Option<Self> {
        if let Some(selector) = value.strip_prefix('@') {
            let (id, dimensions) = selector.split_once(':')?;
            let id = WindowId::new(decimal(id)?);
            if dimensions.is_empty() {
                Some(Self::RemoveWindow(id))
            } else {
                let size = dimensions_size(dimensions, false)?;
                // tmux 3.7b WINDOW_MAXIMUM, measured at both axis boundaries.
                (size.cols <= WINDOW_MAXIMUM && size.rows <= WINDOW_MAXIMUM)
                    .then_some(Self::Window(id, size))
            }
        } else {
            Some(Self::Global(dimensions_size(value, true)?))
        }
    }
}

fn decimal<T: std::str::FromStr>(value: &str) -> Option<T> {
    if value.is_empty() || !value.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    value.parse().ok()
}

fn dimensions_size(value: &str, comma: bool) -> Option<TerminalSize> {
    let (cols, rows) =
        value
            .split_once('x')
            .or_else(|| if comma { value.split_once(',') } else { None })?;
    let size = TerminalSize {
        cols: decimal(cols)?,
        rows: decimal(rows)?,
    };
    (size.cols > 0 && size.rows > 0).then_some(size)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn control_size_parses_global_set_and_remove() {
        let size = TerminalSize {
            cols: 101,
            rows: 31,
        };
        for text in ["101x31", "101,31"] {
            assert_eq!(
                ControlClientSize::parse(text).unwrap(),
                ControlClientSize::Global(size)
            );
        }
        assert_eq!(
            ControlClientSize::parse("@42:101x31").unwrap(),
            ControlClientSize::Window(WindowId::new(42), size)
        );
        assert_eq!(
            ControlClientSize::parse("@42:").unwrap(),
            ControlClientSize::RemoveWindow(WindowId::new(42))
        );
        assert!(ControlClientSize::parse("@4294967295:10000x1").is_ok());
        assert!(
            ControlClientSize::parse("65535x1").is_ok(),
            "retain the existing global u16 range"
        );
    }

    #[test]
    fn control_size_rejects_malformed_and_overflow_without_a_removal_fallback() {
        for text in [
            "",
            "101",
            "0x31",
            "101,0",
            "65536x1",
            "1x65536",
            "101x31,20",
            "101,31x20",
            "@",
            "@:1x1",
            "@1",
            "@1:bad",
            "@1:1,1",
            "@1:1x",
            "@1:0x1",
            "@1:1x1tail",
            "@1::",
            "@-1:",
            "@+1:",
            "@4294967296:",
            "@1:65536x1",
            "@1:10001x1",
            "@1:1x10001",
            " 1x1",
            "+1x1",
            "@1:1x1 ",
        ] {
            assert!(ControlClientSize::parse(text).is_err(), "{text}");
        }
    }
}
