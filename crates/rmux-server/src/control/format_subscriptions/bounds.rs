use super::{control_escape, control_escaped_len, decimal_u32_len};

const MAX_CONTROL_FORMAT_DEFINITION_BATCH: usize = 256;
const MAX_CONTROL_FORMAT_DEFINITION_BATCH_BYTES: usize = 1024 * 1024;
pub(crate) const MAX_CONTROL_FORMAT_SUBSCRIPTIONS: usize = 256;
const MAX_CONTROL_FORMAT_REGISTRY_BYTES: usize = 1024 * 1024;
const MAX_CONTROL_FORMAT_RETAINED_VALUE_BYTES: usize = 4 * 1024 * 1024;
const MAX_CONTROL_FORMAT_TARGET_CACHE_ENTRIES: usize = 1024;
const MAX_CONTROL_FORMAT_NOTIFICATION_BATCH: usize = 1024;
const MAX_CONTROL_FORMAT_NOTIFICATION_BYTES: usize = 4 * 1024 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct ControlFormatLimits {
    pub(crate) max_definition_batch: usize,
    pub(crate) max_definition_batch_bytes: usize,
    pub(crate) max_subscriptions: usize,
    pub(crate) max_registry_bytes: usize,
    pub(crate) max_retained_value_bytes: usize,
    pub(crate) max_target_cache_entries: usize,
    pub(crate) max_notification_batch: usize,
    pub(crate) max_notification_bytes: usize,
}

impl Default for ControlFormatLimits {
    fn default() -> Self {
        Self {
            max_definition_batch: MAX_CONTROL_FORMAT_DEFINITION_BATCH,
            max_definition_batch_bytes: MAX_CONTROL_FORMAT_DEFINITION_BATCH_BYTES,
            max_subscriptions: MAX_CONTROL_FORMAT_SUBSCRIPTIONS,
            max_registry_bytes: MAX_CONTROL_FORMAT_REGISTRY_BYTES,
            max_retained_value_bytes: MAX_CONTROL_FORMAT_RETAINED_VALUE_BYTES,
            max_target_cache_entries: MAX_CONTROL_FORMAT_TARGET_CACHE_ENTRIES,
            max_notification_batch: MAX_CONTROL_FORMAT_NOTIFICATION_BATCH,
            max_notification_bytes: MAX_CONTROL_FORMAT_NOTIFICATION_BYTES,
        }
    }
}

impl ControlFormatLimits {
    pub(crate) fn validate_definition_batch(
        self,
        definitions: &[String],
    ) -> Result<(), ControlFormatLimitExceeded> {
        if definitions.len() > self.max_definition_batch {
            return Err(ControlFormatLimitExceeded::new(
                "definition batch",
                self.max_definition_batch,
                "entries",
            ));
        }
        let bytes = definitions.iter().fold(0usize, |sum, definition| {
            sum.saturating_add(definition.len())
        });
        if bytes > self.max_definition_batch_bytes {
            return Err(ControlFormatLimitExceeded::new(
                "definition batch",
                self.max_definition_batch_bytes,
                "bytes",
            ));
        }
        Ok(())
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ControlFormatLimitExceeded {
    message: String,
}

impl ControlFormatLimitExceeded {
    pub(crate) fn new(metric: &'static str, limit: usize, unit: &'static str) -> Self {
        Self {
            message: format!("control format subscription {metric} exceeds {limit} {unit}"),
        }
    }

    #[cfg(test)]
    pub(crate) fn message(&self) -> &str {
        &self.message
    }

    pub(crate) fn into_message(self) -> String {
        self.message
    }
}

pub(crate) fn validate_control_format_definition_batch(
    definitions: &[String],
) -> Result<(), ControlFormatLimitExceeded> {
    ControlFormatLimits::default().validate_definition_batch(definitions)
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ControlFormatNotification {
    pub(crate) name: String,
    pub(crate) session_id: String,
    pub(crate) window_id: Option<String>,
    pub(crate) window_index: Option<u32>,
    pub(crate) pane_id: Option<String>,
    pub(crate) value: String,
}

#[derive(Debug)]
pub(crate) struct ControlFormatNotificationBatch {
    notifications: Vec<ControlFormatNotification>,
    encoded_bytes: usize,
    limits: ControlFormatLimits,
}

impl ControlFormatNotification {
    pub(crate) fn into_line(self) -> String {
        format!(
            "%subscription-changed {} {} {} {} {} : {}",
            control_escape(&self.name),
            self.session_id,
            self.window_id.as_deref().unwrap_or("-"),
            self.window_index
                .map(|index| index.to_string())
                .unwrap_or_else(|| "-".to_owned()),
            self.pane_id.as_deref().unwrap_or("-"),
            control_escape(&self.value),
        )
    }

    pub(crate) fn encoded_bytes(&self) -> usize {
        "%subscription-changed "
            .len()
            .saturating_add(control_escaped_len(&self.name))
            .saturating_add(1)
            .saturating_add(self.session_id.len())
            .saturating_add(1)
            .saturating_add(self.window_id.as_deref().unwrap_or("-").len())
            .saturating_add(1)
            .saturating_add(
                self.window_index
                    .map(decimal_u32_len)
                    .unwrap_or_else(|| "-".len()),
            )
            .saturating_add(1)
            .saturating_add(self.pane_id.as_deref().unwrap_or("-").len())
            .saturating_add(" : ".len())
            .saturating_add(control_escaped_len(&self.value))
            .saturating_add(1)
    }
}

impl ControlFormatNotificationBatch {
    pub(crate) fn new(limits: ControlFormatLimits) -> Self {
        Self {
            notifications: Vec::new(),
            encoded_bytes: 0,
            limits,
        }
    }

    pub(crate) fn ensure_can_push(
        &self,
        notification: &ControlFormatNotification,
    ) -> Result<(), ControlFormatLimitExceeded> {
        if self.notifications.len() >= self.limits.max_notification_batch {
            return Err(ControlFormatLimitExceeded::new(
                "notification batch",
                self.limits.max_notification_batch,
                "entries",
            ));
        }
        let next_bytes = self
            .encoded_bytes
            .saturating_add(notification.encoded_bytes());
        if next_bytes > self.limits.max_notification_bytes {
            return Err(ControlFormatLimitExceeded::new(
                "notification batch",
                self.limits.max_notification_bytes,
                "encoded bytes",
            ));
        }
        Ok(())
    }

    pub(crate) fn push_unchecked(&mut self, notification: ControlFormatNotification) {
        self.encoded_bytes = self
            .encoded_bytes
            .saturating_add(notification.encoded_bytes());
        self.notifications.push(notification);
    }

    pub(crate) fn into_notifications(self) -> Vec<ControlFormatNotification> {
        self.notifications
    }

    #[cfg(test)]
    pub(crate) fn into_lines(self) -> Vec<String> {
        self.notifications
            .into_iter()
            .map(ControlFormatNotification::into_line)
            .collect()
    }
}
