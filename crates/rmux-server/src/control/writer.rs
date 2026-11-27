//! Optional, explicitly negotiated `stream.control.records.v1` output.
//! Input remains control text. Output records are tag 0 + u32 little-endian
//! byte count + text (at most 8192 bytes), or terminal tag 1 + i32 exit status.
//! The legacy upgrade and detached wire v9 retain their existing meanings.

use rmux_ipc::LocalStream;
use std::{
    io,
    pin::Pin,
    task::{ready, Context, Poll},
};
use tokio::io::{AsyncWrite, AsyncWriteExt, WriteHalf};

pub(super) struct ControlWriter {
    inner: WriteHalf<LocalStream>,
    records: bool,
    pending: Vec<u8>,
    offset: usize,
    finished: bool,
}

impl ControlWriter {
    pub(super) fn new(inner: WriteHalf<LocalStream>, records: bool) -> Self {
        Self {
            inner,
            records,
            pending: Vec::new(),
            offset: 0,
            finished: false,
        }
    }

    pub(super) fn has_typed_status(&self) -> bool {
        self.records
    }

    pub(super) async fn finish(&mut self, status: i32) -> io::Result<()> {
        if self.records && !self.finished {
            self.flush().await?;
            let mut record = vec![1];
            record.extend_from_slice(&status.to_le_bytes());
            super::output_queue::write_all_bounded(&mut self.inner, &record).await?;
            self.inner.flush().await?;
            self.finished = true;
        }
        Ok(())
    }

    fn drain(&mut self, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        while self.offset < self.pending.len() {
            let n = ready!(Pin::new(&mut self.inner).poll_write(cx, &self.pending[self.offset..]))?;
            if n == 0 {
                return Poll::Ready(Err(io::ErrorKind::WriteZero.into()));
            }
            self.offset += n;
        }
        self.pending.clear();
        self.offset = 0;
        Poll::Ready(Ok(()))
    }
}

impl AsyncWrite for ControlWriter {
    fn poll_write(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        if !self.records {
            return Pin::new(&mut self.inner).poll_write(cx, bytes);
        }
        ready!(self.drain(cx))?;
        if bytes.is_empty() {
            return Pin::new(&mut self.inner).poll_write(cx, bytes);
        }
        if self.finished {
            return Poll::Ready(Err(io::ErrorKind::BrokenPipe.into()));
        }
        let n = bytes.len().min(8192);
        self.pending.push(0);
        self.pending.extend_from_slice(&(n as u32).to_le_bytes());
        self.pending.extend_from_slice(&bytes[..n]);
        Poll::Ready(Ok(n))
    }

    fn poll_flush(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        ready!(self.drain(cx))?;
        Pin::new(&mut self.inner).poll_flush(cx)
    }

    fn poll_shutdown(mut self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<io::Result<()>> {
        ready!(self.drain(cx))?;
        Pin::new(&mut self.inner).poll_shutdown(cx)
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use tokio::io::AsyncReadExt;

    #[tokio::test]
    async fn control_records_bound_chunks_and_finish_after_all_text() {
        let (server, mut peer) = tokio::net::UnixStream::pair().unwrap();
        let (reader, writer) = tokio::io::split(server);
        drop(reader);
        let task = tokio::spawn(async move {
            let mut writer = ControlWriter::new(writer, true);
            writer.write_all(&vec![b'x'; 100_000]).await.unwrap();
            writer.write_all(b"%exit\n").await.unwrap();
            writer.finish(7).await.unwrap();
            writer.finish(1).await.unwrap();
        });
        let mut wire = Vec::new();
        peer.read_to_end(&mut wire).await.unwrap();
        task.await.unwrap();
        let mut cursor = wire.as_slice();
        let mut text = Vec::new();
        while cursor[0] == 0 {
            let n = u32::from_le_bytes(cursor[1..5].try_into().unwrap()) as usize;
            assert!((1..=8192).contains(&n));
            text.extend_from_slice(&cursor[5..5 + n]);
            cursor = &cursor[5 + n..];
        }
        assert_eq!(cursor, [1, 7, 0, 0, 0]);
        assert_eq!(text, [vec![b'x'; 100_000], b"%exit\n".to_vec()].concat());
    }
}
