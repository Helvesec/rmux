use std::io::{self, Write};
use std::os::unix::thread::JoinHandleExt;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, MutexGuard};
use std::thread;

const NO_ATTACH: i32 = 0;
const ATTACH_ACTIVE: i32 = -1;
const TERMINATION_SIGNALS: [i32; 4] = [libc::SIGHUP, libc::SIGTERM, libc::SIGINT, libc::SIGQUIT];

static ATTACH_SIGNAL_STATE: AtomicI32 = AtomicI32::new(NO_ATTACH);
static ATTACH_SIGNAL_LOCK: Mutex<()> = Mutex::new(());

pub(super) struct AttachTerminationGuard {
    previous_actions: Vec<libc::sigaction>,
    armed: bool,
    _lock: MutexGuard<'static, ()>,
}

impl AttachTerminationGuard {
    pub(super) fn install() -> io::Result<Self> {
        let lock = ATTACH_SIGNAL_LOCK
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if ATTACH_SIGNAL_STATE
            .compare_exchange(NO_ATTACH, ATTACH_ACTIVE, Ordering::SeqCst, Ordering::SeqCst)
            .is_err()
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "another Unix attach signal guard is already active",
            ));
        }

        let mut previous_actions = Vec::with_capacity(TERMINATION_SIGNALS.len());
        for signal in TERMINATION_SIGNALS {
            match install_handler(signal) {
                Ok(previous) => previous_actions.push(previous),
                Err(error) => {
                    for (installed_signal, previous) in TERMINATION_SIGNALS
                        .into_iter()
                        .zip(previous_actions.iter())
                        .rev()
                    {
                        let _ = restore_handler(installed_signal, previous);
                    }
                    ATTACH_SIGNAL_STATE.store(NO_ATTACH, Ordering::SeqCst);
                    return Err(error);
                }
            }
        }

        Ok(Self {
            previous_actions,
            armed: true,
            _lock: lock,
        })
    }

    pub(super) fn finish(mut self) -> io::Result<()> {
        let signal = self.restore_handlers()?;
        if let Some(signal) = signal {
            raise_signal(signal)?;
        }
        Ok(())
    }

    fn restore_handlers(&mut self) -> io::Result<Option<i32>> {
        if !self.armed {
            return Ok(None);
        }

        let mut restore_error = None;
        for (signal, previous) in TERMINATION_SIGNALS
            .into_iter()
            .zip(self.previous_actions.iter())
        {
            if let Err(error) = restore_handler(signal, previous) {
                restore_error.get_or_insert(error);
            }
        }
        let observed = ATTACH_SIGNAL_STATE.swap(NO_ATTACH, Ordering::SeqCst);
        self.armed = false;

        if let Some(error) = restore_error {
            return Err(error);
        }
        Ok((observed > 0).then_some(observed))
    }
}

impl Drop for AttachTerminationGuard {
    fn drop(&mut self) {
        if let Ok(Some(signal)) = self.restore_handlers() {
            let _ = raise_signal(signal);
        }
    }
}

pub(super) fn was_requested() -> bool {
    ATTACH_SIGNAL_STATE.load(Ordering::SeqCst) > 0
}

pub(super) fn requested_signal() -> Option<i32> {
    let signal = ATTACH_SIGNAL_STATE.load(Ordering::SeqCst);
    (signal > 0).then_some(signal)
}

pub(super) fn interrupt_thread<T>(thread: &thread::JoinHandle<T>) {
    if let Some(signal) = requested_signal() {
        let _ = unsafe {
            // SAFETY: `as_pthread_t` refers to the unconsumed join handle held
            // by the caller. All captured signal handlers remain installed, so
            // delivery only interrupts the output syscall and re-observes the
            // already-recorded termination.
            libc::pthread_kill(thread.as_pthread_t(), signal)
        };
    }
}

pub(super) struct TerminationAwareWriter<Output> {
    inner: Output,
    enabled: bool,
}

impl<Output> TerminationAwareWriter<Output> {
    pub(super) const fn new(inner: Output, enabled: bool) -> Self {
        Self { inner, enabled }
    }

    fn fail_if_requested(&self) -> io::Result<()> {
        if self.enabled && was_requested() {
            Err(interruption_error())
        } else {
            Ok(())
        }
    }
}

impl<Output> Write for TerminationAwareWriter<Output>
where
    Output: Write,
{
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.fail_if_requested()?;
        match self.inner.write(bytes) {
            Err(error)
                if error.kind() == io::ErrorKind::Interrupted
                    && self.enabled
                    && was_requested() =>
            {
                Err(interruption_error())
            }
            result => result,
        }
    }

    fn write_all(&mut self, mut bytes: &[u8]) -> io::Result<()> {
        while !bytes.is_empty() {
            self.fail_if_requested()?;
            match self.inner.write(bytes) {
                Ok(0) => {
                    return Err(io::Error::new(
                        io::ErrorKind::WriteZero,
                        "failed to write the complete attach output frame",
                    ))
                }
                Ok(written) => bytes = &bytes[written..],
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
        self.fail_if_requested()
    }

    fn flush(&mut self) -> io::Result<()> {
        loop {
            self.fail_if_requested()?;
            match self.inner.flush() {
                Ok(()) => return self.fail_if_requested(),
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) => return Err(error),
            }
        }
    }
}

pub(super) fn interruption_error() -> io::Error {
    io::Error::new(
        io::ErrorKind::Interrupted,
        "attach interrupted by a termination signal",
    )
}

fn install_handler(signal: i32) -> io::Result<libc::sigaction> {
    let mut action = unsafe {
        // SAFETY: `sigaction` is a plain C struct. Zero initialization covers
        // platform-specific fields before the portable fields are populated.
        std::mem::zeroed::<libc::sigaction>()
    };
    action.sa_sigaction = record_termination_signal as *const () as usize;
    action.sa_flags = 0;
    let empty_mask = unsafe {
        // SAFETY: `action.sa_mask` points to initialized writable storage.
        libc::sigemptyset(&mut action.sa_mask)
    };
    if empty_mask != 0 {
        return Err(io::Error::last_os_error());
    }

    let mut previous = unsafe {
        // SAFETY: The kernel initializes every field through the successful
        // `sigaction` call below.
        std::mem::zeroed::<libc::sigaction>()
    };
    let result = unsafe {
        // SAFETY: `signal` is a supported libc constant, `action` is fully
        // initialized, and `previous` is writable output storage.
        libc::sigaction(signal, &action, &mut previous)
    };
    if result == 0 {
        Ok(previous)
    } else {
        Err(io::Error::last_os_error())
    }
}

fn restore_handler(signal: i32, previous: &libc::sigaction) -> io::Result<()> {
    let result = unsafe {
        // SAFETY: `previous` was returned by a successful `sigaction` call for
        // this same signal and remains valid for the duration of this call.
        libc::sigaction(signal, previous, std::ptr::null_mut())
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

fn raise_signal(signal: i32) -> io::Result<()> {
    let result = unsafe {
        // SAFETY: The signal was captured from the kernel as SIGHUP or SIGTERM.
        libc::raise(signal)
    };
    if result == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

extern "C" fn record_termination_signal(signal: i32) {
    let _ = ATTACH_SIGNAL_STATE.compare_exchange(
        ATTACH_ACTIVE,
        signal,
        Ordering::SeqCst,
        Ordering::SeqCst,
    );
}
