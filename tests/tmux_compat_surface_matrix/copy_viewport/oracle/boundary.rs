use super::super::{read_chunk, wait_for_viewports, CopyViewport, ViewportPhase};
use super::*;
use std::fs::{self, File};
use std::os::{fd::OwnedFd, unix::net::UnixStream};
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::{Duration, Instant};

const FIXTURES: &str = "tests/tmux_compat_surface_matrix/copy_viewport/fixtures";

fn bounded(mut predicate: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(3);
    while !predicate() {
        assert!(Instant::now() < end, "controlled child barrier timed out");
        std::thread::yield_now();
    }
}

fn available(oracle: &Oracle) -> i32 {
    let mut bytes = 0;
    // SAFETY: query only, on this oracle's live owned descriptor.
    assert_eq!(
        unsafe { libc::ioctl(oracle.output.as_raw_fd(), libc::FIONREAD, &mut bytes) },
        0
    );
    bytes
}

fn reaped(pid: u32) {
    // SAFETY: nonblocking check of this test's child after owned teardown.
    assert_eq!(
        unsafe { libc::waitpid(pid as i32, std::ptr::null_mut(), libc::WNOHANG) },
        -1
    );
    assert_eq!(
        io::Error::last_os_error().raw_os_error(),
        Some(libc::ECHILD)
    );
}

fn controlled(mode: &str) -> (Oracle, PathBuf) {
    static NEXT: AtomicUsize = AtomicUsize::new(0);
    let dir = std::env::temp_dir().join(format!(
        "ob-{}-{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    fs::create_dir(&dir).unwrap();
    let root = Path::new(env!("CARGO_MANIFEST_DIR"));
    let node = std::env::var_os("RMUX_XTERM_NODE").unwrap_or_else(|| "node".into());
    let oracle =
        Oracle::spawn(
            Command::new(node)
                .arg(root.join(
                    "tests/tmux_compat_surface_matrix/copy_viewport/oracle/boundary-child.mjs",
                ))
                .arg(mode)
                .arg(&dir)
                .arg(root.join(FIXTURES).join("r1-xterm.json"))
                .arg(root.join(FIXTURES).join("r1.hex")),
        )
        .unwrap();
    (oracle, dir)
}

fn raw() -> Vec<u8> {
    include_str!("../fixtures/r1.hex")
        .trim()
        .as_bytes()
        .chunks_exact(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}

fn settle(viewport: &mut CopyViewport) {
    bounded(|| {
        viewport.oracle.service(&viewport.bytes).unwrap();
        viewport.oracle.current(viewport.bytes.len())
    });
}

struct Harness {
    pair: [CopyViewport; 2],
    readers: [File; 2],
    _writers: [UnixStream; 2],
    dir: PathBuf,
    ids: [u32; 2],
}

impl Harness {
    fn new(mode: &str) -> Self {
        let (oracle, dir) = controlled(mode);
        let mut left = CopyViewport::new().unwrap();
        left.oracle = oracle;
        let mut pair = [left, CopyViewport::new().unwrap()];
        let pipe = || {
            let (r, w) = UnixStream::pair().unwrap();
            r.set_nonblocking(true).unwrap();
            (File::from(OwnedFd::from(r)), w)
        };
        let (a, wa) = pipe();
        let (b, wb) = pipe();
        let mut readers = [a, b];
        let mut writers = [wa, wb];
        for i in 0..2 {
            writers[i].write_all(&raw()).unwrap(); // 2398, never a large blocking preload.
            assert!(!read_chunk(&mut readers[i], &mut pair[i], &mut [0; 8192]).unwrap());
            assert_eq!(pair[i].bytes, raw());
        }
        pair[0].oracle.service(&pair[0].bytes).unwrap();
        bounded(|| dir.join("request").exists());
        let request: Value =
            serde_json::from_slice(&fs::read(dir.join("request")).unwrap()).unwrap();
        assert_eq!(request["bytes"], json!(raw()));
        settle(&mut pair[1]); // Real pinned xterm peer is ready first.
                              // Retained independent xterm baseline, SHA256 fb96160724231b30369a43f7dc997534f52b02ea016538599924300c354bc001.
        let expected: Value =
            serde_json::from_str(include_str!("../fixtures/r1-xterm.json")).unwrap();
        assert_eq!(json!(pair[1].rows()), expected["baseline"]);
        assert!(pair[1].ready(ViewportPhase::CopyTop));
        let ids = pair.each_ref().map(|v| v.oracle.child.id());
        let mut h = Self {
            pair,
            readers,
            _writers: writers,
            dir,
            ids,
        };
        h.drained();
        h
    }

    fn release(&self, marker: &str) {
        fs::write(self.dir.join(marker), b"go").unwrap();
    }

    fn queued(&self, n: i32) {
        bounded(|| available(&self.pair[0].oracle) >= n);
        assert_eq!(available(&self.pair[0].oracle), n);
        println!("queued={n}, requests=2398/2398, peer_ready=true, PTY_writers=open");
    }

    fn drained(&mut self) {
        for reader in &mut self.readers {
            assert_eq!(
                reader.read(&mut [0]).unwrap_err().kind(),
                io::ErrorKind::WouldBlock
            );
        }
        assert!(self.pair.iter().all(|v| v.bytes == raw()));
    }

    fn wait(&mut self) -> io::Result<()> {
        let [a, b] = &mut self.readers;
        wait_for_viewports(
            a,
            b,
            &mut self.pair,
            ViewportPhase::CopyTop,
            Duration::from_millis(100),
        )
    }

    fn reject(&mut self) {
        let result = self.wait();
        self.drained();
        println!("acceptance={result:?}");
        assert!(
            result.is_err(),
            "valid ACK hid an observable oracle failure"
        );
        for pid in self.ids {
            reaped(pid);
        }
    }
}

impl Drop for Harness {
    fn drop(&mut self) {
        for v in &mut self.pair {
            v.oracle.stop();
        }
        for pid in self.ids {
            reaped(pid);
        }
        // Passive barrier/request files are retained for the test run's evidence.
    }
}

#[test]
fn r2_already_exited_child_with_queued_ack_is_rejected() {
    let mut h = Harness::new("exit7");
    h.release("release");
    bounded(|| {
        // WNOHANG bounds the barrier; WNOWAIT leaves reap to Oracle's Child.
        let mut info: libc::siginfo_t = unsafe { std::mem::zeroed() };
        assert_eq!(
            unsafe {
                libc::waitid(
                    libc::P_PID,
                    h.ids[0] as libc::id_t,
                    &mut info,
                    libc::WEXITED | libc::WNOWAIT | libc::WNOHANG,
                )
            },
            0
        );
        info.si_signo != 0
    });
    h.queued(589);
    println!("waitid: exited before acceptance, not reaped by barrier");
    h.reject();
}

#[test]
fn r2_duplicate_after_8192_byte_ack_is_rejected() {
    let mut h = Harness::new("duplicate");
    h.release("release");
    h.queued(8781);
    h.reject();
}

#[test]
fn r2_incomplete_extra_after_8192_byte_ack_is_rejected() {
    let mut h = Harness::new("extra");
    h.release("release");
    h.queued(8193);
    h.reject();
}

#[test]
fn r2_observed_residue_cannot_allow_a_second_wait() {
    let mut h = Harness::new("residue");
    h.release("release");
    h.queued(589);
    h.wait().unwrap();
    h.release("tail");
    h.queued(1);
    let v = &mut h.pair[0];
    let service = v.oracle.service(&v.bytes);
    println!(
        "residue service={service:?}, buffered={}, current={}",
        v.oracle.response.len(),
        v.oracle.current(v.bytes.len())
    );
    // b83 buffers '{' with no request and then accepts a second complete wait.
    h.reject();
}

#[test]
fn valid_fragmented_and_boundary_sized_ack_reach_a_clean_boundary() {
    for (mode, size) in [("fragment", 17), ("valid", 8192)] {
        let mut h = Harness::new(mode);
        h.release("release");
        h.queued(size);
        let v = &mut h.pair[0];
        v.oracle.service(&v.bytes).unwrap();
        assert!(!v.oracle.current(v.bytes.len()));
        if mode == "fragment" {
            assert_eq!(v.oracle.response.len(), 17);
            h.release("tail");
            h.queued(572);
        } else {
            assert!(
                v.oracle.needs_service(v.bytes.len()),
                "must check WouldBlock without another poll event"
            );
        }
        h.wait().unwrap();
        h.drained();
        assert_eq!(h.pair[0].rows(), h.pair[1].rows());
    }
}

#[test]
fn buffered_unsolicited_data_invalidates_cached_currentness() {
    let mut h = Harness::new("residue");
    h.release("release");
    h.queued(589);
    h.wait().unwrap();
    // Challenge the accessor independently of service's earlier rejection.
    h.pair[0].oracle.response.push(b'{');
    assert!(!h.pair[0].oracle.current(2398));
    h.reject();
}

#[test]
fn new_pty_tail_invalidates_an_inflight_ack() {
    let mut h = Harness::new("valid");
    h._writers[0].write_all(b"\x1b[2;80HX\x1b[K").unwrap();
    assert!(!read_chunk(&mut h.readers[0], &mut h.pair[0], &mut [0; 8192]).unwrap());
    h.release("release");
    h.queued(8192);
    for _ in 0..2 {
        let v = &mut h.pair[0];
        v.oracle.service(&v.bytes).unwrap();
        assert!(!v.oracle.current(v.bytes.len()));
    }
    assert_eq!(h.pair[0].oracle.acknowledged, Some(2398));
    assert_eq!(h.pair[0].bytes.len(), 2409);
    let [a, b] = &mut h.readers;
    assert_eq!(
        wait_for_viewports(
            a,
            b,
            &mut h.pair,
            ViewportPhase::CopyTop,
            Duration::from_millis(30)
        )
        .unwrap_err()
        .kind(),
        io::ErrorKind::TimedOut
    );
}

#[test]
fn continuous_response_is_bounded_and_does_not_starve_peer_pty() {
    let mut h = Harness::new("continuous");
    h.release("release");
    bounded(|| available(&h.pair[0].oracle) >= 8192);
    h._writers[1].write_all(b"\0").unwrap();
    let start = Instant::now();
    let error = h.wait().unwrap_err();
    assert!(error.to_string().contains("reply limit"));
    assert_eq!(h.pair[1].bytes.len(), 2399);
    assert!(start.elapsed() < Duration::from_secs(2));
    println!(
        "continuous response: peer PTY consumed, bounded failure in {:?}",
        start.elapsed()
    );
}

#[test]
fn controlled_pipe_occupancy_preserves_partial_request_writes() {
    let (mut oracle, dir) = controlled("nonreading");
    let pid = oracle.child.id();
    let mut filled = 0;
    loop {
        match oracle.input.write(&[b' '; 4096]) {
            Ok(n) => {
                filled += n;
                assert!(filled <= 1_048_576);
            }
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => break,
            other => panic!("{other:?}"),
        }
    }
    fs::write(dir.join("drain"), b"go").unwrap();
    bounded(|| dir.join("drained").exists());
    oracle.service(&[255; 8192]).unwrap();
    let written = oracle.written;
    assert!(written > 0 && written < oracle.request.len());
    oracle.service(&[255; 8192]).unwrap();
    assert_eq!(oracle.written, written);
    assert!(!oracle.current(8192));
    println!(
        "partial write: filled={filled}, released=8192, written={written}/{}",
        oracle.request.len()
    );
    drop(oracle);
    reaped(pid);
}
