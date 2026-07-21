#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

fn repo_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn python(source: &str) -> Output {
    Command::new("python3")
        .arg("-c")
        .arg(source)
        .current_dir(repo_root())
        .output()
        .expect("run downstream writer fixture")
}

fn assert_python(source: &str) {
    let output = python(source);
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn release_workflows_and_composite_actions_are_valid_yaml() {
    let root = repo_root();
    let mut files = Vec::new();
    for entry in fs::read_dir(root.join(".github/workflows")).expect("workflow directory") {
        let path = entry.expect("workflow entry").path();
        if path.extension().is_some_and(|extension| extension == "yml") {
            files.push(path);
        }
    }
    for entry in fs::read_dir(root.join(".github/actions")).expect("action directory") {
        let path = entry.expect("action entry").path().join("action.yml");
        if path.is_file() {
            files.push(path);
        }
    }
    files.sort();
    let output = Command::new("ruby")
        .args([
            "-e",
            "require 'yaml'; ARGV.each { |path| YAML.load_file(path, aliases: true) }",
        ])
        .args(&files)
        .current_dir(root)
        .output()
        .expect("parse workflow YAML with Ruby stdlib");
    assert!(
        output.status.success(),
        "stdout={}\nstderr={}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn exact_artifact_and_snap_sets_reject_extras_and_symlinks() {
    assert_python(
        r#"
import importlib.util
import pathlib
import sys
import tempfile

sys.path.insert(0, 'scripts/release')

def load(name, path):
    spec = importlib.util.spec_from_file_location(name, path)
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module

exact = load('verify_exact_file_set', 'scripts/release/verify-exact-file-set.py')
snap = load('snap_candidate_status', 'scripts/release/snap-candidate-status.py')

with tempfile.TemporaryDirectory(dir=pathlib.Path.cwd()) as directory:
    root = pathlib.Path(directory)
    artifact = root / 'artifact'
    artifact.mkdir()
    (artifact / 'one.json').write_text('{}\n', encoding='utf-8')
    exact.verify(artifact, ['one.json'])
    (artifact / 'extra.json').write_text('{}\n', encoding='utf-8')
    try:
        exact.verify(artifact, ['one.json'])
    except ValueError:
        pass
    else:
        raise SystemExit('extra artifact file was accepted')
    (artifact / 'extra.json').unlink()
    (artifact / 'link.json').symlink_to(artifact / 'one.json')
    try:
        exact.verify(artifact, ['one.json'])
    except ValueError:
        pass
    else:
        raise SystemExit('symbolic artifact file was accepted')

    payload = root / 'snap'
    payload.mkdir()
    for architecture in ('amd64', 'arm64'):
        (payload / f'rmux-1.2.3-snap-{architecture}.snap').write_bytes(architecture.encode())
    snap.payloads(payload, '1.2.3')
    (payload / 'extra').mkdir()
    try:
        snap.payloads(payload, '1.2.3')
    except ValueError:
        pass
    else:
        raise SystemExit('extra Snap payload directory was accepted')
"#,
    );
}

#[test]
fn web_share_live_check_binds_commit_provenance_and_public_wasm() {
    assert_python(
        r#"
import hashlib
import json
import pathlib
import sys
import tempfile

sys.path.insert(0, 'scripts/release')
import web_share_live as live

source = 'a' * 40
commit = 'b' * 40
wasm = b'exact-wasm-bytes'
digest = hashlib.sha256(wasm).hexdigest()

with tempfile.TemporaryDirectory(dir=pathlib.Path.cwd()) as directory:
    provenance = pathlib.Path(directory) / 'provenance.json'
    provenance.write_text(json.dumps({
        'version': '1.2.3',
        'source': {'source_commit': source},
        'artifacts': {'rmux_web_crypto_wasm_bg.wasm': f'sha256:{digest}'},
    }), encoding='utf-8')
    manifest = {
        'schema_version': 1,
        'project': 'rmux-web-share',
        'public_origin': live.PUBLIC_ORIGIN,
        'repository': live.REPOSITORY_URL,
        'commit_sha1': commit,
        'commit_url': f'{live.REPOSITORY_URL}/commit/{commit}',
        'assets': [{
            'path': '/_astro/rmux_web_crypto_wasm_bg.Ab12_cd.wasm',
            'bytes': len(wasm),
            'sha256': digest,
        }],
    }
    def fetch(url, _limit, media_type):
        if media_type == 'application/json':
            return json.dumps(manifest).encode()
        if media_type == 'application/wasm':
            return wasm
        raise AssertionError(media_type)
    live.fetch = fetch
    if live.wait_for_live(
        provenance_path=provenance,
        source_sha=source,
        version='1.2.3',
        commit_sha=commit,
    ) != live.MANIFEST_URL:
        raise SystemExit('exact live Web Share was not accepted')

    forged = dict(manifest)
    forged['assets'] = [dict(manifest['assets'][0], path='/_astro/../payload.wasm')]
    try:
        live.validate_manifest(forged, commit_sha=commit, wasm_sha256=digest)
    except ValueError:
        pass
    else:
        raise SystemExit('unsafe Web Share asset path was accepted')

    provenance.write_text('[]\n', encoding='utf-8')
    try:
        live.expected_wasm_hash(provenance, source, '1.2.3')
    except ValueError:
        pass
    else:
        raise SystemExit('non-object Web Share provenance was accepted')
"#,
    );
}

#[test]
fn crate_package_reader_enforces_exact_safe_dependency_ordered_bytes() {
    assert_python(
        r#"
import hashlib
import io
import json
import pathlib
import sys
import tarfile
import tempfile

sys.path.insert(0, 'scripts/release')
from crate_package_set import unpack, validate

source = 'a' * 40
package = b'canonical crate bytes'
manifest = {
    'schema_version': 1,
    'repository_id': 1239918790,
    'source_git_sha': source,
    'version': '1.2.3',
    'publish_order': ['rmux-core'],
    'package_count': 1,
    'packages': [{
        'name': 'rmux-core',
        'version': '1.2.3',
        'file': 'rmux-core-1.2.3.crate',
        'size': len(package),
        'sha256': hashlib.sha256(package).hexdigest(),
        'workspace_dependencies': [],
    }],
}

def write_tar(path, payloads):
    with tarfile.open(path, 'w') as archive:
        for name, data in sorted(payloads.items()):
            info = tarfile.TarInfo(name)
            info.size = len(data)
            archive.addfile(info, io.BytesIO(data))

with tempfile.TemporaryDirectory(dir=pathlib.Path.cwd()) as directory:
    root = pathlib.Path(directory)
    canonical = {
        'crate-package-set.json': (json.dumps(manifest) + '\n').encode(),
        'crates/rmux-core-1.2.3.crate': package,
    }
    archive = root / 'set.tar'
    write_tar(archive, canonical)
    extracted = root / 'exact'
    value = unpack(archive, extracted)
    ordered = validate(value, extracted, source_sha=source, version='1.2.3')
    if [item['name'] for item in ordered] != ['rmux-core']:
        raise SystemExit('canonical crate order changed')

    extra_archive = root / 'extra.tar'
    write_tar(extra_archive, {**canonical, 'crates/unlisted.crate': b'extra'})
    extra_root = root / 'extra'
    extra = unpack(extra_archive, extra_root)
    try:
        validate(extra, extra_root, source_sha=source, version='1.2.3')
    except ValueError:
        pass
    else:
        raise SystemExit('unlisted crate member was accepted')

    unsafe = root / 'unsafe.tar'
    with tarfile.open(unsafe, 'w') as archive_handle:
        member = tarfile.TarInfo('crate-package-set.json')
        member.type = tarfile.SYMTYPE
        member.linkname = '../outside'
        archive_handle.addfile(member)
    try:
        unpack(unsafe, root / 'unsafe')
    except ValueError:
        pass
    else:
        raise SystemExit('symbolic crate member was accepted')
"#,
    );
}

#[test]
fn github_repository_writer_is_atomic_idempotent_and_prefix_exact() {
    assert_python(
        r#"
import urllib.parse
import sys

sys.path.insert(0, 'scripts/release')
from github_repository_writer import publish

class FakeApi:
    def __init__(self, files):
        self.head = '1' * 40
        self.files = dict(files)
        self.blobs = {}
        self.trees = {}
        self.commits = {}
        self.posts = 0

    def sha(self):
        self.posts += 1
        return f'{self.posts + 1:040x}'

    def get(self, path):
        if '/git/ref/heads/' in path:
            return {'object': {'type': 'commit', 'sha': self.head}}
        if '/git/commits/' in path:
            return {'tree': {'sha': 'f' * 40}}
        if '/git/trees/' in path:
            return {
                'truncated': False,
                'tree': [
                    {'path': name, 'type': 'blob'} for name in sorted(self.files)
                ],
            }
        raise AssertionError(path)

    def get_bytes(self, path, *, limit):
        encoded = path.split('/contents/', 1)[1].split('?ref=', 1)[0]
        name = urllib.parse.unquote(encoded)
        if name not in self.files:
            raise ValueError(f'GitHub API GET {path} failed: 404 missing')
        data = self.files[name]
        if len(data) > limit:
            raise AssertionError('fixture exceeds limit')
        return data

    def post(self, path, payload):
        sha = self.sha()
        if path.endswith('/git/blobs'):
            import base64
            self.blobs[sha] = base64.b64decode(payload['content'])
        elif path.endswith('/git/trees'):
            self.trees[sha] = payload['tree']
        elif path.endswith('/git/commits'):
            files = dict(self.files)
            for entry in self.trees[payload['tree']]:
                if entry['sha'] is None:
                    files.pop(entry['path'], None)
                else:
                    files[entry['path']] = self.blobs[entry['sha']]
            self.commits[sha] = files
        else:
            raise AssertionError(path)
        return {'sha': sha}

    def patch(self, path, payload):
        if payload.get('force') is not False or payload['sha'] not in self.commits:
            raise AssertionError('non-atomic ref update')
        self.files = self.commits[payload['sha']]
        self.head = payload['sha']
        return {'object': {'sha': self.head}}

api = FakeApi({'managed/old.bin': b'old', 'keep.txt': b'keep'})
base = api.head
outcome = publish(
    api,
    full_name='Helvesec/rmux-packages',
    branch='main',
    updates={'managed/new.bin': b'new'},
    message='publish exact bytes',
    managed_prefixes=('managed',),
    expected_base=base,
)
if outcome.state != 'public-live' or not outcome.mutation_started:
    raise SystemExit('repository mutation outcome differs')
if api.files != {'managed/new.bin': b'new', 'keep.txt': b'keep'}:
    raise SystemExit(f'managed repository set differs: {api.files!r}')

posts = api.posts
same = publish(
    api,
    full_name='Helvesec/rmux-packages',
    branch='main',
    updates={'managed/new.bin': b'new'},
    message='publish exact bytes',
    managed_prefixes=('managed',),
    expected_base=api.head,
)
if same.state != 'no-op-exact' or same.mutation_started or api.posts != posts:
    raise SystemExit('exact repository no-op wrote Git objects')

try:
    publish(
        api,
        full_name='Helvesec/rmux-packages',
        branch='main',
        updates={'managed/new.bin': b'new'},
        message='stale',
        managed_prefixes=('managed',),
        expected_base='0' * 40,
    )
except ValueError:
    pass
else:
    raise SystemExit('stale repository base was accepted')
"#,
    );
}
