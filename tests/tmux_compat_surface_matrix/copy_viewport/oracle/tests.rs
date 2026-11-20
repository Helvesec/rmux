use super::*;
use rustix::event::{poll, Timespec};
use std::time::{Duration, Instant};

fn node() -> Command {
    Command::new(std::env::var_os("RMUX_XTERM_NODE").unwrap_or_else(|| "node".into()))
}

fn until_reply(oracle: &mut Oracle) -> io::Result<()> {
    let end = Instant::now() + Duration::from_secs(3);
    loop {
        oracle.service(b"hello")?;
        if oracle.current(5) {
            return Ok(());
        }
        let remaining = end
            .checked_duration_since(Instant::now())
            .ok_or_else(|| io::Error::new(io::ErrorKind::TimedOut, "oracle test bound"))?;
        if oracle.needs_service(5) {
            continue;
        }
        poll(
            &mut oracle.poll_fds(),
            Some(&Timespec::try_from(remaining).unwrap()),
        )?;
    }
}

fn assert_reaped(pid: u32) {
    // SAFETY: nonblocking observation of this test's already reaped child.
    assert_eq!(
        unsafe { libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG) },
        -1
    );
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

#[test]
fn missing_node_and_project_local_package_fail_explicitly() {
    let error = Oracle::spawn(&mut Command::new("/nonexistent-rmux-xterm-node"))
        .err()
        .unwrap();
    assert!(error.to_string().contains(PREREQUISITE));
    let dir = crate::common::unique_tmpdir("xterm-absent");
    std::fs::create_dir_all(&dir).unwrap();
    let script = dir.join("viewport-oracle.mjs");
    std::fs::write(
        &script,
        include_str!("../../../xterm-oracle/viewport-oracle.mjs"),
    )
    .unwrap();
    let mut oracle = Oracle::spawn(node().arg(&script)).unwrap();
    let pid = oracle.child.id();
    let error = until_reply(&mut oracle).unwrap_err();
    assert!(error
        .to_string()
        .contains("npm --prefix tests/xterm-oracle ci --ignore-scripts"));
    drop(oracle);
    assert_reaped(pid);
    // This test owns only these two scratch entries; no shared installation.
    std::fs::remove_file(script).unwrap();
    std::fs::remove_dir(dir).unwrap();
}

#[test]
fn malformed_truncated_oversized_and_wrong_ack_replies_fail_closed() {
    for program in [
        "process.stdout.write('not-json\\n')",
        "process.stdout.write('{')",
        "process.stdout.write('x'.repeat(40000))",
        "process.stdin.once('data',()=>process.stdout.write(JSON.stringify({protocol:1,xterm:'6.0.0',sequence:0,end:5,rows:Array(24).fill('')})+'\\n'))",
        "process.stdin.once('data',()=>process.stdout.write(JSON.stringify({protocol:1,xterm:'6.0.0',sequence:1,end:4,rows:Array(24).fill('')})+'\\n'))",
        "process.stdin.once('data',()=>process.stdout.write(JSON.stringify({protocol:1,xterm:'5.0.0',sequence:1,end:5,rows:Array(24).fill('')})+'\\n'))",
        "process.stdin.once('data',()=>process.stdout.write(JSON.stringify({protocol:1,xterm:'6.0.0',sequence:1,end:5,rows:Array(23).fill('')})+'\\n'))",
        "process.exit(7)",
    ] {
        let mut oracle = Oracle::spawn(node().args(["-e", program])).unwrap();
        let pid = oracle.child.id();
        assert!(until_reply(&mut oracle).is_err(), "{program}");
        assert!(!oracle.current(5));
        drop(oracle);
        assert_reaped(pid);
    }
}

#[test]
fn stalled_oracle_does_not_block_either_pty_and_timeout_reaps_children() {
    use super::super::{
        tests::{pipe, retained_r1},
        wait_for_viewports, CopyViewport, ViewportPhase,
    };
    let mut observed = [CopyViewport::new().unwrap(), CopyViewport::new().unwrap()];
    observed[0].oracle = Oracle::spawn(node().args(["-e", "setInterval(()=>{},1000)"])).unwrap();
    let ids = observed.each_ref().map(|v| v.oracle.child.id());
    let (mut left, mut wl) = pipe();
    let (mut right, mut wr) = pipe();
    let bytes = retained_r1();
    wl.write_all(&bytes).unwrap();
    wr.write_all(&bytes).unwrap();
    let end = Instant::now();
    let error = wait_for_viewports(
        &mut left,
        &mut right,
        &mut observed,
        ViewportPhase::CopyTop,
        Duration::from_millis(100),
    )
    .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::TimedOut);
    assert_eq!(observed[0].bytes, bytes);
    assert_eq!(observed[1].bytes, bytes);
    assert!(end.elapsed() < Duration::from_secs(2));
    for pid in ids {
        assert_reaped(pid);
    }
}

#[test]
fn unwinding_owns_oracle_teardown() {
    let oracle = Oracle::new().unwrap();
    let pid = oracle.child.id();
    let result = std::panic::catch_unwind(move || {
        let _owned = oracle;
        panic!("oracle cancellation canary");
    });
    assert!(result.is_err());
    assert_reaped(pid);
}

#[test]
fn input_and_work_limits_fail_explicitly() {
    use super::super::CopyViewport;
    let mut viewport = CopyViewport::new().unwrap();
    assert!(viewport.feed(&[0; 8193]).is_err());
    for _ in 0..128 {
        viewport.feed(&[0; 8192]).unwrap();
    }
    assert!(viewport.feed(&[0]).is_err());
    // Exercise the actual adapter's request limit without spawning per request.
    let mut oracle = Oracle::new().unwrap();
    for n in 0..4096 {
        oracle.acknowledged = None;
        until_reply(&mut oracle).unwrap_or_else(|e| panic!("request {n}: {e}"));
    }
    oracle.acknowledged = None;
    assert!(until_reply(&mut oracle)
        .unwrap_err()
        .to_string()
        .contains("limit"));
}
