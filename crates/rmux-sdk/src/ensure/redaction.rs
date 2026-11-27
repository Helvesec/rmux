use std::io;

use crate::{CollectError, RmuxError};

const REDACTED_ENVIRONMENT: &str = "[redacted process environment]";

pub(super) fn redact_environment_error(
    error: RmuxError,
    environment: Option<&[String]>,
) -> RmuxError {
    let Some(environment) = environment.filter(|environment| !environment.is_empty()) else {
        return error;
    };

    match error {
        RmuxError::Unsupported { feature, hint } => RmuxError::Unsupported {
            feature,
            hint: redact_environment_message(&hint, environment),
        },
        RmuxError::Protocol { source } => RmuxError::from_proto(redact_environment_protocol_error(
            source.into_proto(),
            environment,
        )),
        RmuxError::Transport { operation, source } => {
            let kind = source.kind();
            RmuxError::transport(
                operation,
                io::Error::new(
                    kind,
                    redact_environment_message(&source.to_string(), environment),
                ),
            )
        }
        RmuxError::Collect { source } => RmuxError::collect(
            source
                .into_errors()
                .into_iter()
                .map(|error| redact_environment_error(error, Some(environment)))
                .collect::<CollectError>(),
        ),
        RmuxError::PartialBroadcast { source } => RmuxError::partial_broadcast(source),
        RmuxError::WaitTimeout { source } => RmuxError::wait_timeout(source),
        RmuxError::PaneNotFound {
            session_name,
            pane_id,
        } => RmuxError::pane_not_found(session_name, pane_id),
        RmuxError::ProcessStillRunning { message } => RmuxError::ProcessStillRunning { message },
        RmuxError::SpawnFailed { message } => RmuxError::SpawnFailed {
            message: redact_environment_message(&message, environment),
        },
        RmuxError::InvalidRegex { pattern, message } => {
            RmuxError::InvalidRegex { pattern, message }
        }
        RmuxError::OwnedSessionLeaseLost { message } => {
            RmuxError::OwnedSessionLeaseLost { message }
        }
    }
}

fn redact_environment_protocol_error(
    error: rmux_proto::RmuxError,
    environment: &[String],
) -> rmux_proto::RmuxError {
    match error {
        rmux_proto::RmuxError::InvalidTarget { value, reason } => {
            rmux_proto::RmuxError::InvalidTarget {
                value: redact_environment_message(&value, environment),
                reason: redact_environment_message(&reason, environment),
            }
        }
        rmux_proto::RmuxError::UnknownCommand(command) => {
            rmux_proto::RmuxError::UnknownCommand(redact_environment_message(&command, environment))
        }
        rmux_proto::RmuxError::DuplicateSession(session_name) => {
            rmux_proto::RmuxError::DuplicateSession(session_name)
        }
        rmux_proto::RmuxError::SessionNotFound(session_name) => {
            rmux_proto::RmuxError::SessionNotFound(session_name)
        }
        rmux_proto::RmuxError::PaneNotFound {
            session_name,
            pane_id,
        } => rmux_proto::RmuxError::PaneNotFound {
            session_name,
            pane_id,
        },
        rmux_proto::RmuxError::ProcessStillRunning => rmux_proto::RmuxError::ProcessStillRunning,
        rmux_proto::RmuxError::SpawnFailed { message } => rmux_proto::RmuxError::SpawnFailed {
            message: redact_environment_message(&message, environment),
        },
        rmux_proto::RmuxError::OwnedSessionLeaseLost { session_name } => {
            rmux_proto::RmuxError::OwnedSessionLeaseLost { session_name }
        }
        rmux_proto::RmuxError::Server(message) => {
            rmux_proto::RmuxError::Server(redact_environment_message(&message, environment))
        }
        rmux_proto::RmuxError::Message(message) => {
            rmux_proto::RmuxError::Message(redact_environment_message(&message, environment))
        }
        rmux_proto::RmuxError::InvalidSetOption(message) => {
            rmux_proto::RmuxError::InvalidSetOption(redact_environment_message(
                &message,
                environment,
            ))
        }
        rmux_proto::RmuxError::UnsupportedCapability { feature, supported } => {
            rmux_proto::RmuxError::UnsupportedCapability {
                feature: redact_environment_message(&feature, environment),
                supported: supported
                    .into_iter()
                    .map(|value| redact_environment_message(&value, environment))
                    .collect(),
            }
        }
        rmux_proto::RmuxError::Encode(message) => {
            rmux_proto::RmuxError::Encode(redact_environment_message(&message, environment))
        }
        rmux_proto::RmuxError::Decode(message) => {
            rmux_proto::RmuxError::Decode(redact_environment_message(&message, environment))
        }
        error => error,
    }
}

fn redact_environment_message(message: &str, environment: &[String]) -> String {
    let mut redacted = message.to_owned();
    for binding in environment {
        replace_environment_secret(&mut redacted, binding);
        if let Some((name, value)) = binding.split_once('=') {
            replace_environment_name(&mut redacted, name);
            if value.len() >= 4 {
                replace_environment_secret(&mut redacted, value);
            }
        } else {
            replace_environment_name(&mut redacted, binding);
        }
    }
    redacted
}

fn replace_environment_secret(message: &mut String, secret: &str) {
    if !secret.is_empty() && message.contains(secret) {
        *message = message.replace(secret, REDACTED_ENVIRONMENT);
    }
}

fn replace_environment_name(message: &mut String, name: &str) {
    if !is_environment_name(name) {
        return;
    }

    let mut redacted = String::with_capacity(message.len());
    let mut copied_until = 0;
    for (start, _) in message.match_indices(name) {
        let end = start + name.len();
        if is_environment_name_match(message.as_bytes(), start, end) {
            redacted.push_str(&message[copied_until..start]);
            redacted.push_str(REDACTED_ENVIRONMENT);
            copied_until = end;
        }
    }

    if copied_until != 0 {
        redacted.push_str(&message[copied_until..]);
        *message = redacted;
    }
}

fn is_environment_name(name: &str) -> bool {
    let mut bytes = name.bytes();
    let Some(first) = bytes.next() else {
        return false;
    };

    (first.is_ascii_alphabetic() || first == b'_')
        && bytes.all(|byte| byte.is_ascii_alphanumeric() || byte == b'_')
}

fn is_environment_name_match(message: &[u8], start: usize, end: usize) -> bool {
    !start
        .checked_sub(1)
        .and_then(|index| message.get(index))
        .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
        && !message
            .get(end)
            .is_some_and(|byte| byte.is_ascii_alphanumeric() || *byte == b'_')
}

#[cfg(test)]
mod tests {
    use super::{redact_environment_message, REDACTED_ENVIRONMENT};

    #[test]
    fn environment_redaction_scrubs_binding_value_and_name() {
        let environment = [String::from("SDK_SECRET_NAME=hidden-value")];
        let rendered = redact_environment_message(
            "SDK_SECRET_NAME failed after SDK_SECRET_NAME=hidden-value exposed hidden-value",
            &environment,
        );

        assert!(!rendered.contains("SDK_SECRET_NAME"), "{rendered}");
        assert!(!rendered.contains("hidden-value"), "{rendered}");
        assert!(rendered.contains(REDACTED_ENVIRONMENT), "{rendered}");
    }

    #[test]
    fn environment_name_redaction_respects_identifier_boundaries() {
        let environment = [String::from("SDK_SECRET_NAME=hidden")];
        let rendered = redact_environment_message("prefix_SDK_SECRET_NAME_suffix", &environment);

        assert_eq!(rendered, "prefix_SDK_SECRET_NAME_suffix");
    }
}
