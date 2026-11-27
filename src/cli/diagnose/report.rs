use super::{json_array, json_string, render_feature_list, DiagnoseReport};

impl DiagnoseReport {
    pub(super) fn render_human(&self) -> String {
        let mut output = String::new();
        output.push_str("rmux diagnose\n");
        output.push_str(&format!("schema_version: {}\n", self.schema_version));
        output.push_str(&format!("version: {}\n", self.version));
        output.push_str(&format!("os: {} ({})\n", self.os_name, self.os_arch));
        output.push_str(&format!("os_version: {}\n", self.os_version));
        output.push_str(&format!("terminal_host: {}\n", self.terminal_host));
        output.push_str(&format!("term: {}\n", self.term));
        output.push_str(&format!("term_program: {}\n", self.term_program));
        output.push_str(&format!("shell: {}\n", self.shell));
        output.push_str(&format!("socket_path: {}\n", self.socket_path));
        output.push_str(&format!("config_mode: {}\n", self.config_mode));
        output.push_str("config_paths:\n");
        for path in &self.config_paths {
            output.push_str(&format!("  - {path}\n"));
        }
        output.push_str("config_messages:\n");
        if self.config_messages.is_empty() {
            output.push_str("  - none\n");
        } else {
            for message in &self.config_messages {
                output.push_str(&format!("  - {message}\n"));
            }
        }
        output.push_str("capabilities:\n");
        output.push_str(&format!("  conpty: {}\n", self.conpty));
        output.push_str(&format!("  osc52: {}\n", self.osc52));
        output.push_str(&format!(
            "  terminal_features: {}\n",
            render_feature_list(&self.terminal_features)
        ));
        output.push_str(&format!(
            "compatibility:\n  registry: {}\n",
            self.compatibility_registry
        ));
        if let Some(configuration) = &self.configuration {
            output.push_str(&configuration.render_human());
            output.push_str("divergences (product inventory; not configuration applicability):\n");
            for id in &configuration.divergences {
                output.push_str(&format!("  - {}#{id}\n", self.compatibility_registry));
            }
        }
        output.push_str("privacy: environment values are summarized or redacted\n");
        output
    }

    pub(super) fn render_json(&self) -> String {
        format!(
            concat!(
                "{{\n",
                "  \"schema_version\": {},\n",
                "  \"version\": {},\n",
                "  \"os\": {{\"name\": {}, \"arch\": {}, \"version\": {}}},\n",
                "  \"terminal\": {{\"host\": {}, \"term\": {}, \"term_program\": {}}},\n",
                "  \"shell\": {},\n",
                "  \"socket_path\": {},\n",
                "  \"config\": {{\"mode\": {}, \"paths\": {}, \"messages\": {}}},\n",
                "  \"capabilities\": {{\"conpty\": {}, \"osc52\": {}, \"terminal_features\": {}}},\n",
                "  \"compatibility\": {},\n",
                "{}",
                "  \"privacy\": {{\"environment_values\": \"summarized-or-redacted\"}}\n",
                "}}\n"
            ),
            self.schema_version,
            json_string(&self.version),
            json_string(&self.os_name),
            json_string(&self.os_arch),
            json_string(&self.os_version),
            json_string(&self.terminal_host),
            json_string(&self.term),
            json_string(&self.term_program),
            json_string(&self.shell),
            json_string(&self.socket_path),
            json_string(&self.config_mode),
            json_array(&self.config_paths),
            json_array(&self.config_messages),
            json_string(&self.conpty),
            json_string(&self.osc52),
            json_array(&self.terminal_features),
            self.configuration.as_ref().map_or_else(
                || format!("{{\"registry\": {}}}", json_string(self.compatibility_registry)),
                |configuration| configuration.render_json(self.compatibility_registry),
            ),
            self.configuration.as_ref().map_or_else(String::new, |configuration| {
                format!("  \"divergences\": {},\n", json_array(&configuration.divergences))
            }),
        )
    }
}
