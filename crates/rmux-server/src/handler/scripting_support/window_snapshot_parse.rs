use std::fs;
use std::path::{Path, PathBuf};

use rmux_core::{ManagedSessionStore, TargetFindContext};
use rmux_proto::{
    decode_rmux_window_snapshot_json, decode_rmux_window_snapshot_v2_json,
    ExportWindowSnapshotRequest, ExportWindowSnapshotV2Request, Request,
    RestoreWindowSnapshotRequest, RestoreWindowSnapshotV2Request, RmuxError,
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

pub(super) fn parse_restore_window_snapshot(
    mut args: CommandTokens,
    caller_cwd: Option<&Path>,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let target = parse_snapshot_target_options(
        &mut args,
        "restore-window-snapshot",
        sessions,
        find_context,
    )?;
    let path = args
        .optional()
        .ok_or_else(|| missing_argument("restore-window-snapshot", "snapshot-file"))?;
    args.no_extra("restore-window-snapshot")?;
    if path == "-" {
        return Err(RmuxError::Server(
            "restore-window-snapshot - requires direct CLI stdin".to_owned(),
        ));
    }

    let snapshot_path = resolve_snapshot_path(PathBuf::from(&path), caller_cwd);
    let snapshot_json = fs::read_to_string(&snapshot_path).map_err(|error| {
        RmuxError::Server(format!(
            "restore-window-snapshot: failed to read {}: {error}",
            snapshot_path.display()
        ))
    })?;
    let snapshot = decode_rmux_window_snapshot_json(&snapshot_json).map_err(|error| {
        RmuxError::Server(format!(
            "restore-window-snapshot: invalid RMUX window snapshot: {error}"
        ))
    })?;
    Ok(Request::RestoreWindowSnapshot(Box::new(
        RestoreWindowSnapshotRequest { target, snapshot },
    )))
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
    mut args: CommandTokens,
    caller_cwd: Option<&Path>,
    sessions: &ManagedSessionStore,
    find_context: &TargetFindContext,
) -> Result<Request, RmuxError> {
    let target = parse_snapshot_target_options(
        &mut args,
        "restore-window-snapshot-v2",
        sessions,
        find_context,
    )?;
    let path = args
        .optional()
        .ok_or_else(|| missing_argument("restore-window-snapshot-v2", "snapshot-file"))?;
    args.no_extra("restore-window-snapshot-v2")?;
    if path == "-" {
        return Err(RmuxError::Server(
            "restore-window-snapshot-v2 - requires direct CLI stdin".to_owned(),
        ));
    }

    let snapshot_path = resolve_snapshot_path(PathBuf::from(&path), caller_cwd);
    let snapshot_json = fs::read_to_string(&snapshot_path).map_err(|error| {
        RmuxError::Server(format!(
            "restore-window-snapshot-v2: failed to read {}: {error}",
            snapshot_path.display()
        ))
    })?;
    let snapshot = decode_rmux_window_snapshot_v2_json(&snapshot_json).map_err(|error| {
        RmuxError::Server(format!(
            "restore-window-snapshot-v2: invalid RMUX window snapshot: {error}"
        ))
    })?;
    Ok(Request::RestoreWindowSnapshotV2(Box::new(
        RestoreWindowSnapshotV2Request { target, snapshot },
    )))
}
