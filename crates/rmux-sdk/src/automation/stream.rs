use rmux_proto::PaneOutputSubscriptionId;

use crate::{Result, RmuxError};

pub const LINE_BUFFER_MAX: usize = 1_048_576;

pub fn validate_subscription_id(
    command: &'static str,
    expected: PaneOutputSubscriptionId,
    got: PaneOutputSubscriptionId,
) -> Result<()> {
    if expected == got {
        return Ok(());
    }
    Err(RmuxError::from_proto(rmux_proto::RmuxError::Server(
        format!(
            "rmux daemon sent subscription id {} in `{command}` response for subscription {}",
            got.as_u64(),
            expected.as_u64()
        ),
    )))
}

pub fn split_lines_bounded<E>(
    buffer: &mut Vec<u8>,
    force_flushed: &mut bool,
    bytes: &[u8],
    mut emit: impl FnMut(Vec<u8>) -> std::result::Result<(), E>,
) -> std::result::Result<(), E> {
    for byte in bytes {
        if *byte == b'\n' {
            if buffer.is_empty() && *force_flushed {
                *force_flushed = false;
                continue;
            }
            emit(std::mem::take(buffer))?;
            *force_flushed = false;
        } else {
            buffer.push(*byte);
            *force_flushed = false;
            if buffer.len() >= LINE_BUFFER_MAX {
                emit(std::mem::take(buffer))?;
                *force_flushed = true;
            }
        }
    }
    Ok(())
}
