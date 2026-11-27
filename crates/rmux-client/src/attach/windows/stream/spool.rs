use std::collections::VecDeque;
use std::fs::{read_dir, remove_file, File, OpenOptions};
use std::io::{self, Read, Seek, SeekFrom, Write};
use std::os::windows::fs::OpenOptionsExt;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::OnceLock;

use tracing::warn;
use windows_sys::Win32::Foundation::{
    CloseHandle, GetLastError, ERROR_INVALID_PARAMETER, STILL_ACTIVE,
};
use windows_sys::Win32::Storage::FileSystem::{
    FILE_ATTRIBUTE_TEMPORARY, FILE_FLAG_DELETE_ON_CLOSE,
};
use windows_sys::Win32::System::Threading::{
    GetExitCodeProcess, OpenProcess, PROCESS_QUERY_LIMITED_INFORMATION,
};

use crate::ClientError;

use super::output_queue::{AttachOutputFrame, AttachOutputFrameKind};

pub(super) const ATTACH_OUTPUT_SPOOL_MAX_BYTES: u64 = 64 * 1024 * 1024;
const ATTACH_OUTPUT_SPOOL_PREFIX: &str = "rmux-attach-output-";
const ATTACH_OUTPUT_SPOOL_SUFFIX: &str = ".spool";

#[derive(Default)]
pub(super) struct AttachOutputSpool {
    pub(super) file: Option<File>,
    pub(super) path: Option<PathBuf>,
    pub(super) frames: VecDeque<AttachOutputSpoolFrame>,
    pub(super) end_offset: u64,
    pub(super) outstanding_bytes: u64,
    strict_frames: usize,
    render_frames: usize,
}

#[derive(Debug)]
pub(super) struct AttachOutputSpoolFrame {
    pub(super) kind: AttachOutputFrameKind,
    pub(super) offset: u64,
    pub(super) len: usize,
}

impl AttachOutputSpool {
    pub(super) fn push(&mut self, frame: AttachOutputFrame) -> io::Result<()> {
        self.push_with_limit(frame, ATTACH_OUTPUT_SPOOL_MAX_BYTES)
    }

    pub(super) fn push_with_limit(
        &mut self,
        frame: AttachOutputFrame,
        max_bytes: u64,
    ) -> io::Result<()> {
        let replaced_tail_len = self.tail_render_len_if_replacing(frame.kind);
        let outstanding_after_replace = self
            .outstanding_bytes
            .saturating_sub(replaced_tail_len.unwrap_or(0) as u64);
        let next_outstanding = checked_spool_end_offset(outstanding_after_replace, frame.len())?;
        if next_outstanding > max_bytes {
            return Err(io::Error::other(format!(
                "attach output spool exceeded {max_bytes} bytes"
            )));
        }
        self.replace_tail_render_if_needed(frame.kind)?;
        let offset = self.next_write_offset(frame.kind);
        let len = frame.len();
        let next_end_offset = checked_spool_end_offset(offset, len)?;
        if self.should_compact_for_write(next_end_offset, max_bytes) {
            self.compact()?;
        }
        let offset = self.next_write_offset(frame.kind);
        let next_end_offset = checked_spool_end_offset(offset, len)?;
        let physical_limit = spool_physical_limit(max_bytes);
        if next_end_offset > physical_limit {
            return Err(io::Error::other(format!(
                "attach output spool exceeded {physical_limit} bytes"
            )));
        }
        let file = self.file()?;
        file.seek(SeekFrom::Start(offset))?;
        file.write_all(&frame.bytes)?;
        self.end_offset = next_end_offset;
        self.outstanding_bytes = next_outstanding;
        self.add_frame_kind(frame.kind);
        self.frames.push_back(AttachOutputSpoolFrame {
            kind: frame.kind,
            offset,
            len,
        });
        Ok(())
    }

    pub(super) fn pop(&mut self) -> io::Result<Option<AttachOutputFrame>> {
        let Some(frame) = self.frames.pop_front() else {
            self.cleanup()?;
            return Ok(None);
        };
        let file = self.file()?;
        file.seek(SeekFrom::Start(frame.offset))?;
        let mut bytes = vec![0_u8; frame.len];
        file.read_exact(&mut bytes)?;
        self.outstanding_bytes = self.outstanding_bytes.saturating_sub(frame.len as u64);
        self.remove_frame_kind(frame.kind);
        if self.frames.is_empty() {
            self.cleanup()?;
        }
        Ok(Some(AttachOutputFrame::new(frame.kind, bytes)))
    }

    pub(super) fn contains_strict(&self) -> bool {
        self.strict_frames > 0
    }

    pub(super) fn contains_render(&self) -> bool {
        self.render_frames > 0
    }

    pub(super) fn is_empty(&self) -> bool {
        self.frames.is_empty()
    }

    fn next_write_offset(&self, kind: AttachOutputFrameKind) -> u64 {
        if kind == AttachOutputFrameKind::Render {
            if let Some(frame) = self.frames.back().filter(|frame| frame.is_render()) {
                return frame.offset;
            }
        }
        self.end_offset
    }

    fn should_compact_for_write(&self, next_end_offset: u64, max_bytes: u64) -> bool {
        if next_end_offset <= max_bytes {
            return false;
        }
        next_end_offset > spool_physical_limit(max_bytes)
            || self.reclaimable_bytes() >= spool_compact_threshold(max_bytes)
    }

    fn reclaimable_bytes(&self) -> u64 {
        self.frames.front().map_or(0, |frame| frame.offset)
    }

    fn tail_render_len_if_replacing(&self, kind: AttachOutputFrameKind) -> Option<usize> {
        if kind != AttachOutputFrameKind::Render {
            return None;
        }
        self.frames
            .back()
            .filter(|frame| frame.is_render())
            .map(|frame| frame.len)
    }

    fn replace_tail_render_if_needed(&mut self, kind: AttachOutputFrameKind) -> io::Result<()> {
        if kind != AttachOutputFrameKind::Render
            || !self
                .frames
                .back()
                .is_some_and(|frame| frame.kind == AttachOutputFrameKind::Render)
        {
            return Ok(());
        }

        let Some(frame) = self.frames.pop_back() else {
            return Ok(());
        };
        self.outstanding_bytes = self.outstanding_bytes.saturating_sub(frame.len as u64);
        self.remove_frame_kind(frame.kind);
        self.end_offset = frame.offset;
        if let Some(file) = self.file.as_mut() {
            file.set_len(self.end_offset)?;
        }
        Ok(())
    }

    fn compact(&mut self) -> io::Result<()> {
        if self.frames.is_empty() {
            self.cleanup()?;
            return Ok(());
        }
        let file = self.file.as_mut().ok_or_else(|| {
            io::Error::other("attach output spool has frames without a backing file")
        })?;
        let mut next_offset = 0_u64;
        for frame in &mut self.frames {
            if frame.offset != next_offset {
                file.seek(SeekFrom::Start(frame.offset))?;
                let mut bytes = vec![0_u8; frame.len];
                file.read_exact(&mut bytes)?;
                file.seek(SeekFrom::Start(next_offset))?;
                file.write_all(&bytes)?;
                frame.offset = next_offset;
            }
            next_offset = checked_spool_end_offset(next_offset, frame.len)?;
        }
        file.set_len(next_offset)?;
        self.end_offset = next_offset;
        Ok(())
    }

    fn file(&mut self) -> io::Result<&mut File> {
        if self.file.is_none() {
            let path = attach_output_spool_path();
            let file = open_attach_output_spool_file(&path)?;
            self.path = Some(path);
            self.file = Some(file);
            self.end_offset = 0;
        }
        Ok(self
            .file
            .as_mut()
            .expect("attach output spool file exists after initialization"))
    }

    pub(super) fn cleanup(&mut self) -> io::Result<()> {
        self.frames.clear();
        self.end_offset = 0;
        self.outstanding_bytes = 0;
        self.strict_frames = 0;
        self.render_frames = 0;
        drop(self.file.take());
        if let Some(path) = self.path.take() {
            match remove_file(path) {
                Ok(()) => {}
                Err(error) if error.kind() == io::ErrorKind::NotFound => {}
                Err(error) => return Err(error),
            }
        }
        Ok(())
    }

    fn add_frame_kind(&mut self, kind: AttachOutputFrameKind) {
        match kind {
            AttachOutputFrameKind::Strict => {
                self.strict_frames = self.strict_frames.saturating_add(1);
            }
            AttachOutputFrameKind::Render => {
                self.render_frames = self.render_frames.saturating_add(1);
            }
        }
    }

    fn remove_frame_kind(&mut self, kind: AttachOutputFrameKind) {
        match kind {
            AttachOutputFrameKind::Strict => {
                self.strict_frames = self.strict_frames.saturating_sub(1);
            }
            AttachOutputFrameKind::Render => {
                self.render_frames = self.render_frames.saturating_sub(1);
            }
        }
    }
}

impl AttachOutputSpoolFrame {
    fn is_render(&self) -> bool {
        self.kind == AttachOutputFrameKind::Render
    }
}

impl Drop for AttachOutputSpool {
    fn drop(&mut self) {
        let _ = self.cleanup();
    }
}

fn attach_output_spool_path() -> PathBuf {
    static SPOOL_COUNTER: AtomicU64 = AtomicU64::new(0);
    let sequence = SPOOL_COUNTER.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(attach_output_spool_file_name(std::process::id(), sequence))
}

fn open_attach_output_spool_file(path: &Path) -> io::Result<File> {
    let mut options = OpenOptions::new();
    options
        .create_new(true)
        .read(true)
        .write(true)
        .share_mode(0)
        .custom_flags(FILE_ATTRIBUTE_TEMPORARY | FILE_FLAG_DELETE_ON_CLOSE);
    options.open(path)
}

fn checked_spool_end_offset(offset: u64, len: usize) -> io::Result<u64> {
    let len = u64::try_from(len)
        .map_err(|_| io::Error::other("attach output frame is too large to spool"))?;
    offset
        .checked_add(len)
        .ok_or_else(|| io::Error::other("attach output spool offset overflowed"))
}

fn spool_compact_threshold(max_bytes: u64) -> u64 {
    (max_bytes / 4).max(1)
}

fn spool_physical_limit(max_bytes: u64) -> u64 {
    max_bytes.saturating_add(spool_compact_threshold(max_bytes))
}

pub(super) fn cleanup_orphaned_attach_output_spools() {
    static CLEANED: OnceLock<()> = OnceLock::new();
    CLEANED.get_or_init(cleanup_orphaned_attach_output_spools_now);
}

pub(super) fn cleanup_orphaned_attach_output_spools_now() {
    let Ok(entries) = read_dir(std::env::temp_dir()) else {
        return;
    };
    let current_pid = std::process::id();
    for entry in entries.flatten() {
        let path = entry.path();
        let Some(file_name) = path.file_name().and_then(|name| name.to_str()) else {
            continue;
        };
        let Some(pid) = attach_output_spool_pid(file_name) else {
            continue;
        };
        if pid == current_pid || attach_output_spool_owner_is_running(pid) {
            continue;
        }
        if let Err(error) = remove_file(&path) {
            if error.kind() != io::ErrorKind::NotFound {
                warn!(
                    path = %path.display(),
                    "failed to remove orphaned attach output spool: {error}"
                );
            }
        }
    }
}

pub(super) fn attach_output_spool_pid(file_name: &str) -> Option<u32> {
    let rest = file_name.strip_prefix(ATTACH_OUTPUT_SPOOL_PREFIX)?;
    let rest = rest.strip_suffix(ATTACH_OUTPUT_SPOOL_SUFFIX)?;
    let (pid, sequence) = rest.split_once('-')?;
    sequence.parse::<u64>().ok()?;
    pid.parse().ok()
}

fn attach_output_spool_owner_is_running(pid: u32) -> bool {
    if pid == 0 {
        return false;
    }
    // SAFETY: OpenProcess is called with a plain PID and no inherited handle;
    // the returned handle is checked for null before use, passed only to
    // GetExitCodeProcess with a valid out-pointer to a local, and always
    // released with CloseHandle on the non-null path.
    unsafe {
        let handle = OpenProcess(PROCESS_QUERY_LIMITED_INFORMATION, 0, pid);
        if handle.is_null() {
            return GetLastError() != ERROR_INVALID_PARAMETER;
        }
        let mut exit_code = 0;
        let running =
            GetExitCodeProcess(handle, &mut exit_code) != 0 && exit_code == STILL_ACTIVE as u32;
        let _ = CloseHandle(handle);
        running
    }
}

pub(super) fn attach_output_spool_file_name(pid: u32, sequence: u64) -> String {
    format!("{ATTACH_OUTPUT_SPOOL_PREFIX}{pid}-{sequence}{ATTACH_OUTPUT_SPOOL_SUFFIX}")
}

pub(super) fn spool_error(error: io::Error) -> ClientError {
    ClientError::Io(io::Error::new(
        error.kind(),
        format!("failed to spool blocked attach output: {error}"),
    ))
}
