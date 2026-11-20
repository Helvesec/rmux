use std::env;
use std::fs;
use std::path::PathBuf;

const METRICS_FILE_ENV: &str = "RMUX_ATTACH_METRICS_FILE";
const LARGE_FRAME_THRESHOLD_BYTES: usize = 1024;

#[derive(Clone, Copy, Debug, Default, Eq, PartialEq)]
struct AttachMetrics {
    data_frames: u64,
    data_bytes: u64,
    max_frame_bytes: usize,
    large_frames: u64,
    full_clears: u64,
}

impl AttachMetrics {
    fn observe_data_frame(&mut self, bytes: &[u8]) {
        self.data_frames = self.data_frames.saturating_add(1);
        self.data_bytes = self.data_bytes.saturating_add(bytes.len() as u64);
        self.max_frame_bytes = self.max_frame_bytes.max(bytes.len());
        if bytes.len() >= LARGE_FRAME_THRESHOLD_BYTES {
            self.large_frames = self.large_frames.saturating_add(1);
        }
        if contains_full_clear(bytes) {
            self.full_clears = self.full_clears.saturating_add(1);
        }
    }

    fn to_json(self) -> String {
        format!(
            "{{\"schema\":1,\"data_frames\":{},\"data_bytes\":{},\"max_frame_bytes\":{},\"large_frame_threshold_bytes\":{},\"large_frames\":{},\"full_clears\":{}}}\n",
            self.data_frames,
            self.data_bytes,
            self.max_frame_bytes,
            LARGE_FRAME_THRESHOLD_BYTES,
            self.large_frames,
            self.full_clears
        )
    }
}

#[derive(Debug)]
pub(super) struct AttachMetricsRecorder {
    metrics: AttachMetrics,
    path: Option<PathBuf>,
}

impl AttachMetricsRecorder {
    pub(super) fn from_env() -> Self {
        Self {
            metrics: AttachMetrics::default(),
            path: env::var_os(METRICS_FILE_ENV).map(PathBuf::from),
        }
    }

    pub(super) fn observe_data_frame(&mut self, bytes: &[u8]) {
        self.metrics.observe_data_frame(bytes);
    }

    pub(super) fn flush(&mut self) {
        let Some(path) = self.path.take() else {
            return;
        };
        let _ = fs::write(path, self.metrics.to_json());
    }
}

fn contains_full_clear(bytes: &[u8]) -> bool {
    contains_subslice(bytes, b"\x1b[2J") || contains_subslice(bytes, b"\x1b[3J")
}

fn contains_subslice(haystack: &[u8], needle: &[u8]) -> bool {
    haystack
        .windows(needle.len())
        .any(|window| window == needle)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn attach_windows_metrics_count_full_clears_and_max_frame() {
        let mut metrics = AttachMetrics::default();

        metrics.observe_data_frame(b"abc");
        metrics.observe_data_frame(b"\x1b[H\x1b[2Jabcdef");
        metrics.observe_data_frame(&[b'x'; LARGE_FRAME_THRESHOLD_BYTES]);

        assert_eq!(metrics.data_frames, 3);
        assert_eq!(metrics.data_bytes, 16 + LARGE_FRAME_THRESHOLD_BYTES as u64);
        assert_eq!(metrics.max_frame_bytes, LARGE_FRAME_THRESHOLD_BYTES);
        assert_eq!(metrics.large_frames, 1);
        assert_eq!(metrics.full_clears, 1);
    }

    #[test]
    fn attach_windows_metrics_flushes_json_file() {
        let path = env::temp_dir().join(format!(
            "rmux-attach-metrics-test-{}.json",
            std::process::id()
        ));
        let mut recorder = AttachMetricsRecorder {
            metrics: AttachMetrics::default(),
            path: Some(path.clone()),
        };

        recorder.observe_data_frame(b"\x1b[3Jhello");
        recorder.flush();

        let json = fs::read_to_string(&path).expect("metrics json written");
        let _ = fs::remove_file(&path);
        assert!(json.contains("\"data_frames\":1"));
        assert!(json.contains("\"large_frame_threshold_bytes\":1024"));
        assert!(json.contains("\"full_clears\":1"));
    }
}
