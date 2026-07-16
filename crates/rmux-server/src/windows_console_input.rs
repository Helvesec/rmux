use std::io;
use std::time::Duration;

const TRANSIENT_CONSOLE_INPUT_RETRIES: usize = 8;
const TRANSIENT_CONSOLE_INPUT_RETRY_DELAY: Duration = Duration::from_millis(50);

pub(crate) fn write_with_transient_retry(
    mut write: impl FnMut() -> io::Result<()>,
) -> io::Result<()> {
    for attempt in 0..=TRANSIENT_CONSOLE_INPUT_RETRIES {
        match write() {
            Ok(()) => return Ok(()),
            Err(error)
                if attempt < TRANSIENT_CONSOLE_INPUT_RETRIES
                    && is_transient_console_input_error(&error) =>
            {
                std::thread::sleep(TRANSIENT_CONSOLE_INPUT_RETRY_DELAY);
            }
            Err(error) => return Err(error),
        }
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
            Err(io::Error::from_raw_os_error(5))
        })
        .expect_err("non-transient failure should not be retried");

        assert_eq!(error.raw_os_error(), Some(5));
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn stops_after_transient_retry_budget() {
        let attempts = AtomicUsize::new(0);

        let error = write_with_transient_retry(|| {
            attempts.fetch_add(1, Ordering::SeqCst);
            Err(transient_error())
        })
        .expect_err("retry budget should be bounded");

        assert_eq!(error.raw_os_error(), Some(31));
        assert_eq!(
            attempts.load(Ordering::SeqCst),
            TRANSIENT_CONSOLE_INPUT_RETRIES + 1
        );
    }
}
