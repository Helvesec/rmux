use super::*;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct MessageEntry {
    pub(crate) msg_time: i64,
    pub(crate) msg_num: u64,
    pub(crate) msg: String,
}

impl HandlerState {
    pub(crate) fn add_message(&mut self, message: impl Into<String>) {
        let message = message.into();
        let msg_num = self.next_message_number;
        self.next_message_number = self.next_message_number.saturating_add(1);
        self.message_log.push_back(MessageEntry {
            msg_time: chrono::Local::now().timestamp(),
            msg_num,
            msg: message,
        });

        self.trim_message_log();
    }

    pub(crate) fn trim_message_log(&mut self) {
        let limit = self.message_limit();
        while self.message_log.len() > limit {
            let _ = self.message_log.pop_front();
        }
    }

    fn message_limit(&self) -> usize {
        self.options
            .resolve(None, OptionName::MessageLimit)
            .and_then(|value| value.parse::<usize>().ok())
            .unwrap_or(1000)
    }
}
