use std::io;
use std::path::{Path, PathBuf};

use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::windows::named_pipe::ClientOptions;

#[derive(Clone, Copy, Debug)]
pub(super) struct SpecialFileKind;

pub(super) fn special_path(path: &Path) -> io::Result<Option<SpecialFileKind>> {
    let normalized = path.as_os_str().to_string_lossy().replace('/', "\\");
    let Some(unc_path) = normalized.strip_prefix("\\\\") else {
        return Ok(None);
    };
    let mut components = unc_path.split('\\');
    let _server = components.next();
    Ok(components
        .next()
        .is_some_and(|component| component.eq_ignore_ascii_case("pipe"))
        .then_some(SpecialFileKind))
}

pub(super) async fn read(path: PathBuf, _kind: SpecialFileKind) -> io::Result<Vec<u8>> {
    let mut options = ClientOptions::new();
    options.read(true).write(false);
    let mut pipe = options.open(path)?;
    let mut content = Vec::new();
    pipe.read_to_end(&mut content).await?;
    Ok(content)
}

pub(super) async fn write(
    path: PathBuf,
    content: Vec<u8>,
    _append: bool,
    _kind: SpecialFileKind,
) -> io::Result<()> {
    let mut options = ClientOptions::new();
    options.read(false).write(true);
    let mut pipe = options.open(path)?;
    pipe.write_all(&content).await
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::net::windows::named_pipe::ServerOptions;

    #[test]
    fn named_pipe_paths_are_recognized_without_matching_regular_paths() {
        assert!(special_path(Path::new(r"\\.\pipe\rmux-buffer"))
            .expect("classify local pipe")
            .is_some());
        assert!(special_path(Path::new(r"\\server\pipe\rmux-buffer"))
            .expect("classify remote pipe")
            .is_some());
        assert!(special_path(Path::new(r"C:\tmp\pipe\rmux-buffer"))
            .expect("classify regular path")
            .is_none());
    }

    #[tokio::test]
    async fn blocked_named_pipe_read_is_async_and_cancellable() -> io::Result<()> {
        let path = format!(r"\\.\pipe\rmux-buffer-cancel-read-{}", std::process::id());
        let server = ServerOptions::new().create(&path)?;
        let read_task = tokio::spawn(read(PathBuf::from(&path), SpecialFileKind));
        server.connect().await?;

        tokio::task::yield_now().await;
        assert!(!read_task.is_finished());
        read_task.abort();
        assert!(read_task
            .await
            .expect_err("read should be cancelled")
            .is_cancelled());
        Ok(())
    }

    #[tokio::test]
    async fn large_named_pipe_write_completes_or_is_cancellable() -> io::Result<()> {
        let path = format!(r"\\.\pipe\rmux-buffer-cancel-write-{}", std::process::id());
        let mut server = ServerOptions::new()
            .access_outbound(false)
            .in_buffer_size(1_024)
            .create(&path)?;
        let expected_len = 8 * 1024 * 1024;
        let write_task = tokio::spawn(write(
            PathBuf::from(&path),
            vec![0_u8; expected_len],
            false,
            SpecialFileKind,
        ));
        server.connect().await?;

        // Windows treats the requested pipe buffer size as advisory and may
        // expand it enough to accept the entire write. Both completion and a
        // still-pending, cancellable write are valid outcomes here.
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
        if !write_task.is_finished() {
            write_task.abort();
        }
        match write_task.await {
            Err(error) if error.is_cancelled() => {}
            Ok(result) => {
                result?;
                let mut received = Vec::new();
                server.read_to_end(&mut received).await?;
                assert_eq!(received.len(), expected_len);
                assert!(received.iter().all(|byte| *byte == 0));
            }
            Err(error) => panic!("named pipe writer task failed: {error}"),
        }
        Ok(())
    }
}
