//! Output and diagnostic helpers for the config runtime in `source_runtime`.

mod diagnostics;
mod execute;

pub(super) use diagnostics::config_error_lines;
pub(super) use execute::{append_error_output, nonempty_stdout};
