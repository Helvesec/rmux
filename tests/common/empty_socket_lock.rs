//! Ownership of the single empty abstract endpoint in this network namespace.

use std::error::Error;
use std::io;
use std::os::linux::net::SocketAddrExt;
use std::os::unix::net::{SocketAddr, UnixListener};
use std::path::Path;
use std::thread;
use std::time::{Duration, Instant};

use rmux_client::{connect_or_absent, wait_for_server_endpoint_cleanup, ConnectResult};

// Abstract names have exactly the same network-namespace scope as -S ''.
// Neither cwd, TMPDIR, mount namespace nor uid can split this ownership lock.
const LOCK_NAME: &[u8] = b"rmux-tests-empty-socket-lock";

pub(crate) struct EmptySocketPathLock {
    listener: Option<UnixListener>,
}

pub(crate) fn acquire_empty_socket_path_lock() -> Result<EmptySocketPathLock, Box<dyn Error>> {
    let address = SocketAddr::from_abstract_name(LOCK_NAME)?;
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        match UnixListener::bind_addr(&address) {
            Ok(listener) => {
                return Ok(EmptySocketPathLock {
                    listener: Some(listener),
                });
            }
            Err(error) if error.kind() == io::ErrorKind::AddrInUse => {
                if Instant::now() >= deadline {
                    return Err("timed out waiting for network-namespace empty -S lock".into());
                }
                thread::sleep(Duration::from_millis(10));
            }
            Err(error) => return Err(error.into()),
        }
    }
}

impl Drop for EmptySocketPathLock {
    fn drop(&mut self) {
        // This guard is declared before the harness/daemon guards, so their
        // normal cleanup runs first. Also clean up on an assertion failure,
        // when the test never reached its explicit kill-server command.
        let endpoint = Path::new("");
        let cleanup = (|| -> Result<(), Box<dyn Error>> {
            if let ConnectResult::Connected(mut connection) = connect_or_absent(endpoint)? {
                // Only the guarded endpoint is addressed; no PID scan or signal.
                connection.kill_server_after_write()?;
            }
            wait_for_server_endpoint_cleanup(endpoint)?;
            Ok(())
        })();
        if let Err(error) = cleanup {
            // Do not hand a still-live endpoint to another test. Keep the
            // kernel lock until process exit and report the cleanup failure.
            if let Some(listener) = self.listener.take() {
                std::mem::forget(listener);
            }
            if thread::panicking() {
                eprintln!("empty -S cleanup failed; lock retained: {error}");
            } else {
                panic!("empty -S cleanup failed; lock retained: {error}");
            }
        }
    }
}
