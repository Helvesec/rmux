#![cfg(unix)]

use rmux_server::{DaemonConfig, ServerDaemon};

fn open_fd_count() -> usize {
    #[cfg(target_os = "linux")]
    let path = "/proc/self/fd";
    #[cfg(not(target_os = "linux"))]
    let path = "/dev/fd";
    std::fs::read_dir(path).unwrap().map(Result::unwrap).count()
}

#[tokio::test]
async fn repeated_bind_shutdown_releases_signal_watchers() {
    let directory = std::env::temp_dir().join(format!("rmux-signal-fd-{}", std::process::id()));
    std::fs::create_dir(&directory).unwrap();
    let socket = directory.join("socket");
    let bind = || ServerDaemon::new(DaemonConfig::new(socket.clone())).bind();
    bind().await.unwrap().shutdown().await.unwrap();
    let baseline = open_fd_count();
    for iteration in 0..32 {
        let daemon = bind().await.unwrap();
        let client = tokio::net::UnixStream::connect(&socket).await.unwrap();
        drop(client);
        daemon.shutdown().await.unwrap();
        assert_eq!(
            open_fd_count(),
            baseline,
            "fd leak after bind/shutdown {iteration}"
        );
    }
    std::fs::remove_dir(directory).unwrap();
}
