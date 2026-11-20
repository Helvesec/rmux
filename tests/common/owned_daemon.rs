//! A redirected endpoint is cleanup authority only for its witnessed launch.
use std::error::Error;
use std::fs;
use std::io::{self, Read, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

const LAUNCH_TAG_ENV: &str = "RMUX_TEST_DAEMON_LAUNCH";
/// Upper bound for an owned daemon to answer and finish the shutdown a
/// kill-server starts. The daemon bounds it itself (a foreground run-shell keeps
/// 5 s, then the connection, lifecycle-hook and background drains), and SIGTERM
/// cannot shorten a shutdown already in progress: a shorter wait hands a daemon
/// that is still shutting down, shim included, to the CliHarness shim guard.
const OWNED_DAEMON_SHUTDOWN_DEADLINE: Duration = Duration::from_secs(15);

#[derive(Debug, PartialEq, Eq)]
struct ProcessIdentity {
    pid: u32,
    birth: String,
}

pub(super) struct DaemonLease {
    tag: String,
    launcher: Option<u32>,
    owner: Option<ProcessIdentity>,
}

impl DaemonLease {
    pub(super) fn reserve() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        Self {
            tag: format!(
                "{}-{}-{}",
                std::process::id(),
                SystemTime::now()
                    .duration_since(UNIX_EPOCH)
                    .unwrap()
                    .as_nanos(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ),
            launcher: None,
            owner: None,
        }
    }

    pub(super) fn prepare(&mut self, socket: &Path) -> Result<(), Box<dyn Error>> {
        if let Some(stream) = connect(socket)? {
            if self.owner.is_none() || self.identity(&stream)?.as_ref() != self.owner.as_ref() {
                return Err("redirected test socket already has a foreign daemon".into());
            }
        } else {
            // A new launch needs its own provenance, even at a previously owned path.
            *self = Self::reserve();
        }
        Ok(())
    }

    pub(super) fn configure(&self, command: &mut std::process::Command) {
        command.env(LAUNCH_TAG_ENV, &self.tag);
    }

    pub(super) fn launched(&mut self, pid: u32) {
        self.launcher = Some(pid);
    }

    pub(super) fn capture(&mut self, socket: &Path) -> Result<(), Box<dyn Error>> {
        if self.owner.is_none() && self.launcher.is_some() {
            if let Some(stream) = connect(socket)? {
                self.owner = self.identity(&stream)?;
                if self.owner.is_none() {
                    return Err("redirected daemon was not created by this launch".into());
                }
                eprintln!(
                    "owned daemon launcher={:?} identity={:?}",
                    self.launcher, self.owner
                );
            }
        }
        Ok(())
    }

    fn identity(&self, stream: &UnixStream) -> Result<Option<ProcessIdentity>, Box<dyn Error>> {
        let pid = peer_pid(stream)?;
        let Some(birth) = process_birth(pid)? else {
            return Ok(None);
        };
        // The native environment proves inheritance from this particular launch,
        // including after the command exits and the daemon is reparented.
        let inherited = rmux_os::process::ProcessInspector
            .raw_environment(pid)?
            .is_some_and(|environment| {
                environment.iter().any(|(name, value)| {
                    name == std::ffi::OsStr::new(LAUNCH_TAG_ENV)
                        && value == std::ffi::OsStr::new(&self.tag)
                })
            });
        if !inherited || process_birth(pid)?.as_ref() != Some(&birth) {
            return Ok(None);
        }
        Ok(Some(ProcessIdentity { pid, birth }))
    }

    pub(super) fn shutdown(&mut self, socket: &Path) -> Result<(), Box<dyn Error>> {
        self.capture(socket)?;
        let Some(owner) = &self.owner else {
            return Ok(());
        };
        let Some(mut stream) = connect(socket)? else {
            return Ok(());
        };
        if self.identity(&stream)?.as_ref() != Some(owner) {
            return Ok(());
        }
        let deadline = Instant::now() + OWNED_DAEMON_SHUTDOWN_DEADLINE;
        stream.set_write_timeout(Some(Duration::from_secs(2)))?;
        // Send on the already authenticated connection: replacing the pathname
        // cannot redirect this frame to a different daemon.
        let frame = rmux_proto::encode_frame(&rmux_proto::Request::KillServer(
            rmux_proto::KillServerRequest,
        ))?;
        if stream.write_all(&frame).is_ok() {
            // Stay connected until the daemon answers or exits, as the CLI does.
            // On macOS a daemon that accepts after its peer has closed cannot
            // read the peer PID (LOCAL_PEERPID fails with ENOTCONN), so it drops
            // the connection and this kill-server unread.
            stream.set_read_timeout(Some(deadline.saturating_duration_since(Instant::now())))?;
            let _ = stream.read(&mut [0_u8; 64]);
        }
        drop(stream);
        while process_birth(owner.pid)?.as_ref() == Some(&owner.birth) && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
        // Never use the path-wide fallback that can select a replacement daemon.
        if process_birth(owner.pid)?.as_ref() == Some(&owner.birth) {
            let Some(stream) = connect(socket)? else {
                return Ok(());
            };
            if self.identity(&stream)?.as_ref() == Some(owner) {
                let pid = i32::try_from(owner.pid)?;
                unsafe {
                    // SAFETY: The connected peer's PID, birth and inherited launch
                    // were revalidated above; only that owned process is signalled.
                    libc::kill(pid, libc::SIGTERM);
                }
            }
        }
        Ok(())
    }
}

fn connect(socket: &Path) -> io::Result<Option<UnixStream>> {
    match UnixStream::connect(socket) {
        Ok(stream) => Ok(Some(stream)),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::ConnectionRefused
            ) =>
        {
            Ok(None)
        }
        Err(error) => Err(error),
    }
}

fn peer_pid(stream: &UnixStream) -> io::Result<u32> {
    #[cfg(target_os = "linux")]
    let (level, option, mut value) = (libc::SOL_SOCKET, libc::SO_PEERCRED, unsafe {
        // SAFETY: ucred is a plain C output record initialized by getsockopt.
        std::mem::zeroed::<libc::ucred>()
    });
    #[cfg(target_os = "macos")]
    let (level, option, mut value) = (libc::SOL_LOCAL, libc::LOCAL_PEERPID, 0 as libc::pid_t);
    #[cfg(any(target_os = "linux", target_os = "macos"))]
    {
        let mut length = std::mem::size_of_val(&value) as libc::socklen_t;
        let status = unsafe {
            // SAFETY: value and length are writable for the advertised sizes;
            // stream owns the connected socket throughout this call.
            libc::getsockopt(
                stream.as_raw_fd(),
                level,
                option,
                std::ptr::from_mut(&mut value).cast(),
                &mut length,
            )
        };
        if status != 0 {
            return Err(io::Error::last_os_error());
        }
        #[cfg(target_os = "linux")]
        let pid = value.pid;
        #[cfg(target_os = "macos")]
        let pid = value;
        u32::try_from(pid)
            .ok()
            .filter(|pid| *pid > 0)
            .ok_or_else(|| io::Error::other("connected daemon PID unavailable"))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = stream;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native daemon attribution unavailable",
        ))
    }
}

fn process_birth(pid: u32) -> io::Result<Option<String>> {
    #[cfg(target_os = "linux")]
    {
        let stat = match fs::read_to_string(format!("/proc/{pid}/stat")) {
            Ok(stat) => stat,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        let fields: Vec<_> = stat
            .rsplit_once(')')
            .ok_or_else(|| io::Error::other("invalid proc stat"))?
            .1
            .split_whitespace()
            .collect();
        if fields.first() == Some(&"Z") {
            return Ok(None);
        }
        fields
            .get(19)
            .map(|ticks| Some((*ticks).to_owned()))
            .ok_or_else(|| io::Error::other("process birth unavailable"))
    }
    #[cfg(target_os = "macos")]
    {
        let mut info = std::mem::MaybeUninit::<libc::proc_bsdinfo>::zeroed();
        let size = std::mem::size_of::<libc::proc_bsdinfo>();
        let read = unsafe {
            // SAFETY: info is writable for the full PROC_PIDTBSDINFO record.
            libc::proc_pidinfo(
                i32::try_from(pid).map_err(|_| io::ErrorKind::InvalidInput)?,
                libc::PROC_PIDTBSDINFO,
                0,
                info.as_mut_ptr().cast(),
                size as i32,
            )
        };
        if read as usize != size {
            return Ok(None);
        }
        let info = unsafe {
            // SAFETY: proc_pidinfo initialized the complete record.
            info.assume_init()
        };
        if info.pbi_status == 5 {
            return Ok(None);
        }
        Ok(Some(format!(
            "{}:{}",
            info.pbi_start_tvsec, info.pbi_start_tvusec
        )))
    }
    #[cfg(not(any(target_os = "linux", target_os = "macos")))]
    {
        let _ = pid;
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native process birth unavailable",
        ))
    }
}
