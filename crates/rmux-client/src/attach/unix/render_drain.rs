use std::io::{self, Read, Write};
use std::os::unix::net::UnixStream;

use rmux_proto::AttachFrameDecoder;
use rustix::event::{poll, PollFd, PollFlags, Timespec};

use crate::ClientError;

const NO_WAIT: Timespec = Timespec {
    tv_sec: 0,
    tv_nsec: 0,
};
const RENDER_DRAIN_READ_LIMIT: usize = 64;

pub(super) fn flush_pending_render<Output>(
    output: &mut Output,
    pending_render: &mut Option<Vec<u8>>,
) -> std::result::Result<(), ClientError>
where
    Output: Write,
{
    let Some(bytes) = pending_render.take() else {
        return Ok(());
    };
    output.write_all(&bytes).map_err(ClientError::Io)?;
    output.flush().map_err(ClientError::Io)
}

pub(super) fn drain_available_attach_stream(
    stream: &mut UnixStream,
    decoder: &mut AttachFrameDecoder,
    read_buffer: &mut [u8],
) -> std::result::Result<bool, ClientError> {
    let mut read_any = false;
    for _ in 0..RENDER_DRAIN_READ_LIMIT {
        let mut fds = [PollFd::new(
            &*stream,
            PollFlags::IN | PollFlags::ERR | PollFlags::HUP,
        )];
        match poll(&mut fds, Some(&NO_WAIT)) {
            Ok(0) => break,
            Ok(_) => {}
            Err(rustix::io::Errno::INTR) => continue,
            Err(error) => return Err(ClientError::Io(error.into())),
        }
        let ready = fds[0].revents();
        if !ready.contains(PollFlags::IN) {
            break;
        }
        match stream.read(read_buffer) {
            Ok(0) => break,
            Ok(bytes_read) => {
                read_any = true;
                decoder.push_bytes(&read_buffer[..bytes_read]);
            }
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(ClientError::Io(error)),
        }
    }
    Ok(read_any)
}
