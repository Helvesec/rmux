use std::collections::BTreeMap;
use std::ffi::{CString, OsStr, OsString};
use std::os::windows::ffi::OsStrExt;

use crate::{ChildCommand, PtyError};

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

pub(super) fn environment_block(command: &ChildCommand) -> Result<Option<Vec<u16>>, PtyError> {
    if !command.clear_env && command.env.is_empty() {
        return Ok(None);
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

    // An environment block separates its entries with NUL, so an interior NUL
    // would end that entry early and hand CreateProcessW a silently truncated
    // variable the child then accepts. Unix refuses the same configuration when
    // std spawns the child; refuse it here too, at the same boundary, so the
    // spawn fails and its rollback runs.
    //
    // The check is on the resolved entries, not on the caller's raw list.
    // `ChildCommand::env` sets *or overrides*, and the map above applies the
    // last write with the case-insensitive key Windows uses, so a value that is
    // replaced never reaches the block and must not fail the spawn.
    let mut block = Vec::new();
    for (_normalized, (key, value)) in env {
        reject_interior_nul(&key)?;
        reject_interior_nul(&value)?;
        block.extend(key.encode_wide());
        block.push(EQUALS);
        block.extend(value.encode_wide());
        block.push(0);
    }
    if block.is_empty() {
        block.push(0);
    }
    block.push(0);
    Ok(Some(block))
}

/// Rejects an interior NUL with the typed error the crate already documents.
///
/// `NulError` is only constructible through `CString`, so an equivalent byte
/// sequence of the same length carries the offending index. That index counts
/// UTF-16 units of the rejected value and the bytes are filler: it locates the
/// NUL for a reader, it is not the standard byte offset of the original value
/// and no caller should present it as one.
fn reject_interior_nul(value: &OsStr) -> Result<(), PtyError> {
    let Some(position) = value.encode_wide().position(|unit| unit == 0) else {
        return Ok(());
    };
    let mut bytes = vec![b'_'; position + 1];
    bytes[position] = 0;
    Err(PtyError::Nul(
        CString::new(bytes).expect_err("the sequence contains an interior NUL"),
    ))
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

    fn nul_error(result: Result<Option<Vec<u16>>, PtyError>) -> std::ffi::NulError {
        match result {
            Err(PtyError::Nul(error)) => error,
            other => panic!("expected an interior-NUL rejection, got {other:?}"),
        }
    }

    #[test]
    fn environment_block_rejects_an_interior_nul_in_a_value() {
        // The gate2 case: -e FNP_FAIL=\0 must fail the spawn, not become an
        // accepted variable truncated at the NUL.
        let command = ChildCommand::new("prog.exe")
            .clear_env()
            .env("FNP_FAIL", OsString::from_wide(&[0]));

        assert_eq!(nul_error(environment_block(&command)).nul_position(), 0);
    }

    #[test]
    fn environment_block_rejects_an_interior_nul_wherever_it_sits() {
        for (label, units) in [
            ("leading", vec![0, b'v' as u16, b'l' as u16]),
            ("middle", vec![b'v' as u16, 0, b'l' as u16]),
            ("trailing", vec![b'v' as u16, b'l' as u16, 0]),
        ] {
            let expected = units.iter().position(|unit| *unit == 0).unwrap();
            let command = ChildCommand::new("prog.exe")
                .clear_env()
                .env("K", OsString::from_wide(&units));

            assert_eq!(
                nul_error(environment_block(&command)).nul_position(),
                expected,
                "{label}"
            );
        }
    }

    #[test]
    fn environment_block_rejects_an_interior_nul_in_a_name() {
        let command = ChildCommand::new("prog.exe")
            .clear_env()
            .env(OsString::from_wide(&[b'K' as u16, 0, b'2' as u16]), "value");

        assert_eq!(nul_error(environment_block(&command)).nul_position(), 1);
    }

    #[test]
    fn environment_block_accepts_a_valid_value_that_overrides_an_earlier_nul() {
        // ChildCommand::env sets *or overrides*. A replaced value never reaches
        // the block, so it must not fail the spawn.
        let command = ChildCommand::new("prog.exe")
            .clear_env()
            .env("K", OsString::from_wide(&[b'b' as u16, 0, b'd' as u16]))
            .env("K", "valid");
        let block = environment_block(&command)
            .expect("the last override is valid")
            .expect("environment block present");

        assert_eq!(block, "K=valid\0\0".encode_utf16().collect::<Vec<u16>>());
    }

    #[test]
    fn environment_block_accepts_a_case_variant_override_of_an_earlier_nul() {
        // Windows resolves environment names case-insensitively, so the later
        // lower-case key wins and carries its own spelling into the block.
        let command = ChildCommand::new("prog.exe")
            .clear_env()
            .env("K", OsString::from_wide(&[b'b' as u16, 0, b'd' as u16]))
            .env("k", "valid");
        let block = environment_block(&command)
            .expect("the last override is valid")
            .expect("environment block present");

        assert_eq!(block, "k=valid\0\0".encode_utf16().collect::<Vec<u16>>());
    }

    #[test]
    fn environment_block_still_rejects_a_nul_that_wins_the_override() {
        // The mirror case: when the NUL is the value that survives resolution,
        // the spawn must still fail.
        let command = ChildCommand::new("prog.exe")
            .clear_env()
            .env("K", "valid")
            .env("K", OsString::from_wide(&[b'b' as u16, 0, b'd' as u16]));

        assert_eq!(nul_error(environment_block(&command)).nul_position(), 1);
    }

    #[test]
    fn environment_block_keeps_valid_values_including_equals_and_raw_units() {
        // A value may legitimately contain '=' and non-Unicode wide units, and
        // a Windows drive entry legitimately starts with '='. None of these is
        // an interior NUL and none may be rejected.
        let raw = OsString::from_wide(&[0xD800]);
        let command = ChildCommand::new("prog.exe")
            .clear_env()
            .env("WITH_EQUALS", "a=b=c")
            .env("RAW", raw)
            .env("=C:", r"C:\dir");
        let block = environment_block(&command)
            .expect("valid environment builds")
            .expect("environment block present");

        assert!(block.contains(&0xD800));
        let rendered = String::from_utf16_lossy(&block);
        assert!(rendered.contains("WITH_EQUALS=a=b=c"), "{rendered:?}");
        assert!(rendered.contains("=C:="), "{rendered:?}");
    }

    #[test]
    fn environment_block_without_overrides_inherits_and_is_not_rejected() {
        let command = ChildCommand::new("prog.exe");

        assert_eq!(environment_block(&command).unwrap(), None);
    }

    #[test]
    fn empty_environment_block_is_double_nul_terminated() {
        let command = ChildCommand::new("prog.exe").clear_env();

        assert_eq!(environment_block(&command).unwrap(), Some(vec![0, 0]));
    }

    #[test]
    fn environment_block_deduplicates_keys_case_insensitively() {
        let command = ChildCommand::new("prog.exe")
            .clear_env()
            .env("Path", "first")
            .env("PATH", "second");
        let block = environment_block(&command)
            .expect("environment block builds")
            .expect("environment block present");

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
