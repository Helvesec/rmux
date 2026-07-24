//! Strict on-disk format for private Windows endpoint discovery.

#![cfg(windows)]

use std::io;

pub(crate) const KEY_HEX_LEN: usize = 64;
pub(crate) const NONCE_HEX_LEN: usize = 32;
pub(crate) const LEGACY_COMPONENT_MAX_LEN: usize = 256;
const STATE_FORMAT_V1: &str = "rmux-endpoint-state-v1";
const STATE_FORMAT_V2: &str = "rmux-endpoint-state-v2";

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum EndpointPhase {
    Resolved,
    Starting,
    Running,
    Stopped,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct EndpointRecord {
    pub(crate) phase: EndpointPhase,
    pub(crate) key: String,
    pub(crate) nonce: String,
    pub(crate) legacy_component: Option<String>,
    pub(crate) process: ProcessStamp,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ProcessStamp {
    pub(crate) pid: u32,
    pub(crate) created: u64,
}

pub(crate) fn serialize(record: &EndpointRecord) -> String {
    let phase = match record.phase {
        EndpointPhase::Resolved => "resolved",
        EndpointPhase::Starting => "starting",
        EndpointPhase::Running => "running",
        EndpointPhase::Stopped => "stopped",
    };
    format!(
        "{STATE_FORMAT_V2}\nphase={phase}\nkey={}\nnonce={}\nlegacy={}\npid={}\ncreated={}\n",
        record.key,
        record.nonce,
        record.legacy_component.as_deref().unwrap_or_default(),
        record.process.pid,
        record.process.created
    )
}

pub(crate) fn parse(text: &str, expected_key: &str) -> io::Result<EndpointRecord> {
    let mut lines = text.lines();
    let format = lines
        .next()
        .ok_or_else(|| invalid_state("managed endpoint state version is missing"))?;
    if !matches!(format, STATE_FORMAT_V1 | STATE_FORMAT_V2) {
        return Err(invalid_state("managed endpoint state version is invalid"));
    };
    let phase = match field(&mut lines, "phase")? {
        "resolved" => EndpointPhase::Resolved,
        "starting" => EndpointPhase::Starting,
        "running" => EndpointPhase::Running,
        "stopped" => EndpointPhase::Stopped,
        _ => return Err(invalid_state("managed endpoint state phase is invalid")),
    };
    let key = field(&mut lines, "key")?.to_owned();
    let nonce = field(&mut lines, "nonce")?.to_owned();
    let legacy_component = if format == STATE_FORMAT_V2 {
        match field(&mut lines, "legacy")? {
            "" => None,
            value => Some(value.to_owned()),
        }
    } else {
        None
    };
    let pid = field(&mut lines, "pid")?
        .parse::<u32>()
        .map_err(|_| invalid_state("managed endpoint state pid is invalid"))?;
    let created = field(&mut lines, "created")?
        .parse::<u64>()
        .map_err(|_| invalid_state("managed endpoint state creation time is invalid"))?;
    if lines.next().is_some()
        || key != expected_key
        || key.len() != KEY_HEX_LEN
        || nonce.len() != NONCE_HEX_LEN
        || !is_lower_hex(&key)
        || !is_lower_hex(&nonce)
        || legacy_component
            .as_deref()
            .is_some_and(|component| !is_legacy_component(component))
        || pid == 0
    {
        return Err(invalid_state("managed endpoint state contents are invalid"));
    }
    Ok(EndpointRecord {
        phase,
        key,
        nonce,
        legacy_component,
        process: ProcessStamp { pid, created },
    })
}

fn is_legacy_component(value: &str) -> bool {
    !value.is_empty()
        && value.len() <= LEGACY_COMPONENT_MAX_LEN
        && value
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~'))
}

pub(crate) fn is_lower_hex(value: &str) -> bool {
    !value.is_empty()
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

fn field<'a>(lines: &mut impl Iterator<Item = &'a str>, name: &str) -> io::Result<&'a str> {
    let line = lines
        .next()
        .ok_or_else(|| invalid_state("managed endpoint state is truncated"))?;
    line.strip_prefix(name)
        .and_then(|value| value.strip_prefix('='))
        .ok_or_else(|| invalid_state("managed endpoint state field order is invalid"))
}

fn invalid_state(message: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, message)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn record_round_trips_strictly() {
        let key = "a".repeat(KEY_HEX_LEN);
        let record = EndpointRecord {
            phase: EndpointPhase::Running,
            key: key.clone(),
            nonce: "b".repeat(NONCE_HEX_LEN),
            legacy_component: Some("default".to_owned()),
            process: ProcessStamp {
                pid: 42,
                created: 99,
            },
        };
        assert_eq!(
            parse(&serialize(&record), &key).expect("parse state"),
            record
        );
    }

    #[test]
    fn record_rejects_trailing_or_mismatched_data() {
        let key = "a".repeat(KEY_HEX_LEN);
        let record = EndpointRecord {
            phase: EndpointPhase::Starting,
            key: key.clone(),
            nonce: "b".repeat(NONCE_HEX_LEN),
            legacy_component: None,
            process: ProcessStamp { pid: 1, created: 1 },
        };
        let mut text = serialize(&record);
        text.push_str("extra=value\n");
        assert!(parse(&text, &key).is_err());
        assert!(parse(&serialize(&record), &"c".repeat(KEY_HEX_LEN)).is_err());
    }

    #[test]
    fn v1_record_remains_readable_without_a_legacy_bridge() {
        let key = "a".repeat(KEY_HEX_LEN);
        let text = format!(
            "{STATE_FORMAT_V1}\nphase=running\nkey={key}\nnonce={}\npid=42\ncreated=99\n",
            "b".repeat(NONCE_HEX_LEN)
        );
        let record = parse(&text, &key).expect("parse v1 state");
        assert_eq!(record.legacy_component, None);
    }
}
