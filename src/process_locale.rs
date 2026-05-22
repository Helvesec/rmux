#[cfg(unix)]
use std::ffi::CStr;
use std::ffi::CString;

// SAFETY: This declares libc's process-global timezone refresh entrypoint.
// Calls are wrapped in `LibcLocaleBackend::tzset`.
#[cfg(unix)]
unsafe extern "C" {
    fn tzset();
}

pub(crate) fn initialize_process_locale() -> Result<(), String> {
    initialize_locale(&LibcLocaleBackend)
}

trait LocaleBackend {
    fn set_ctype(&self, locale: &str) -> bool;
    fn set_ctype_from_environment(&self) -> bool;
    fn codeset(&self) -> Option<String>;
    fn set_time_from_environment(&self);
    fn tzset(&self);
}

fn initialize_locale(backend: &impl LocaleBackend) -> Result<(), String> {
    if !backend.set_ctype("en_US.UTF-8") && !backend.set_ctype("C.UTF-8") {
        if !backend.set_ctype_from_environment() {
            return Err("invalid LC_ALL, LC_CTYPE or LANG".to_owned());
        }

        let codeset = backend
            .codeset()
            .unwrap_or_else(|| "unknown".to_owned())
            .to_ascii_lowercase();
        if codeset != "utf-8".to_ascii_lowercase() && codeset != "utf8" {
            return Err(format!(
                "need UTF-8 locale (LC_CTYPE) but have {}",
                backend.codeset().unwrap_or_else(|| "unknown".to_owned())
            ));
        }
    }

    backend.set_time_from_environment();
    backend.tzset();
    Ok(())
}

struct LibcLocaleBackend;

impl LocaleBackend for LibcLocaleBackend {
    fn set_ctype(&self, locale: &str) -> bool {
        setlocale(libc::LC_CTYPE, locale)
    }

    fn set_ctype_from_environment(&self) -> bool {
        setlocale(libc::LC_CTYPE, "")
    }

    #[cfg(unix)]
    fn codeset(&self) -> Option<String> {
        // SAFETY: `nl_langinfo(CODESET)` returns either null or a pointer to a
        // process-owned NUL-terminated string for the active locale.
        let codeset = unsafe { libc::nl_langinfo(libc::CODESET) };
        if codeset.is_null() {
            return None;
        }
        Some(
            // SAFETY: The null case is handled above, and libc guarantees a
            // NUL-terminated string for this locale item.
            unsafe { CStr::from_ptr(codeset) }
                .to_string_lossy()
                .into_owned(),
        )
    }

    #[cfg(windows)]
    fn codeset(&self) -> Option<String> {
        Some("UTF-8".to_owned())
    }

    fn set_time_from_environment(&self) {
        let _ = setlocale(libc::LC_TIME, "");
    }

    #[cfg(unix)]
    fn tzset(&self) {
        // SAFETY: `tzset` updates libc process-global timezone state and takes
        // no pointers or Rust-owned resources.
        unsafe { tzset() }
    }

    #[cfg(windows)]
    fn tzset(&self) {}
}

fn setlocale(category: libc::c_int, locale: &str) -> bool {
    #[cfg(windows)]
    let locale = match locale {
        "en_US.UTF-8" | "C.UTF-8" => ".UTF-8",
        other => other,
    };

    let Ok(locale) = CString::new(locale) else {
        return false;
    };
    // SAFETY: `locale` is a live NUL-terminated string for the duration of the
    // call, and `category` is supplied by the libc constants used by callers.
    let result = unsafe { libc::setlocale(category, locale.as_ptr()) };
    !result.is_null()
}

#[cfg(test)]
mod tests {
    use super::{initialize_locale, LocaleBackend};
    use std::cell::RefCell;

    #[derive(Default)]
    struct MockLocaleBackend {
        ctype_attempts: RefCell<Vec<String>>,
        ctype_results: RefCell<Vec<bool>>,
        env_result: bool,
        codeset: Option<String>,
        time_calls: RefCell<usize>,
        tzset_calls: RefCell<usize>,
    }

    impl MockLocaleBackend {
        fn with_results(ctype_results: Vec<bool>, env_result: bool, codeset: Option<&str>) -> Self {
            Self {
                ctype_results: RefCell::new(ctype_results),
                env_result,
                codeset: codeset.map(str::to_owned),
                ..Self::default()
            }
        }
    }

    impl LocaleBackend for MockLocaleBackend {
        fn set_ctype(&self, locale: &str) -> bool {
            self.ctype_attempts.borrow_mut().push(locale.to_owned());
            self.ctype_results.borrow_mut().remove(0)
        }

        fn set_ctype_from_environment(&self) -> bool {
            self.ctype_attempts.borrow_mut().push(String::new());
            self.env_result
        }

        fn codeset(&self) -> Option<String> {
            self.codeset.clone()
        }

        fn set_time_from_environment(&self) {
            *self.time_calls.borrow_mut() += 1;
        }

        fn tzset(&self) {
            *self.tzset_calls.borrow_mut() += 1;
        }
    }

    #[test]
    fn builtin_utf8_locale_short_circuits_before_environment_fallback() {
        let backend = MockLocaleBackend::with_results(vec![true], true, Some("UTF-8"));

        assert_eq!(initialize_locale(&backend), Ok(()));
        assert_eq!(backend.ctype_attempts.borrow().as_slice(), ["en_US.UTF-8"]);
        assert_eq!(*backend.time_calls.borrow(), 1);
        assert_eq!(*backend.tzset_calls.borrow(), 1);
    }

    #[test]
    fn c_utf8_fallback_matches_tmux_startup_order() {
        let backend = MockLocaleBackend::with_results(vec![false, true], true, Some("UTF-8"));

        assert_eq!(initialize_locale(&backend), Ok(()));
        assert_eq!(
            backend.ctype_attempts.borrow().as_slice(),
            ["en_US.UTF-8", "C.UTF-8"]
        );
        assert_eq!(*backend.time_calls.borrow(), 1);
        assert_eq!(*backend.tzset_calls.borrow(), 1);
    }

    #[test]
    fn environment_fallback_accepts_utf8_codesets() {
        let backend = MockLocaleBackend::with_results(vec![false, false], true, Some("UTF8"));

        assert_eq!(initialize_locale(&backend), Ok(()));
        assert_eq!(
            backend.ctype_attempts.borrow().as_slice(),
            ["en_US.UTF-8", "C.UTF-8", ""]
        );
    }

    #[test]
    fn environment_fallback_rejects_non_utf8_codesets() {
        let backend = MockLocaleBackend::with_results(vec![false, false], true, Some("ISO-8859-1"));

        assert_eq!(
            initialize_locale(&backend),
            Err("need UTF-8 locale (LC_CTYPE) but have ISO-8859-1".to_owned())
        );
    }

    #[test]
    fn invalid_locale_environment_uses_tmux_error_text() {
        let backend = MockLocaleBackend::with_results(vec![false, false], false, None);

        assert_eq!(
            initialize_locale(&backend),
            Err("invalid LC_ALL, LC_CTYPE or LANG".to_owned())
        );
    }
}
