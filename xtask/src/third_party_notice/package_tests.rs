use super::tests::{cargo, write};
use std::fs;
use std::path::Path;
use std::process::Command;
use std::time::{SystemTime, UNIX_EPOCH};

#[test]
#[cfg(unix)]
fn build_notices_survive_packaging_and_private_installation() {
    let repo = Path::new(env!("CARGO_MANIFEST_DIR")).parent().unwrap();
    let root = repo.join("target/audit").join(format!(
        "notice-package-{}-{}",
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    ));
    fs::create_dir_all(&root).unwrap();
    write(
        &root,
        "Cargo.toml",
        "[workspace]\nmembers = [\"app\"]\nresolver = \"2\"\n",
    );
    write(&root, "app/Cargo.toml", "[package]\nname = \"notice-package-fixture\"\nversion = \"0.1.0\"\nedition = \"2021\"\nlicense = \"MIT\"\n");
    write(
        &root,
        "app/LICENSE",
        "Copyright package fixture author\nPermission to redistribute fixture bytes\n",
    );
    write(&root, "app/src/main.rs", "fn main() {}\n");
    let metadata = root.join("metadata.json");
    fs::write(
        &metadata,
        cargo(&root, &["metadata", "--offline", "--format-version=1"]),
    )
    .unwrap();
    let log = root.join("build.jsonl");
    fs::write(
        &log,
        cargo(
            &root,
            &[
                "build",
                "--offline",
                "--locked",
                "--message-format=json-render-diagnostics",
            ],
        ),
    )
    .unwrap();
    let notice = super::generate(
        &metadata,
        std::slice::from_ref(&log),
        &repo.join("deny.toml"),
    )
    .unwrap();
    assert!(notice.contains("Copyright package fixture author"));
    fs::write(root.join("NOTICE.txt"), notice).unwrap();
    let output = Command::new("python3")
        .arg(repo.join("tests/fixtures/installed_package_notices.py"))
        .arg(&root)
        .output()
        .unwrap();
    assert!(
        output.status.success(),
        "{}\n{}\nfixture: {}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr),
        root.display()
    );
}
