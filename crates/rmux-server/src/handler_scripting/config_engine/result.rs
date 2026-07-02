#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ConfigDiagnosticSeverity {
    Ignored,
    Warning,
    Error,
}

impl ConfigDiagnosticSeverity {
    pub(crate) const ALL: [Self; 3] = [Self::Ignored, Self::Warning, Self::Error];
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct ConfigLoadResult {
    pub(super) files_attempted: usize,
    pub(super) files_loaded: usize,
    pub(super) parsed_count: usize,
    pub(super) executed_count: usize,
    pub(super) ignored_count: usize,
    pub(super) error_count: usize,
}

impl ConfigLoadResult {
    pub(crate) fn assert_boundary_invariants(&self) {
        debug_assert!(self.files_loaded <= self.files_attempted);
        let _ = self.parsed_count;
        let _ = self.executed_count;
        let _ = self.ignored_count;
        let _ = self.error_count;
    }
}
