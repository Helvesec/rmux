use std::collections::HashMap;
use std::io;

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpStream;

const HTTP_READ_LIMIT: usize = 8 * 1024;

pub(super) async fn read_http_request(stream: &mut TcpStream) -> io::Result<HttpRequest> {
    let mut buffer = Vec::new();
    loop {
        let mut chunk = [0u8; 1024];
        let read = stream.read(&mut chunk).await?;
        if read == 0 {
            return Err(io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "connection closed before request",
            ));
        }
        buffer.extend_from_slice(&chunk[..read]);
        if buffer.len() > HTTP_READ_LIMIT {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "HTTP request headers exceed rmux web limit",
            ));
        }
        if buffer.windows(4).any(|window| window == b"\r\n\r\n") {
            break;
        }
    }
    parse_http_request(&buffer)
}

pub(super) async fn write_response(
    stream: &mut TcpStream,
    status: u16,
    content_type: &str,
    body: &[u8],
    include_body: bool,
) -> io::Result<()> {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        431 => "Request Header Fields Too Large",
        503 => "Service Unavailable",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\n\
         Content-Type: {content_type}\r\n\
         Content-Length: {}\r\n\
         Cache-Control: no-store\r\n\
         X-Content-Type-Options: nosniff\r\n\
         Connection: close\r\n\
         \r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    if include_body {
        stream.write_all(body).await?;
    }
    Ok(())
}

#[derive(Debug)]
pub(super) struct HttpRequest {
    pub(super) method: String,
    pub(super) path: String,
    pub(super) headers: HashMap<String, String>,
}

impl HttpRequest {
    pub(super) fn is_websocket_upgrade(&self) -> bool {
        self.headers
            .get("upgrade")
            .is_some_and(|value| value.eq_ignore_ascii_case("websocket"))
            && self
                .headers
                .get("connection")
                .is_some_and(|value| has_header_token(value, "upgrade"))
    }
}

fn parse_http_request(buffer: &[u8]) -> io::Result<HttpRequest> {
    let mut headers = [httparse::EMPTY_HEADER; 64];
    let mut request = httparse::Request::new(&mut headers);
    let status = request
        .parse(buffer)
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidData, error.to_string()))?;
    if !status.is_complete() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "incomplete HTTP request",
        ));
    }
    let method = request.method.unwrap_or_default().to_owned();
    let target = request.path.unwrap_or_default();
    let path = path_from_target(target);
    let headers = request
        .headers
        .iter()
        .map(|header| {
            let value = String::from_utf8_lossy(header.value).trim().to_owned();
            (header.name.to_ascii_lowercase(), value)
        })
        .collect();
    Ok(HttpRequest {
        method,
        path: path.to_owned(),
        headers,
    })
}

pub(super) fn path_from_target(target: &str) -> &str {
    target.split_once('?').map_or(target, |(path, _)| path)
}

fn has_header_token(value: &str, expected: &str) -> bool {
    value
        .split(',')
        .any(|token| token.trim().eq_ignore_ascii_case(expected))
}
