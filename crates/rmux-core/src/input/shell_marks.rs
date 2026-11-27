//! OSC 133 has no effect until its BEL or complete ST terminator arrives.

use super::{InputParser, ScreenWriter};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellMark {
    Prompt,
    Output,
}

impl ShellMark {
    pub(crate) fn parse(data: &str) -> Option<Self> {
        match data.split(';').next()? {
            "A" => Some(Self::Prompt),
            "C" => Some(Self::Output),
            _ => None,
        }
    }
}

impl InputParser {
    pub(super) fn dispatch_shell_integration<W: ScreenWriter + ?Sized>(
        &mut self,
        data: &str,
        writer: &mut W,
    ) {
        match self.ch {
            0x07 => writer.osc_shell_integration(data),
            // The common OSC parser exits on ESC; only OSC 133 defers its
            // effect until the next byte proves this is an ST terminator.
            0x1b => self.pending_shell_integration = Some(data.to_owned()),
            _ => {} // CAN/SUB cancel the mark, as do malformed terminators.
        }
    }

    pub(super) fn finish_pending_shell_integration<W: ScreenWriter + ?Sized>(
        &mut self,
        writer: &mut W,
    ) {
        if let Some(data) = self.pending_shell_integration.take() {
            if self.ch == b'\\' {
                writer.osc_shell_integration(&data);
            }
        }
    }
}
