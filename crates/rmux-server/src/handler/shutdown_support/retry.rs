#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct ShutdownRetryToken(u64);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum DetachedConnectionScope {
    CountAll,
    Exclude(u64),
}

impl DetachedConnectionScope {
    fn from_exclusion(excluded_connection_id: Option<u64>) -> Self {
        match excluded_connection_id {
            Some(connection_id) => Self::Exclude(connection_id),
            None => Self::CountAll,
        }
    }

    fn tighten(self, requested: Self) -> Self {
        match (self, requested) {
            (Self::CountAll, _) | (_, Self::CountAll) => Self::CountAll,
            (Self::Exclude(current), Self::Exclude(requested)) if current == requested => self,
            (Self::Exclude(_), Self::Exclude(_)) => Self::CountAll,
        }
    }

    fn exclusion(self) -> Option<u64> {
        match self {
            Self::CountAll => None,
            Self::Exclude(connection_id) => Some(connection_id),
        }
    }
}

pub(super) fn tighten_exclusions(current: Option<u64>, requested: Option<u64>) -> Option<u64> {
    DetachedConnectionScope::from_exclusion(current)
        .tighten(DetachedConnectionScope::from_exclusion(requested))
        .exclusion()
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
struct ScheduledRetry {
    token: ShutdownRetryToken,
    scope: DetachedConnectionScope,
}

#[derive(Debug)]
pub(in crate::handler) struct ShutdownRetryState {
    scheduled: Option<ScheduledRetry>,
    next_token: u64,
}

impl ShutdownRetryState {
    pub(in crate::handler) fn new() -> Self {
        Self {
            scheduled: None,
            next_token: 1,
        }
    }

    pub(super) fn tighten_if_scheduled(&mut self, excluded_connection_id: Option<u64>) -> bool {
        if self.scheduled.is_none() {
            return false;
        }
        let _ = self.effective_exclusion(excluded_connection_id);
        true
    }

    pub(super) fn effective_exclusion(
        &mut self,
        excluded_connection_id: Option<u64>,
    ) -> Option<u64> {
        let Some(scheduled) = self.scheduled.as_mut() else {
            return excluded_connection_id;
        };
        scheduled.scope = scheduled
            .scope
            .tighten(DetachedConnectionScope::from_exclusion(
                excluded_connection_id,
            ));
        scheduled.scope.exclusion()
    }

    pub(super) fn schedule_or_tighten(
        &mut self,
        excluded_connection_id: Option<u64>,
    ) -> Option<ShutdownRetryToken> {
        if self.tighten_if_scheduled(excluded_connection_id) {
            return None;
        }

        let token = ShutdownRetryToken(self.next_token);
        self.next_token = self.next_token.wrapping_add(1);
        if self.next_token == 0 {
            self.next_token = 1;
        }
        self.scheduled = Some(ScheduledRetry {
            token,
            scope: DetachedConnectionScope::from_exclusion(excluded_connection_id),
        });
        Some(token)
    }

    pub(super) fn take(&mut self, token: ShutdownRetryToken) -> Option<Option<u64>> {
        let scheduled = self
            .scheduled
            .filter(|scheduled| scheduled.token == token)?;
        self.scheduled = None;
        Some(scheduled.scope.exclusion())
    }

    pub(super) fn cancel(&mut self, token: ShutdownRetryToken) {
        if self
            .scheduled
            .is_some_and(|scheduled| scheduled.token == token)
        {
            self.scheduled = None;
        }
    }
}
