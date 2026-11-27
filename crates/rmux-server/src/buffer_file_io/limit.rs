//! Size ceiling shared by every platform's buffer-file reader.
//!
//! tmux reads a `load-buffer` file in the client; RMUX reads it inside the
//! daemon. An endless source such as `/dev/zero` or a huge file must therefore
//! end as an error returned to the client, never as an allocation failure
//! that aborts the daemon and every session it serves.

use std::fs::File;
use std::io::{self, Read};

/// Largest file a single buffer read may load into the daemon (64 MiB).
///
/// tmux 3.7b has no explicit ceiling: it loads 64 MiB and 256 MiB files and
/// fails only when memory runs out. 64 MiB keeps every ordinary file loadable
/// while bounding the daemon's per-read allocation, which this module never
/// lets exceed the ceiling.
pub(crate) const MAX_BUFFER_FILE_BYTES: usize = 64 * 1024 * 1024;

const READ_CHUNK_BYTES: usize = 16 * 1024;

pub(super) fn too_large_error(limit: usize) -> io::Error {
    io::Error::new(
        io::ErrorKind::FileTooLarge,
        format!("buffer file exceeds the {limit}-byte limit"),
    )
}

/// Appends `chunk`, failing instead of growing `content` past `limit` bytes.
///
/// Capacity grows geometrically but is clamped to `limit`, and a refused
/// allocation is returned as an error instead of aborting the process.
pub(super) fn append_bounded(content: &mut Vec<u8>, chunk: &[u8], limit: usize) -> io::Result<()> {
    let remaining = limit.saturating_sub(content.len());
    if chunk.len() > remaining {
        return Err(too_large_error(limit));
    }
    if content.capacity() - content.len() < chunk.len() {
        let growth = content.len().max(chunk.len()).min(remaining);
        content
            .try_reserve_exact(growth)
            .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
    }
    content.extend_from_slice(chunk);
    Ok(())
}

/// Reads `reader` to EOF into `content`, refusing to hold more than `limit`.
pub(super) fn read_to_end_bounded(
    reader: &mut impl Read,
    content: &mut Vec<u8>,
    limit: usize,
) -> io::Result<()> {
    let mut chunk = [0_u8; READ_CHUNK_BYTES];
    loop {
        match reader.read(&mut chunk) {
            Ok(0) => return Ok(()),
            Ok(length) => append_bounded(content, &chunk[..length], limit)?,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

/// Reads an opened regular file. Its metadata rejects an oversized file before
/// any byte is read; the bounded read still covers a file that grows meanwhile.
pub(super) fn read_regular_file(mut file: File) -> io::Result<Vec<u8>> {
    let declared_length = file.metadata()?.len();
    if declared_length > MAX_BUFFER_FILE_BYTES as u64 {
        return Err(too_large_error(MAX_BUFFER_FILE_BYTES));
    }
    let mut content = Vec::new();
    content
        .try_reserve_exact(declared_length as usize)
        .map_err(|_| io::Error::from(io::ErrorKind::OutOfMemory))?;
    read_to_end_bounded(&mut file, &mut content, MAX_BUFFER_FILE_BYTES)?;
    Ok(content)
}

/// Asynchronous counterpart of [`read_to_end_bounded`], used by Windows named
/// pipes and exercised on every platform by the unit tests below.
#[cfg(any(windows, test))]
pub(super) async fn read_async_to_end_bounded(
    reader: &mut (impl tokio::io::AsyncRead + Unpin),
    limit: usize,
) -> io::Result<Vec<u8>> {
    use tokio::io::AsyncReadExt;

    let mut content = Vec::new();
    let mut chunk = [0_u8; READ_CHUNK_BYTES];
    loop {
        match reader.read(&mut chunk).await {
            Ok(0) => return Ok(content),
            Ok(length) => append_bounded(&mut content, &chunk[..length], limit)?,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => {}
            Err(error) => return Err(error),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SMALL_LIMIT: usize = 3 * READ_CHUNK_BYTES + 7;

    #[test]
    fn append_accepts_exactly_the_limit_and_never_allocates_past_it() {
        let mut content = Vec::new();
        let chunk = [1_u8; READ_CHUNK_BYTES];
        for _ in 0..3 {
            append_bounded(&mut content, &chunk, SMALL_LIMIT).expect("below limit");
        }
        append_bounded(&mut content, &[2_u8; 7], SMALL_LIMIT).expect("exactly at limit");
        assert_eq!(content.len(), SMALL_LIMIT);
        assert!(content.capacity() <= SMALL_LIMIT);

        let error = append_bounded(&mut content, &[3_u8], SMALL_LIMIT)
            .expect_err("one byte past the limit");
        assert_eq!(error.kind(), io::ErrorKind::FileTooLarge);
        assert_eq!(content.len(), SMALL_LIMIT, "a refused chunk is not stored");
    }

    #[test]
    fn endless_reader_is_refused_instead_of_exhausting_memory() {
        let mut content = Vec::new();
        let error = read_to_end_bounded(&mut io::repeat(0), &mut content, SMALL_LIMIT)
            .expect_err("an endless source must hit the limit");
        assert_eq!(error.kind(), io::ErrorKind::FileTooLarge);
        assert!(content.capacity() <= SMALL_LIMIT);
    }

    #[test]
    fn finite_reader_at_the_limit_is_read_whole() {
        let source = vec![9_u8; SMALL_LIMIT];
        let mut content = Vec::new();
        read_to_end_bounded(&mut source.as_slice(), &mut content, SMALL_LIMIT)
            .expect("a source of exactly the limit fits");
        assert_eq!(content, source);
    }

    // Portable stand-in for the Windows named-pipe path: an endless async
    // source must end in the same error as the Unix device path.
    #[tokio::test]
    async fn endless_async_reader_is_refused_instead_of_exhausting_memory() {
        let error = read_async_to_end_bounded(&mut tokio::io::repeat(0), SMALL_LIMIT)
            .await
            .expect_err("an endless async source must hit the limit");
        assert_eq!(error.kind(), io::ErrorKind::FileTooLarge);
    }

    #[tokio::test]
    async fn finite_async_reader_at_the_limit_is_read_whole() {
        let source = vec![5_u8; SMALL_LIMIT];
        let content = read_async_to_end_bounded(&mut source.as_slice(), SMALL_LIMIT)
            .await
            .expect("an async source of exactly the limit fits");
        assert_eq!(content, source);
    }
}
