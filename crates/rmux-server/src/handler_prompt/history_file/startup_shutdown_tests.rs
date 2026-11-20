use std::fs;
use std::io::{Read, Write};
use std::time::Duration;

use rmux_proto::{encode_frame, FrameDecoder, KillServerRequest, Request, Response};

use super::unique_temp_root;
use crate::{DaemonConfig, ServerDaemon};

#[cfg(unix)]
const SIGNAL_CHILD: &str = "RMUX_TEST_HISTORY_STARTUP_SIGNAL_CHILD";

#[tokio::test]
async fn shutdown_during_startup_preserves_history_before_import() {
    #[cfg(unix)]
    let signal_ready = std::env::var_os(SIGNAL_CHILD);
    #[cfg(not(unix))]
    let signal_ready: Option<std::ffi::OsString> = None;
    let mut outcomes = Vec::new();
    for kill_server in [false, true] {
        let root = unique_temp_root("history-startup-shutdown");
        let history = root.join("history");
        let entered = root.join("entered");
        let continued = root.join("continued");
        let config = root.join("startup.conf");
        let original = b"command:KEEP\nsearch:keep:these-bytes\n";
        fs::write(&history, original).expect("prefilled history");
        // The buffer file acknowledges the option mutation. The following
        // foreground run-shell waits on its timer, so startup
        // cannot reach its history import, regardless of scheduler timing.
        fs::write(
            &config,
            format!(
                "set-option -s history-file '{}'\nset-buffer ready\nsave-buffer '{}'\nrun-shell -d 600\nsave-buffer '{}'\n",
                history.display(), entered.display(), continued.display()
            ),
        )
        .expect("startup config");
        let endpoint = rmux_ipc::endpoint_for_label(format!(
            "history-startup-{}-{}",
            std::process::id(),
            u8::from(kill_server)
        ))
        .expect("private endpoint");
        let daemon = ServerDaemon::new(
            DaemonConfig::new(endpoint.as_path().to_path_buf()).with_config_files(
                vec![config],
                false,
                Some(root.clone()),
            ),
        )
        .bind()
        .await
        .expect("bind daemon");
        tokio::time::timeout(Duration::from_secs(5), async {
            while !entered.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("startup acknowledged history-file before suspension");
        assert!(!continued.exists(), "startup remains suspended");

        tokio::time::timeout(Duration::from_secs(15), async {
            if let Some(ready) = &signal_ready {
                fs::write(ready, b"ready").expect("isolated daemon signal readiness");
                daemon.wait().await.expect("signal shutdown");
            } else if kill_server {
                tokio::task::spawn_blocking(move || {
                    let mut client = rmux_ipc::connect_blocking(&endpoint, Duration::from_secs(5))
                        .expect("connect kill-server client");
                    client
                        .write_all(
                            &encode_frame(&Request::KillServer(KillServerRequest))
                                .expect("encode kill-server"),
                        )
                        .expect("send kill-server");
                    let mut decoder = FrameDecoder::new();
                    let mut bytes = [0; 1024];
                    loop {
                        let count = client.read(&mut bytes).expect("read shutdown reply");
                        assert_ne!(count, 0, "kill-server receives its reply");
                        decoder.push_bytes(&bytes[..count]);
                        if let Some(response) =
                            decoder.next_frame::<Response>().expect("reply frame")
                        {
                            assert!(matches!(response, Response::KillServer(_)), "{response:?}");
                            break;
                        }
                    }
                })
                .await
                .expect("kill-server client task");
                daemon.wait().await.expect("kill-server shutdown");
            } else {
                daemon.shutdown().await.expect("handle shutdown");
            }
        })
        .await
        .expect("shutdown cancels suspended startup");
        assert!(!continued.exists(), "cancelled startup never continued");
        let preserved = fs::read(&history).expect("history after stop") == original;
        fs::remove_dir_all(root).expect("remove fixture");
        if signal_ready.is_some() {
            assert!(
                preserved,
                "signal shutdown must preserve uninitialized history"
            );
            return;
        }
        outcomes.push((
            if kill_server { "kill-server" } else { "handle" },
            preserved,
        ));
    }
    #[cfg(unix)]
    for (name, signal) in [
        ("SIGTERM", rustix::process::Signal::TERM),
        ("SIGINT", rustix::process::Signal::INT),
    ] {
        outcomes.push((name, isolated_signal_shutdown(signal).await));
    }
    assert!(
        outcomes.iter().all(|(_, preserved)| *preserved),
        "shutdown must preserve an uninitialized history file: {outcomes:?}"
    );
}

#[cfg(unix)]
async fn isolated_signal_shutdown(signal: rustix::process::Signal) -> bool {
    let root = unique_temp_root("history-signal-parent");
    let ready = root.join("ready");
    // Signal flags are process-wide: run this daemon alone so another crate
    // test cannot consume its signal or be stopped by it.
    let name = concat!(
        module_path!(),
        "::shutdown_during_startup_preserves_history_before_import"
    );
    let mut child = std::process::Command::new(std::env::current_exe().expect("test executable"))
        .args(["--exact", name.split_once("::").unwrap().1, "--nocapture"])
        .env(SIGNAL_CHILD, &ready)
        .spawn()
        .expect("isolated daemon test child");
    tokio::time::timeout(Duration::from_secs(10), async {
        while !ready.exists() {
            assert!(child.try_wait().unwrap().is_none(), "child starts daemon");
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("isolated daemon is ready for its signal");
    let pid = rustix::process::Pid::from_raw(child.id() as i32).expect("child pid");
    rustix::process::kill_process(pid, signal).expect("signal owned daemon child");
    let status = tokio::time::timeout(Duration::from_secs(20), async {
        loop {
            if let Some(status) = child.try_wait().expect("wait signal child") {
                break status;
            }
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("signal child exited");
    fs::remove_dir_all(root).expect("remove signal acknowledgement");
    status.success()
}
