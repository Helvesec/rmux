use std::io;
use std::pin::Pin;
use std::task::{Context, Poll, Waker};

use rmux_ipc::{is_peer_disconnect, LocalStream};
use rmux_proto::AttachFrameDecoder;
#[cfg(feature = "web")]
use tokio::io::DuplexStream;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadBuf, ReadHalf, WriteHalf};
use tokio::sync::Mutex;

const ATTACH_READ_BUFFER_SIZE: usize = 8192;
#[cfg(feature = "web")]
const IN_PROCESS_ATTACH_BUFFER_SIZE: usize = 64 * 1024;

pub(crate) struct AttachTransport {
    reader: Mutex<Box<dyn AsyncRead + Send + Unpin>>,
    writer: Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
}

pub(super) enum TryAttachRead {
    Read,
    Closed,
    WouldBlock,
}

impl AttachTransport {
    pub(super) fn from_io<T>(stream: T) -> Self
    where
        T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (reader, writer) = tokio::io::split(stream);
        Self::from_split(reader, writer)
    }

    pub(super) async fn read_into(&self, decoder: &mut AttachFrameDecoder) -> io::Result<bool> {
        let mut buffer = [0_u8; ATTACH_READ_BUFFER_SIZE];
        let mut reader = self.reader.lock().await;
        match reader.read(&mut buffer).await {
            Ok(0) => Ok(false),
            Ok(bytes_read) => {
                decoder.push_bytes(&buffer[..bytes_read]);
                Ok(true)
            }
            Err(error) => Err(error),
        }
    }

    pub(super) fn try_read_into(
        &self,
        decoder: &mut AttachFrameDecoder,
    ) -> io::Result<TryAttachRead> {
        let Ok(mut reader) = self.reader.try_lock() else {
            return Ok(TryAttachRead::WouldBlock);
        };
        let mut buffer = [0_u8; ATTACH_READ_BUFFER_SIZE];
        let mut read_buffer = ReadBuf::new(&mut buffer);
        let waker = Waker::noop();
        let mut context = Context::from_waker(waker);
        match Pin::new(&mut **reader).poll_read(&mut context, &mut read_buffer) {
            Poll::Ready(Ok(())) if read_buffer.filled().is_empty() => Ok(TryAttachRead::Closed),
            Poll::Ready(Ok(())) => {
                decoder.push_bytes(read_buffer.filled());
                Ok(TryAttachRead::Read)
            }
            Poll::Ready(Err(error)) => Err(error),
            Poll::Pending => Ok(TryAttachRead::WouldBlock),
        }
    }

    pub(super) async fn write_all(&self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let mut writer = self.writer.lock().await;
        match writer.write_all(bytes).await {
            Ok(()) => Ok(()),
            Err(error) if is_peer_disconnect(&error) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn from_split<T>(reader: ReadHalf<T>, writer: WriteHalf<T>) -> Self
    where
        T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        Self {
            reader: Mutex::new(Box::new(reader)),
            writer: Mutex::new(Box::new(writer)),
        }
    }
}

impl From<LocalStream> for AttachTransport {
    fn from(stream: LocalStream) -> Self {
        Self::from_io(stream)
    }
}

#[cfg(feature = "web")]
pub(crate) fn in_process_attach_pair() -> (AttachTransport, DuplexStream) {
    let (client, server) = tokio::io::duplex(IN_PROCESS_ATTACH_BUFFER_SIZE);
    (AttachTransport::from_io(server), client)
}

#[cfg(all(test, feature = "web"))]
mod tests {
    use rmux_proto::{encode_attach_message, AttachFrameDecoder, AttachMessage};

    use super::{in_process_attach_pair, TryAttachRead};

    #[tokio::test]
    async fn in_process_transport_reads_attach_frames() {
        let (transport, mut client) = in_process_attach_pair();
        let frame =
            encode_attach_message(&AttachMessage::Data(b"hello".to_vec())).expect("frame encodes");
        tokio::io::AsyncWriteExt::write_all(&mut client, &frame)
            .await
            .expect("client writes frame");

        let mut decoder = AttachFrameDecoder::new();
        assert!(transport
            .read_into(&mut decoder)
            .await
            .expect("transport reads"));
        assert_eq!(
            decoder.next_message().expect("frame decodes"),
            Some(AttachMessage::Data(b"hello".to_vec()))
        );
    }

    #[tokio::test]
    async fn empty_in_process_transport_try_read_would_block() {
        let (transport, _client) = in_process_attach_pair();
        let mut decoder = AttachFrameDecoder::new();
        assert!(matches!(
            transport
                .try_read_into(&mut decoder)
                .expect("try read succeeds"),
            TryAttachRead::WouldBlock
        ));
    }
}
