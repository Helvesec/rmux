//! Size policy is separate from the shared cancellable file transport.
use std::fs::File;
use std::io::{self, Read};

use super::limit;

#[derive(Clone, Copy, Debug)]
pub(super) enum ReadLimit {
    Buffer,
    Unlimited,
}

impl ReadLimit {
    #[cfg(unix)]
    pub(super) fn append(self, content: &mut Vec<u8>, chunk: &[u8]) -> io::Result<()> {
        match self {
            Self::Buffer => limit::append_bounded(content, chunk, limit::MAX_BUFFER_FILE_BYTES),
            Self::Unlimited => {
                content
                    .try_reserve(chunk.len())
                    .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
                content.extend_from_slice(chunk);
                Ok(())
            }
        }
    }

    pub(super) fn read_to_end(
        self,
        reader: &mut impl Read,
        content: &mut Vec<u8>,
    ) -> io::Result<()> {
        match self {
            Self::Buffer => {
                limit::read_to_end_bounded(reader, content, limit::MAX_BUFFER_FILE_BYTES)
            }
            Self::Unlimited => reader.read_to_end(content).map(|_| ()),
        }
    }

    pub(super) fn read_regular_file(self, mut file: File) -> io::Result<Vec<u8>> {
        match self {
            Self::Buffer => limit::read_regular_file(file),
            Self::Unlimited => {
                let mut content = Vec::new();
                self.read_to_end(&mut file, &mut content)?;
                Ok(content)
            }
        }
    }

    #[cfg(windows)]
    pub(super) async fn read_async_to_end(
        self,
        reader: &mut (impl tokio::io::AsyncRead + Unpin),
    ) -> io::Result<Vec<u8>> {
        use tokio::io::AsyncReadExt;

        match self {
            Self::Buffer => {
                limit::read_async_to_end_bounded(reader, limit::MAX_BUFFER_FILE_BYTES).await
            }
            Self::Unlimited => {
                let mut content = Vec::new();
                reader.read_to_end(&mut content).await?;
                Ok(content)
            }
        }
    }
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;

    #[test]
    fn snapshot_append_crosses_the_buffer_limit_without_relaxing_buffers() {
        let mut content = vec![b' '; limit::MAX_BUFFER_FILE_BYTES];
        let error = ReadLimit::Buffer
            .append(&mut content, b" ")
            .expect_err("buffers must refuse the extra byte");
        assert_eq!(error.kind(), io::ErrorKind::FileTooLarge);
        assert_eq!(content.len(), limit::MAX_BUFFER_FILE_BYTES);

        ReadLimit::Unlimited
            .append(&mut content, b" ")
            .expect("snapshots have no buffer ceiling");
        assert_eq!(content.len(), limit::MAX_BUFFER_FILE_BYTES + 1);
    }
}
