//! Decoder for the opt-in `stream.control.records.v1` transport.
use std::io;

#[derive(Debug, Default)]
pub(super) struct Records {
    header: Vec<u8>,
    remaining: usize,
    pub(super) status: Option<i32>,
}

impl Records {
    pub(super) fn decode(&mut self, mut bytes: &[u8]) -> io::Result<Vec<u8>> {
        let mut text = Vec::new();
        while !bytes.is_empty() {
            if self.status.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "data after control status",
                ));
            }
            if self.remaining != 0 {
                let n = bytes.len().min(self.remaining);
                text.extend_from_slice(&bytes[..n]);
                self.remaining -= n;
                bytes = &bytes[n..];
                continue;
            }
            let n = bytes.len().min(5 - self.header.len());
            self.header.extend_from_slice(&bytes[..n]);
            bytes = &bytes[n..];
            if self.header.len() != 5 {
                continue;
            }
            let value = u32::from_le_bytes(self.header[1..].try_into().expect("four bytes"));
            match self.header[0] {
                0 if (1..=8192).contains(&value) => self.remaining = value as usize,
                1 if value <= 255 => self.status = Some(value as i32),
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "invalid control record",
                    ))
                }
            }
            self.header.clear();
        }
        Ok(text)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_fragment_boundary_preserves_text_and_typed_status() {
        let text = b"%end 1 1 0\nunknown command: not-a-command\n%exit\n";
        for status in [0_u32, 1, 7] {
            let mut wire = vec![0];
            wire.extend_from_slice(&(text.len() as u32).to_le_bytes());
            wire.extend_from_slice(text);
            wire.push(1);
            wire.extend_from_slice(&status.to_le_bytes());
            for split in 0..=wire.len() {
                let mut decoder = Records::default();
                let mut output = decoder.decode(&wire[..split]).unwrap();
                output.extend(decoder.decode(&wire[split..]).unwrap());
                assert_eq!(output, text);
                assert_eq!(decoder.status, Some(status as i32));
            }
        }
    }

    #[test]
    fn rejects_invalid_length_tag_status_and_trailing_data() {
        for wire in [
            vec![0, 1, 32, 0, 0],
            vec![2, 0, 0, 0, 0],
            vec![1, 0, 1, 0, 0],
            vec![1, 0, 0, 0, 0, 0],
        ] {
            assert!(Records::default().decode(&wire).is_err());
        }
    }
}
