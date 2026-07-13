use std::io;

use rmux_proto::DEFAULT_MAX_FRAME_LENGTH;

const MAX_RETAINED_ATTACHED_CONTROL_INPUT: usize = DEFAULT_MAX_FRAME_LENGTH;

// Mouse, key, and prompt decoders only need a short prefix to decide whether
// an escape sequence is complete. Keeping an arbitrarily long CSI prefix
// makes fragmented input both memory-heavy and increasingly expensive to
// rescan, while no supported key encoding approaches this bound.
const MAX_RETAINED_ATTACHED_ESCAPE_INPUT: usize = 64;

pub(in crate::handler) fn retain_partial_attached_control_input(
    context: &str,
    pending_input: &mut Vec<u8>,
) -> io::Result<()> {
    retain_partial_attached_input(context, pending_input, MAX_RETAINED_ATTACHED_CONTROL_INPUT)
}

pub(in crate::handler) fn retain_partial_attached_escape_input(
    context: &str,
    pending_input: &mut Vec<u8>,
) -> io::Result<()> {
    retain_partial_attached_input(context, pending_input, MAX_RETAINED_ATTACHED_ESCAPE_INPUT)
}

fn retain_partial_attached_input(
    context: &str,
    pending_input: &mut Vec<u8>,
    maximum: usize,
) -> io::Result<()> {
    let retained = pending_input.len();
    if retained <= maximum {
        return Ok(());
    }

    pending_input.clear();
    Err(io::Error::new(
        io::ErrorKind::InvalidData,
        format!(
            "{context} retained {retained} bytes of partial attached control input; maximum is {maximum}"
        ),
    ))
}

#[cfg(test)]
mod tests {
    use super::{
        retain_partial_attached_control_input, retain_partial_attached_escape_input,
        MAX_RETAINED_ATTACHED_ESCAPE_INPUT,
    };

    #[test]
    fn escape_retention_rejects_and_clears_an_overlong_prefix() {
        let mut pending = vec![b'1'; MAX_RETAINED_ATTACHED_ESCAPE_INPUT + 1];

        let error = retain_partial_attached_escape_input("test escape", &mut pending)
            .expect_err("overlong escape prefixes must fail closed");

        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert!(
            pending.is_empty(),
            "rejected input must not remain retained"
        );
    }

    #[test]
    fn protocol_retention_keeps_payloads_larger_than_escape_prefixes() {
        let mut pending = vec![b'x'; MAX_RETAINED_ATTACHED_ESCAPE_INPUT + 1];

        retain_partial_attached_control_input("test protocol", &mut pending)
            .expect("bounded protocol payload should remain accepted");

        assert_eq!(pending.len(), MAX_RETAINED_ATTACHED_ESCAPE_INPUT + 1);
    }
}
