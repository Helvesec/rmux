#[cfg(unix)]
mod unix {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::time::{SystemTime, UNIX_EPOCH};

    const SOURCE_SHA: &str = "1111111111111111111111111111111111111111";
    const RELEASE_REF: &str = "v1.2.3";
    const PREDICATE_TYPE: &str = "https://rmux.io/attestations/release-publication-receipt/v1";

    struct Fixture {
        root: PathBuf,
        gh: PathBuf,
        state: PathBuf,
        bundle: PathBuf,
        predicate: PathBuf,
        output: PathBuf,
        arguments: PathBuf,
    }

    impl Fixture {
        fn new() -> Self {
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock")
                .as_nanos();
            let root = std::env::temp_dir().join(format!(
                "rmux-receipt-attestation-{}-{nonce}",
                std::process::id()
            ));
            fs::create_dir(&root).expect("fixture root");
            let gh = root.join("gh");
            let state = root.join("release-state.json");
            let bundle = root.join("publication-receipt.sigstore.json");
            let predicate = root.join("publication-receipt-predicate.json");
            let output = root.join("verification.json");
            let arguments = root.join("arguments.txt");
            fs::write(
                &gh,
                "#!/bin/sh\nprintf '%s\\n' \"$@\" > \"$RMUX_ARGS_LOG\"\ncat \"$RMUX_FAKE_OUTPUT\"\n",
            )
            .expect("fake gh");
            let mut permissions = fs::metadata(&gh).expect("gh metadata").permissions();
            permissions.set_mode(0o700);
            fs::set_permissions(&gh, permissions).expect("gh permissions");
            fs::write(&state, b"{\"immutable\":true}\n").expect("release state");
            fs::write(&bundle, b"{}\n").expect("bundle");
            fs::write(
                &predicate,
                format!(
                    "{{\"predicate_type\":\"{PREDICATE_TYPE}\",\"repository_id\":1239918790,\"source_git_sha\":\"{SOURCE_SHA}\",\"release\":{{\"ref\":\"{RELEASE_REF}\"}},\"status\":\"disarmed-non-authoritative\",\"downstream_authority\":false}}\n"
                ),
            )
            .expect("predicate");
            let fixture = Self {
                root,
                gh,
                state,
                bundle,
                predicate,
                output,
                arguments,
            };
            fixture.write_verification(None, true);
            fixture
        }

        fn write_verification(&self, subject_digest: Option<&str>, timestamps: bool) {
            let digest = subject_digest
                .map(str::to_owned)
                .unwrap_or_else(|| sha256(&self.state));
            let predicate: serde_json::Value =
                serde_json::from_slice(&fs::read(&self.predicate).expect("read predicate"))
                    .expect("parse predicate");
            let value = serde_json::json!([{
                "verificationResult": {
                    "signature": {"certificate": {"subject": "trusted"}},
                    "verifiedTimestamps": if timestamps {
                        serde_json::json!([{"type": "transparency-log"}])
                    } else {
                        serde_json::json!([])
                    },
                    "statement": {
                        "subject": [{
                            "name": "release-state.json",
                            "digest": {"sha256": digest}
                        }],
                        "predicateType": PREDICATE_TYPE,
                        "predicate": predicate
                    }
                }
            }]);
            fs::write(
                &self.output,
                serde_json::to_vec(&value).expect("verification JSON"),
            )
            .expect("write verification");
        }

        fn command(&self) -> Command {
            let mut command = Command::new("python3");
            command
                .arg("scripts/release/verify-receipt-attestation.py")
                .arg("--gh")
                .arg(&self.gh)
                .arg("--release-state")
                .arg(&self.state)
                .arg("--bundle")
                .arg(&self.bundle)
                .arg("--predicate")
                .arg(&self.predicate)
                .arg("--source-sha")
                .arg(SOURCE_SHA)
                .arg("--release-ref")
                .arg(RELEASE_REF)
                .env("RMUX_FAKE_OUTPUT", &self.output)
                .env("RMUX_ARGS_LOG", &self.arguments);
            command
        }
    }

    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.root);
        }
    }

    fn sha256(path: &Path) -> String {
        let output = Command::new("sha256sum")
            .arg(path)
            .output()
            .expect("sha256sum");
        assert!(output.status.success());
        String::from_utf8(output.stdout)
            .expect("sha256 output")
            .split_whitespace()
            .next()
            .expect("sha256 digest")
            .to_owned()
    }

    #[test]
    fn exact_receipt_subject_and_signer_policy_verify() {
        let fixture = Fixture::new();
        let output = fixture.command().output().expect("receipt verifier");
        assert!(
            output.status.success(),
            "{}",
            String::from_utf8_lossy(&output.stderr)
        );
        let arguments = fs::read_to_string(&fixture.arguments).expect("arguments");
        for required in [
            "--deny-self-hosted-runners",
            "Helvesec/rmux/.github/workflows/release-receipt.yml",
            "--signer-digest",
            "--source-digest",
            "--source-ref",
            "refs/tags/v1.2.3",
            "--predicate-type",
            PREDICATE_TYPE,
        ] {
            assert!(arguments.lines().any(|line| line == required), "{required}");
        }
    }

    #[test]
    fn changed_subject_digest_fails_closed() {
        let fixture = Fixture::new();
        fixture.write_verification(Some(&"0".repeat(64)), true);
        let output = fixture.command().output().expect("receipt verifier");
        assert!(!output.status.success());
        assert!(String::from_utf8_lossy(&output.stderr)
            .contains("statement differs from exact local bytes"));
    }

    #[test]
    fn missing_signed_timestamp_fails_closed() {
        let fixture = Fixture::new();
        fixture.write_verification(None, false);
        let output = fixture.command().output().expect("receipt verifier");
        assert!(!output.status.success());
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("lacks signed verification evidence")
        );
    }
}
