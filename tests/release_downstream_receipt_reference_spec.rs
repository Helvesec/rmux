#![cfg(unix)]

use std::path::PathBuf;
use std::process::Command;

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

#[test]
fn rich_receipt_release_is_normalized_for_downstream_documents() {
    let source = r#"
import runpy
import sys

sys.path.insert(0, "scripts/release")
module = runpy.run_path("scripts/release/build-downstream-receipt-reference.py")
normalize = module["downstream_release_identity"]
release = {
    "id": 7,
    "ref": "v1.2.3",
    "intent_id": "release:v1.2.3:test",
    "kind": "stable",
    "tag_object_sha": "a" * 40,
    "immutable": True,
    "created_at": "2026-07-20T00:00:00Z",
    "published_at": "2026-07-20T00:01:00Z",
}
expected = {
    key: release[key]
    for key in ("id", "ref", "intent_id", "kind", "tag_object_sha", "immutable")
}
assert normalize(release) == expected

with_extra = {**release, "unexpected": True}
try:
    normalize(with_extra)
except ValueError as error:
    assert "publication receipt release keys differ" in str(error)
else:
    raise AssertionError("unexpected release fields were accepted")

out_of_order = {
    **release,
    "created_at": "2026-07-20T00:02:00Z",
    "published_at": "2026-07-20T00:01:00Z",
}
try:
    normalize(out_of_order)
except ValueError as error:
    assert "publication predates release creation" in str(error)
else:
    raise AssertionError("out-of-order release timestamps were accepted")
"#;
    let output = Command::new("python3")
        .arg("-c")
        .arg(source)
        .current_dir(repo_root())
        .output()
        .expect("run downstream receipt normalization");
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}
