use std::time::{Duration, UNIX_EPOCH};

use chrono::{DateTime, Utc};
use rmux_proto::{CommandOutput, WebShareSummary};

pub(super) struct CreatedOutput<'a> {
    pub(super) spectator_url: Option<&'a str>,
    pub(super) operator_url_emitted: bool,
    pub(super) tunnel_provider: Option<&'a str>,
    pub(super) tunnel_public_url: Option<&'a str>,
    pub(super) operator_pin: Option<&'a str>,
    pub(super) spectator_pin: Option<&'a str>,
    pub(super) expires_at_unix: Option<u64>,
    pub(super) kill_session_on_expire: bool,
}

pub(super) fn created_output(created: CreatedOutput<'_>) -> CommandOutput {
    let mut output = String::new();
    if let Some(provider) = created.tunnel_provider {
        output.push_str("tunnel provider ");
        output.push_str(provider);
        output.push('\n');
    }
    if let Some(public_url) = created.tunnel_public_url {
        output.push_str("tunnel url ");
        output.push_str(public_url);
        output.push('\n');
    }
    if let Some(spectator_url) = created.spectator_url {
        output.push_str("spectator ");
        output.push_str(spectator_url);
        output.push('\n');
    }
    if created.operator_url_emitted {
        output.push_str("operator URL emitted on stderr\n");
    }
    if let Some(expires_at_unix) = created.expires_at_unix {
        output.push_str("share expires at ");
        output.push_str(&format_unix_rfc3339(expires_at_unix));
        output.push('\n');
    } else {
        output.push_str("share does not expire\n");
    }
    if created.kill_session_on_expire {
        output.push_str("session will be killed on expiry\n");
    }
    if let Some(operator_pin) = created.operator_pin {
        output.push_str("operator pin ");
        output.push_str(operator_pin);
        output.push('\n');
    }
    if let Some(spectator_pin) = created.spectator_pin {
        output.push_str("spectator pin ");
        output.push_str(spectator_pin);
        output.push('\n');
    }
    CommandOutput::from_stdout(output)
}

pub(super) fn list_output(shares: &[WebShareSummary]) -> CommandOutput {
    let mut output = String::new();
    for share in shares {
        output.push_str(&share.share_id);
        output.push(' ');
        output.push_str(&share.scope.to_string());
        output.push(' ');
        output.push_str(share.spectator_url.as_deref().unwrap_or("-"));
        output.push('\n');
    }
    CommandOutput::from_stdout(output)
}

pub(super) fn lookup_output(share: Option<&WebShareSummary>) -> CommandOutput {
    match share {
        Some(share) => list_output(std::slice::from_ref(share)),
        None => CommandOutput::from_stdout(Vec::new()),
    }
}

pub(super) fn stopped_output(share_id: &str, stopped: bool) -> CommandOutput {
    let status = if stopped { "stopped" } else { "missing" };
    CommandOutput::from_stdout(format!("{status} {share_id}\n"))
}

fn format_unix_rfc3339(value: u64) -> String {
    DateTime::<Utc>::from(UNIX_EPOCH + Duration::from_secs(value)).to_rfc3339()
}
