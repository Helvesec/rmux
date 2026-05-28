use std::path::Path;

pub(crate) fn shell_quote_path(path: &Path) -> String {
    let text = path.display().to_string();
    if !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"/._-+=:@".contains(&byte))
    {
        return text;
    }

    format!("'{}'", text.replace('\'', "'\\''"))
}
