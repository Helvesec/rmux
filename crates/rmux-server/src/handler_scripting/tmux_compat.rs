use super::source_files::SourceInput;

pub(super) fn tmux_compat_input(input: &SourceInput) -> SourceInput {
    SourceInput {
        current_file: input.current_file.clone(),
        contents: input.contents.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::tmux_compat_input;
    use crate::handler::scripting_support::source_files::SourceInput;

    #[test]
    fn tmux_compat_input_preserves_runtime_commands() {
        let input = SourceInput {
            current_file: "/tmp/.tmux.conf".to_owned(),
            contents: "set -g status off\nrun-shell 'true'\nset -g @plugin 'tmux-plugins/tpm'\n"
                .to_owned(),
        };

        let preserved = tmux_compat_input(&input);

        assert_eq!(preserved.current_file, input.current_file);
        assert_eq!(preserved.contents, input.contents);
    }
}
