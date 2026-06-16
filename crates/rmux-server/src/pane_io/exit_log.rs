use std::fmt;
use std::io::Write;
use std::time::{SystemTime, UNIX_EPOCH};

use rmux_proto::SessionName;

const ATTACH_EXIT_LOG_ENV: &str = "RMUX_ATTACH_EXIT_LOG";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum AttachExitReason {
    AttachControlDetach,
    AttachControlExited,
    AttachControlDetachKill,
    AttachControlDetachExec,
    AttachStreamClosed,
    AttachClosingFlag,
    ServerShutdown,
    PendingServerShutdown,
    AttachError,
}

impl fmt::Display for AttachExitReason {
    fn fmt(&self, formatter: &mut fmt::Formatter<'_>) -> fmt::Result {
        formatter.write_str(match self {
            Self::AttachControlDetach => "attach-control-detach",
            Self::AttachControlExited => "attach-control-exited",
            Self::AttachControlDetachKill => "attach-control-detach-kill",
            Self::AttachControlDetachExec => "attach-control-detach-exec",
            Self::AttachStreamClosed => "attach-stream-closed",
            Self::AttachClosingFlag => "attach-closing-flag",
            Self::ServerShutdown => "server-shutdown",
            Self::PendingServerShutdown => "pending-server-shutdown",
            Self::AttachError => "attach-error",
        })
    }
}

pub(super) fn record_attach_exit(
    attach_pid: u32,
    session_name: &SessionName,
    reason: AttachExitReason,
) {
    record_attach_exit_line(attach_pid, session_name, reason, None);
}

pub(super) fn record_attach_error(
    attach_pid: u32,
    session_name: &SessionName,
    error: &dyn std::fmt::Display,
) {
    record_attach_exit_line(
        attach_pid,
        session_name,
        AttachExitReason::AttachError,
        Some(&error.to_string()),
    );
}

fn record_attach_exit_line(
    attach_pid: u32,
    session_name: &SessionName,
    reason: AttachExitReason,
    detail: Option<&str>,
) {
    let Some(path) = std::env::var_os(ATTACH_EXIT_LOG_ENV) else {
        return;
    };
    let timestamp_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|duration| duration.as_millis())
        .unwrap_or_default();
    let Ok(mut file) = std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(path)
    else {
        return;
    };
    match detail {
        Some(detail) => {
            let escaped = detail.replace('\n', "\\n");
            let _ = writeln!(
                file,
                "time_ms={timestamp_ms} process_pid={} attach_pid={attach_pid} session={} reason={reason} detail={escaped:?}",
                std::process::id(),
                session_name
            );
        }
        None => {
            let _ = writeln!(
                file,
                "time_ms={timestamp_ms} process_pid={} attach_pid={attach_pid} session={} reason={reason}",
                std::process::id(),
                session_name
            );
        }
    }
}
