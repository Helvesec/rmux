use std::collections::HashMap;

use rmux_proto::ClientTerminalContext;

use super::OuterTerminal;

#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub(crate) struct OuterTerminalContext {
    theme_reports_disabled: bool,
    term: Option<String>,
    colorterm: Option<String>,
    term_program: Option<String>,
    term_program_version: Option<String>,
    terminal_features: Vec<String>,
    utf8: bool,
}

impl OuterTerminalContext {
    pub(crate) fn from_environment(environment: Option<&HashMap<String, String>>) -> Self {
        let Some(environment) = environment else {
            return Self::default();
        };

        Self {
            theme_reports_disabled: false,
            term: copy_environment_value(environment, "TERM"),
            colorterm: copy_environment_value(environment, "COLORTERM"),
            term_program: copy_environment_value(environment, "TERM_PROGRAM"),
            term_program_version: copy_environment_value(environment, "TERM_PROGRAM_VERSION"),
            terminal_features: Vec::new(),
            utf8: false,
        }
    }

    pub(crate) fn with_client_terminal(mut self, client_terminal: &ClientTerminalContext) -> Self {
        self.terminal_features = client_terminal
            .terminal_features
            .iter()
            .flat_map(|value| value.split(','))
            .map(str::trim)
            .filter(|feature| !feature.is_empty())
            .map(ToOwned::to_owned)
            .collect();
        self.utf8 = client_terminal.utf8;
        self
    }

    pub(crate) fn term_name(&self) -> &str {
        self.term.as_deref().unwrap_or("")
    }

    #[cfg(feature = "web")]
    pub(crate) fn without_theme_reports(mut self) -> Self {
        self.theme_reports_disabled = true;
        self
    }

    pub(crate) fn allows_theme_reports(&self) -> bool {
        !self.theme_reports_disabled
    }

    pub(super) fn term_program(&self) -> &str {
        self.term_program.as_deref().unwrap_or("")
    }

    pub(super) fn colorterm(&self) -> &str {
        self.colorterm.as_deref().unwrap_or("")
    }

    pub(crate) fn terminal_features(&self) -> &[String] {
        &self.terminal_features
    }

    pub(crate) fn explicit_features_string(&self) -> String {
        OuterTerminal::from_feature_names(self.terminal_features()).features_string()
    }

    pub(crate) fn utf8(&self) -> bool {
        self.utf8
    }

    #[cfg(test)]
    pub(crate) fn from_pairs(values: &[(&str, &str)]) -> Self {
        let environment = values
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect::<HashMap<_, _>>();
        Self::from_environment(Some(&environment))
    }
}

fn copy_environment_value(environment: &HashMap<String, String>, name: &str) -> Option<String> {
    environment
        .get(name)
        .cloned()
        .filter(|value| !value.is_empty())
}
