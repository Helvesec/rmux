use rmux_proto::RmuxError;

pub(super) fn parse_usize(command: &str, flag: &str, value: &str) -> Result<usize, RmuxError> {
    value.parse::<usize>().map_err(|error| {
        RmuxError::Server(format!(
            "{command} {flag} expects an unsigned integer: {error}"
        ))
    })
}

pub(super) fn parse_u16(command: &str, flag: &str, value: &str) -> Result<u16, RmuxError> {
    value.parse().map_err(|error| {
        RmuxError::Server(format!(
            "{command} {flag} expects an unsigned 16-bit integer: {error}"
        ))
    })
}

pub(super) fn parse_u32(command: &str, flag: &str, value: &str) -> Result<u32, RmuxError> {
    value.parse().map_err(|error| {
        RmuxError::Server(format!(
            "{command} {flag} expects an unsigned 32-bit integer: {error}"
        ))
    })
}

pub(super) fn parse_u64(command: &str, flag: &str, value: &str) -> Result<u64, RmuxError> {
    value.parse().map_err(|error| {
        RmuxError::Server(format!(
            "{command} {flag} expects an unsigned 64-bit integer: {error}"
        ))
    })
}

pub(super) fn parse_percentage(command: &str, flag: &str, value: &str) -> Result<u8, RmuxError> {
    let percentage = value.parse::<u8>().map_err(|error| {
        RmuxError::Server(format!("{command} {flag} expects a percentage: {error}"))
    })?;
    if percentage > 100 {
        return Err(RmuxError::Server(format!(
            "{command} {flag} expects a percentage between 0 and 100"
        )));
    }
    Ok(percentage)
}

pub(super) fn parse_f64(command: &str, flag: &str, value: &str) -> Result<f64, RmuxError> {
    value.parse().map_err(|error| {
        RmuxError::Server(format!(
            "{command} {flag} expects a floating-point number: {error}"
        ))
    })
}

pub(super) fn parse_non_negative_f64(
    command: &str,
    flag: &str,
    value: &str,
) -> Result<f64, RmuxError> {
    let parsed = parse_f64(command, flag, value)?;
    if !parsed.is_finite() || parsed < 0.0 {
        return Err(RmuxError::Server(format!(
            "{command} {flag} expects a non-negative finite delay"
        )));
    }
    Ok(parsed)
}

pub(super) fn missing_argument(command: &str, argument: &str) -> RmuxError {
    RmuxError::Server(format!("{command} requires {argument}"))
}

pub(super) fn unsupported_flag(command: &str, flag: &str) -> RmuxError {
    RmuxError::Server(format!("command {command}: unknown flag {flag}"))
}

/// Rejects an option-looking token before a parser starts consuming its
/// positional tail.
///
/// A bare `-` remains a positional value, while an explicit `--` is consumed
/// by each command parser before this helper is reached. This keeps
/// dash-prefixed commands available through `--` without silently treating a
/// misspelled option as a shell command, binding, template, or wait channel.
pub(super) fn reject_unknown_option_before_positional(
    command: &str,
    token: &str,
) -> Result<(), RmuxError> {
    if token.starts_with('-') && token != "-" {
        return Err(unsupported_flag(command, token));
    }
    Ok(())
}
