use rmux_core::KeyCode;
use rmux_proto::{PaneTarget, RmuxError};

use super::RequestHandler;
use crate::handler::{mode_tree_support::ModeTreeInputError, prompt_support::decode_prompt_key};
use crate::keys::{parse_key_code, resolve_hex_key};

// Like tmux, named keys are single events, unknown names are literal characters,
// and malformed hexadecimal arguments contribute no events.
pub(super) fn key_codes(token: &str, hex: bool, literal: bool) -> Vec<KeyCode> {
    if hex {
        return resolve_hex_key(token)
            .map(KeyCode::from)
            .into_iter()
            .collect();
    }
    if !literal {
        if let Some(key) = parse_key_code(token) {
            return vec![key];
        }
    }
    token
        .chars()
        .map(|character| character as KeyCode)
        .collect()
}

impl RequestHandler {
    pub(super) async fn consume_pane_mode_key_tokens(
        &self,
        requester_pid: u32,
        target: &PaneTarget,
        tokens: &[String],
    ) -> Result<Vec<String>, RmuxError> {
        for (index, token) in tokens.iter().enumerate() {
            if let Some(client) = self.mode_tree_client_for_target(target).await {
                let origin = self.capture_requester_origin(requester_pid).await;
                let _access = self.require_requester_origin_write(&origin).await?;
                for key in key_codes(token, false, false) {
                    self.handle_mode_tree_key_event_for_identity(client, decode_prompt_key(key))
                        .await
                        .map_err(ModeTreeInputError::into_rmux_error)?;
                }
                continue;
            }
            // Only an argv boundary may switch from copy mode to pane input.
            if !self.target_is_in_copy_mode(target).await? {
                return Ok(tokens[index..].to_vec());
            }
            for key in key_codes(token, false, false) {
                // A binding such as q may leave the mode midway through this
                // argument. Its remaining characters still belong to the mode.
                if !self.target_is_in_copy_mode(target).await?
                    || !self
                        .handle_detached_copy_mode_key_code(requester_pid, target.clone(), key)
                        .await?
                {
                    return Err(RmuxError::Server("not in a mode".to_owned()));
                }
            }
        }
        Ok(Vec::new())
    }
}
