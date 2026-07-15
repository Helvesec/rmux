use std::io;
use std::sync::atomic::{AtomicI32, Ordering};
use std::sync::{Mutex, MutexGuard};

const NO_ATTACH: i32 = 0;
const ATTACH_ACTIVE: i32 = -1;
const TERMINATION_SIGNALS: [i32; 2] = [libc::SIGHUP, libc::SIGTERM];

static ATTACH_SIGNAL_STATE: AtomicI32 = AtomicI32::new(NO_ATTACH);
static ATTACH_SIGNAL_LOCK: Mutex<()> = Mutex::new(());

pub(super) struct AttachTerminationGuard {
    previous_actions: [libc::sigaction; 2],
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

        let previous_hup = match install_handler(libc::SIGHUP) {
            Ok(previous) => previous,
            Err(error) => {
                ATTACH_SIGNAL_STATE.store(NO_ATTACH, Ordering::SeqCst);
                return Err(error);
            }
        };
        let previous_term = match install_handler(libc::SIGTERM) {
            Ok(previous) => previous,
            Err(error) => {
                let _ = restore_handler(libc::SIGHUP, &previous_hup);
                ATTACH_SIGNAL_STATE.store(NO_ATTACH, Ordering::SeqCst);
                return Err(error);
            }
        };

        Ok(Self {
            previous_actions: [previous_hup, previous_term],
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
