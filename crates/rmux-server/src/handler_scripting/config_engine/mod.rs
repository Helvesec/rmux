//! Shared config loading pipeline.
//!
//! RMUX config loading is best-effort and diagnostic-driven. Startup config,
//! tmux import, explicit `source-file`, parse-only `source-file`, and nested
//! `source-file` are routed through this module so they share one set of
//! source, parse, lower, execute, and diagnostic rules.

mod diagnostics;
mod execute;
mod lower;
mod parse;
mod request;
mod result;
mod source;

use rmux_proto::RmuxError;

use super::super::RequestHandler;
use super::source_files::LoadedSourceFile;
use result::{ConfigDiagnosticSeverity, ConfigLoadResult};

pub(super) use diagnostics::config_error_lines;
pub(super) use execute::{append_error_output, nonempty_stdout};
pub(super) use request::{ConfigLoadOrigin, ConfigLoadRequest};

pub(super) async fn load(
    handler: &RequestHandler,
    request: ConfigLoadRequest<'_>,
) -> Result<LoadedSourceFile, RmuxError> {
    request.assert_boundary_invariants();
    let result = ConfigLoadResult::default();
    result.assert_boundary_invariants();
    let _ = ConfigDiagnosticSeverity::ALL;
    handler
        .load_source_file_command_inner(request.command, request.depth)
        .await
}
