use std::io;

use base64::Engine;
use sha1::{Digest, Sha1};
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const WEBSOCKET_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const CLIENT_FRAME_LIMIT: u64 = 4 * 1024;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WebSocketMessage {
    Text(String),
    Binary(Vec<u8>),
    Close,
}

pub(crate) struct WebSocket {
    stream: TcpStream,
}

impl WebSocket {
    pub(crate) async fn accept(mut stream: TcpStream, key: &str) -> io::Result<Self> {
        let accept = websocket_accept_key(key);
        let response = format!(
            "HTTP/1.1 101 Switching Protocols\r\n\
             Upgrade: websocket\r\n\
             Connection: Upgrade\r\n\
             Sec-WebSocket-Accept: {accept}\r\n\
             \r\n"
        );
        stream.write_all(response.as_bytes()).await?;
        Ok(Self { stream })
    }

    pub(crate) async fn read_message(&mut self) -> io::Result<WebSocketMessage> {
        loop {
            let frame = self.read_frame().await?;
            match frame.opcode {
                OPCODE_TEXT => {
                    let text = String::from_utf8(frame.payload).map_err(|error| {
                        io::Error::new(io::ErrorKind::InvalidData, error.to_string())
                    })?;
                    return Ok(WebSocketMessage::Text(text));
                }
                OPCODE_BINARY => return Ok(WebSocketMessage::Binary(frame.payload)),
                OPCODE_CLOSE => return Ok(WebSocketMessage::Close),
                OPCODE_PING => self.write_frame(OPCODE_PONG, &frame.payload).await?,
                OPCODE_PONG => {}
                _ => {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidData,
                        "unsupported websocket frame opcode",
                    ));
                }
            }
        }
    }

    pub(crate) async fn write_text(&mut self, text: &str) -> io::Result<()> {
        self.write_frame(OPCODE_TEXT, text.as_bytes()).await
    }

    pub(crate) async fn write_close(&mut self) -> io::Result<()> {
        self.write_frame(OPCODE_CLOSE, &[]).await
    }

    async fn read_frame(&mut self) -> io::Result<WebSocketFrame> {
        let mut head = [0u8; 2];
        self.stream.read_exact(&mut head).await?;
        let fin = head[0] & 0x80 != 0;
        if !fin {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "fragmented websocket frames are not supported",
            ));
        }
        let opcode = head[0] & 0x0f;
        let masked = head[1] & 0x80 != 0;
        if !masked {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "client websocket frames must be masked",
            ));
        }
        let mut len = u64::from(head[1] & 0x7f);
        if len == 126 {
            let mut bytes = [0u8; 2];
            self.stream.read_exact(&mut bytes).await?;
            len = u64::from(u16::from_be_bytes(bytes));
        } else if len == 127 {
            let mut bytes = [0u8; 8];
            self.stream.read_exact(&mut bytes).await?;
            len = u64::from_be_bytes(bytes);
        }
        if len > CLIENT_FRAME_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "websocket frame exceeds rmux web limit",
            ));
        }
        let mut mask = [0u8; 4];
        self.stream.read_exact(&mut mask).await?;
        let mut payload = vec![0u8; len as usize];
        self.stream.read_exact(&mut payload).await?;
        for (index, byte) in payload.iter_mut().enumerate() {
            *byte ^= mask[index % mask.len()];
        }
        Ok(WebSocketFrame { opcode, payload })
    }

    async fn write_frame(&mut self, opcode: u8, payload: &[u8]) -> io::Result<()> {
        let mut head = Vec::with_capacity(10);
        head.push(0x80 | opcode);
        if payload.len() < 126 {
            head.push(payload.len() as u8);
        } else if u16::try_from(payload.len()).is_ok() {
            head.push(126);
            head.extend_from_slice(&(payload.len() as u16).to_be_bytes());
        } else {
            head.push(127);
            head.extend_from_slice(&(payload.len() as u64).to_be_bytes());
        }
        self.stream.write_all(&head).await?;
        self.stream.write_all(payload).await
    }
}

#[derive(Debug)]
struct WebSocketFrame {
    opcode: u8,
    payload: Vec<u8>,
}

const OPCODE_TEXT: u8 = 0x1;
const OPCODE_BINARY: u8 = 0x2;
const OPCODE_CLOSE: u8 = 0x8;
const OPCODE_PING: u8 = 0x9;
const OPCODE_PONG: u8 = 0xA;

fn websocket_accept_key(key: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(WEBSOCKET_GUID.as_bytes());
    let digest = hasher.finalize();
    base64::engine::general_purpose::STANDARD.encode(digest)
}

#[cfg(test)]
mod tests {
    use super::websocket_accept_key;

    #[test]
    fn websocket_accept_key_matches_rfc_fixture() {
        assert_eq!(
            websocket_accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }
}
