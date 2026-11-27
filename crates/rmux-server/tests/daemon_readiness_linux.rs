#![cfg(target_os = "linux")]

#[cfg(not(target_has_atomic = "64"))]
use portable_atomic::AtomicU64;
use std::os::fd::{AsRawFd, OwnedFd};
use std::path::PathBuf;
#[cfg(target_has_atomic = "64")]
use std::sync::atomic::AtomicU64;
use std::sync::atomic::Ordering;

use rmux_server::{DaemonConfig, ServerDaemon};
use rustix::event::{eventfd, EventfdFlags};

struct SocketFixture(PathBuf);
impl SocketFixture {
    fn new() -> Self {
        static NEXT: AtomicU64 = AtomicU64::new(0);
        let root = std::env::temp_dir().join(format!(
            "rmux-ready-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir(&root).unwrap();
        Self(root)
    }
    fn path(&self) -> PathBuf {
        self.0.join("socket")
    }
}
impl Drop for SocketFixture {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn counter() -> OwnedFd {
    eventfd(0, EventfdFlags::CLOEXEC | EventfdFlags::NONBLOCK).unwrap()
}
fn assert_unsignaled(fd: &OwnedFd) {
    assert_eq!(
        rustix::io::read(fd, &mut [0_u8; 8]),
        Err(rustix::io::Errno::AGAIN)
    );
}

#[tokio::test]
async fn readiness_is_sent_once_after_binding_and_not_by_config_clone() {
    let fixture = SocketFixture::new();
    let fd = counter();
    let config = DaemonConfig::new(fixture.path()).with_startup_ready_fd(fd.as_raw_fd());
    let cloned = config.clone();
    assert_eq!(config, cloned);
    assert_unsignaled(&fd);
    let daemon = ServerDaemon::new(cloned).bind().await.unwrap();
    let mut bytes = [0_u8; 8];
    assert_eq!(rustix::io::read(&fd, &mut bytes).unwrap(), 8);
    assert_eq!(u64::from_ne_bytes(bytes), 1);
    let client = tokio::net::UnixStream::connect(fixture.path())
        .await
        .unwrap();
    drop(client);
    daemon.shutdown().await.unwrap();
    assert_unsignaled(&fd);
}

#[tokio::test]
async fn failed_bind_does_not_signal_readiness() {
    let fixture = SocketFixture::new();
    let fd = counter();
    std::fs::write(fixture.path(), b"occupied").unwrap();
    let result =
        ServerDaemon::new(DaemonConfig::new(fixture.path()).with_startup_ready_fd(fd.as_raw_fd()))
            .bind()
            .await;
    assert!(result.is_err());
    assert_unsignaled(&fd);
}

#[tokio::test]
async fn readiness_transport_error_still_does_not_fail_a_successful_bind() {
    let fixture = SocketFixture::new();
    let daemon = ServerDaemon::new(DaemonConfig::new(fixture.path()).with_startup_ready_fd(-1))
        .bind()
        .await
        .unwrap();
    let client = tokio::net::UnixStream::connect(fixture.path())
        .await
        .unwrap();
    drop(client);
    daemon.shutdown().await.unwrap();
}

#[tokio::test]
async fn default_readiness_is_disabled_and_bind_still_accepts_connections() {
    let fixture = SocketFixture::new();
    let daemon = ServerDaemon::new(DaemonConfig::new(fixture.path()))
        .bind()
        .await
        .unwrap();
    let client = tokio::net::UnixStream::connect(fixture.path())
        .await
        .unwrap();
    drop(client);
    daemon.shutdown().await.unwrap();
}
