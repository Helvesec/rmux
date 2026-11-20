use super::*;
use std::panic::catch_unwind;
use std::process::{Child, Command, Stdio};
use std::sync::{mpsc, TryLockError};
use std::time::{Duration, Instant};

const CHILD_ENV: &str = "RMUX_TERMINATION_POLICY_CHILD";
const DEADLINE: Duration = Duration::from_secs(15);
static DELIVERED_SIGNAL: AtomicI32 = AtomicI32::new(0);
static DELIVERY_COUNT: AtomicI32 = AtomicI32::new(0);
static STATE_AT_DELIVERY: AtomicI32 = AtomicI32::new(ATTACH_ACTIVE);

// Only the exact, fresh child executes signal/poison experiments. The shared
// runner supervises it without acquiring the owner lock or changing sigactions.
fn run_in_subprocess(test: &str) -> bool {
    let module = module_path!().split_once("::").unwrap().1;
    let name = format!("{module}::{test}");
    // SAFETY: getppid has no preconditions and does not mutate process state.
    let parent = unsafe { libc::getppid() };
    if std::env::var(CHILD_ENV).as_deref() == Ok(&format!("{parent}:{name}")) {
        return false;
    }

    struct OwnedChild(Option<Child>);
    impl Drop for OwnedChild {
        fn drop(&mut self) {
            if let Some(child) = self.0.as_mut() {
                let _ = child.kill();
                child.wait().expect("reap owned termination test child");
            }
        }
    }

    let mut child = OwnedChild(Some(
        Command::new(std::env::current_exe().unwrap())
            .args(["--exact", &name, "--nocapture", "--test-threads=1"])
            .env(CHILD_ENV, format!("{}:{name}", std::process::id()))
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .expect("spawn isolated termination test"),
    ));
    let pid = child.0.as_ref().unwrap().id();
    let deadline = Instant::now() + DEADLINE;
    let timed_out = loop {
        if child.0.as_mut().unwrap().try_wait().unwrap().is_some() {
            break false;
        }
        if Instant::now() >= deadline {
            let _ = child.0.as_mut().unwrap().kill();
            break true;
        }
        // Deadline polling only; no elapsed delay is used as a lock witness.
        thread::yield_now();
    };
    let output = child.0.take().unwrap().wait_with_output().unwrap();
    let stdout = String::from_utf8_lossy(&output.stdout);
    let stderr = String::from_utf8_lossy(&output.stderr);
    eprintln!("reaped child {pid}: {}\n{stdout}{stderr}", output.status);
    assert!(!timed_out, "isolated test exceeded {DEADLINE:?}");
    assert!(output.status.success(), "isolated test failed: {name}");
    assert!(
        stdout.contains("1 passed; 0 failed") && stdout.contains(&name),
        "child must actually execute the selected test"
    );
    true
}

#[test]
fn unwind_restores_saved_actions_without_clearing_poison() {
    if run_in_subprocess("unwind_restores_saved_actions_without_clearing_poison") {
        return;
    }
    let saved = prepare_saved_handlers();
    assert!(!ATTACH_SIGNAL_LOCK.is_poisoned());
    let panic = catch_unwind(|| {
        let _guard = AttachTerminationGuard::install().unwrap();
        assert_installed();
        deliver(libc::SIGHUP);
        deliver(libc::SIGTERM);
        assert!(was_requested());
        assert_eq!(requested_signal(), Some(libc::SIGHUP));
        assert_eq!(DELIVERED_SIGNAL.load(Ordering::SeqCst), 0);
        panic!("intentional unwind while the production guard owns handlers");
    });
    assert_eq!(
        panic.unwrap_err().downcast_ref::<&str>(),
        Some(&"intentional unwind while the production guard owns handlers")
    );
    assert_restored(&saved);
    assert_eq!(DELIVERED_SIGNAL.load(Ordering::SeqCst), libc::SIGHUP);
    assert_eq!(DELIVERY_COUNT.load(Ordering::SeqCst), 1);
    assert_eq!(STATE_AT_DELIVERY.load(Ordering::SeqCst), NO_ATTACH);
    assert_poisoned_but_available();

    // The recovered unit guard must permit a real future installation. Exercise
    // both cleanup entries and every supported signal through the kernel.
    for finish in [true, false] {
        for signal in TERMINATION_SIGNALS {
            DELIVERED_SIGNAL.store(0, Ordering::SeqCst);
            DELIVERY_COUNT.store(0, Ordering::SeqCst);
            let guard = AttachTerminationGuard::install().unwrap();
            assert_installed();
            assert!(ATTACH_SIGNAL_LOCK.is_poisoned());
            deliver(signal);
            assert_eq!(requested_signal(), Some(signal));
            if finish {
                guard.finish().unwrap();
            } else {
                drop(guard);
            }
            assert_restored(&saved);
            assert_eq!(DELIVERED_SIGNAL.load(Ordering::SeqCst), signal);
            assert_eq!(DELIVERY_COUNT.load(Ordering::SeqCst), 1);
            assert_eq!(STATE_AT_DELIVERY.load(Ordering::SeqCst), NO_ATTACH);
            assert_poisoned_but_available();
        }
    }
}

#[test]
fn poisoned_install_does_not_recover_domain_state() {
    if run_in_subprocess("poisoned_install_does_not_recover_domain_state") {
        return;
    }
    let saved = prepare_saved_handlers();
    poison_by_unwinding();
    assert_restored(&saved);

    for state in [ATTACH_ACTIVE, libc::SIGTERM] {
        // Deliberately model stale domain state without an extant lock holder.
        // Only this fresh child owns the fixture; poison recovery must not reset it.
        ATTACH_SIGNAL_STATE.store(state, Ordering::SeqCst);
        let error = AttachTerminationGuard::install().err().unwrap();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert_eq!(ATTACH_SIGNAL_STATE.load(Ordering::SeqCst), state);
        assert_eq!(requested_signal(), (state > 0).then_some(state));
        assert_eq!(was_requested(), state > 0);
        assert_actions(&saved);
        assert_poisoned_but_available();
    }

    // Explicit fixture repair, not a property attributed to lock recovery.
    ATTACH_SIGNAL_STATE.store(NO_ATTACH, Ordering::SeqCst);
    let guard = AttachTerminationGuard::install().unwrap();
    assert_installed();
    guard.finish().unwrap();
    assert_restored(&saved);
    assert_eq!(DELIVERED_SIGNAL.load(Ordering::SeqCst), 0);
    assert_poisoned_but_available();
}

#[test]
fn poisoned_guard_serializes_install_until_finish_or_drop() {
    if run_in_subprocess("poisoned_guard_serializes_install_until_finish_or_drop") {
        return;
    }
    let saved = prepare_saved_handlers();
    poison_by_unwinding();
    assert_restored(&saved);

    for finish in [true, false] {
        let first = AttachTerminationGuard::install().unwrap();
        let (witness_tx, witness_rx) = mpsc::channel();
        let (installed_tx, installed_rx) = mpsc::channel();
        let (release_tx, release_rx) = mpsc::channel();
        let contender = thread::spawn(move || {
            // Positive exclusion witness from another thread, even when poisoned.
            assert!(matches!(
                ATTACH_SIGNAL_LOCK.try_lock(),
                Err(TryLockError::WouldBlock)
            ));
            witness_tx.send(()).unwrap();
            let next = AttachTerminationGuard::install().unwrap();
            assert_installed();
            installed_tx.send(()).unwrap();
            release_rx.recv_timeout(DEADLINE).unwrap();
            next.finish().unwrap();
        });
        witness_rx.recv_timeout(DEADLINE).unwrap();
        assert_installed();
        if finish {
            first.finish().unwrap();
        } else {
            drop(first);
        }
        installed_rx.recv_timeout(DEADLINE).unwrap();
        assert!(matches!(
            ATTACH_SIGNAL_LOCK.try_lock(),
            Err(TryLockError::WouldBlock)
        ));
        assert_installed();
        release_tx.send(()).unwrap();
        contender.join().unwrap();
        assert_restored(&saved);
        assert_poisoned_but_available();
        eprintln!("exclusion witnessed in both threads; handoff completed (finish={finish})");
    }
}

fn poison_by_unwinding() {
    let panic = catch_unwind(|| {
        let _guard = AttachTerminationGuard::install().unwrap();
        assert_installed();
        panic!("intentional production guard unwind");
    });
    assert_eq!(
        panic.unwrap_err().downcast_ref::<&str>(),
        Some(&"intentional production guard unwind")
    );
    assert_poisoned_but_available();
}

fn assert_poisoned_but_available() {
    assert!(ATTACH_SIGNAL_LOCK.is_poisoned());
    match ATTACH_SIGNAL_LOCK.try_lock() {
        Err(TryLockError::Poisoned(poisoned)) => drop(poisoned.into_inner()),
        other => panic!("expected an available, still-poisoned lock: {other:?}"),
    }
}

extern "C" fn saved_handler(signal: i32) {
    STATE_AT_DELIVERY.store(ATTACH_SIGNAL_STATE.load(Ordering::SeqCst), Ordering::SeqCst);
    DELIVERED_SIGNAL.store(signal, Ordering::SeqCst);
    DELIVERY_COUNT.fetch_add(1, Ordering::SeqCst);
}

fn prepare_saved_handlers() -> Vec<libc::sigaction> {
    assert_eq!(ATTACH_SIGNAL_STATE.load(Ordering::SeqCst), NO_ATTACH);
    // SAFETY: all pointers name initialized writable C storage. Only this owned
    // child changes dispositions/masks, for supported signals. No saved handler
    // escapes the child; its entire address space disappears on the explicit reap.
    unsafe {
        let mut mask: libc::sigset_t = std::mem::zeroed();
        assert_eq!(libc::sigemptyset(&mut mask), 0);
        for signal in TERMINATION_SIGNALS {
            assert_eq!(libc::sigaddset(&mut mask, signal), 0);
        }
        assert_eq!(
            libc::pthread_sigmask(libc::SIG_UNBLOCK, &mask, std::ptr::null_mut()),
            0
        );
        for signal in TERMINATION_SIGNALS {
            let mut action: libc::sigaction = std::mem::zeroed();
            action.sa_sigaction = saved_handler as *const () as usize;
            action.sa_flags = libc::SA_RESTART;
            assert_eq!(libc::sigemptyset(&mut action.sa_mask), 0);
            assert_eq!(libc::sigaddset(&mut action.sa_mask, libc::SIGUSR1), 0);
            assert_eq!(libc::sigaction(signal, &action, std::ptr::null_mut()), 0);
        }
    }
    TERMINATION_SIGNALS.into_iter().map(read_action).collect()
}

fn read_action(signal: i32) -> libc::sigaction {
    // SAFETY: null new-action queries only; the kernel fills valid output storage.
    unsafe {
        let mut action = std::mem::zeroed();
        assert_eq!(libc::sigaction(signal, std::ptr::null(), &mut action), 0);
        action
    }
}

fn assert_actions(expected: &[libc::sigaction]) {
    for (signal, expected) in TERMINATION_SIGNALS.into_iter().zip(expected) {
        let actual = read_action(signal);
        assert_eq!(
            actual.sa_sigaction, expected.sa_sigaction,
            "handler {signal}"
        );
        assert_eq!(actual.sa_flags, expected.sa_flags, "flags {signal}");
        for member in TERMINATION_SIGNALS.into_iter().chain([libc::SIGUSR1]) {
            // SAFETY: both masks came from successful sigaction queries; member
            // is a supported signal. Avoid comparing C struct padding bytes.
            unsafe {
                assert_eq!(
                    libc::sigismember(&actual.sa_mask, member),
                    libc::sigismember(&expected.sa_mask, member),
                    "mask {signal}, member {member}"
                );
            }
        }
    }
}

fn assert_installed() {
    assert_eq!(ATTACH_SIGNAL_STATE.load(Ordering::SeqCst), ATTACH_ACTIVE);
    assert!(!was_requested());
    assert_eq!(requested_signal(), None);
    for signal in TERMINATION_SIGNALS {
        assert_eq!(
            read_action(signal).sa_sigaction,
            record_termination_signal as *const () as usize
        );
    }
}

fn assert_restored(saved: &[libc::sigaction]) {
    assert_eq!(ATTACH_SIGNAL_STATE.load(Ordering::SeqCst), NO_ATTACH);
    assert!(!was_requested());
    assert_eq!(requested_signal(), None);
    assert_actions(saved);
}

fn deliver(signal: i32) {
    // SAFETY: a supported signal is sent synchronously to this child's calling
    // thread, after installation and unblocking; no other process is targeted.
    assert_eq!(unsafe { libc::raise(signal) }, 0);
}
