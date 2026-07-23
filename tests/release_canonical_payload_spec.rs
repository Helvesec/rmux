use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

const SOURCE: &str = "0123456789abcdef0123456789abcdef01234567";

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn temp_dir(label: &str) -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "rmux-canonical-payload-{label}-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("create canonical payload fixture");
    path
}

fn python() -> &'static str {
    if cfg!(windows) {
        "python.exe"
    } else {
        "python3"
    }
}

#[test]
fn wasm_payload_is_deterministic_and_rejects_mutated_source_bytes() {
    let root = temp_dir("wasm");
    let package = root.join("pkg");
    let first = root.join("first");
    let second = root.join("second");
    fs::create_dir_all(&package).expect("create package fixture");
    fs::create_dir_all(&first).expect("create first output");
    fs::create_dir_all(&second).expect("create second output");
    for name in [
        "README.md",
        "package.json",
        "rmux_web_crypto_wasm.d.ts",
        "rmux_web_crypto_wasm.js",
        "rmux_web_crypto_wasm_bg.wasm",
        "rmux_web_crypto_wasm_bg.wasm.d.ts",
    ] {
        fs::write(package.join(name), format!("fixture:{name}\n"))
            .expect("write WASM package fixture");
    }
    let script = repo_root().join("scripts/release/canonical-wasm-bundle.py");
    let invoke = |command: &str, output: &PathBuf| {
        Command::new(python())
            .arg(&script)
            .args([
                command,
                "--source-sha",
                SOURCE,
                "--version",
                "0.9.0",
                "--package-dir",
            ])
            .arg(&package)
            .arg("--output-dir")
            .arg(output)
            .output()
            .expect("run WASM payload helper")
    };
    let created = invoke("create", &first);
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    assert!(invoke("create", &second).status.success());
    for name in [
        "rmux-web-crypto-wasm-0.9.0.tar",
        "rmux-web-crypto-wasm-0.9.0.provenance.json",
    ] {
        assert_eq!(
            fs::read(first.join(name)).expect("read first payload"),
            fs::read(second.join(name)).expect("read second payload"),
            "canonical WASM payload changed between identical builds"
        );
    }
    let provenance: serde_json::Value = serde_json::from_slice(
        &fs::read(first.join("rmux-web-crypto-wasm-0.9.0.provenance.json"))
            .expect("read WASM provenance"),
    )
    .expect("parse WASM provenance");
    assert_eq!(provenance["version"], "0.9.0");
    fs::write(package.join("rmux_web_crypto_wasm_bg.wasm"), b"mutated\n")
        .expect("mutate WASM fixture");
    let rejected = invoke("verify", &first);
    assert!(!rejected.status.success());
    assert!(String::from_utf8_lossy(&rejected.stderr).contains("bytes differ"));
    fs::remove_dir_all(root).expect("remove WASM fixture");
}

#[test]
fn crate_payload_packages_workspace_orders_dependencies_and_rejects_substitution() {
    let root = temp_dir("crates");
    let fixture = root.join("fixture.py");
    fs::write(
        &fixture,
        r#"
import importlib.util
import json
import pathlib
import sys

repo, root = map(pathlib.Path, sys.argv[1:])
path = repo / "scripts/release/canonical-crate-set.py"
spec = importlib.util.spec_from_file_location("canonical_crate_set", path)
module = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(module)

calls = []
class Completed:
    returncode = 0
    stderr = ""
def record_run(command, **kwargs):
    calls.append(command)
    return Completed()
module.subprocess.run = record_run
module.run_cargo_package(["crate-a", "crate-b"], root / "absent-target")
assert calls == [[
    "cargo", "package",
    "--package", "crate-a",
    "--package", "crate-b",
    "--locked", "--no-verify",
]], calls

def package(name, identifier, dependencies):
    return {
        "name": name,
        "id": identifier,
        "version": "0.9.0",
        "publish": ["crates-io"],
        "dependencies": [
            {"name": dependency, "kind": None, "path": f"../{dependency}"}
            for dependency in dependencies
        ],
    }

metadata = {
    "workspace_members": ["a-id", "b-id", "c-id", "private-id"],
    "packages": [
        package("crate-c", "c-id", ["crate-b"]),
        package("crate-a", "a-id", []),
        package("crate-b", "b-id", ["crate-a"]),
        {**package("private", "private-id", []), "publish": []},
    ],
}
packages = module.publishable_packages(metadata, "0.9.0")
order, dependencies = module.dependency_order(packages)
assert order == ["crate-a", "crate-b", "crate-c"], order
target = root / "target" / "package"
target.mkdir(parents=True)
for name in order:
    (target / f"{name}-0.9.0.crate").write_bytes(f"package:{name}\n".encode())
manifest, payloads = module.build_manifest(
    "0123456789abcdef0123456789abcdef01234567",
    "0.9.0",
    order,
    dependencies,
    target,
)
assert manifest["package_count"] == 3
assert manifest["publish_order"] == order
archive = root / "crate-set.tar"
module.write_tar(archive, payloads)
module.verify_tar(archive, payloads)
raw = archive.read_bytes()
needle = b"package:crate-b"
offset = raw.find(needle)
assert offset >= 0
archive.write_bytes(raw[:offset] + b"tamper!:crate-b" + raw[offset + len(needle):])
try:
    module.verify_tar(archive, payloads)
except ValueError as error:
    assert "bytes differ" in str(error), error
else:
    raise AssertionError("mutated crate package set was accepted")
"#,
    )
    .expect("write crate payload fixture");
    let output = Command::new(python())
        .arg(&fixture)
        .arg(repo_root())
        .arg(&root)
        .output()
        .expect("run crate payload fixture");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(root).expect("remove crate fixture");
}

#[test]
fn canonical_payload_helpers_remain_non_publishing_and_bounded() {
    let wasm = include_str!("../scripts/release/canonical-wasm-bundle.py");
    let crates = include_str!("../scripts/release/canonical-crate-set.py");
    assert!(wasm.lines().count() < 600);
    assert!(crates.lines().count() < 600);
    for forbidden in [
        "cargo publish",
        "choco push",
        "snapcraft upload",
        "git push",
    ] {
        assert!(!wasm.contains(forbidden), "WASM helper gained {forbidden}");
        assert!(
            !crates.contains(forbidden),
            "crate helper gained {forbidden}"
        );
    }
    for required in ["cargo", "package", "--locked", "--no-verify"] {
        assert!(crates.contains(required), "crate helper lost {required}");
    }
}

#[cfg(unix)]
#[test]
fn canonical_payload_helpers_are_executable() {
    use std::os::unix::fs::PermissionsExt;

    for filename in ["canonical-wasm-bundle.py", "canonical-crate-set.py"] {
        let path = repo_root().join("scripts/release").join(filename);
        let mode = fs::metadata(&path)
            .unwrap_or_else(|error| panic!("read {filename} metadata: {error}"))
            .permissions()
            .mode();
        assert_ne!(mode & 0o111, 0, "{filename} is not executable");
    }
}
