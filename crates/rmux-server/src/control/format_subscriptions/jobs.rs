use super::ControlFormatSubscriptions;

#[derive(Debug, Clone)]
pub(super) struct SubscriptionJobContext {
    pub(super) owner: u64,
    pub(super) socket_path: Option<std::path::PathBuf>,
    pub(super) runtime: Option<crate::status_jobs::StatusJobRuntime>,
}

impl ControlFormatSubscriptions {
    pub(crate) fn prepare_jobs(
        &mut self,
        socket_path: &std::path::Path,
        runtime: &crate::status_jobs::StatusJobRuntime,
    ) {
        for entry in &mut self.entries {
            entry.jobs.socket_path = Some(socket_path.to_owned());
            entry.jobs.runtime = Some(runtime.clone());
        }
    }

    pub(super) fn retire_replaced_jobs(&self, next: &Self) {
        for entry in &self.entries {
            if !next
                .entries
                .iter()
                .any(|kept| kept.jobs.owner == entry.jobs.owner)
            {
                if let Some(runtime) = entry.jobs.runtime.as_ref() {
                    runtime.cancel_owner(&crate::status_jobs::StatusJobOwner::Subscription(
                        entry.jobs.owner,
                    ));
                }
            }
        }
    }
}
