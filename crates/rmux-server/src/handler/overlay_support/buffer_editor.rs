use super::super::{mode_tree_support::BufferEditRequest, RequestHandler};
use super::{
    completion::arm_popup_completion, identity::OverlayIdentity, parse::parse_display_popup,
};
use rmux_proto::RmuxError;
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::PathBuf;

struct EditorFile(PathBuf);
impl Drop for EditorFile {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}
impl EditorFile {
    fn create(content: &[u8]) -> Result<Self, RmuxError> {
        let mut random = [0u8; 16];
        getrandom::fill(&mut random).map_err(|error| RmuxError::Server(error.to_string()))?;
        let name = random
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>();
        let path = std::env::temp_dir().join(format!("rmux-buffer-{name}"));
        let mut options = OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut opened = options
            .open(&path)
            .map_err(|error| RmuxError::Server(error.to_string()))?;
        let file = Self(path);
        opened
            .write_all(content)
            .map_err(|error| RmuxError::Server(error.to_string()))?;
        Ok(file)
    }
}

impl RequestHandler {
    pub(in crate::handler) async fn open_buffer_editor(
        &self,
        request: BufferEditRequest,
    ) -> Result<(), RmuxError> {
        let Some(admission) = self.admit_display_popup(request.client).await else {
            return Ok(());
        };
        let worker = self.reserve_lifecycle_producer_task("rmux-buffer-editor")?;
        let file = EditorFile::create(&request.content)?;
        let quoted_path = {
            let state = self.state.lock().await;
            let profile = crate::terminal::TerminalProfile::for_run_shell(
                &state.environment,
                &state.options,
                Some(request.target.session_name()),
                Some(request.client.session_id().as_u32()),
                &self.socket_path(),
                !self.config_loading_active(),
                Some(&std::env::temp_dir()),
            )?;
            profile.quote_shell_path(&file.0)
        };
        let command = format!("{} {}", request.editor, quoted_path);
        let parsed = parse_display_popup(vec![
            "-E".into(),
            "-w".into(),
            "90%".into(),
            "-h".into(),
            "90%".into(),
            "-d".into(),
            std::env::temp_dir().to_string_lossy().into_owned(),
            command,
        ])?;
        let identity = {
            let mut state = self.state.lock().await;
            OverlayIdentity::capture(&mut state, request.client, request.target.clone())?
        };
        let Some(mut popup) = self
            .build_display_popup_state(
                request.client.attach_pid(),
                parsed,
                request.target.clone(),
                identity,
            )
            .await?
        else {
            return Ok(());
        };
        let receiver = arm_popup_completion(&mut popup, false).expect("editor completion armed");
        let installed = self
            .install_popup_state(&mut popup, Some(request.action))
            .await?;
        drop(admission);
        let popup_id = popup.id;
        let client = request.client;
        let handler = self.downgrade();
        drop(self.spawn_pre_admitted_lifecycle_producer_task_handle(
            "rmux-buffer-editor",
            worker,
            async move {
                let status = receiver.await.unwrap_or(129);
                let edited = if status == 0 {
                    crate::buffer_file_io::read(file.0.clone()).await.ok()
                } else {
                    None
                };
                if let Some(handler) = handler.upgrade() {
                    handler
                        .finish_buffer_editor(request, popup_id, edited)
                        .await;
                }
                drop(file);
            },
        ));
        let result = self.activate_installed_popup(&popup, installed).await;
        drop(popup);
        if result.is_err() {
            let _ = self
                .clear_interactive_overlay_for_identity(client, true)
                .await;
        }
        result
    }
}
