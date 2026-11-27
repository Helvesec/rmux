use rmux_proto::RmuxError;

/// Parses refresh-client's positional argument only when a pan flag is present.
/// A plain refresh ignores its positional argument, as tmux 3.7b does.
pub fn parse_pan_adjustment(value: &str) -> Result<u32, RmuxError> {
    let number = value.trim_start().parse::<i64>().map_err(|error| {
        let reason = match error.kind() {
            std::num::IntErrorKind::PosOverflow => "too large",
            std::num::IntErrorKind::NegOverflow => "too small",
            _ => "invalid",
        };
        RmuxError::Server(format!("adjustment {reason}"))
    })?;
    if number < 1 {
        return Err(RmuxError::Server("adjustment too small".to_owned()));
    }
    if number > i64::from(i32::MAX) {
        return Err(RmuxError::Server("adjustment too large".to_owned()));
    }
    Ok(number as u32)
}
