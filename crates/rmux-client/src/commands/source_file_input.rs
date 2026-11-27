use rmux_proto::SourceFileRelayV1Request;
use std::io::{self, Read};

use crate::{ClientError, Connection};
use rmux_proto::{Request, Response, SourceFileInputDataV1Request, SourceFileInputV1Request};

impl Connection {
    /// Sends a file-backed source and its binary input once, without replay.
    pub fn source_file_input_v1(
        &mut self,
        request: SourceFileInputV1Request,
    ) -> Result<Response, ClientError> {
        self.roundtrip_without_read_timeout(&Request::SourceFileInputV1(Box::new(request)))
    }

    /// Runs the source once and answers at most one negotiated stdin read.
    pub fn source_file_with_stdin(
        &mut self,
        request: SourceFileInputV1Request,
        input: impl Read,
    ) -> Result<Response, ClientError> {
        let pull = request.requests_input_pull();
        let response = self.source_file_input_v1(request)?;
        if pull && matches!(response, Response::SourceFileReadStdinV1(_)) {
            let data = read_finished_input(input);
            self.roundtrip_without_read_timeout(&Request::SourceFileInputDataV1(
                SourceFileInputDataV1Request { data },
            ))
        } else {
            Ok(response)
        }
    }
}

fn read_finished_input(source: impl Read) -> Result<Vec<u8>, String> {
    let maximum = SourceFileInputDataV1Request::MAX_BYTES;
    let mut bytes = Vec::new();
    source
        .take(maximum as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| format!("source-file: failed to read stdin: {error}"))?;
    if bytes.len() > maximum {
        return Err("source-file input exceeds one frame".to_owned());
    }
    Ok(bytes)
}

impl Connection {
    /// Serve one independent stdin offer until the consumer finishes or disappears.
    /// Called in a dedicated CLI child so an outstanding OS read ends with the child.
    pub fn serve_source_file_stdin(
        &mut self,
        ticket: [u8; 16],
        proof: [u8; 16],
    ) -> Result<(), ClientError> {
        use std::sync::mpsc;
        use std::time::Duration;
        let claim = Request::SourceFileRelayV1(SourceFileRelayV1Request { ticket });
        if !matches!(self.roundtrip(&claim)?, Response::SourceFileRelayReadyV1(ready) if ready.proof == proof)
        {
            return Err(io::Error::other("invalid source-file stdin relay response").into());
        }
        match self.read_response_without_read_timeout() {
            Ok(Response::SourceFileReadStdinV1(_)) => {}
            Err(ClientError::UnexpectedEof) => return Ok(()), // unused offer
            Err(error) => return Err(error),
            Ok(_) => return Err(io::Error::other("expected source-file stdin demand").into()),
        }
        let (finished, bytes) = mpsc::channel();
        std::thread::Builder::new()
            .name("source-stdin".into())
            .spawn(move || {
                let _ = finished.send(read_finished_input(io::stdin().lock()));
            })?;
        let data = loop {
            match bytes.recv_timeout(Duration::from_millis(250)) {
                Ok(data) => break data,
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err(io::Error::other("source-file stdin reader stopped").into())
                }
                Err(mpsc::RecvTimeoutError::Timeout) => {}
            }
            // A producer may keep a pipe open forever. Exit the helper if the
            // daemon has cancelled the read or shut down in the meantime.
            match self.read_response_with_read_timeout(Duration::from_millis(10)) {
                Err(ClientError::Io(error)) if error.kind() == io::ErrorKind::TimedOut => {}
                Err(ClientError::UnexpectedEof) => return Ok(()),
                Err(error) => return Err(error),
                Ok(_) => {
                    return Err(
                        io::Error::other("unexpected source-file stdin relay response").into(),
                    )
                }
            }
        };
        self.write_request(&Request::SourceFileInputDataV1(
            SourceFileInputDataV1Request { data },
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn source_input_reader_preserves_binary_eof_and_refuses_oversize() {
        assert_eq!(
            read_finished_input(&b"\0\xff\x1b\n\r"[..]).unwrap(),
            b"\0\xff\x1b\n\r"
        );
        assert!(read_finished_input(&b""[..]).unwrap().is_empty());
        let bytes = vec![0; rmux_proto::DEFAULT_MAX_DETACHED_FRAME_LENGTH + 1];
        assert!(read_finished_input(bytes.as_slice()).is_err());
    }
}
