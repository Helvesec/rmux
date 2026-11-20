//! Cleanup authority stays attached to the witnessed process and connection.
use std::error::Error;
use std::ffi::{OsStr, OsString};
use std::fs;
use std::io::{self, Write};
use std::os::fd::AsRawFd;
use std::os::unix::net::UnixStream;
use std::path::PathBuf;
use std::time::{Duration, Instant};

#[derive(Clone, Debug, PartialEq, Eq)]
pub(super) struct OwnedDaemonIdentity {
    pid: u32,
    birth: String,
    launch: OsString,
    process: String,
}

impl OwnedDaemonIdentity {
    pub(super) fn capture(pid: u32) -> Result<Self, Box<dyn Error>> {
        let birth = process_birth(pid)?.ok_or("owned daemon is absent")?;
        let launch = rmux_os::process::ProcessInspector
            .raw_environment(pid)?
            .and_then(|environment| {
                environment.into_iter().find_map(|(name, value)| {
                    (name == OsStr::new("RMUX_TEST_DAEMON_LAUNCH") && !value.is_empty())
                        .then_some(value)
                })
            })
            .ok_or("owned daemon launch provenance is absent")?;
        let process = super::daemon_identity(pid)?;
        if process.is_empty() || process_birth(pid)?.as_ref() != Some(&birth) {
            return Err("owned daemon identity changed during capture".into());
        }
        Ok(Self {
            pid,
            birth,
            launch,
            process,
        })
    }

    fn authenticates(&self, stream: &UnixStream) -> bool {
        peer_pid(stream).is_ok_and(|pid| pid == self.pid)
            && Self::capture(self.pid).is_ok_and(|current| current == *self)
    }
}

pub(super) struct OwnedDaemonRescue {
    pub(super) socket: PathBuf,
    pub(super) identity: Option<OwnedDaemonIdentity>,
}

impl OwnedDaemonRescue {
    fn shutdown_on(&self, mut stream: UnixStream) {
        let Some(owner) = &self.identity else {
            return;
        };
        if !owner.authenticates(&stream) {
            return;
        }
        if stream
            .set_write_timeout(Some(Duration::from_secs(2)))
            .is_err()
        {
            return;
        }
        let Ok(frame) = rmux_proto::encode_frame(&rmux_proto::Request::KillServer(
            rmux_proto::KillServerRequest,
        )) else {
            return;
        };
        // The authenticated stream remains the destination even if its pathname
        // is replaced. No reconnect or path-wide signal fallback is authorized.
        if stream.write_all(&frame).is_err() {
            return;
        }
        let deadline = Instant::now() + Duration::from_secs(2);
        while process_birth(owner.pid).is_ok_and(|current| current.as_ref() == Some(&owner.birth))
            && Instant::now() < deadline
        {
            std::thread::sleep(Duration::from_millis(20));
        }
    }
}

impl Drop for OwnedDaemonRescue {
    fn drop(&mut self) {
        if self.identity.is_none() {
            return;
        }
        if let Ok(stream) = UnixStream::connect(&self.socket) {
            self.shutdown_on(stream);
        }
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

#[test]
fn rescue_rejects_changed_pid_birth_provenance_and_failed_spawn() -> Result<(), Box<dyn Error>> {
    let harness = crate::common::TmuxCompatHarness::new("rescue-identity")?;
    let root = crate::common::unique_tmpdir("rescue-identity");
    fs::create_dir_all(&root)?;
    let socket = crate::common::default_socket_path_in(&root)?;
    let config = crate::common::TmuxCompatRunConfig::default().with_tmpdir(root.clone());
    assert_eq!(
        harness
            .run_rmux_with(&["new-session", "-d", "-s", "owned", "sleep 60"], &config)?
            .status_code,
        Some(0)
    );
    let pid = harness.run_rmux_with(&["display-message", "-p", "#{pid}"], &config)?;
    let pid = pid.stdout_string().trim().parse::<u32>()?;
    let owner = OwnedDaemonIdentity::capture(pid)?;
    for changed in ["pid", "birth", "provenance", "process", "spawn-failed"] {
        let mut identity = owner.clone();
        match changed {
            "pid" => identity.pid += 1,
            "birth" => identity.birth.push_str("-different-birth"),
            "provenance" => identity.launch.push("-other-launch"),
            "process" => identity.process.push_str("-other-process"),
            _ => {}
        }
        let identity = if changed == "spawn-failed" {
            assert!(std::process::Command::new(root.join("missing-program"))
                .spawn()
                .is_err());
            None
        } else {
            Some(identity)
        };
        drop(OwnedDaemonRescue {
            socket: socket.clone(),
            identity,
        });
        assert_eq!(
            harness
                .run_rmux_with(&["display-message", "-p", "#{pid}"], &config)?
                .stdout_string()
                .trim(),
            pid.to_string(),
            "{changed}: rescue stopped an unauthenticated daemon"
        );
    }
    drop(OwnedDaemonRescue {
        socket: socket.clone(),
        identity: Some(owner),
    });
    crate::wait_for_socket_absent(&socket, Duration::from_secs(5))?;
    assert!(
        process_birth(pid)?.is_none(),
        "normal cleanup must stop the attributed daemon"
    );
    drop(harness);
    fs::remove_dir_all(root)?;
    Ok(())
}
