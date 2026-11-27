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
sys.path.insert(0, str(repo / "scripts/release"))
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

#[test]
fn root_crate_excludes_installed_xterm_dependencies() {
    let manifest =
        fs::read_to_string(repo_root().join("Cargo.toml")).expect("read root Cargo manifest");
    assert!(
        manifest.contains("\"!/tests/xterm-oracle/node_modules/**\""),
        "the published root crate must not include npm-installed oracle dependencies"
    );
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

fn run_python_fixture(label: &str, script: &str) {
    let root = temp_dir(label);
    let fixture = root.join("fixture.py");
    fs::write(&fixture, script).expect("write publish order fixture");
    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".to_owned());
    let output = Command::new(python())
        .arg("-B")
        .arg(&fixture)
        .arg(repo_root())
        .arg(&root)
        .arg(cargo)
        .output()
        .expect("run publish order fixture");
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    fs::remove_dir_all(root).expect("remove publish order fixture");
}

const LOAD_CRATE_SET: &str = r#"
import importlib.util
import os
import pathlib
import sys

repo, root = pathlib.Path(sys.argv[1]), pathlib.Path(sys.argv[2])
cargo = sys.argv[3]
sys.path.insert(0, str(repo / "scripts/release"))
path = repo / "scripts/release/canonical-crate-set.py"
spec = importlib.util.spec_from_file_location("canonical_crate_set", path)
module = importlib.util.module_from_spec(spec)
assert spec.loader is not None
spec.loader.exec_module(module)
"#;

#[test]
fn real_workspace_publishes_every_crate_after_each_dependency_cargo_keeps() {
    // Cargo keeps every non-dev edge, and every dev edge that declares a
    // version, in the packaged manifest; crates.io must already serve it.
    run_python_fixture(
        "real-order",
        &format!(
            "{LOAD_CRATE_SET}{}",
            r#"
environment = {**os.environ, "CARGO_NET_OFFLINE": "true"}
metadata = module.cargo_metadata(source_dir=repo, cargo=cargo, environment=environment)
members = set(metadata["workspace_members"])
root_package = next(
    package for package in metadata["packages"]
    if package["id"] in members and package["name"] == "rmux"
)
packages = module.publishable_packages(metadata, root_package["version"])
order, dependencies = module.dependency_order(packages)
position = {name: index for index, name in enumerate(order)}
violations = []
checked = 0
for package in packages:
    for dependency in package["dependencies"]:
        name = dependency["name"]
        if name not in position or dependency.get("path") is None:
            continue
        if dependency.get("kind") == "dev" and dependency["req"] == "*":
            continue
        checked += 1
        if position[name] >= position[package["name"]]:
            violations.append(
                f"{package['name']} (#{position[package['name']] + 1}) needs "
                f"{name} {dependency['req']} [{dependency.get('kind') or 'normal'}] "
                f"published at #{position[name] + 1}"
            )
        if name not in dependencies[package["name"]]:
            violations.append(f"{package['name']} omits workspace dependency {name}")
assert checked > 0, "the real workspace exposed no internal edge"
assert not violations, "publish order: " + ", ".join(order) + "\n" + "\n".join(violations)
"#
        ),
    );
}

#[test]
fn publish_order_counts_kept_dev_edges_and_packaged_manifests_bind_them() {
    run_python_fixture(
        "kept-dev-order",
        &format!(
            "{LOAD_CRATE_SET}{}",
            r#"
import io
import tarfile

def edge(name, kind=None, req="^0.9.0", target=None):
    return {"name": name, "kind": kind, "req": req, "path": f"../{name}", "target": target}

def package(name, dependencies):
    return {"name": name, "id": f"{name}-id", "version": "0.9.0",
            "publish": ["crates-io"], "dependencies": dependencies}

packages = [
    # A path-only dev edge is stripped by Cargo and may close a test cycle.
    package("crate-a", [edge("crate-b", kind="dev", req="*")]),
    # A versioned dev edge stays in the packaged manifest and orders first.
    package("crate-b", [edge("crate-c", kind="dev", target="cfg(unix)")]),
    package("crate-c", [edge("crate-a")]),
]
order, dependencies = module.dependency_order(packages)
assert order == ["crate-a", "crate-c", "crate-b"], order
assert dependencies == {
    "crate-a": [], "crate-b": ["crate-c"], "crate-c": ["crate-a"],
}, dependencies
cycle = [
    package("crate-x", [edge("crate-y", kind="dev")]),
    package("crate-y", [edge("crate-x")]),
]
try:
    module.dependency_order(cycle)
except ValueError as error:
    assert "cycle" in str(error), error
else:
    raise AssertionError("a versioned dev cycle was accepted")

def write_crate(directory, name, manifest):
    data = manifest.encode()
    buffer = io.BytesIO()
    with tarfile.open(fileobj=buffer, mode="w:gz") as archive:
        info = tarfile.TarInfo(f"{name}-0.9.0/Cargo.toml")
        info.size = len(data)
        archive.addfile(info, io.BytesIO(data))
    (directory / f"{name}-0.9.0.crate").write_bytes(buffer.getvalue())

def packaged(label, manifests):
    directory = root / label
    directory.mkdir()
    for name, manifest in manifests.items():
        write_crate(directory, name, manifest)
    return directory

header = '[package]\nname = "{0}"\nversion = "0.9.0"\n'
accepted = packaged("accepted", {
    "crate-a": header.format("crate-a"),
    "crate-b": header.format("crate-b")
    + '\n[target."cfg(unix)".dev-dependencies.crate-c]\nversion = "0.9.0"\n',
    "crate-c": header.format("crate-c")
    + '\n[dependencies.crate-a]\nversion = "0.9.0"\n\n[dependencies.serde]\nversion = "1"\n',
})
module.verify_packaged_requirements(order, dependencies, accepted, "0.9.0")

for label, manifest in {
    # Metadata reports an explicit version "*" exactly like a path-only edge.
    "star": '\n[dev-dependencies.crate-b]\nversion = "*"\n',
    "renamed": '\n[target."cfg(windows)".dev-dependencies.alias]\n'
    'package = "crate-c"\nversion = "0.9.0"\n',
    "build": '\n[build-dependencies]\ncrate-b = "0.9.0"\n',
}.items():
    rejected = packaged(label, {
        "crate-a": header.format("crate-a") + manifest,
        "crate-b": header.format("crate-b"),
        "crate-c": header.format("crate-c"),
    })
    try:
        module.verify_packaged_requirements(order, dependencies, rejected, "0.9.0")
    except ValueError as error:
        assert "crate-a" in str(error) and "publish predecessors" in str(error), error
    else:
        raise AssertionError(f"packaged {label} requirement escaped the order")
"#
        ),
    );
}
