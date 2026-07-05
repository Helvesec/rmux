use std::collections::BTreeMap;
use std::ffi::{OsStr, OsString};
use std::os::windows::ffi::OsStrExt;

use crate::ChildCommand;

const BACKSLASH: u16 = b'\\' as u16;
const DOUBLE_QUOTE: u16 = b'"' as u16;
const EQUALS: u16 = b'=' as u16;
const SPACE: u16 = b' ' as u16;
const TAB: u16 = b'\t' as u16;
const ASCII_LOWER_A: u16 = b'a' as u16;
const ASCII_LOWER_Z: u16 = b'z' as u16;

pub(super) fn command_line(command: &ChildCommand) -> Vec<u16> {
    let mut line = Vec::new();
    append_quoted_arg(
        &mut line,
        command
            .arg0
            .as_deref()
            .unwrap_or_else(|| command.program.as_os_str()),
    );
    for arg in &command.args {
        line.push(SPACE);
        append_quoted_arg(&mut line, arg);
    }
    if let Some(args) = &command.windows_verbatim_args {
        line.push(SPACE);
        line.extend(args.encode_wide());
    }
    line.push(0);
    line
}

pub(super) fn environment_block(command: &ChildCommand) -> Option<Vec<u16>> {
    if !command.clear_env && command.env.is_empty() {
        return None;
    }

    let mut env = BTreeMap::<NormalizedEnvKey, (OsString, OsString)>::new();
    if !command.clear_env {
        for (key, value) in std::env::vars_os() {
            env.insert(NormalizedEnvKey::from_os_str(&key), (key, value));
        }
    }
    for (key, value) in &command.env {
        env.insert(
            NormalizedEnvKey::from_os_str(key),
            (key.clone(), value.clone()),
        );
    }

    let mut block = Vec::new();
    for (_normalized, (key, value)) in env {
        block.extend(key.encode_wide());
        block.push(EQUALS);
        block.extend(value.encode_wide());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Some(block)
}

pub(super) fn wide_null(value: &OsStr) -> Vec<u16> {
    value.encode_wide().chain(std::iter::once(0)).collect()
}

fn append_quoted_arg(output: &mut Vec<u16>, arg: &OsStr) {
    let units = arg.encode_wide().collect::<Vec<_>>();
    if !needs_quotes(&units) {
        output.extend(units);
        return;
    }

    output.push(DOUBLE_QUOTE);
    let mut backslashes = 0;
    for unit in units {
        match unit {
            BACKSLASH => backslashes += 1,
            DOUBLE_QUOTE => {
                output.extend(std::iter::repeat_n(BACKSLASH, backslashes * 2 + 1));
                output.push(DOUBLE_QUOTE);
                backslashes = 0;
            }
            _ => {
                output.extend(std::iter::repeat_n(BACKSLASH, backslashes));
                backslashes = 0;
                output.push(unit);
            }
        }
    }
    output.extend(std::iter::repeat_n(BACKSLASH, backslashes * 2));
    output.push(DOUBLE_QUOTE);
}

fn needs_quotes(units: &[u16]) -> bool {
    units.is_empty()
        || units
            .iter()
            .any(|unit| matches!(*unit, SPACE | TAB | DOUBLE_QUOTE))
}

#[derive(Clone, Debug, Eq, PartialEq, Ord, PartialOrd)]
struct NormalizedEnvKey(Vec<u16>);

impl NormalizedEnvKey {
    fn from_os_str(value: &OsStr) -> Self {
        Self(value.encode_wide().map(ascii_upper_unit).collect())
    }
}

fn ascii_upper_unit(unit: u16) -> u16 {
    match unit {
        ASCII_LOWER_A..=ASCII_LOWER_Z => unit - 32,
        _ => unit,
    }
}

#[cfg(test)]
mod tests {
    use std::os::windows::ffi::OsStringExt;

    use super::*;

    #[test]
    fn command_line_preserves_non_unicode_wide_units() {
        let raw_arg = OsString::from_wide(&[0xD800]);
        let command = ChildCommand::new("prog.exe").arg(raw_arg);
        let line = command_line(&command);

        assert!(line.contains(&0xD800));
    }

    #[test]
    fn command_line_quotes_spaces_quotes_and_trailing_backslashes() {
        let command = ChildCommand::new("prog.exe").arg(r#"C:\two words\ends\"#);
        let line = command_line(&command);
        let rendered = String::from_utf16_lossy(&line[..line.len() - 1]);

        assert_eq!(rendered, r#"prog.exe "C:\two words\ends\\""#);
    }

    #[test]
    fn command_line_appends_windows_verbatim_args_without_msvc_quoting() {
        let command = ChildCommand::new("cmd.exe")
            .arg("/D")
            .arg("/S")
            .windows_verbatim_args(r#"/C ""C:\two words\script.bat" "echo hi"""#);
        let line = command_line(&command);
        let rendered = String::from_utf16_lossy(&line[..line.len() - 1]);

        assert_eq!(
            rendered,
            r#"cmd.exe /D /S /C ""C:\two words\script.bat" "echo hi"""#
        );
    }

    #[test]
    fn empty_environment_block_is_double_nul_terminated() {
        let command = ChildCommand::new("prog.exe").clear_env();

        assert_eq!(environment_block(&command), Some(vec![0, 0]));
    }

    #[test]
    fn environment_block_deduplicates_keys_case_insensitively() {
        let command = ChildCommand::new("prog.exe")
            .clear_env()
            .env("Path", "first")
            .env("PATH", "second");
        let block = environment_block(&command).expect("environment block");

        assert_eq!(env_entries(&block), vec!["PATH=second"]);
        assert_eq!(&block[block.len() - 2..], &[0, 0]);
    }

    fn env_entries(block: &[u16]) -> Vec<String> {
        block
            .split(|unit| *unit == 0)
            .take_while(|entry| !entry.is_empty())
            .map(String::from_utf16_lossy)
            .collect()
    }
}
