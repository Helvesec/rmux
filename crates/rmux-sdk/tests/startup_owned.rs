#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use rmux_sdk::RmuxBuilder;

static ENVIRONMENT: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

#[tokio::test]
async fn startup_timeout_returns_after_its_owned_daemon_has_ended() {
    check_owned_startup_end(false).await;
}

#[tokio::test]
async fn cancelled_startup_returns_after_its_owned_daemon_has_ended() {
    check_owned_startup_end(true).await;
}

async fn check_owned_startup_end(cancel: bool) {
    let _environment = ENVIRONMENT.lock().await;
    let target = std::env::var_os("CARGO_TARGET_DIR")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            std::env::current_exe()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .parent()
                .unwrap()
                .to_path_buf()
        });
    let binary = target.join("debug/examples/startup_owned_fixture");
    if !binary.is_file() {
        let status = Command::new(std::env::var_os("CARGO").unwrap_or_else(|| "cargo".into()))
            .args([
                "build",
                "--locked",
                "--offline",
                "-p",
                "rmux-sdk",
                "--example",
                "startup_owned_fixture",
            ])
            .env("CARGO_TARGET_DIR", &target)
            .current_dir(PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../.."))
            .status()
            .expect("build startup fixture");
        assert!(status.success(), "startup fixture build failed");
    }
    let suffix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    let root = PathBuf::from(format!(
        "/tmp/rmux-sdk-startup-owned-{}-{suffix}",
        std::process::id()
    ));
    let endpoint = root.join("daemon.sock");
    let previous = std::env::var_os("RMUX_SDK_DAEMON_BINARY");
    std::env::set_var("RMUX_SDK_DAEMON_BINARY", &binary);
    let startup = RmuxBuilder::new()
        .unix_socket(&endpoint)
        .default_timeout(if cancel {
            Duration::from_secs(60)
        } else {
            Duration::from_millis(200)
        })
        .connect_or_start();
    let error = if cancel {
        assert!(
            tokio::time::timeout(Duration::from_millis(200), startup)
                .await
                .is_err(),
            "startup must still be pending when its owner cancels it"
        );
        "cancelled".to_owned()
    } else {
        match startup.await {
            Ok(_) => panic!("non-listening daemon unexpectedly connected"),
            Err(error) => error.to_string(),
        }
    };
    match previous {
        Some(value) => std::env::set_var("RMUX_SDK_DAEMON_BINARY", value),
        None => std::env::remove_var("RMUX_SDK_DAEMON_BINARY"),
    }
    let mut survivor = None;
    let mut status = 0;
    // This test binary has only these serialized probes. Cargo/verification
    // helpers were already waited; its only remaining child can be the owned
    // daemon launcher, even if that launcher has not executed the fixture yet.
    let waited = unsafe { libc::waitpid(-1, &mut status, libc::WNOHANG) };
    if waited == 0 {
        #[cfg(target_os = "linux")]
        for entry in std::fs::read_dir("/proc").unwrap().flatten() {
            if let Ok(stat) = std::fs::read_to_string(entry.path().join("stat")) {
                let fields: Vec<_> = stat
                    .rsplit_once(')')
                    .unwrap()
                    .1
                    .split_whitespace()
                    .collect();
                if fields.get(1).and_then(|value| value.parse::<u32>().ok())
                    == Some(std::process::id())
                {
                    println!("owned direct child at startup boundary: {stat}");
                }
            }
        }
        // Preserve the failed boundary, then collect the finite fixture's
        // original natural end. No signal can turn this failure into a pass.
        let pid = unsafe { libc::waitpid(-1, &mut status, 0) };
        assert!(pid > 0);
        assert!(libc::WIFEXITED(status));
        assert_eq!(libc::WEXITSTATUS(status), 0);
        survivor = Some(pid);
    } else if waited > 0 {
        survivor = Some(waited);
    } else {
        assert_eq!(
            std::io::Error::last_os_error().raw_os_error(),
            Some(libc::ECHILD)
        );
    }
    let _ = std::fs::remove_dir_all(&root);
    assert!(
        cancel || error.contains("timed out"),
        "unexpected startup result: {error}"
    );
    assert!(
        survivor.is_none(),
        "startup returned with owned daemon {survivor:?} still alive"
    );
}
