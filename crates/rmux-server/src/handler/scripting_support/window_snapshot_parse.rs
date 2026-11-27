use std::path::{Path, PathBuf};

use rmux_core::{ManagedSessionStore, TargetFindContext};
use rmux_proto::{
    decode_rmux_window_snapshot_json, decode_rmux_window_snapshot_v2_json,
    ExportWindowSnapshotRequest, ExportWindowSnapshotV2Request, Request,
    RestoreWindowSnapshotRequest, RestoreWindowSnapshotV2Request, RmuxError, WindowTarget,
};

use super::tokens::{parse_compact_flag_cluster, CommandTokens, CompactFlag};
use super::values::{missing_argument, reject_unknown_option_before_positional};
use super::{implicit_window_target, parse_window_target};

pub(super) fn parse_export_window_snapshot(
    mut args: CommandTokens,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let target =
        parse_snapshot_target_options(&mut args, "export-window-snapshot", sessions, find_context)?;
    args.no_extra("export-window-snapshot")?;
    Ok(Request::ExportWindowSnapshot(ExportWindowSnapshotRequest {
        target,
    }))
}

/// A file operand, deliberately unopened during syntax validation.
#[derive(Debug, Clone)]
pub(super) struct ParsedRestoreWindowSnapshot {
    target: WindowTarget,
    path: PathBuf,
    version: SnapshotVersion,
}

#[derive(Debug, Clone, Copy)]
enum SnapshotVersion {
    V1,
    V2,
}

impl SnapshotVersion {
    fn command(self) -> &'static str {
        match self {
            Self::V1 => "restore-window-snapshot",
            Self::V2 => "restore-window-snapshot-v2",
        }
    }
}

impl ParsedRestoreWindowSnapshot {
    pub(super) async fn load(self) -> Result<Request, RmuxError> {
        let command = self.version.command();
        // Use the cancellable file reader: dropping tokio::fs::read_to_string
        // leaves a FIFO open/read blocked in the runtime's blocking pool.
        let json = crate::buffer_file_io::read_snapshot(self.path.clone())
            .await
            .and_then(|bytes| {
                String::from_utf8(bytes)
                    .map_err(|error| std::io::Error::new(std::io::ErrorKind::InvalidData, error))
            })
            .map_err(|error| {
                RmuxError::Server(format!(
                    "{command}: failed to read {}: {error}",
                    self.path.display()
                ))
            })?;
        let invalid =
            |error| RmuxError::Server(format!("{command}: invalid RMUX window snapshot: {error}"));
        match self.version {
            SnapshotVersion::V1 => Ok(Request::RestoreWindowSnapshot(Box::new(
                RestoreWindowSnapshotRequest {
                    target: self.target,
                    snapshot: decode_rmux_window_snapshot_json(&json).map_err(invalid)?,
                },
            ))),
            SnapshotVersion::V2 => Ok(Request::RestoreWindowSnapshotV2(Box::new(
                RestoreWindowSnapshotV2Request {
                    target: self.target,
                    snapshot: decode_rmux_window_snapshot_v2_json(&json).map_err(invalid)?,
                },
            ))),
        }
    }
}

pub(super) fn parse_restore_window_snapshot(
    args: CommandTokens,
    caller_cwd: Option<&Path>,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<ParsedRestoreWindowSnapshot, RmuxError> {
    parse_restore_snapshot(
        args,
        caller_cwd,
        sessions,
        find_context,
        SnapshotVersion::V1,
    )
}

fn parse_restore_snapshot(
    mut args: CommandTokens,
    caller_cwd: Option<&Path>,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
    version: SnapshotVersion,
) -> Result<ParsedRestoreWindowSnapshot, RmuxError> {
    let command = version.command();
    let target = parse_snapshot_target_options(&mut args, command, sessions, find_context)?;
    let path = args
        .optional()
        .ok_or_else(|| missing_argument(command, "snapshot-file"))?;
    args.no_extra(command)?;
    if path == "-" {
        return Err(RmuxError::Server(format!(
            "{command} - requires direct CLI stdin"
        )));
    }
    Ok(ParsedRestoreWindowSnapshot {
        target,
        path: resolve_snapshot_path(PathBuf::from(path), caller_cwd),
        version,
    })
}

fn parse_snapshot_target_options(
    args: &mut CommandTokens,
    command: &str,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<rmux_proto::WindowTarget, RmuxError> {
    let mut target = None;

    while let Some(token) = args.peek().map(str::to_owned) {
        match token.as_str() {
            "--" => {
                let _ = args.optional();
                break;
            }
            "-t" => {
                let _ = args.optional();
                target = Some(parse_window_target(command, args.required("-t target")?)?);
            }
            _ => {
                let Some(cluster) = parse_compact_flag_cluster(command, &token, "", "t")? else {
                    reject_unknown_option_before_positional(command, &token)?;
                    break;
                };
                let _ = args.optional();
                for flag in cluster {
                    match flag {
                        compact_flag @ CompactFlag::Value { flag: 't', .. } => {
                            target = Some(parse_window_target(
                                command,
                                compact_flag.value_or_next(args, "-t target")?,
                            )?);
                        }
                        _ => unreachable!("compact snapshot flags are prevalidated"),
                    }
                }
            }
        }
    }

    target.map_or_else(
        || implicit_window_target(sessions, find_context, command),
        Ok,
    )
}

fn resolve_snapshot_path(path: PathBuf, caller_cwd: Option<&Path>) -> PathBuf {
    if path.is_absolute() {
        path
    } else {
        caller_cwd.map_or(path.clone(), |cwd| cwd.join(path))
    }
}

pub(super) fn parse_export_window_snapshot_v2(
    mut args: CommandTokens,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let target = parse_snapshot_target_options(
        &mut args,
        "export-window-snapshot-v2",
        sessions,
        find_context,
    )?;
    args.no_extra("export-window-snapshot-v2")?;
    Ok(Request::ExportWindowSnapshotV2(
        ExportWindowSnapshotV2Request { target },
    ))
}

pub(super) fn parse_restore_window_snapshot_v2(
    args: CommandTokens,
    caller_cwd: Option<&Path>,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<ParsedRestoreWindowSnapshot, RmuxError> {
    parse_restore_snapshot(
        args,
        caller_cwd,
        sessions,
        find_context,
        SnapshotVersion::V2,
    )
}
