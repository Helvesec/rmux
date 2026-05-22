use super::*;
use std::sync::Mutex;

static ENV_LOCK: Mutex<()> = Mutex::new(());

struct EnvVarGuard {
    name: &'static str,
    value: Option<OsString>,
}

impl EnvVarGuard {
    fn capture(name: &'static str) -> Self {
        Self {
            name,
            value: std::env::var_os(name),
        }
    }
}

impl Drop for EnvVarGuard {
    fn drop(&mut self) {
        match self.value.as_ref() {
            // TODO: Audit that the environment access only happens in single-threaded code.
            Some(value) => unsafe { std::env::set_var(self.name, value) },
            // TODO: Audit that the environment access only happens in single-threaded code.
            None => unsafe { std::env::remove_var(self.name) },
        }
    }
}

#[test]
fn parser_detects_diagnose_after_socket_flags() {
    let invocation = parse_invocation(&[
        OsString::from("-Ldiag"),
        OsString::from("-Tclipboard,RGB"),
        OsString::from("diagnose"),
        OsString::from("--json"),
    ])
    .expect("parse diagnose")
    .expect("diagnose invocation");

    assert_eq!(invocation.format, DiagnoseFormat::Json);
    assert_eq!(invocation.socket_name, Some(OsString::from("diag")));
    assert_eq!(
        invocation.terminal_features,
        vec!["clipboard".to_owned(), "RGB".to_owned()]
    );
}

#[test]
fn parser_ignores_non_diagnose_commands() {
    assert_eq!(
        parse_invocation(&[OsString::from("new-session")]).expect("parse"),
        None
    );
}

#[test]
fn json_renderer_escapes_strings() {
    assert_eq!(json_string("a\"b\\c\n"), "\"a\\\"b\\\\c\\n\"");
}

#[test]
fn path_redaction_replaces_home_prefix() {
    let _guard = ENV_LOCK.lock().expect("env lock");
    let _home = EnvVarGuard::capture("HOME");
    let home = std::env::temp_dir().join("rmux-diagnose-home");
    // TODO: Audit that the environment access only happens in single-threaded code.
    unsafe { std::env::set_var("HOME", &home) };

    assert_eq!(redact_path(&home.join("rmux.conf")), "~/rmux.conf");
}

#[cfg(windows)]
#[test]
fn detected_shell_uses_windows_resolver_module() {
    assert_ne!(detected_shell(), "");
}
