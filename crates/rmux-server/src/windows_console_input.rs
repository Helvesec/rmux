use std::io;
use std::time::Duration;

const TRANSIENT_CONSOLE_INPUT_RETRIES: usize = 8;
const TRANSIENT_CONSOLE_INPUT_RETRY_DELAY: Duration = Duration::from_millis(50);

/// Retries one console-input operation while it fails transiently.
///
/// The operation must be replayable: it is re-run from the start, so every
/// console-visible effect it performed before failing happens again. Compose
/// multi-effect injections with [`write_console_key_then_processed_interrupt`]
/// instead of retrying them as a single closure.
pub(crate) fn write_with_transient_retry<T>(
    mut write: impl FnMut() -> io::Result<T>,
) -> io::Result<T> {
    let mut attempt = 0;
    loop {
        match write() {
            Ok(value) => return Ok(value),
            Err(error) => {
                if attempt >= TRANSIENT_CONSOLE_INPUT_RETRIES
                    || !is_transient_console_input_error(&error)
                {
                    return Err(error);
                }
                attempt += 1;
                std::thread::sleep(TRANSIENT_CONSOLE_INPUT_RETRY_DELAY);
            }
        }
    }
}

/// Injects a console key record and, when the pane console is in processed
/// input mode, the console interrupt that record implies.
///
/// `write_key` reports whether the interrupt is still required. The two steps
/// carry separate retry budgets on purpose: one physical keystroke must produce
/// exactly one key record and at most one interrupt, so a transient failure of
/// the interrupt must never replay the key record that was already delivered.
pub(crate) fn write_console_key_then_processed_interrupt(
    write_key: impl FnMut() -> io::Result<bool>,
    interrupt: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    if write_with_transient_retry(write_key)? {
        return write_with_transient_retry(interrupt);
    }
    Ok(())
}

fn is_transient_console_input_error(error: &io::Error) -> bool {
    const ERROR_GEN_FAILURE: i32 = 31;
    error.raw_os_error() == Some(ERROR_GEN_FAILURE)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn transient_error() -> io::Error {
        io::Error::from_raw_os_error(31)
    }

    #[test]
    fn retries_transient_console_input_failure() {
        let attempts = AtomicUsize::new(0);

        write_with_transient_retry(|| {
            let attempt = attempts.fetch_add(1, Ordering::SeqCst);
            if attempt < 2 {
                return Err(transient_error());
            }
            Ok(())
        })
        .expect("transient failure should be retried");

        assert_eq!(attempts.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn does_not_retry_non_transient_console_input_failure() {
        let attempts = AtomicUsize::new(0);

        let error = write_with_transient_retry(|| {
            attempts.fetch_add(1, Ordering::SeqCst);
            Err::<(), _>(io::Error::from_raw_os_error(5))
        })
        .expect_err("non-transient failure should not be retried");

        assert_eq!(error.raw_os_error(), Some(5));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn transient_interrupt_failure_does_not_replay_the_console_key_record() {
        let key_records = AtomicUsize::new(0);
        let interrupts = AtomicUsize::new(0);

        write_console_key_then_processed_interrupt(
            || {
                key_records.fetch_add(1, Ordering::SeqCst);
                Ok(true)
            },
            || {
                let attempt = interrupts.fetch_add(1, Ordering::SeqCst);
                if attempt < 2 {
                    return Err(transient_error());
                }
                Ok(())
            },
        )
        .expect("transient interrupt failure should be retried");

        assert_eq!(
            key_records.load(Ordering::SeqCst),
            1,
            "one keystroke must inject exactly one console key record"
        );
        assert_eq!(interrupts.load(Ordering::SeqCst), 3);
    }

    #[test]
    fn raw_input_console_key_is_not_followed_by_an_interrupt() {
        let interrupts = AtomicUsize::new(0);

        write_console_key_then_processed_interrupt(
            || Ok(false),
            || {
                interrupts.fetch_add(1, Ordering::SeqCst);
                Ok(())
            },
        )
        .expect("raw-mode key write should succeed");

        assert_eq!(interrupts.load(Ordering::SeqCst), 0);
    }

    #[test]
    fn stops_after_transient_retry_budget() {
        let attempts = AtomicUsize::new(0);

        let error = write_with_transient_retry(|| {
            attempts.fetch_add(1, Ordering::SeqCst);
            Err::<(), _>(transient_error())
        })
        .expect_err("retry budget should be bounded");

        assert_eq!(error.raw_os_error(), Some(31));
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            TRANSIENT_CONSOLE_INPUT_RETRIES + 1
        );
    }
}
