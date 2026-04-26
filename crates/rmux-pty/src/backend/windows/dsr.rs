use std::ffi::OsStr;
use std::path::Path;
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT_MS: u64 = 200;
const MIN_TIMEOUT_MS: u64 = 50;
const MAX_TIMEOUT_MS: u64 = 2_000;
const DSR_REQUEST: &[u8] = b"\x1b[6n";
const DSR_RESPONSE: &[u8] = b"\x1b[1;1R";

#[derive(Debug)]
pub(crate) struct DsrBootstrap {
    deadline: Instant,
    completed: bool,
}

impl DsrBootstrap {
    pub(crate) fn from_env() -> Self {
        Self {
            deadline: Instant::now() + configured_timeout(),
            completed: false,
        }
    }

    pub(crate) fn filter<'a>(&mut self, bytes: &'a mut [u8]) -> DsrFilter<'a> {
        if self.completed || Instant::now() > self.deadline {
            self.completed = true;
            return DsrFilter {
                bytes,
                response: None,
            };
        }

        let Some(offset) = find_subslice(bytes, DSR_REQUEST) else {
            return DsrFilter {
                bytes,
                response: None,
            };
        };

        self.completed = true;
        let tail_start = offset + DSR_REQUEST.len();
        bytes.copy_within(tail_start.., offset);
        let len = bytes.len() - DSR_REQUEST.len();
        DsrFilter {
            bytes: &mut bytes[..len],
            response: Some(DSR_RESPONSE),
        }
    }
}

pub(crate) struct DsrFilter<'a> {
    pub(crate) bytes: &'a mut [u8],
    pub(crate) response: Option<&'static [u8]>,
}

pub(crate) fn should_enable_dsr_bootstrap(program: &Path) -> bool {
    let Some(name) = program.file_name().and_then(OsStr::to_str) else {
        return false;
    };
    matches!(
        name.to_ascii_lowercase().as_str(),
        "pwsh.exe" | "powershell.exe"
    )
}

fn configured_timeout() -> Duration {
    let millis = std::env::var("RMUX_DSR_BOOTSTRAP_TIMEOUT_MS")
        .ok()
        .and_then(|value| value.parse::<u64>().ok())
        .unwrap_or(DEFAULT_TIMEOUT_MS)
        .clamp(MIN_TIMEOUT_MS, MAX_TIMEOUT_MS);
    Duration::from_millis(millis)
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn powershell_detection_is_basename_only() {
        assert!(should_enable_dsr_bootstrap(Path::new(
            "C:\\Program Files\\PowerShell\\7\\pwsh.exe"
        )));
        assert!(should_enable_dsr_bootstrap(Path::new(
            "C:\\Windows\\System32\\WindowsPowerShell\\v1.0\\powershell.exe"
        )));
        assert!(!should_enable_dsr_bootstrap(Path::new("vim.exe")));
    }

    #[test]
    fn filter_removes_first_dsr_and_requests_response() {
        let mut helper = DsrBootstrap {
            deadline: Instant::now() + Duration::from_secs(1),
            completed: false,
        };
        let mut bytes = *b"before\x1b[6nafter";

        let filtered = helper.filter(&mut bytes);

        assert_eq!(filtered.bytes, b"beforeafter");
        assert_eq!(filtered.response, Some(DSR_RESPONSE));
    }
}
