//! Versioned detached RPC frame envelope.

use std::ops::RangeInclusive;

use crate::RmuxError;

/// Magic byte that identifies versioned RMUX detached RPC frames.
pub const RMUX_FRAME_MAGIC: u8 = 0x52;
/// Current detached RPC wire version.
pub const RMUX_WIRE_VERSION: u32 = 2;

/// Supported detached RPC wire-version range for this build.
pub const SUPPORTED_WIRE_VERSION: RangeInclusive<u32> = RMUX_WIRE_VERSION..=RMUX_WIRE_VERSION;

/// Encodes a u32 as unsigned LEB128.
pub(crate) fn encode_varint_u32(mut value: u32, output: &mut Vec<u8>) {
    loop {
        let mut byte = (value & 0x7f) as u8;
        value >>= 7;
        if value != 0 {
            byte |= 0x80;
        }
        output.push(byte);
        if value == 0 {
            break;
        }
    }
}

/// Decodes a u32 from unsigned LEB128 bytes.
pub(crate) fn decode_varint_u32(bytes: &[u8]) -> Result<Option<(u32, usize)>, RmuxError> {
    let mut value = 0_u32;
    let mut shift = 0;

    for (index, byte) in bytes.iter().copied().enumerate().take(5) {
        let payload = byte & 0x7f;
        if index == 4 && payload > 0x0f {
            return Err(RmuxError::Decode(
                "wire-version varint overflows u32".to_owned(),
            ));
        }
        value |= u32::from(payload) << shift;
        if byte & 0x80 == 0 {
            if index > 0 && payload == 0 {
                return Err(RmuxError::Decode(
                    "wire-version varint is not minimally encoded".to_owned(),
                ));
            }
            return Ok(Some((value, index + 1)));
        }
        shift += 7;
    }

    if bytes.len() < 5 {
        Ok(None)
    } else {
        Err(RmuxError::Decode(
            "wire-version varint exceeds u32 length".to_owned(),
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::{decode_varint_u32, encode_varint_u32};

    #[test]
    fn varint_round_trips_representative_values() {
        for value in [0, 1, 127, 128, 16_384, u32::MAX] {
            let mut encoded = Vec::new();
            encode_varint_u32(value, &mut encoded);
            assert_eq!(
                decode_varint_u32(&encoded),
                Ok(Some((value, encoded.len())))
            );
        }
    }

    #[test]
    fn varint_reports_incomplete_values() {
        assert_eq!(decode_varint_u32(&[0x80]), Ok(None));
    }

    #[test]
    fn varint_rejects_non_minimal_encodings() {
        assert!(decode_varint_u32(&[0x82, 0x00]).is_err());
        assert!(decode_varint_u32(&[0x80, 0x00]).is_err());
    }

    #[test]
    fn varint_rejects_u32_overflow_bits() {
        assert!(decode_varint_u32(&[0xff, 0xff, 0xff, 0xff, 0x10]).is_err());
        assert!(decode_varint_u32(&[0xff, 0xff, 0xff, 0xff, 0x8f]).is_err());
    }
}
