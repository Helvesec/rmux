use super::{run_status_job, run_status_job_with_timeout, StatusJobRuntime};
use std::path::{Path, PathBuf};
use std::process::Command;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

#[test]
fn normal_completion_terminates_job_object_descendants() {
    let probe = WindowsStatusJobProbe::new("normal");
    let started = Instant::now();

    let output = run_status_job(&probe.normal_completion_command(), None);

    assert_eq!(output, "complete");
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "normal completion should terminate the Job Object without reaching the status timeout"
    );
    probe.assert_process_dead("normal completion");
}

#[test]
fn timeout_terminates_windows_status_job_tree() {
    let started = Instant::now();
    let output = run_status_job_with_timeout(
        "ping.exe -n 30 127.0.0.1 >NUL",
        None,
        Duration::from_millis(500),
    );

    assert_eq!(output, "");
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "the Windows status timeout must terminate and reap its Job Object"
    );
}

#[test]
fn shutdown_terminates_and_joins_windows_status_job_tree() {
    let runtime = StatusJobRuntime::new();
    let probe = WindowsStatusJobProbe::new("shutdown");
    let command = probe.running_command();

    let _ = runtime.cached_output(&command, None, Duration::ZERO);
    probe.wait_for_pid();
    assert_eq!(runtime.active_job_count(), 1);
    let started = Instant::now();

    runtime.shutdown_and_join();

    assert_eq!(
        runtime.active_job_count(),
        0,
        "shutdown must join its worker"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "shutdown should terminate the Windows Job Object promptly"
    );
    probe.assert_process_dead("runtime shutdown");
}

struct WindowsStatusJobProbe {
    root: PathBuf,
    pid_path: PathBuf,
}

impl WindowsStatusJobProbe {
    fn new(label: &str) -> Self {
        let unique = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time after Unix epoch")
            .as_nanos();
        let root = std::env::temp_dir().join(format!(
            "rmux-status-job-{label}-{}-{unique}",
            std::process::id()
        ));
        std::fs::create_dir_all(&root).expect("create Windows status probe root");
        let pid_path = root.join("descendant.pid");
        Self { root, pid_path }
    }

    fn normal_completion_command(&self) -> String {
        self.powershell_command("[Console]::Out.Write('complete')")
    }

    fn running_command(&self) -> String {
        self.powershell_command("while ($true) { Start-Sleep -Seconds 30 }")
    }

    fn powershell_command(&self, tail: &str) -> String {
        let pid_path = powershell_single_quote(&self.pid_path);
        format!(
            "powershell.exe -NoLogo -NoProfile -NonInteractive -Command \"\
             $child = Start-Process -PassThru -WindowStyle Hidden \
             -FilePath (Join-Path $env:SystemRoot 'System32\\ping.exe') \
             -ArgumentList '-t','127.0.0.1' -RedirectStandardOutput 'NUL'; \
             [IO.File]::WriteAllText('{pid_path}', [string]$child.Id); {tail}\""
        )
    }

    fn wait_for_pid(&self) -> u32 {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(pid) = read_pid(&self.pid_path) {
                return pid;
            }
            assert!(
                Instant::now() < deadline,
                "Windows status descendant did not record its pid"
            );
            std::thread::sleep(Duration::from_millis(20));
        }
    }

    fn assert_process_dead(&self, stage: &str) {
        let pid = self.wait_for_pid();
        assert!(
            !rmux_os::process::is_live(pid),
            "Windows status descendant {pid} survived {stage}"
        );
    }
}

impl Drop for WindowsStatusJobProbe {
    fn drop(&mut self) {
        if let Some(pid) = read_pid(&self.pid_path) {
            let _ = Command::new("taskkill")
                .args(["/PID", &pid.to_string(), "/T", "/F"])
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .status();
        }
        let _ = std::fs::remove_dir_all(&self.root);
    }
}

fn read_pid(path: &Path) -> Option<u32> {
    std::fs::read_to_string(path)
        .ok()?
        .trim_start_matches('\u{feff}')
        .trim()
        .parse()
        .ok()
}

fn powershell_single_quote(path: &Path) -> String {
    path.to_string_lossy().replace('\'', "''")
}
