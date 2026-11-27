use std::path::Path;
use std::time::Duration;

use super::RuntimeFormatContext;
use crate::status_jobs::{StatusJobOwner, StatusJobScope};

#[derive(Clone)]
pub(super) struct FormatJobContext {
    owner: StatusJobOwner,
    ttl: Duration,
}

impl RuntimeFormatContext<'_> {
    pub(crate) fn with_format_jobs(mut self, owner: StatusJobOwner, ttl: Duration) -> Self {
        self.jobs = Some(FormatJobContext { owner, ttl });
        self
    }

    pub(crate) fn with_status_jobs(self, ttl: Duration) -> Self {
        let client = self
            .client_bindings
            .as_ref()
            .and_then(|client| client.job_identity);
        self.with_format_jobs(StatusJobOwner::Status(client), ttl)
    }

    pub(super) fn job_output(&self, source: &str, command: &str) -> Option<String> {
        let jobs = self.jobs.as_ref()?;
        let runtime = self.status_jobs()?;
        let profile = self.status_job_profile();
        if self.session.is_some() && self.base_socket_path().is_some() && profile.is_none() {
            return Some(String::new());
        }
        let scope = StatusJobScope {
            owner: jobs.owner.clone(),
            session: self.session.map(|session| session.id().as_u32()),
            window: self.window.map(|window| window.id().as_u32()),
            window_index: self.window_index,
            pane: self.pane.map(|pane| pane.id().as_u32()),
            client: self
                .client_bindings
                .as_ref()
                .and_then(|client| client.job_identity),
        };
        Some(runtime.scoped_output(source, command, profile.as_ref(), scope, jobs.ttl))
    }

    pub(super) fn inherit_jobs(&self, mut runtime: Self) -> Self {
        runtime.jobs = self.jobs.clone();
        if let Some(socket) = self.base_socket_path() {
            runtime = runtime.with_socket_path(Path::new(&socket));
        }
        runtime
    }

    fn base_socket_path(&self) -> Option<String> {
        use rmux_core::formats::FormatVariables;
        self.base.format_value_by_name("socket_path")
    }
}
