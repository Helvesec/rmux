use std::ffi::OsStr;
use std::path::Path;
use std::time::{Duration, Instant};

const DEFAULT_TIMEOUT_MS: u64 = 200;
const MIN_TIMEOUT_MS: u64 = 50;
const MAX_TIMEOUT_MS: u64 = 2_000;
const DSR_REQUEST: &[u8] = b"\x1b[6n";
const DSR_RESPONSE: &[u8] = b"\x1b[1;1R";
const MAX_DEFERRED_BYTES: usize = 64 * 1024;

#[derive(Debug)]
pub(crate) struct DsrBootstrap {
    deadline: Instant,
    completed: bool,
    pending: Vec<u8>,
    deferred: Vec<u8>,
}

impl DsrBootstrap {
    pub(crate) fn from_env() -> Self {
        Self {
            deadline: Instant::now() + configured_timeout(),
            completed: false,
            pending: Vec::new(),
            deferred: Vec::new(),
        }
    }

    pub(crate) fn drain_deferred(&mut self, buffer: &mut [u8]) -> Option<usize> {
        if self.deferred.is_empty() {
            return None;
        }

        let len = buffer.len().min(self.deferred.len());
        buffer[..len].copy_from_slice(&self.deferred[..len]);
        self.deferred.drain(..len);
        Some(len)
    }

    pub(crate) fn filter(&mut self, buffer: &mut [u8], bytes_read: usize) -> DsrFilter {
        if self.completed || Instant::now() > self.deadline {
            self.completed = true;
            return self.emit_with_pending(buffer, bytes_read, None);
        }

        let mut combined = Vec::with_capacity(self.pending.len() + bytes_read);
        combined.extend_from_slice(&self.pending);
        combined.extend_from_slice(&buffer[..bytes_read]);

        if let Some(offset) = find_subslice(&combined, DSR_REQUEST) {
            self.completed = true;
            self.pending.clear();
            let mut output = Vec::with_capacity(combined.len() - DSR_REQUEST.len());
            output.extend_from_slice(&combined[..offset]);
            output.extend_from_slice(&combined[offset + DSR_REQUEST.len()..]);
            return emit_output(buffer, &mut self.deferred, &output, Some(DSR_RESPONSE));
        }

        let pending_len = partial_dsr_prefix_len(&combined);
        self.pending.clear();
        self.pending
            .extend_from_slice(&combined[combined.len() - pending_len..]);
        emit_output(
            buffer,
            &mut self.deferred,
            &combined[..combined.len() - pending_len],
            None,
        )
    }

    fn emit_with_pending(
        &mut self,
        buffer: &mut [u8],
        bytes_read: usize,
        response: Option<&'static [u8]>,
    ) -> DsrFilter {
        if self.pending.is_empty() {
            return DsrFilter {
                len: bytes_read,
                response,
            };
        }

        let mut output = Vec::with_capacity(self.pending.len() + bytes_read);
        output.extend_from_slice(&self.pending);
        output.extend_from_slice(&buffer[..bytes_read]);
        self.pending.clear();
        emit_output(buffer, &mut self.deferred, &output, response)
    }
}

pub(crate) struct DsrFilter {
    pub(crate) len: usize,
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

fn partial_dsr_prefix_len(bytes: &[u8]) -> usize {
    (1..DSR_REQUEST.len())
        .rev()
        .find(|len| bytes.ends_with(&DSR_REQUEST[..*len]))
        .unwrap_or(0)
}

fn emit_output(
    buffer: &mut [u8],
    deferred: &mut Vec<u8>,
    output: &[u8],
    response: Option<&'static [u8]>,
) -> DsrFilter {
    let len = output.len().min(buffer.len());
    buffer[..len].copy_from_slice(&output[..len]);
    deferred.clear();
    let remaining = &output[len..];
    deferred.extend_from_slice(&remaining[..remaining.len().min(MAX_DEFERRED_BYTES)]);
    DsrFilter { len, response }
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
            pending: Vec::new(),
            deferred: Vec::new(),
        };
        let data = b"before\x1b[6nafter";
        let mut bytes = [0; 32];
        bytes[..data.len()].copy_from_slice(data);

        let filtered = helper.filter(&mut bytes, data.len());

        assert_eq!(&bytes[..filtered.len], b"beforeafter");
        assert_eq!(filtered.response, Some(DSR_RESPONSE));
    }

    #[test]
    fn filter_detects_dsr_split_across_reads() {
        let mut helper = DsrBootstrap {
            deadline: Instant::now() + Duration::from_secs(1),
            completed: false,
            pending: Vec::new(),
            deferred: Vec::new(),
        };
        let mut first = [0; 16];
        first[..8].copy_from_slice(b"before\x1b[");

        let filtered = helper.filter(&mut first, 8);

        assert_eq!(&first[..filtered.len], b"before");
        assert_eq!(filtered.response, None);

        let mut second = [0; 16];
        second[..7].copy_from_slice(b"6nafter");
        let filtered = helper.filter(&mut second, 7);

        assert_eq!(&second[..filtered.len], b"after");
        assert_eq!(filtered.response, Some(DSR_RESPONSE));
    }

    #[test]
    fn filter_replays_false_partial_prefix_on_next_read() {
        let mut helper = DsrBootstrap {
            deadline: Instant::now() + Duration::from_secs(1),
            completed: false,
            pending: Vec::new(),
            deferred: Vec::new(),
        };
        let mut first = [0; 16];
        first[..8].copy_from_slice(b"before\x1b[");

        let filtered = helper.filter(&mut first, 8);
        assert_eq!(&first[..filtered.len], b"before");

        let mut second = [0; 16];
        second[..2].copy_from_slice(b"XX");
        let filtered = helper.filter(&mut second, 2);

        assert_eq!(&second[..filtered.len], b"\x1b[XX");
        assert_eq!(filtered.response, None);
    }

    #[test]
    fn deferred_output_is_bounded() {
        let mut buffer = [0_u8; 1];
        let mut deferred = Vec::new();
        let output = vec![b'x'; MAX_DEFERRED_BYTES + 32];

        let filtered = emit_output(&mut buffer, &mut deferred, &output, None);

        assert_eq!(filtered.len, 1);
        assert_eq!(deferred.len(), MAX_DEFERRED_BYTES);
    }
}
