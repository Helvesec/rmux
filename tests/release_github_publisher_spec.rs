#![cfg(unix)]

use serde_json::{json, Value};
use std::collections::BTreeMap;
use std::fs;
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::os::unix::fs::{symlink, PermissionsExt};
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, SystemTime, UNIX_EPOCH};

const SOURCE: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const TAG_OBJECT: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const AUDIT_DIGEST: &str =
    "sha256:cccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccccc";
const TITLE: &str = "RMUX 1.2.3";
const NOTES: &str = "Exact release notes.\n";
const NOW: &str = "2026-07-19T00:05:00Z";

#[derive(Clone)]
struct AssetSpec {
    name: String,
    bytes: Vec<u8>,
    digest: String,
}

struct Fixture {
    root: PathBuf,
    predicate: PathBuf,
    envelope: PathBuf,
    assets_dir: PathBuf,
    notes: PathBuf,
    ledger: PathBuf,
    gh_verifier: PathBuf,
    assets: Vec<AssetSpec>,
}

impl Drop for Fixture {
    fn drop(&mut self) {
        fs::remove_dir_all(&self.root).expect("remove publisher fixture");
    }
}

#[derive(Clone, Copy)]
enum InitialRelease {
    Absent,
    ExactDraft(usize),
    Public,
    ExtraAsset,
    DifferentAsset,
}

#[derive(Clone)]
struct RequestRecord {
    method: String,
    path: String,
    body: Vec<u8>,
}

struct ServerState {
    draft: Option<bool>,
    assets: BTreeMap<String, Value>,
    requests: Vec<RequestRecord>,
    expected_assets: BTreeMap<String, AssetSpec>,
}

struct FakeServer {
    address: String,
    state: Arc<Mutex<ServerState>>,
    stop: Arc<AtomicBool>,
    handle: Option<JoinHandle<()>>,
}

impl FakeServer {
    fn start(fixture: &Fixture, initial: InitialRelease) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").expect("bind fake GitHub API");
        listener
            .set_nonblocking(true)
            .expect("set fake API nonblocking");
        let address = listener.local_addr().expect("fake API address").to_string();
        let expected_assets: BTreeMap<_, _> = fixture
            .assets
            .iter()
            .cloned()
            .map(|asset| (asset.name.clone(), asset))
            .collect();
        let mut assets = BTreeMap::new();
        let draft = match initial {
            InitialRelease::Absent => None,
            InitialRelease::Public => Some(false),
            _ => Some(true),
        };
        match initial {
            InitialRelease::ExactDraft(count) => {
                for asset in fixture.assets.iter().take(count) {
                    assets.insert(asset.name.clone(), asset_json(asset));
                }
            }
            InitialRelease::ExtraAsset => {
                assets.insert(
                    "evil.txt".to_owned(),
                    json!({"id": 99, "name": "evil.txt", "state": "uploaded", "size": 4, "digest": format!("sha256:{}", "d".repeat(64))}),
                );
            }
            InitialRelease::DifferentAsset => {
                let asset = &fixture.assets[0];
                assets.insert(
                    asset.name.clone(),
                    json!({"id": 99, "name": asset.name, "state": "uploaded", "size": asset.bytes.len(), "digest": format!("sha256:{}", "e".repeat(64))}),
                );
            }
            InitialRelease::Absent | InitialRelease::Public => {}
        }
        let state = Arc::new(Mutex::new(ServerState {
            draft,
            assets,
            requests: Vec::new(),
            expected_assets,
        }));
        let stop = Arc::new(AtomicBool::new(false));
        let thread_state = Arc::clone(&state);
        let thread_stop = Arc::clone(&stop);
        let handle = thread::spawn(move || {
            while !thread_stop.load(Ordering::SeqCst) {
                match listener.accept() {
                    Ok((stream, _)) => handle_request(stream, &thread_state),
                    Err(error) if error.kind() == std::io::ErrorKind::WouldBlock => {
                        thread::sleep(Duration::from_millis(2));
                    }
                    Err(error) => panic!("fake API accept failed: {error}"),
                }
            }
        });
        Self {
            address,
            state,
            stop,
            handle: Some(handle),
        }
    }

    fn url(&self) -> String {
        format!("http://{}", self.address)
    }

    fn requests(&self) -> Vec<RequestRecord> {
        self.state.lock().expect("fake API state").requests.clone()
    }

    fn is_public(&self) -> bool {
        self.state.lock().expect("fake API state").draft == Some(false)
    }
}

impl Drop for FakeServer {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::SeqCst);
        let _ = TcpStream::connect(&self.address);
        if let Some(handle) = self.handle.take() {
            handle.join().expect("join fake API");
        }
    }
}

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn temp_dir() -> PathBuf {
    let nonce = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .expect("clock before epoch")
        .as_nanos();
    let path = std::env::temp_dir().join(format!(
        "rmux-release-github-publisher-{}-{nonce}",
        std::process::id()
    ));
    fs::create_dir_all(&path).expect("create publisher fixture");
    path
}

fn write_json(path: &Path, value: &Value) {
    let mut encoded = serde_json::to_string_pretty(value).expect("encode JSON fixture");
    encoded.push('\n');
    fs::write(path, encoded).expect("write JSON fixture");
}

fn sha256(path: &Path) -> String {
    let output = Command::new("sha256sum")
        .arg(path)
        .output()
        .expect("run sha256sum");
    assert!(output.status.success());
    String::from_utf8(output.stdout)
        .expect("sha256sum UTF-8")
        .split_whitespace()
        .next()
        .expect("sha256sum output")
        .to_owned()
}

fn asset_record(asset: &AssetSpec, role: &str) -> Value {
    json!({
        "name": asset.name,
        "platform_key": if role == "archive" { Some("linux-x86_64") } else { None },
        "role": role,
        "size": asset.bytes.len(),
        "sha256": asset.digest,
    })
}

fn fixture(active: bool) -> Fixture {
    let root = temp_dir();
    let assets_dir = root.join("assets");
    fs::create_dir(&assets_dir).expect("create assets directory");
    let raw_assets = [
        (
            "rmux-v1.2.3-linux-x86_64.tar.gz",
            b"archive bytes\n".as_slice(),
        ),
        ("SHA256SUMS", b"checksums bytes\n".as_slice()),
        (
            "SHA256SUMS.sigstore.json",
            b"{\"verificationMaterial\":\"fixture\"}\n".as_slice(),
        ),
    ];
    let mut assets = Vec::new();
    for (name, bytes) in raw_assets {
        let path = assets_dir.join(name);
        fs::write(&path, bytes).expect("write authorized asset");
        assets.push(AssetSpec {
            name: name.to_owned(),
            bytes: bytes.to_vec(),
            digest: sha256(&path),
        });
    }
    let predicate = root.join("promotion-authorization.json");
    let archive = asset_record(&assets[0], "archive");
    let sums = asset_record(&assets[1], "checksums");
    let authorization = json!({
        "run_id": 501,
        "run_attempt": 1,
        "workflow_id": 316435346,
        "workflow_path": ".github/workflows/release-promote.yml",
    });
    let predicate_value = json!({
        "schema_version": 1,
        "predicate_type": "https://rmux.io/attestations/release-promotion-authorization/v1",
        "status": "promotion-authorized",
        "publication_authority": true,
        "repository": {"id": 1239918790, "full_name": "Helvesec/rmux"},
        "source_git_sha": SOURCE,
        "release": {"intent_id": "release-1.2.3", "ref": "v1.2.3", "kind": "stable", "version": "1.2.3", "is_prerelease": false},
        "candidate": {
            "schema_version": 1, "status": "shadow-non-authoritative",
            "repository_id": 1239918790, "source_git_sha": SOURCE,
            "candidate_run_id": 401, "candidate_run_attempt": 1,
            "manifest_run_id": 402, "manifest_run_attempt": 1,
            "manifest_workflow_id": 316223904,
            "manifest_workflow_path": ".github/workflows/release-shadow.yml",
            "manifest_artifact_id": 403,
            "manifest_artifact_digest": format!("sha256:{}", "5".repeat(64)),
            "manifest_sha256": "6".repeat(64),
            "manifest_created_at": "2026-07-18T23:00:00Z",
            "manifest_expires_at": "2026-07-19T01:00:00Z"
        },
        "signed_tag": {
            "schema_version": 1,
            "status": "verified-signed-annotated-tag",
            "repository_id": 1239918790,
            "release_ref": "v1.2.3",
            "release_intent_id": "release-1.2.3",
            "release_kind": "stable",
            "tag_object_sha": TAG_OBJECT,
            "target_git_sha": SOURCE,
            "candidate_run_id": 401,
            "candidate_manifest_artifact_id": 403,
            "candidate_manifest_artifact_digest": format!("sha256:{}", "5".repeat(64)),
            "candidate_manifest_sha256": "6".repeat(64),
            "release_policy_root_sha256": "3".repeat(64),
            "object_type": "tag",
            "annotated": true,
            "signature": {"verified": true, "format": "ssh", "key_fingerprint": format!("SHA256:{}", "A".repeat(43)), "signing_principal": "rmux-release@rmux.io"},
            "verified_at": "2026-07-19T00:00:30Z",
        },
        "policy_audit": {
            "repository_id": 1239918790,
            "source_git_sha": SOURCE,
            "release_intent_id": "release-1.2.3",
            "policy_audit_run_id": 501,
            "policy_audit_run_attempt": 1,
            "predicate_artifact_id": 503,
            "predicate_artifact_digest": AUDIT_DIGEST,
            "predicate_sha256": "1".repeat(64),
            "reference_sha256": "2".repeat(64),
            "app_id": 4344532,
            "installation_id": 147749910,
            "workflow_id": 316435346,
            "workflow_path": ".github/workflows/release-promote.yml",
            "release_policy_sha256": "3".repeat(64),
            "emitted_at": "2026-07-19T00:00:00Z",
            "expires_at": "2026-07-19T00:15:00Z",
        },
        "release_policy_sha256": "3".repeat(64),
        "authorization": authorization,
        "issued_at": "2026-07-19T00:01:00Z",
        "expires_at": "2026-07-19T00:10:00Z",
        "asset_count": 2,
        "sha256sums_sha256": assets[1].digest,
        "assets": [archive, sums],
    });
    write_json(&predicate, &predicate_value);
    let verification = root.join("attestation-verification.json");
    write_json(
        &verification,
        &json!([{
            "verificationResult": {
                "statement": {
                    "subject": [{"name": assets[0].name, "digest": {"sha256": assets[0].digest}}],
                    "predicateType": "https://rmux.io/attestations/release-promotion-authorization/v1",
                    "predicate": predicate_value
                }
            }
        }]),
    );
    let gh_verifier = root.join("gh");
    fs::write(
        &gh_verifier,
        format!("#!/bin/sh\ncat '{}'\n", verification.display()),
    )
    .expect("write fake attestation verifier");
    let mut permissions = fs::metadata(&gh_verifier)
        .expect("fake verifier metadata")
        .permissions();
    permissions.set_mode(0o700);
    fs::set_permissions(&gh_verifier, permissions).expect("make fake verifier executable");
    let envelope = root.join("promotion-authorization-envelope.json");
    write_json(
        &envelope,
        &json!({
            "schema_version": 1,
            "envelope_type": "https://rmux.io/envelopes/release-promotion-authorization/v1",
            "status": "promotion-authorized",
            "publication_authority": true,
            "repository_id": 1239918790,
            "source_git_sha": SOURCE,
            "release_ref": "v1.2.3",
            "release_intent_id": "release-1.2.3",
            "authorization": authorization,
            "predicate_sha256": sha256(&predicate),
            "sha256sums_sha256": assets[1].digest,
            "attestation": {"attestation_id": "attestation-1", "bundle_file": "SHA256SUMS.sigstore.json", "bundle_sha256": assets[2].digest},
            "authorization_bundle": {
                "artifact_id": 701,
                "name": format!("rmux-promotion-authorization-{SOURCE}"),
                "archive_digest": format!("sha256:{}", "4".repeat(64)),
                "size_in_bytes": 4096,
            },
            "public_metadata_assets": [{"name": assets[2].name, "role": "authorization-attestation", "size": assets[2].bytes.len(), "sha256": assets[2].digest}],
            "created_at": "2026-07-19T00:02:00Z",
        }),
    );
    let notes = root.join("notes.md");
    fs::write(&notes, NOTES).expect("write release notes");
    let ledger = root.join("release-activation.json");
    write_json(
        &ledger,
        &json!({
            "schema_version": 1,
            "status": if active { "active" } else { "disarmed" },
            "description": "offline publisher fixture",
            "cutover_pr": "PR8",
            "runtime_override_allowed": false,
            "capabilities": {
                "downstream_channels": false,
                "github_release_publication": active,
                "policy_audit": active,
                "promotion_authorization": active,
                "publication_receipt": active,
                "signed_tag_creation": active,
            },
        }),
    );
    Fixture {
        root,
        predicate,
        envelope,
        assets_dir,
        notes,
        ledger,
        gh_verifier,
        assets,
    }
}

fn base_arguments(fixture: &Fixture, server: &FakeServer) -> Vec<String> {
    vec![
        "--predicate".into(),
        fixture.predicate.display().to_string(),
        "--envelope".into(),
        fixture.envelope.display().to_string(),
        "--assets-dir".into(),
        fixture.assets_dir.display().to_string(),
        "--notes-file".into(),
        fixture.notes.display().to_string(),
        "--title".into(),
        TITLE.into(),
        "--activation-ledger".into(),
        fixture.ledger.display().to_string(),
        "--api-root".into(),
        server.url(),
        "--test-only-loopback-api".into(),
        "--now".into(),
        NOW.into(),
    ]
}

fn invoke(fixture: &Fixture, server: &FakeServer, execute: bool) -> Output {
    let mut args = base_arguments(fixture, server);
    if execute {
        args.extend([
            "--execute".into(),
            "--token".into(),
            "explicit-test-token".into(),
            "--gh-verifier".into(),
            fixture.gh_verifier.display().to_string(),
        ]);
    }
    Command::new("python3")
        .arg(repo_root().join("scripts/release/publish-github-release.py"))
        .args(args)
        .current_dir(repo_root())
        .output()
        .expect("run GitHub Release publisher")
}

fn assert_success(output: Output) -> Value {
    assert!(
        output.status.success(),
        "stdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    serde_json::from_slice(&output.stdout).expect("publisher JSON output")
}

fn assert_rejected(output: Output, expected: &str) {
    assert!(!output.status.success(), "publisher unexpectedly succeeded");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(
        stderr.contains(expected),
        "missing {expected:?} in stderr:\n{stderr}"
    );
}

fn asset_json(asset: &AssetSpec) -> Value {
    json!({
        "id": 1000 + asset.name.len(),
        "name": asset.name,
        "state": "uploaded",
        "size": asset.bytes.len(),
        "digest": format!("sha256:{}", asset.digest),
    })
}

fn release_json(draft: bool) -> Value {
    json!({
        "id": 42,
        "tag_name": "v1.2.3",
        "target_commitish": SOURCE,
        "name": TITLE,
        "body": NOTES,
        "draft": draft,
        "prerelease": false,
        "immutable": !draft,
    })
}

fn read_http_request(stream: &mut TcpStream) -> RequestRecord {
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("set fake API read timeout");
    let mut bytes = Vec::new();
    let mut buffer = [0_u8; 8192];
    let header_end = loop {
        let count = stream.read(&mut buffer).expect("read fake API request");
        assert!(count > 0, "fake API request ended before headers");
        bytes.extend_from_slice(&buffer[..count]);
        if let Some(index) = bytes.windows(4).position(|window| window == b"\r\n\r\n") {
            break index + 4;
        }
    };
    let headers = String::from_utf8(bytes[..header_end].to_vec()).expect("request headers UTF-8");
    let content_length = headers
        .lines()
        .find_map(|line| {
            let (name, value) = line.split_once(':')?;
            name.eq_ignore_ascii_case("content-length")
                .then(|| value.trim().parse::<usize>().expect("content length"))
        })
        .unwrap_or(0);
    while bytes.len() < header_end + content_length {
        let count = stream.read(&mut buffer).expect("read fake API body");
        assert!(count > 0, "fake API request body ended early");
        bytes.extend_from_slice(&buffer[..count]);
    }
    let first = headers.lines().next().expect("request line");
    let mut parts = first.split_whitespace();
    RequestRecord {
        method: parts.next().expect("request method").to_owned(),
        path: parts.next().expect("request path").to_owned(),
        body: bytes[header_end..header_end + content_length].to_vec(),
    }
}

fn respond(stream: &mut TcpStream, status: u16, value: &Value) {
    let body = serde_json::to_vec(value).expect("encode fake API response");
    let reason = match status {
        200 => "OK",
        201 => "Created",
        404 => "Not Found",
        422 => "Unprocessable Entity",
        _ => "Error",
    };
    write!(
        stream,
        "HTTP/1.1 {status} {reason}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    )
    .expect("write fake API headers");
    stream.write_all(&body).expect("write fake API response");
}

fn handle_request(mut stream: TcpStream, shared: &Arc<Mutex<ServerState>>) {
    let request = read_http_request(&mut stream);
    let mut state = shared.lock().expect("fake API state");
    state.requests.push(request.clone());
    let release_path = "/repos/Helvesec/rmux/releases/tags/v1.2.3";
    let assets_path = "/repos/Helvesec/rmux/releases/42/assets?per_page=100";
    let (status, value) = match (request.method.as_str(), request.path.as_str()) {
        ("GET", path) if path == release_path => match state.draft {
            Some(draft) => (200, release_json(draft)),
            None => (404, json!({"message": "Not Found"})),
        },
        ("POST", "/repos/Helvesec/rmux/releases") if state.draft.is_none() => {
            let body: Value = serde_json::from_slice(&request.body).expect("create draft body");
            assert_eq!(body["tag_name"], "v1.2.3");
            assert_eq!(body["target_commitish"], SOURCE);
            assert_eq!(body["name"], TITLE);
            assert_eq!(body["body"], NOTES);
            assert_eq!(body["draft"], true);
            assert_eq!(body["make_latest"], "true");
            state.draft = Some(true);
            (201, release_json(true))
        }
        ("GET", path) if path == assets_path && state.draft.is_some() => {
            (200, Value::Array(state.assets.values().cloned().collect()))
        }
        ("POST", path)
            if path.starts_with("/repos/Helvesec/rmux/releases/42/assets?name=")
                && state.draft == Some(true) =>
        {
            let name = path.split_once("?name=").expect("upload query").1;
            if let Some(expected) = state.expected_assets.get(name).cloned() {
                assert_eq!(request.body, expected.bytes, "uploaded bytes differ");
                let value = asset_json(&expected);
                state.assets.insert(name.to_owned(), value.clone());
                (201, value)
            } else {
                (422, json!({"message": "unexpected asset"}))
            }
        }
        ("GET", "/repos/Helvesec/rmux/git/ref/tags/v1.2.3") => (
            200,
            json!({"ref": "refs/tags/v1.2.3", "object": {"type": "tag", "sha": TAG_OBJECT, "url": "https://api.example/tag"}}),
        ),
        ("GET", path) if path == format!("/repos/Helvesec/rmux/git/tags/{TAG_OBJECT}") => (
            200,
            json!({"tag": "v1.2.3", "object": {"type": "commit", "sha": SOURCE}, "verification": {"verified": true, "reason": "valid"}}),
        ),
        ("GET", "/repos/Helvesec/rmux/actions/runs/501") => (
            200,
            json!({"id": 501, "run_attempt": 1, "workflow_id": 316435346, "path": ".github/workflows/release-promote.yml", "head_sha": SOURCE, "status": "in_progress", "conclusion": null, "repository": {"id": 1239918790}}),
        ),
        ("GET", "/repos/Helvesec/rmux/actions/artifacts/503") => (
            200,
            json!({"id": 503, "expired": false, "digest": AUDIT_DIGEST, "workflow_run": {"id": 501, "head_sha": SOURCE}}),
        ),
        ("PATCH", "/repos/Helvesec/rmux/releases/42") if state.draft == Some(true) => {
            assert_eq!(
                serde_json::from_slice::<Value>(&request.body).expect("publish body"),
                json!({"draft": false, "make_latest": "true"})
            );
            state.draft = Some(false);
            (200, release_json(false))
        }
        _ => (404, json!({"message": "unexpected fake API request"})),
    };
    respond(&mut stream, status, &value);
}

fn mutating(request: &RequestRecord) -> bool {
    matches!(request.method.as_str(), "POST" | "PATCH" | "PUT" | "DELETE")
}

#[test]
fn disabled_activation_fails_before_any_http_request() {
    let fixture = fixture(false);
    let server = FakeServer::start(&fixture, InitialRelease::Absent);
    assert_rejected(invoke(&fixture, &server, true), "not activated");
    assert!(server.requests().is_empty());
}

#[test]
fn simulation_can_never_enter_execute_mode() {
    let fixture = fixture(true);
    let server = FakeServer::start(&fixture, InitialRelease::Absent);
    let mut arguments = base_arguments(&fixture, &server);
    arguments.extend(["--simulation".into(), "--execute".into()]);
    let output = Command::new("python3")
        .arg(repo_root().join("scripts/release/publish-github-release.py"))
        .args(arguments)
        .current_dir(repo_root())
        .output()
        .expect("run simulation execute rejection");
    assert_rejected(
        output,
        "simulation requires a read-only loopback publication plan",
    );
    assert!(server.requests().is_empty());
}

#[test]
fn publication_requires_all_upstream_capabilities_armed() {
    let fixture = fixture(true);
    let mut ledger: Value =
        serde_json::from_str(&fs::read_to_string(&fixture.ledger).expect("read activation ledger"))
            .expect("parse activation ledger");
    ledger["capabilities"]["publication_receipt"] = json!(false);
    write_json(&fixture.ledger, &ledger);
    let server = FakeServer::start(&fixture, InitialRelease::Absent);
    assert_rejected(
        invoke(&fixture, &server, true),
        "prerequisites are not activated",
    );
    assert!(server.requests().is_empty());
}

#[test]
fn default_mode_is_a_read_only_plan() {
    let fixture = fixture(false);
    let server = FakeServer::start(&fixture, InitialRelease::Absent);
    let result = assert_success(invoke(&fixture, &server, false));
    assert_eq!(result["mode"], "plan");
    assert_eq!(result["action"], "create-draft");
    assert_eq!(result["mutations"], false);
    let requests = server.requests();
    assert_eq!(requests.len(), 1);
    assert!(requests.iter().all(|request| request.method == "GET"));
}

#[test]
fn invalid_authorization_attestation_fails_before_any_http_request() {
    let fixture = fixture(true);
    fs::write(&fixture.gh_verifier, "#!/bin/sh\nexit 1\n")
        .expect("break fake attestation verifier");
    let server = FakeServer::start(&fixture, InitialRelease::Absent);
    assert_rejected(
        invoke(&fixture, &server, true),
        "attestation verification failed closed",
    );
    assert!(server.requests().is_empty());
}

#[test]
fn wrong_policy_audit_app_identity_fails_before_any_http_request() {
    let fixture = fixture(false);
    let mut predicate: Value = serde_json::from_slice(
        &fs::read(&fixture.predicate).expect("read authorization predicate"),
    )
    .expect("parse authorization predicate");
    predicate["policy_audit"]["app_id"] = json!(4344533);
    write_json(&fixture.predicate, &predicate);
    let mut envelope: Value =
        serde_json::from_slice(&fs::read(&fixture.envelope).expect("read authorization envelope"))
            .expect("parse authorization envelope");
    envelope["predicate_sha256"] = json!(sha256(&fixture.predicate));
    write_json(&fixture.envelope, &envelope);
    let server = FakeServer::start(&fixture, InitialRelease::Absent);
    assert_rejected(
        invoke(&fixture, &server, false),
        "authorization policy audit binding changed",
    );
    assert!(server.requests().is_empty());
}

#[test]
fn symlinked_attestation_verifier_fails_before_any_http_request() {
    let fixture = fixture(true);
    let verifier_target = fixture.root.join("gh-verifier-target");
    fs::rename(&fixture.gh_verifier, &verifier_target).expect("move verifier target");
    symlink(&verifier_target, &fixture.gh_verifier).expect("symlink verifier");
    let server = FakeServer::start(&fixture, InitialRelease::Absent);
    assert_rejected(
        invoke(&fixture, &server, true),
        "attestation verifier must be one regular file",
    );
    assert!(server.requests().is_empty());
}

#[test]
fn creates_exact_draft_uploads_exact_bytes_then_publishes_once() {
    let fixture = fixture(true);
    let server = FakeServer::start(&fixture, InitialRelease::Absent);
    let result = assert_success(invoke(&fixture, &server, true));
    assert_eq!(result["published"], true);
    assert!(server.is_public());
    let requests = server.requests();
    let first_mutation = requests
        .iter()
        .position(mutating)
        .expect("publisher mutation");
    for required in [
        "/repos/Helvesec/rmux/git/ref/tags/v1.2.3",
        "/repos/Helvesec/rmux/actions/runs/501",
        "/repos/Helvesec/rmux/actions/artifacts/503",
    ] {
        assert!(requests[..first_mutation]
            .iter()
            .any(|request| request.method == "GET" && request.path == required));
    }
    assert_eq!(
        requests
            .iter()
            .filter(|item| item.method == "PATCH")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|item| item.method == "POST" && item.path == "/repos/Helvesec/rmux/releases")
            .count(),
        1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|item| item.method == "POST" && item.path.contains("/assets?name="))
            .count(),
        fixture.assets.len()
    );
    assert!(requests
        .iter()
        .all(|item| item.method != "PUT" && item.method != "DELETE"));
    let tail: Vec<_> = requests
        .iter()
        .rev()
        .take(3)
        .map(|item| item.method.as_str())
        .collect();
    assert_eq!(tail, ["PATCH", "GET", "GET"]);
}

#[test]
fn attestation_policy_pins_workflow_source_ref_and_hosted_runner() {
    let security = include_str!("../scripts/release/release_publish_security.py");
    for required in [
        "--signer-workflow",
        "--signer-digest",
        "--source-digest",
        "--source-ref",
        "--predicate-type",
        "--deny-self-hosted-runners",
    ] {
        assert!(
            security.contains(required),
            "missing attestation gate {required}"
        );
    }
    assert!(security.contains("actual_subjects != expected_subjects"));
}

#[test]
fn resumes_only_the_exact_draft_without_clobbering_existing_asset() {
    let fixture = fixture(true);
    let server = FakeServer::start(&fixture, InitialRelease::ExactDraft(1));
    assert_success(invoke(&fixture, &server, true));
    let requests = server.requests();
    assert!(!requests
        .iter()
        .any(|item| item.method == "POST" && item.path == "/repos/Helvesec/rmux/releases"));
    assert_eq!(
        requests
            .iter()
            .filter(|item| item.method == "POST" && item.path.contains("/assets?name="))
            .count(),
        fixture.assets.len() - 1
    );
    assert_eq!(
        requests
            .iter()
            .filter(|item| item.method == "PATCH")
            .count(),
        1
    );
}

#[test]
fn public_extra_or_different_existing_state_is_never_mutated() {
    for (initial, expected) in [
        (InitialRelease::Public, "existing public"),
        (InitialRelease::ExtraAsset, "extra or duplicated"),
        (
            InitialRelease::DifferentAsset,
            "differs from authorized bytes",
        ),
    ] {
        let fixture = fixture(true);
        let server = FakeServer::start(&fixture, initial);
        assert_rejected(invoke(&fixture, &server, true), expected);
        assert!(server.requests().iter().all(|request| !mutating(request)));
    }
}
