use std::io::{self, Read};
use std::thread;

use tokio::sync::mpsc;
use tracing::debug;

use super::preset::ProcessOutput;

const LINE_CHANNEL_CAPACITY: usize = 64;
const MAX_PROVIDER_LINE_BYTES: usize = 16 * 1024;
const PROVIDER_READ_CHUNK_BYTES: usize = 4 * 1024;

type ProviderLine = (ProcessOutput, String);

pub(super) fn channel() -> (mpsc::Sender<ProviderLine>, mpsc::Receiver<ProviderLine>) {
    mpsc::channel(LINE_CHANNEL_CAPACITY)
}

pub(super) fn spawn_reader<R>(
    name: &'static str,
    reader: R,
    tx: mpsc::Sender<ProviderLine>,
    source: ProcessOutput,
) where
    R: Read + Send + 'static,
{
    let _ = thread::Builder::new().name(name.to_owned()).spawn(move || {
        read_lines(source, reader, tx);
    });
}

fn read_lines<R>(source: ProcessOutput, reader: R, tx: mpsc::Sender<ProviderLine>)
where
    R: Read,
{
    let mut reader = io::BufReader::new(reader);
    let mut chunk = [0_u8; PROVIDER_READ_CHUNK_BYTES];
    let mut line = Vec::with_capacity(MAX_PROVIDER_LINE_BYTES);
    let mut discarding_overflow = false;

    loop {
        let read = match reader.read(&mut chunk) {
            Ok(0) => {
                if !discarding_overflow && !line.is_empty() {
                    let _ = send_line(source, &mut line, false, &tx);
                }
                return;
            }
            Ok(read) => read,
            Err(error) => {
                debug!("web-share tunnel output read failed: {error}");
                return;
            }
        };

        for &byte in &chunk[..read] {
            if byte == b'\n' {
                if !discarding_overflow && !send_line(source, &mut line, true, &tx) {
                    return;
                }
                line.clear();
                discarding_overflow = false;
                continue;
            }
            if discarding_overflow {
                continue;
            }
            if line.len() == MAX_PROVIDER_LINE_BYTES {
                debug!(
                    limit = MAX_PROVIDER_LINE_BYTES,
                    "web-share tunnel output line truncated"
                );
                if !send_line(source, &mut line, false, &tx) {
                    return;
                }
                line.clear();
                discarding_overflow = true;
                continue;
            }
            line.push(byte);
        }
    }
}

fn send_line(
    source: ProcessOutput,
    line: &mut Vec<u8>,
    strip_carriage_return: bool,
    tx: &mpsc::Sender<ProviderLine>,
) -> bool {
    if strip_carriage_return && line.last() == Some(&b'\r') {
        line.pop();
    }
    let line = String::from_utf8_lossy(line).into_owned();
    tx.blocking_send((source, line)).is_ok()
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use tokio::sync::mpsc;

    use super::{read_lines, MAX_PROVIDER_LINE_BYTES};
    use crate::web::tunnel::preset::ProcessOutput;

    #[test]
    fn reader_bounds_overlong_lines_and_resumes() {
        let mut output = vec![b'a'; MAX_PROVIDER_LINE_BYTES + 1];
        output.extend_from_slice(b"\nnext\r\n");
        let (tx, mut rx) = mpsc::channel(4);

        read_lines(ProcessOutput::Stdout, Cursor::new(output), tx);

        let (_, bounded) = rx.blocking_recv().expect("bounded prefix is forwarded");
        let (_, next) = rx.blocking_recv().expect("next line is forwarded");
        assert_eq!(bounded.len(), MAX_PROVIDER_LINE_BYTES);
        assert!(bounded.bytes().all(|byte| byte == b'a'));
        assert_eq!(next, "next");
        assert!(rx.blocking_recv().is_none());
    }

    #[test]
    fn reader_forwards_final_unterminated_line() {
        let (tx, mut rx) = mpsc::channel(2);

        read_lines(ProcessOutput::Stderr, Cursor::new(b"last line"), tx);

        let (_, line) = rx.blocking_recv().expect("unterminated line is forwarded");
        assert_eq!(line, "last line");
        assert!(rx.blocking_recv().is_none());
    }
}
