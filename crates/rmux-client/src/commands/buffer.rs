use rmux_proto::{
    CapturePaneRequest, CapturePaneTargetActionRequest, ClearHistoryRequest, DeleteBufferRequest,
    ListBuffersRequest, LoadBufferRequest, PaneTarget, PasteBufferRequest, Request, Response,
    SaveBufferRequest, SetBufferRequest, ShowBufferRequest,
};
use std::path::{Path, PathBuf};

use crate::{connection::Connection, ClientError};

impl Connection {
    /// Sends a `set-buffer` request over the detached RPC channel.
    pub fn set_buffer(
        &mut self,
        name: Option<String>,
        content: Vec<u8>,
        append: bool,
        new_name: Option<String>,
        set_clipboard: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::SetBuffer(SetBufferRequest {
            name,
            content,
            append,
            new_name,
            set_clipboard,
        }))
    }

    /// Sends a `show-buffer` request over the detached RPC channel.
    pub fn show_buffer(&mut self, name: Option<String>) -> Result<Response, ClientError> {
        self.roundtrip(&Request::ShowBuffer(ShowBufferRequest { name }))
    }

    /// Sends a `paste-buffer` request over the detached RPC channel.
    #[allow(clippy::too_many_arguments)]
    pub fn paste_buffer(
        &mut self,
        name: Option<String>,
        target: PaneTarget,
        delete_after: bool,
        separator: Option<String>,
        linefeed: bool,
        raw: bool,
        bracketed: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::PasteBuffer(Box::new(PasteBufferRequest {
            name,
            target,
            delete_after,
            separator,
            linefeed,
            raw,
            bracketed,
        })))
    }

    /// Sends a `list-buffers` request over the detached RPC channel.
    pub fn list_buffers(
        &mut self,
        format: Option<String>,
        filter: Option<String>,
        sort_order: Option<String>,
        reversed: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::ListBuffers(ListBuffersRequest {
            format,
            filter,
            sort_order,
            reversed,
        }))
    }

    /// Sends a `delete-buffer` request over the detached RPC channel.
    pub fn delete_buffer(&mut self, name: Option<String>) -> Result<Response, ClientError> {
        self.roundtrip(&Request::DeleteBuffer(DeleteBufferRequest { name }))
    }

    /// Sends a `load-buffer` request over the detached RPC channel.
    pub fn load_buffer(
        &mut self,
        path: String,
        name: Option<String>,
        set_clipboard: bool,
    ) -> Result<Response, ClientError> {
        let cwd = caller_cwd_for_path(&path)?;
        self.roundtrip(&Request::LoadBuffer(LoadBufferRequest {
            path,
            cwd,
            name,
            set_clipboard,
        }))
    }

    /// Sends a `save-buffer` request over the detached RPC channel.
    pub fn save_buffer(
        &mut self,
        path: String,
        name: Option<String>,
        append: bool,
    ) -> Result<Response, ClientError> {
        let cwd = caller_cwd_for_path(&path)?;
        self.roundtrip(&Request::SaveBuffer(SaveBufferRequest {
            path,
            cwd,
            name,
            append,
        }))
    }

    /// Sends a `capture-pane` request over the detached RPC channel.
    pub fn capture_pane(&mut self, request: CapturePaneRequest) -> Result<Response, ClientError> {
        self.roundtrip(&Request::CapturePane(Box::new(request)))
    }

    /// Sends a `capture-pane` request with server-side target resolution.
    pub fn capture_pane_target_action(
        &mut self,
        request: CapturePaneTargetActionRequest,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::CapturePaneTargetAction(Box::new(request)))
    }

    /// Sends a `clear-history` request over the detached RPC channel.
    pub fn clear_history(
        &mut self,
        target: PaneTarget,
        reset_hyperlinks: bool,
    ) -> Result<Response, ClientError> {
        self.roundtrip(&Request::ClearHistory(ClearHistoryRequest {
            target,
            reset_hyperlinks,
        }))
    }
}

fn caller_cwd_for_path(path: &str) -> Result<Option<PathBuf>, ClientError> {
    if Path::new(path).is_absolute() {
        Ok(None)
    } else {
        std::env::current_dir().map(Some).map_err(Into::into)
    }
}
