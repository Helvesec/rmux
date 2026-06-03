use std::io;

use base64::Engine;
use sha1::{Digest, Sha1};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};
use tokio::net::tcp::{OwnedReadHalf, OwnedWriteHalf};
use tokio::net::TcpStream;
use tokio::time::{timeout, Duration};

const WEBSOCKET_GUID: &str = "258EAFA5-E914-47DA-95CA-C5AB0DC85B11";
const CLIENT_FRAME_LIMIT: u64 = 8 * 1024;
const FRAME_CONTINUATION_TIMEOUT: Duration = Duration::from_secs(5);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum WebSocketMessage {
    Text(String),
    Binary(Vec<u8>),
    Ping(Vec<u8>),
    Pong,
    Close,
}

pub(crate) struct WebSocket {
    stream: TcpStream,
}

pub(crate) struct WebSocketReader {
    stream: OwnedReadHalf,
}

pub(crate) struct WebSocketWriter {
    stream: OwnedWriteHalf,
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
            let frame = read_frame(&mut self.stream).await?;
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

    pub(crate) fn split(self) -> (WebSocketReader, WebSocketWriter) {
        let (reader, writer) = self.stream.into_split();
        (
            WebSocketReader { stream: reader },
            WebSocketWriter { stream: writer },
        )
    }

    pub(crate) async fn write_text(&mut self, text: &str) -> io::Result<()> {
        self.write_frame(OPCODE_TEXT, text.as_bytes()).await
    }

    pub(crate) async fn write_close_code(&mut self, code: u16, reason: &str) -> io::Result<()> {
        let reason = reason.as_bytes();
        let reason = &reason[..reason.len().min(123)];
        let mut payload = Vec::with_capacity(2 + reason.len());
        payload.extend_from_slice(&code.to_be_bytes());
        payload.extend_from_slice(reason);
        self.write_frame(OPCODE_CLOSE, &payload).await
    }

    async fn write_frame(&mut self, opcode: u8, payload: &[u8]) -> io::Result<()> {
        write_frame(&mut self.stream, opcode, payload).await
    }
}

impl WebSocketReader {
    pub(crate) async fn read_message(&mut self) -> io::Result<WebSocketMessage> {
        let frame = read_frame(&mut self.stream).await?;
        match frame.opcode {
            OPCODE_TEXT => {
                let text = String::from_utf8(frame.payload).map_err(|error| {
                    io::Error::new(io::ErrorKind::InvalidData, error.to_string())
                })?;
                Ok(WebSocketMessage::Text(text))
            }
            OPCODE_BINARY => Ok(WebSocketMessage::Binary(frame.payload)),
            OPCODE_CLOSE => Ok(WebSocketMessage::Close),
            OPCODE_PING => Ok(WebSocketMessage::Ping(frame.payload)),
            OPCODE_PONG => Ok(WebSocketMessage::Pong),
            _ => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "unsupported websocket frame opcode",
            )),
        }
    }
}

impl WebSocketWriter {
    pub(crate) async fn write_binary(&mut self, payload: &[u8]) -> io::Result<()> {
        self.write_frame(OPCODE_BINARY, payload).await
    }

    pub(crate) async fn write_close(&mut self) -> io::Result<()> {
        self.write_frame(OPCODE_CLOSE, &[]).await
    }

    pub(crate) async fn write_close_code(&mut self, code: u16, reason: &str) -> io::Result<()> {
        let reason = reason.as_bytes();
        let reason = &reason[..reason.len().min(123)];
        let mut payload = Vec::with_capacity(2 + reason.len());
        payload.extend_from_slice(&code.to_be_bytes());
        payload.extend_from_slice(reason);
        self.write_frame(OPCODE_CLOSE, &payload).await
    }

    pub(crate) async fn write_pong(&mut self, payload: &[u8]) -> io::Result<()> {
        self.write_frame(OPCODE_PONG, payload).await
    }

    async fn write_frame(&mut self, opcode: u8, payload: &[u8]) -> io::Result<()> {
        write_frame(&mut self.stream, opcode, payload).await
    }
}

#[derive(Debug)]
struct WebSocketFrame {
    opcode: u8,
    payload: Vec<u8>,
}

async fn read_frame(stream: &mut (impl AsyncRead + Unpin)) -> io::Result<WebSocketFrame> {
    read_frame_with_continuation_timeout(stream, FRAME_CONTINUATION_TIMEOUT).await
}

async fn read_frame_with_continuation_timeout(
    stream: &mut (impl AsyncRead + Unpin),
    continuation_timeout: Duration,
) -> io::Result<WebSocketFrame> {
    let mut head = [0u8; 2];
    read_frame_continuation(stream, &mut head[..1], continuation_timeout).await?;
    read_frame_continuation(stream, &mut head[1..], continuation_timeout).await?;
    let fin = head[0] & 0x80 != 0;
    if head[0] & 0x70 != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "websocket extensions are not negotiated",
        ));
    }
    if !fin {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "fragmented websocket frames are not supported",
        ));
    }
    let opcode = head[0] & 0x0f;
    if !valid_client_opcode(opcode) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "unsupported websocket frame opcode",
        ));
    }
    let masked = head[1] & 0x80 != 0;
    if !masked {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "client websocket frames must be masked",
        ));
    }
    let mut len = u64::from(head[1] & 0x7f);
    if is_control_opcode(opcode) && len >= 126 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "websocket control frame payload is too long",
        ));
    }
    if len == 126 {
        let mut bytes = [0u8; 2];
        read_frame_continuation(stream, &mut bytes, continuation_timeout).await?;
        len = u64::from(u16::from_be_bytes(bytes));
    } else if len == 127 {
        let mut bytes = [0u8; 8];
        read_frame_continuation(stream, &mut bytes, continuation_timeout).await?;
        len = u64::from_be_bytes(bytes);
    }
    if len > CLIENT_FRAME_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "websocket frame exceeds rmux web limit",
        ));
    }
    if opcode == OPCODE_CLOSE && len == 1 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "websocket close frame payload is invalid",
        ));
    }
    let mut mask = [0u8; 4];
    read_frame_continuation(stream, &mut mask, continuation_timeout).await?;
    let mut payload = vec![0u8; len as usize];
    read_frame_continuation(stream, &mut payload, continuation_timeout).await?;
    for (index, byte) in payload.iter_mut().enumerate() {
        *byte ^= mask[index % mask.len()];
    }
    Ok(WebSocketFrame { opcode, payload })
}

async fn read_frame_continuation(
    stream: &mut (impl AsyncRead + Unpin),
    buffer: &mut [u8],
    continuation_timeout: Duration,
) -> io::Result<()> {
    match timeout(continuation_timeout, stream.read_exact(buffer)).await {
        Ok(Ok(_)) => Ok(()),
        Ok(Err(error)) => Err(error),
        Err(_) => Err(io::Error::new(
            io::ErrorKind::TimedOut,
            "websocket frame read timed out",
        )),
    }
}

async fn write_frame(
    stream: &mut (impl AsyncWrite + Unpin),
    opcode: u8,
    payload: &[u8],
) -> io::Result<()> {
    let mut frame = Vec::with_capacity(10 + payload.len());
    frame.push(0x80 | opcode);
    if payload.len() < 126 {
        frame.push(payload.len() as u8);
    } else if u16::try_from(payload.len()).is_ok() {
        frame.push(126);
        frame.extend_from_slice(&(payload.len() as u16).to_be_bytes());
    } else {
        frame.push(127);
        frame.extend_from_slice(&(payload.len() as u64).to_be_bytes());
    }
    frame.extend_from_slice(payload);
    stream.write_all(&frame).await
}

const OPCODE_TEXT: u8 = 0x1;
const OPCODE_BINARY: u8 = 0x2;
const OPCODE_CLOSE: u8 = 0x8;
const OPCODE_PING: u8 = 0x9;
const OPCODE_PONG: u8 = 0xA;

fn valid_client_opcode(opcode: u8) -> bool {
    matches!(
        opcode,
        OPCODE_TEXT | OPCODE_BINARY | OPCODE_CLOSE | OPCODE_PING | OPCODE_PONG
    )
}

fn is_control_opcode(opcode: u8) -> bool {
    opcode & 0x08 != 0
}

fn websocket_accept_key(key: &str) -> String {
    let mut hasher = Sha1::new();
    hasher.update(key.as_bytes());
    hasher.update(WEBSOCKET_GUID.as_bytes());
    let digest = hasher.finalize();
    base64::engine::general_purpose::STANDARD.encode(digest)
}

pub(crate) fn valid_client_key(key: &str) -> bool {
    let Ok(decoded) = base64::engine::general_purpose::STANDARD.decode(key) else {
        return false;
    };
    decoded.len() == 16
}

#[cfg(feature = "fuzzing")]
pub(crate) fn fuzz_client_frame(data: &[u8]) {
    let mut cursor = std::io::Cursor::new(data);
    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("fuzz runtime builds");
    let _ = runtime.block_on(read_frame(&mut cursor));
}

#[cfg(test)]
mod tests {
    use super::{
        read_frame, read_frame_with_continuation_timeout, valid_client_key, websocket_accept_key,
        OPCODE_CLOSE, OPCODE_PING, OPCODE_TEXT,
    };
    use std::time::Duration;
    use tokio::io::AsyncWriteExt;

    fn masked_frame(first: u8, payload: &[u8]) -> Vec<u8> {
        let mask = [0x11, 0x22, 0x33, 0x44];
        let mut frame = Vec::with_capacity(2 + mask.len() + payload.len());
        frame.push(first);
        frame.push(0x80 | u8::try_from(payload.len()).expect("short test payload"));
        frame.extend_from_slice(&mask);
        for (index, byte) in payload.iter().enumerate() {
            frame.push(byte ^ mask[index % mask.len()]);
        }
        frame
    }

    #[test]
    fn websocket_accept_key_matches_rfc_fixture() {
        assert_eq!(
            websocket_accept_key("dGhlIHNhbXBsZSBub25jZQ=="),
            "s3pPLMBiTxaQ9kYGzzhZRbK+xOo="
        );
    }

    #[test]
    fn websocket_key_must_decode_to_sixteen_bytes() {
        assert!(valid_client_key("dGhlIHNhbXBsZSBub25jZQ=="));
        assert!(!valid_client_key("not-base64"));
        assert!(!valid_client_key("Zm9v"));
    }

    #[tokio::test]
    async fn client_frames_reject_rsv_bits_without_extensions() {
        let mut frame = std::io::Cursor::new(masked_frame(0x80 | 0x40 | OPCODE_TEXT, b"hi"));
        let error = read_frame(&mut frame)
            .await
            .expect_err("RSV bits must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "websocket extensions are not negotiated");
    }

    #[tokio::test]
    async fn client_frames_reject_reserved_opcodes_before_payload() {
        let mut frame = std::io::Cursor::new(masked_frame(0x80 | 0x3, b"hi"));
        let error = read_frame(&mut frame)
            .await
            .expect_err("reserved opcode must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(error.to_string(), "unsupported websocket frame opcode");
    }

    #[tokio::test]
    async fn control_frames_reject_extended_lengths() {
        let mut frame = std::io::Cursor::new(vec![0x80 | OPCODE_PING, 0x80 | 126]);
        let error = read_frame(&mut frame)
            .await
            .expect_err("extended control lengths must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            error.to_string(),
            "websocket control frame payload is too long"
        );
    }

    #[tokio::test]
    async fn close_frames_reject_single_byte_payloads() {
        let mut frame = std::io::Cursor::new(masked_frame(0x80 | OPCODE_CLOSE, &[0]));
        let error = read_frame(&mut frame)
            .await
            .expect_err("single-byte close payload must be rejected");
        assert_eq!(error.kind(), std::io::ErrorKind::InvalidData);
        assert_eq!(
            error.to_string(),
            "websocket close frame payload is invalid"
        );
    }

    #[tokio::test]
    async fn frame_continuation_times_out_after_first_byte() {
        let (mut client, mut server) = tokio::io::duplex(8);
        client
            .write_all(&[0x80 | OPCODE_TEXT])
            .await
            .expect("write first byte");

        let error = read_frame_with_continuation_timeout(&mut server, Duration::from_millis(1))
            .await
            .expect_err("partial frame must time out");

        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    }

    #[tokio::test]
    async fn frame_read_times_out_before_first_byte() {
        let (_client, mut server) = tokio::io::duplex(8);

        let error = read_frame_with_continuation_timeout(&mut server, Duration::from_millis(1))
            .await
            .expect_err("idle client must time out before first frame byte");

        assert_eq!(error.kind(), std::io::ErrorKind::TimedOut);
    }
}
