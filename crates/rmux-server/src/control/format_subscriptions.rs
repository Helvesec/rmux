mod jobs;
use jobs::SubscriptionJobContext;
mod bounds;
mod wildcard;
#[cfg(all(test, unix))]
pub(crate) use bounds::MAX_CONTROL_FORMAT_SUBSCRIPTIONS;
pub(crate) use bounds::{
    validate_control_format_definition_batch, ControlFormatLimitExceeded,
    ControlFormatNotificationBatch,
};
use bounds::{ControlFormatLimits, ControlFormatNotification};
use wildcard::TargetValues;

use std::collections::HashMap;

use rmux_core::formats::FormatContext;
use rmux_core::{encode_paste_bytes, ManagedSessionView, Pane, Window};

use crate::client_format::{ClientFormatBindings, ClientLoopSnapshot};
use crate::format_runtime::{render_runtime_template, RuntimeFormatContext};
use crate::pane_terminals::HandlerState;

#[derive(Clone, Copy)]
pub(crate) struct SubscriptionContext<'a> {
    pub(crate) session: ManagedSessionView<'a>,
    pub(crate) subscriber: &'a ClientFormatBindings,
    pub(crate) clients: &'a [ClientLoopSnapshot],
}

#[derive(Debug, Clone)]
pub(crate) struct ControlFormatSubscriptions {
    entries: Vec<ControlFormatSubscription>,
    indexes: HashMap<String, usize>,
    limits: ControlFormatLimits,
}

#[derive(Debug, Clone)]
struct ControlFormatSubscription {
    jobs: SubscriptionJobContext,
    name: String,
    selector: ControlFormatSelector,
    template: String,
    last_value: Option<String>,
    target_values: TargetValues,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ControlFormatSelector {
    Current,
    AllPanes,
    AllWindows,
    PaneId(u32),
    WindowId(u32),
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ParsedControlTargetId {
    Target(u32),
    CurrentFallback,
    Missing,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum ControlFormatDefinition {
    Remove(String),
    Upsert {
        name: String,
        selector: ControlFormatSelector,
        template: String,
    },
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ControlFormatSnapshot {
    session_id: String,
    window_id: Option<String>,
    window_index: Option<u32>,
    pane_id: Option<String>,
    value: String,
}

impl Default for ControlFormatSubscriptions {
    fn default() -> Self {
        Self::with_limits(ControlFormatLimits::default())
    }
}

impl ControlFormatSubscriptions {
    fn with_limits(limits: ControlFormatLimits) -> Self {
        Self {
            entries: Vec::new(),
            indexes: HashMap::new(),
            limits,
        }
    }

    pub(crate) fn apply_definitions(
        &mut self,
        definitions: Vec<String>,
    ) -> Result<(), ControlFormatLimitExceeded> {
        let next = self.definitions_applied(&definitions)?;
        self.retire_replaced_jobs(&next);
        *self = next;
        Ok(())
    }

    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    fn definitions_applied(
        &self,
        definitions: &[String],
    ) -> Result<Self, ControlFormatLimitExceeded> {
        self.limits.validate_definition_batch(definitions)?;
        let mut next = self.clone();
        for definition in definitions {
            next.apply_definition(ControlFormatDefinition::parse(definition))?;
        }
        Ok(next)
    }

    fn apply_definition(
        &mut self,
        definition: ControlFormatDefinition,
    ) -> Result<(), ControlFormatLimitExceeded> {
        match definition {
            ControlFormatDefinition::Remove(name) => self.remove(&name),
            ControlFormatDefinition::Upsert {
                name,
                selector,
                template,
            } => self.upsert(name, selector, template)?,
        }
        Ok(())
    }

    fn remove(&mut self, name: &str) {
        let Some(index) = self.indexes.remove(name) else {
            return;
        };
        self.entries.remove(index);
        self.rebuild_indexes();
    }

    fn upsert(
        &mut self,
        name: String,
        selector: ControlFormatSelector,
        template: String,
    ) -> Result<(), ControlFormatLimitExceeded> {
        let entry = ControlFormatSubscription {
            jobs: SubscriptionJobContext {
                owner: crate::status_jobs::new_job_owner_id(),
                socket_path: None,
                runtime: None,
            },
            name: name.clone(),
            selector,
            template,
            last_value: None,
            target_values: TargetValues::default(),
        };
        if let Some(&index) = self.indexes.get(&name) {
            self.entries[index] = entry;
        } else {
            if self.entries.len() >= self.limits.max_subscriptions {
                return Err(ControlFormatLimitExceeded::new(
                    "registry",
                    self.limits.max_subscriptions,
                    "entries",
                ));
            }
            self.indexes.insert(name, self.entries.len());
            self.entries.push(entry);
        }
        self.ensure_registry_bytes()
    }

    fn rebuild_indexes(&mut self) {
        self.indexes.clear();
        for (index, entry) in self.entries.iter().enumerate() {
            self.indexes.insert(entry.name.clone(), index);
        }
    }

    fn ensure_registry_bytes(&self) -> Result<(), ControlFormatLimitExceeded> {
        if self.registry_bytes() > self.limits.max_registry_bytes {
            return Err(ControlFormatLimitExceeded::new(
                "registry",
                self.limits.max_registry_bytes,
                "bytes",
            ));
        }
        Ok(())
    }

    pub(crate) fn changed_notifications(
        &mut self,
        state: &HandlerState,
        context: Option<SubscriptionContext<'_>>,
    ) -> Result<ControlFormatNotificationBatch, ControlFormatLimitExceeded> {
        for entry in &mut self.entries {
            entry.target_values.prune(state);
        }
        self.ensure_cache_limits()?;
        let mut notifications = ControlFormatNotificationBatch::new(self.limits);
        let Some(context) = context else {
            return Ok(notifications);
        };

        for index in 0..self.entries.len() {
            match self.entries[index].selector {
                ControlFormatSelector::Missing => continue,
                ControlFormatSelector::Current => {}
                selector => {
                    self.changed_target_notifications(
                        index,
                        selector,
                        state,
                        context,
                        &mut notifications,
                    )?;
                    continue;
                }
            }
            let snapshot = self.entries[index].evaluate_current(state, context);
            if self.entries[index].last_value.as_ref() == Some(&snapshot.value) {
                continue;
            }
            let notification = self.entries[index].notification(&snapshot);
            notifications.ensure_can_push(&notification)?;
            self.ensure_last_value_budget(index, &snapshot.value)?;
            self.entries[index].last_value = Some(snapshot.value);
            notifications.push_unchecked(notification);
        }
        Ok(notifications)
    }

    fn registry_bytes(&self) -> usize {
        self.entries.iter().fold(0usize, |sum, entry| {
            sum.saturating_add(entry.registry_bytes())
        })
    }

    fn retained_value_bytes(&self) -> usize {
        self.entries.iter().fold(0usize, |sum, entry| {
            let last_value_bytes = entry
                .last_value
                .as_deref()
                .map(control_escaped_len)
                .unwrap_or(0);
            sum.saturating_add(last_value_bytes)
                .saturating_add(entry.target_values.encoded_value_bytes())
        })
    }

    fn target_count(&self) -> usize {
        self.entries.iter().fold(0usize, |sum, entry| {
            sum.saturating_add(entry.target_values.len())
        })
    }

    fn ensure_cache_limits(&self) -> Result<(), ControlFormatLimitExceeded> {
        if self.target_count() > self.limits.max_target_cache_entries {
            return Err(ControlFormatLimitExceeded::new(
                "target cache",
                self.limits.max_target_cache_entries,
                "entries",
            ));
        }
        if self.retained_value_bytes() > self.limits.max_retained_value_bytes {
            return Err(ControlFormatLimitExceeded::new(
                "retained values",
                self.limits.max_retained_value_bytes,
                "encoded bytes",
            ));
        }
        Ok(())
    }

    fn ensure_last_value_budget(
        &self,
        entry_index: usize,
        new_value: &str,
    ) -> Result<(), ControlFormatLimitExceeded> {
        let old_bytes = self.entries[entry_index]
            .last_value
            .as_deref()
            .map(control_escaped_len)
            .unwrap_or(0);
        let next_bytes = self
            .retained_value_bytes()
            .saturating_sub(old_bytes)
            .saturating_add(control_escaped_len(new_value));
        if next_bytes > self.limits.max_retained_value_bytes {
            return Err(ControlFormatLimitExceeded::new(
                "retained values",
                self.limits.max_retained_value_bytes,
                "encoded bytes",
            ));
        }
        Ok(())
    }
}

impl ControlFormatSubscription {
    fn registry_bytes(&self) -> usize {
        control_escaped_len(&self.name)
            .saturating_add(self.selector.registry_bytes())
            .saturating_add(self.template.len())
    }

    fn evaluate_current(
        &self,
        state: &HandlerState,
        context: SubscriptionContext<'_>,
    ) -> ControlFormatSnapshot {
        let session = context.session;
        let window = session.window();
        let value = render_subscription_value(
            state,
            context,
            session.active_window_index(),
            window,
            window.active_pane(),
            (&self.template, &self.jobs),
        );
        ControlFormatSnapshot {
            session_id: session.id().to_string(),
            window_id: None,
            window_index: None,
            pane_id: None,
            value,
        }
    }

    fn notification(&self, snapshot: &ControlFormatSnapshot) -> ControlFormatNotification {
        ControlFormatNotification {
            name: self.name.clone(),
            session_id: snapshot.session_id.clone(),
            window_id: snapshot.window_id.clone(),
            window_index: snapshot.window_index,
            pane_id: snapshot.pane_id.clone(),
            value: snapshot.value.clone(),
        }
    }
}

impl ControlFormatDefinition {
    fn parse(raw: &str) -> Self {
        let mut fields = raw.splitn(3, ':');
        let name = fields.next().unwrap_or_default().to_owned();
        let Some(selector) = fields.next() else {
            return Self::Remove(name);
        };
        let Some(template) = fields.next() else {
            return Self::Remove(name);
        };
        Self::Upsert {
            name,
            selector: ControlFormatSelector::parse(selector),
            template: template.to_owned(),
        }
    }
}

impl ControlFormatSelector {
    fn parse(raw: &str) -> Self {
        if raw == "%*" {
            return Self::AllPanes;
        }
        if raw == "@*" {
            return Self::AllWindows;
        }
        if raw.is_empty() {
            return Self::Current;
        }
        if let Some(pane_id) = raw.strip_prefix('%') {
            return match parse_control_target_id_prefix(pane_id) {
                ParsedControlTargetId::Target(id) => Self::PaneId(id),
                ParsedControlTargetId::CurrentFallback => Self::Current,
                ParsedControlTargetId::Missing => Self::Missing,
            };
        }
        if let Some(window_id) = raw.strip_prefix('@') {
            return match parse_control_target_id_prefix(window_id) {
                ParsedControlTargetId::Target(id) => Self::WindowId(id),
                ParsedControlTargetId::CurrentFallback => Self::Current,
                ParsedControlTargetId::Missing => Self::Missing,
            };
        }
        Self::Current
    }

    fn registry_bytes(self) -> usize {
        match self {
            Self::Current | Self::Missing => 0,
            Self::AllPanes | Self::AllWindows => 2,
            Self::PaneId(id) | Self::WindowId(id) => 1usize.saturating_add(decimal_u32_len(id)),
        }
    }
}

fn render_subscription_value(
    state: &HandlerState,
    context: SubscriptionContext<'_>,
    window_index: u32,
    window: &Window,
    pane: Option<&Pane>,
    job_template: (&str, &SubscriptionJobContext),
) -> String {
    let (template, jobs) = job_template;
    let session = context.session;
    let active_window_index = session.active_window_index();
    let mut base = FormatContext::new()
        .with_managed_session(session)
        .with_window(
            window_index,
            window,
            window_index == active_window_index,
            Some(window_index) == session.last_window_index(),
        );
    if let Some(pane) = pane {
        base = base.with_window_pane(window, pane);
    }

    let mut runtime = RuntimeFormatContext::new(base)
        .with_state(state)
        .with_session(session)
        .with_window(window_index, window);
    if let Some(pane) = pane {
        runtime = runtime.with_pane(pane);
    }
    runtime = context
        .subscriber
        .apply(runtime.with_client_loop_snapshots(context.clients));
    if let Some(socket_path) = jobs.socket_path.as_ref() {
        runtime = runtime.with_socket_path(socket_path).with_format_jobs(
            crate::status_jobs::StatusJobOwner::Subscription(jobs.owner),
            std::time::Duration::from_secs(1),
        );
    }
    render_runtime_template(template, &runtime, false)
}

fn control_escape(value: &str) -> String {
    String::from_utf8_lossy(&control_escaped_bytes(value)).into_owned()
}

fn control_escaped_len(value: &str) -> usize {
    control_escaped_bytes(value).len()
}

fn control_escaped_bytes(value: &str) -> Vec<u8> {
    // Double literal backslashes before generating control-character escapes.
    let value = value.replace('\\', "\\\\");
    // Control records escape CR and TAB even though pane pastes keep them raw.
    let value = value.replace('\r', "\\r").replace('\t', "\\t");
    encode_paste_bytes(value.as_bytes())
}

fn decimal_u32_len(value: u32) -> usize {
    value.to_string().len()
}

fn is_control_target_c_space(byte: u8) -> bool {
    matches!(byte, b' ' | b'\t' | b'\n' | 0x0b | 0x0c | b'\r')
}

fn parse_control_target_id_prefix(raw: &str) -> ParsedControlTargetId {
    let bytes = raw.as_bytes();
    let mut index = bytes
        .iter()
        .take_while(|byte| is_control_target_c_space(**byte))
        .count();
    let Some(first) = bytes.get(index).copied() else {
        return ParsedControlTargetId::CurrentFallback;
    };

    let negative = match first {
        b'+' => {
            index += 1;
            false
        }
        b'-' => {
            index += 1;
            true
        }
        _ => false,
    };

    let digit_start = index;
    index += bytes[index..]
        .iter()
        .take_while(|byte| byte.is_ascii_digit())
        .count();
    if index == digit_start {
        return ParsedControlTargetId::CurrentFallback;
    }
    let digits = &raw[digit_start..index];
    if negative && digits.bytes().any(|byte| byte != b'0') {
        return ParsedControlTargetId::CurrentFallback;
    }
    match digits.parse::<u32>() {
        Ok(id) => ParsedControlTargetId::Target(id),
        Err(_) => ParsedControlTargetId::Missing,
    }
}

#[cfg(test)]
#[path = "format_subscriptions/tests.rs"]
mod tests;

#[cfg(test)]
#[path = "format_subscriptions/wildcard_tests.rs"]
mod wildcard_tests;
