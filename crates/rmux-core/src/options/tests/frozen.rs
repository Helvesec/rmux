use std::{collections::BTreeSet, ffi::OsStr, fs, io, path::Path};

use regex::Regex;
use sha2::{Digest, Sha256};

const CONTRACT: &str = include_str!(concat!(
    env!("CARGO_MANIFEST_DIR"),
    "/../../tests/reference/tmux_compat/frozen_reference.yaml"
));
const DEFAULT_PATH: &str = "/opt/rmux/reference/tmux/options-table.c";

pub(super) struct Inventory {
    pub names: BTreeSet<String>,
    pub styles: BTreeSet<String>,
}

pub(super) fn inventory() -> Option<Inventory> {
    let selected = std::env::var_os("RMUX_FROZEN_OPTIONS_TABLE");
    let required = std::env::var_os("RMUX_REQUIRE_FROZEN_TMUX");
    let source = load(
        selected.as_deref(),
        required.as_deref(),
        Path::new(DEFAULT_PATH),
        CONTRACT,
        |path| fs::read(path),
    )
    .unwrap_or_else(|error| panic!("{error}"))?;
    Some(scan(&source).expect("authenticated ordinary options table"))
}

fn member_digest(contract: &str) -> Result<&str, String> {
    let section = contract
        .split_once("\ntmux_reference:\n")
        .ok_or("missing tmux_reference identity")?
        .1
        .split("\nfrozen_tmux_binary_acquisition:")
        .next()
        .unwrap();
    let field = |key: &str| -> Result<&str, String> {
        let prefix = format!("  {key}: \"");
        let values = section
            .lines()
            .filter_map(|line| line.strip_prefix(&prefix).and_then(|s| s.strip_suffix('"')))
            .collect::<Vec<_>>();
        match values.as_slice() {
            [value] => Ok(value),
            _ => Err(format!("missing/duplicate frozen source field {key}")),
        }
    };
    for (key, expected) in [
        ("source_tag", "3.7b"),
        ("source_sha", "e802909de06012a4df6209d55e86487c56223163"),
        (
            "source_tarball_sha256",
            "87f2e99e3b685973f2ca002ffd6ed7e51a5744f7009daae5a15670b6d532db96",
        ),
    ] {
        if field(key)? != expected {
            return Err(format!("wrong frozen source identity: {key}"));
        }
    }
    let digest = field("options_table_sha256")?;
    if digest.len() != 64 || !digest.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err("invalid options_table_sha256".into());
    }
    Ok(digest)
}

fn required(value: Option<&OsStr>) -> bool {
    matches!(
        value.and_then(OsStr::to_str),
        Some("1" | "true" | "TRUE" | "on" | "ON" | "yes" | "YES")
    )
}

fn load(
    selected: Option<&OsStr>,
    require: Option<&OsStr>,
    default: &Path,
    contract: &str,
    read: impl FnOnce(&Path) -> io::Result<Vec<u8>>,
) -> Result<Option<String>, String> {
    let digest = member_digest(contract)?;
    let path = selected.map(Path::new).unwrap_or(default);
    let bytes = match read(path) {
        Ok(bytes) => bytes,
        Err(error)
            if selected.is_none()
                && !required(require)
                && error.kind() == io::ErrorKind::NotFound =>
        {
            eprintln!("frozen inventory assertions NOT EXECUTED: {}: {error}; set RMUX_REQUIRE_FROZEN_TMUX=1 to require them", path.display());
            return Ok(None);
        }
        Err(error) => {
            return Err(format!(
                "frozen options-table.c unreadable at {}: {error}",
                path.display()
            ))
        }
    };
    let actual = format!("{:x}", Sha256::digest(&bytes));
    if actual != digest {
        return Err(format!(
            "frozen options-table.c digest mismatch at {}: expected {digest}, got {actual}",
            path.display()
        ));
    }
    String::from_utf8(bytes)
        .map(Some)
        .map_err(|error| error.to_string())
}

// This scanner accepts the pinned table's literal initializers and hook macros,
// not arbitrary C/preprocessor input. Authentication precedes scanning.
fn scan(source: &str) -> Result<Inventory, String> {
    let lexer =
        Regex::new(r#"(?s)/\*.*?\*/|//[^\n]*|"(?:\\.|[^"\\])*"|[A-Za-z_][A-Za-z_0-9]*|[^\s]"#)
            .unwrap();
    let tokens = lexer
        .find_iter(source)
        .map(|m| m.as_str())
        .filter(|s| !s.starts_with("/*") && !s.starts_with("//"))
        .collect::<Vec<_>>();
    let start = tokens
        .windows(5)
        .position(|w| w == ["options_table", "[", "]", "=", "{"])
        .ok_or("missing options_table[]")?
        + 5;
    let mut rest = &tokens[start..];
    let mut names = BTreeSet::new();
    let mut styles = BTreeSet::new();
    let mut all_names = BTreeSet::new();
    loop {
        match rest.first().copied() {
            Some(",") => rest = &rest[1..],
            Some("}") if rest.get(1) == Some(&";") => break,
            Some("{") => {
                let end = rest
                    .iter()
                    .position(|s| *s == "}")
                    .ok_or("unterminated initializer")?;
                let entry = &rest[1..end];
                if entry.contains(&"{") {
                    return Err("nested initializer unsupported".into());
                }
                let fields = entry
                    .windows(4)
                    .filter(|w| w[..3] == [".", "name", "="])
                    .collect::<Vec<_>>();
                let [field] = fields.as_slice() else {
                    return Err("malformed name initializer".into());
                };
                if field[3] != "NULL" {
                    let name = field[3]
                        .strip_prefix('"')
                        .and_then(|s| s.strip_suffix('"'))
                        .ok_or("nonliteral option name")?;
                    if !all_names.insert(name.to_owned()) {
                        return Err(format!("duplicate option: {name}"));
                    }
                    let flags = entry
                        .windows(3)
                        .position(|w| w == [".", "flags", "="])
                        .map(|i| entry[i + 3..].split(|t| *t == ",").next().unwrap())
                        .unwrap_or(&[]);
                    if !flags.contains(&"OPTIONS_TABLE_IS_HOOK") {
                        names.insert(name.to_owned());
                        if flags.contains(&"OPTIONS_TABLE_IS_STYLE") {
                            styles.insert(name.to_owned());
                        }
                    }
                }
                rest = &rest[end + 1..];
            }
            Some(
                "OPTIONS_TABLE_HOOK" | "OPTIONS_TABLE_PANE_HOOK" | "OPTIONS_TABLE_WINDOW_HOOK",
            ) => {
                if rest.get(1) != Some(&"(")
                    || rest.get(3) != Some(&",")
                    || rest.get(5) != Some(&")")
                {
                    return Err("malformed hook macro".into());
                }
                rest = &rest[6..];
            }
            token => return Err(format!("unexpected table token: {token:?}")),
        }
    }
    Ok(Inventory { names, styles })
}

pub(super) fn difference(
    actual: &BTreeSet<String>,
    expected: &BTreeSet<String>,
) -> Result<(), String> {
    let missing = expected.difference(actual).collect::<Vec<_>>();
    let extra = actual.difference(expected).collect::<Vec<_>>();
    if missing.is_empty() && extra.is_empty() {
        return Ok(());
    }
    Err(format!(
        "missing: {missing:?}; extra: {extra:?}; actual {}, frozen {}",
        actual.len(),
        expected.len()
    ))
}

#[test]
fn fixture_selection_and_authentication_negatives() {
    let path = Path::new(DEFAULT_PATH);
    let missing = |_: &Path| Err(io::Error::from(io::ErrorKind::NotFound));
    assert!(load(None, None, path, CONTRACT, missing).unwrap().is_none());
    for value in ["1", "true", "TRUE", "on", "ON", "yes", "YES"] {
        assert!(load(None, Some(OsStr::new(value)), path, CONTRACT, missing)
            .unwrap_err()
            .contains("unreadable"));
    }
    assert!(!required(Some(OsStr::new("false"))));
    assert!(load(Some(OsStr::new("explicit")), None, path, CONTRACT, missing).is_err());
    assert!(load(
        None,
        None,
        path,
        CONTRACT,
        |_| Ok(b"wrong content".to_vec())
    )
    .unwrap_err()
    .contains("digest mismatch"));
    assert!(member_digest(&CONTRACT.replace("3.7b", "3.6"))
        .unwrap_err()
        .contains("identity"));
    assert!(member_digest(&CONTRACT.replace("c1245a28", "oops")).is_err());
    let source = b"fixture bytes";
    let digest = format!("{:x}", Sha256::digest(source));
    let test_contract = CONTRACT.replace(member_digest(CONTRACT).unwrap(), &digest);
    let selected = OsStr::new("chosen/options-table.c");
    assert_eq!(
        load(Some(selected), None, path, &test_contract, |p| {
            assert_eq!(p, Path::new(selected));
            Ok(source.to_vec())
        })
        .unwrap()
        .unwrap(),
        "fixture bytes"
    );
}

#[test]
fn scanner_excludes_comments_strings_and_hooks_and_rejects_duplicates() {
    let source = r#"
      /* .name = "outside" */ const char *decoy = ".name = \"bad\"";
      const struct options_table_entry options_table[] = {
        { .name = "tree-mode-preview-style", .flags = OPTIONS_TABLE_IS_STYLE,
          .text = "OPTIONS_TABLE_IS_HOOK } .name" },
        // { .name = "comment", .flags = OPTIONS_TABLE_IS_STYLE },
        { .name = "plain", .text = "OPTIONS_TABLE_IS_STYLE" },
        { .name = "hook", .flags = OPTIONS_TABLE_IS_HOOK },
        OPTIONS_TABLE_HOOK("macro-hook", ""),
        { .name = NULL }
      };
    "#;
    let inventory = scan(source).unwrap();
    assert_eq!(
        inventory.names,
        ["plain", "tree-mode-preview-style"]
            .map(str::to_owned)
            .into()
    );
    assert_eq!(
        inventory.styles,
        ["tree-mode-preview-style".to_owned()].into()
    );
    assert!(
        scan(&source.replace("\"plain\"", "\"tree-mode-preview-style\""))
            .err()
            .unwrap()
            .contains("duplicate")
    );
    assert!(scan(&source.replace(".name = NULL", ".bad = NULL")).is_err());
    let mut renamed = inventory.names.clone();
    renamed.remove("tree-mode-preview-style");
    renamed.insert("renamed".into());
    let error = difference(&renamed, &inventory.names).unwrap_err();
    assert!(error.starts_with("missing: [\"tree-mode-preview-style\"]; extra: [\"renamed\"]"));
    assert!(difference(&BTreeSet::new(), &inventory.styles)
        .unwrap_err()
        .contains("tree-mode-preview-style"));
}
