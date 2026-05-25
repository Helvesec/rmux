use std::io;

use rmux_ipc::{is_peer_disconnect, LocalStream};
use rmux_proto::AttachFrameDecoder;
#[cfg(feature = "web")]
use tokio::io::DuplexStream;
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt, ReadHalf, WriteHalf};
use tokio::sync::{mpsc, Mutex};
use tokio::task::JoinHandle;

const ATTACH_READ_BUFFER_SIZE: usize = 8192;
const ATTACH_CHANNEL_CAPACITY: usize = 32;
#[cfg(feature = "web")]
const IN_PROCESS_ATTACH_BUFFER_SIZE: usize = 64 * 1024;

pub(crate) struct AttachTransport {
    reader: Mutex<mpsc::Receiver<AttachReadEvent>>,
    writer: Mutex<Box<dyn AsyncWrite + Send + Unpin>>,
    read_task: JoinHandle<()>,
}

pub(super) enum TryAttachRead {
    Read,
    Closed,
    WouldBlock,
}

enum AttachReadEvent {
    Bytes(Vec<u8>),
    Closed,
    Error(io::Error),
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
        match self.next_event().await? {
            AttachReadEvent::Bytes(bytes) => {
                decoder.push_bytes(&bytes);
                Ok(true)
            }
            AttachReadEvent::Closed => Ok(false),
            AttachReadEvent::Error(error) => Err(error),
        }
    }

    pub(super) fn try_read_into(
        &self,
        decoder: &mut AttachFrameDecoder,
    ) -> io::Result<TryAttachRead> {
        let Ok(mut reader) = self.reader.try_lock() else {
            return Ok(TryAttachRead::WouldBlock);
        };
        match reader.try_recv() {
            Ok(AttachReadEvent::Bytes(bytes)) => {
                decoder.push_bytes(&bytes);
                Ok(TryAttachRead::Read)
            }
            Ok(AttachReadEvent::Closed) => Ok(TryAttachRead::Closed),
            Ok(AttachReadEvent::Error(error)) => Err(error),
            Err(mpsc::error::TryRecvError::Empty) => Ok(TryAttachRead::WouldBlock),
            Err(mpsc::error::TryRecvError::Disconnected) => Ok(TryAttachRead::Closed),
        }
    }

    pub(super) async fn write_all(&self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() {
            return Ok(());
        }
        let mut writer = self.writer.lock().await;
        match writer.write_all(bytes).await {
            Ok(()) => writer.flush().await,
            Err(error) if is_peer_disconnect(&error) => Ok(()),
            Err(error) => Err(error),
        }
    }

    fn from_split<T>(reader: ReadHalf<T>, writer: WriteHalf<T>) -> Self
    where
        T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
    {
        let (tx, rx) = mpsc::channel(ATTACH_CHANNEL_CAPACITY);
        let read_task = tokio::spawn(read_loop(reader, tx));
        Self {
            reader: Mutex::new(rx),
            writer: Mutex::new(Box::new(writer)),
            read_task,
        }
    }

    async fn next_event(&self) -> io::Result<AttachReadEvent> {
        let mut reader = self.reader.lock().await;
        reader.recv().await.ok_or_else(|| {
            io::Error::new(
                io::ErrorKind::UnexpectedEof,
                "attach transport read task stopped",
            )
        })
    }
}

impl Drop for AttachTransport {
    fn drop(&mut self) {
        self.read_task.abort();
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

async fn read_loop<T>(mut reader: ReadHalf<T>, tx: mpsc::Sender<AttachReadEvent>)
where
    T: AsyncRead + AsyncWrite + Send + Unpin + 'static,
{
    let mut buffer = [0_u8; ATTACH_READ_BUFFER_SIZE];
    loop {
        let event = match reader.read(&mut buffer).await {
            Ok(0) => AttachReadEvent::Closed,
            Ok(bytes_read) => AttachReadEvent::Bytes(buffer[..bytes_read].to_vec()),
            Err(error) => AttachReadEvent::Error(error),
        };
        let closed = matches!(event, AttachReadEvent::Closed | AttachReadEvent::Error(_));
        if tx.send(event).await.is_err() || closed {
            break;
        }
    }
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
