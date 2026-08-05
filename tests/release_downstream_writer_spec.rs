#![cfg(unix)]

use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

const GITHUB_REPOSITORY_WRITER: &str =
    include_str!("../scripts/release/github_repository_writer.py");
const GITHUB_REPOSITORY_COMMIT_PLAN: &str =
    include_str!("../scripts/release/github_repository_commit_plan.py");

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
    let ruby = Command::new("ruby")
        .args([
            "-e",
            "require 'yaml'; loader = YAML.method(:load_file); supports_aliases = loader.parameters.any? { |kind, name| (kind == :key && name == :aliases) || kind == :keyrest }; ARGV.each { |path| supports_aliases ? YAML.load_file(path, aliases: true) : YAML.load_file(path) }",
        ])
        .args(&files)
        .current_dir(&root)
        .output();
    let output = match ruby {
        Ok(output) => output,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => Command::new("python3")
            .args([
                "-c",
                "import sys, yaml\nfor path in sys.argv[1:]:\n    with open(path, encoding='utf-8') as stream:\n        yaml.safe_load(stream)",
            ])
            .args(&files)
            .current_dir(&root)
            .output()
            .expect("parse workflow YAML with PyYAML"),
        Err(error) => panic!("start workflow YAML parser: {error}"),
    };
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
    if snap.package_version('v1.2.3-rc.1') != '1.2.3':
        raise SystemExit('RC Snap payload did not preserve the package version')
    try:
        snap.package_version('v1.2.3-rc.01')
    except ValueError:
        pass
    else:
        raise SystemExit('non-canonical RC Snap ref was accepted')
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
fn downstream_collector_rejects_repository_scope_and_ruleset_drift() {
    assert_python(
        r#"
import importlib.util
import sys

sys.path.insert(0, 'scripts/release')
spec = importlib.util.spec_from_file_location(
    'collect_downstream_repository',
    'scripts/release/collect-downstream-repository.py',
)
collector = importlib.util.module_from_spec(spec)
spec.loader.exec_module(collector)

_, repositories = collector.repository_contract()

class Api:
    def __init__(self, values):
        self.values = values

    def get(self, path):
        return self.values[path]

selected = [
    {'id': item['id'], 'full_name': item['full_name']}
    for item in repositories
]
api = Api({
    '/installation/repositories?per_page=100': {
        'total_count': len(selected),
        'repositories': selected,
    },
})
if collector.exact_installation_repositories(api, repositories) != sorted(
    item['id'] for item in repositories
):
    raise SystemExit('exact downstream repository scope was not preserved')

api.values['/installation/repositories?per_page=100']['repositories'].append({
    'id': 1,
    'full_name': 'Helvesec/unexpected',
})
api.values['/installation/repositories?per_page=100']['total_count'] += 1
try:
    collector.exact_installation_repositories(api, repositories)
except ValueError:
    pass
else:
    raise SystemExit('expanded downstream repository scope was accepted')

ruleset_api = Api({
    '/repos/Helvesec/homebrew-rmux/rulesets?per_page=100': [{'id': 42}],
    '/repos/Helvesec/homebrew-rmux/rulesets/42': {'id': 42},
})
if collector.collect_rulesets(ruleset_api, 'Helvesec/homebrew-rmux') != [{'id': 42}]:
    raise SystemExit('exact downstream ruleset was not collected')
ruleset_api.values['/repos/Helvesec/homebrew-rmux/rulesets?per_page=100'] = [
    {'id': '../../installation'}
]
try:
    collector.collect_rulesets(ruleset_api, 'Helvesec/homebrew-rmux')
except ValueError:
    pass
else:
    raise SystemExit('unsafe downstream ruleset identity was accepted')
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
    assert!(GITHUB_REPOSITORY_COMMIT_PLAN.contains("createCommitOnBranch"));
    assert!(GITHUB_REPOSITORY_COMMIT_PLAN.contains("GRAPHQL_COMMIT_MAX_BYTES = 25_000_000"));
    assert!(GITHUB_REPOSITORY_COMMIT_PLAN.contains("rmux-release-stage"));
    assert!(GITHUB_REPOSITORY_WRITER.contains("wasSignedByGitHub"));
    assert!(GITHUB_REPOSITORY_WRITER.contains("verification.get(\"verified\") is not True"));
    assert!(GITHUB_REPOSITORY_WRITER.contains("/git/refs/heads/"));
    assert!(GITHUB_REPOSITORY_WRITER.contains("{\"sha\": commit_sha, \"force\": False}"));
    assert!(!GITHUB_REPOSITORY_WRITER.contains("f\"/repos/{full_name}/git/commits\""));
    assert_python(
        r#"
import base64
import hashlib
import urllib.parse
import sys

sys.path.insert(0, 'scripts/release')
import github_repository_commit_plan as commit_plan
from github_repository_writer import publish

def blob_sha(data):
    digest = hashlib.sha1()
    digest.update(f'blob {len(data)}\0'.encode())
    digest.update(data)
    return digest.hexdigest()

class FakeApi:
    def __init__(self, files, verified=True):
        root = '1' * 40
        self.branches = {'main': root}
        self.trees = {root: dict(files)}
        self.parents = {}
        self.signed = set()
        self.graphql_calls = 0
        self.ref_updates = 0
        self.ref_deletes = 0
        self.fail_graphql_at = None
        self.transient_graphql_at = None
        self.transient_advances = None
        self.verified = verified

    @property
    def head(self):
        return self.branches['main']

    @property
    def files(self):
        return self.trees[self.head]

    def sha(self):
        return f'{len(self.parents) + 2:040x}'

    def get(self, path):
        if '/git/ref/heads/' in path or '/git/refs/heads/' in path:
            marker = '/git/refs/heads/' if '/git/refs/heads/' in path else '/git/ref/heads/'
            branch = urllib.parse.unquote(path.split(marker, 1)[1])
            if branch not in self.branches:
                raise ValueError(f'GitHub API GET {path} failed: 404 missing')
            return {
                'ref': f'refs/heads/{branch}',
                'object': {'type': 'commit', 'sha': self.branches[branch]},
            }
        if '/git/commits/' in path:
            sha = path.rsplit('/', 1)[1]
            if sha in self.signed:
                return {
                    'parents': [{'sha': self.parents[sha]}],
                    'verification': {
                        'verified': self.verified,
                        'reason': 'valid' if self.verified else 'unsigned',
                        'signature': 'github-signature' if self.verified else None,
                    },
                }
            raise AssertionError(f'unknown commit {sha}')
        if '/git/trees/' in path:
            sha = path.split('/git/trees/', 1)[1].split('?', 1)[0]
            return {
                'truncated': False,
                'tree': [
                    {'path': name, 'type': 'blob', 'sha': blob_sha(self.trees[sha][name])}
                    for name in sorted(self.trees[sha])
                ],
            }
        raise AssertionError(path)

    def get_bytes(self, path, *, limit):
        encoded = path.split('/contents/', 1)[1].split('?ref=', 1)[0]
        name = urllib.parse.unquote(encoded)
        ref = urllib.parse.unquote(path.split('?ref=', 1)[1])
        sha = self.branches.get(ref, ref)
        files = self.trees[sha]
        if name not in files:
            raise ValueError(f'GitHub API GET {path} failed: 404 missing')
        data = files[name]
        if len(data) > limit:
            raise AssertionError('fixture exceeds limit')
        return data

    def graphql(self, query, variables):
        if 'createCommitOnBranch' not in query:
            raise AssertionError('wrong mutation')
        self.graphql_calls += 1
        if self.fail_graphql_at == self.graphql_calls:
            raise ValueError('injected GraphQL failure')
        payload = variables['input']
        branch = payload['branch']
        if branch['repositoryNameWithOwner'] != 'Helvesec/rmux-packages':
            raise AssertionError('wrong branch identity')
        branch_name = branch['branchName']
        if payload['expectedHeadOid'] != self.branches[branch_name]:
            raise AssertionError('non-atomic branch update')
        if self.transient_graphql_at == self.graphql_calls:
            self.transient_graphql_at = None
            if self.transient_advances:
                self.apply_payload(
                    payload,
                    tamper=self.transient_advances == 'unexpected',
                )
            raise ValueError('GitHub API POST /graphql failed: 502 transient')
        sha = self.apply_payload(payload)
        return {
            'createCommitOnBranch': {
                'commit': {
                    'oid': sha,
                    'signature': {
                        'isValid': self.verified,
                        'state': 'VALID' if self.verified else 'UNSIGNED',
                        'wasSignedByGitHub': self.verified,
                    },
                },
                'ref': {'target': {'oid': sha}},
            }
        }

    def apply_payload(self, payload, *, tamper=False):
        branch_name = payload['branch']['branchName']
        sha = self.sha()
        files = dict(self.trees[payload['expectedHeadOid']])
        changes = payload['fileChanges']
        for entry in changes['deletions']:
            files.pop(entry['path'], None)
        for entry in changes['additions']:
            files[entry['path']] = base64.b64decode(entry['contents'])
        if tamper:
            files['managed/unexpected.bin'] = b'unexpected'
        self.trees[sha] = files
        self.parents[sha] = payload['expectedHeadOid']
        self.signed.add(sha)
        self.branches[branch_name] = sha
        return sha

    def post(self, path, payload):
        if path != '/repos/Helvesec/rmux-packages/git/refs':
            raise AssertionError(path)
        branch = payload['ref'].removeprefix('refs/heads/')
        if branch in self.branches:
            raise AssertionError('staging branch already exists')
        self.branches[branch] = payload['sha']
        return {'ref': payload['ref'], 'object': {'sha': payload['sha']}}

    def patch(self, path, payload):
        branch = urllib.parse.unquote(path.split('/git/refs/heads/', 1)[1])
        if payload.get('force') is not False:
            raise AssertionError('force update requested')
        current = payload['sha']
        while current != self.branches[branch]:
            current = self.parents.get(current)
            if current is None:
                raise AssertionError('non-fast-forward staging promotion')
        self.branches[branch] = payload['sha']
        self.ref_updates += 1
        return {'ref': f'refs/heads/{branch}', 'object': {'sha': payload['sha']}}

    def delete(self, path):
        branch = urllib.parse.unquote(path.split('/git/refs/heads/', 1)[1])
        if branch == 'main':
            raise AssertionError('main deletion requested')
        del self.branches[branch]
        self.ref_deletes += 1

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

posts = api.graphql_calls
same = publish(
    api,
    full_name='Helvesec/rmux-packages',
    branch='main',
    updates={'managed/new.bin': b'new'},
    message='publish exact bytes',
    managed_prefixes=('managed',),
    expected_base=base,
)
if same.state != 'no-op-exact' or same.mutation_started or api.graphql_calls != posts:
    raise SystemExit('exact stale-base recovery wrote Git objects')

try:
    publish(
        api,
        full_name='Helvesec/rmux-packages',
        branch='main',
        updates={'managed/different.bin': b'different'},
        message='stale',
        managed_prefixes=('managed',),
        expected_base=base,
    )
except ValueError:
    pass
else:
    raise SystemExit('stale repository base was accepted')

commit_plan.GRAPHQL_COMMIT_MAX_BYTES = 1_600
large = FakeApi({'managed/old.bin': b'old', 'keep.txt': b'keep'})
large_base = large.head
large_updates = {
    'managed/one.bin': b'a' * 600,
    'managed/two.bin': b'b' * 600,
    'managed/three.bin': b'c' * 600,
}
large_outcome = publish(
    large,
    full_name='Helvesec/rmux-packages',
    branch='main',
    updates=large_updates,
    message='publish chunked bytes',
    managed_prefixes=('managed',),
    expected_base=large_base,
)
if large_outcome.state != 'public-live' or large.ref_updates != 1:
    raise SystemExit('chunked update did not advance main exactly once')
if large.graphql_calls < 2 or large.files != {**large_updates, 'keep.txt': b'keep'}:
    raise SystemExit('chunked update did not preserve the exact final tree')
if set(large.branches) != {'main'} or large.ref_deletes != 1:
    raise SystemExit('chunked update left its staging branch behind')

transient = FakeApi({'managed/old.bin': b'old'})
transient.transient_graphql_at = 2
transient_outcome = publish(
    transient,
    full_name='Helvesec/rmux-packages',
    branch='main',
    updates=large_updates,
    message='retry transient chunked bytes',
    managed_prefixes=('managed',),
    expected_base=transient.head,
)
if transient_outcome.state != 'public-live' or transient.ref_updates != 1:
    raise SystemExit('transient signed commit failure was not recovered')

ambiguous = FakeApi({'managed/old.bin': b'old'})
ambiguous_base = ambiguous.head
ambiguous.transient_graphql_at = 2
ambiguous.transient_advances = 'exact'
ambiguous_outcome = publish(
    ambiguous,
    full_name='Helvesec/rmux-packages',
    branch='main',
    updates=large_updates,
    message='recover exact committed response',
    managed_prefixes=('managed',),
    expected_base=ambiguous_base,
)
if ambiguous_outcome.state != 'public-live' or ambiguous.ref_updates != 1:
    raise SystemExit('exact signed commit response loss was not recovered')
if ambiguous.files != large_updates or set(ambiguous.branches) != {'main'}:
    raise SystemExit('recovered signed commit tree differs or leaked staging')

unexpected = FakeApi({'managed/old.bin': b'old'})
unexpected_base = unexpected.head
unexpected.transient_graphql_at = 2
unexpected.transient_advances = 'unexpected'
try:
    publish(
        unexpected,
        full_name='Helvesec/rmux-packages',
        branch='main',
        updates=large_updates,
        message='reject unexpected committed bytes',
        managed_prefixes=('managed',),
        expected_base=unexpected_base,
    )
except ValueError as error:
    if 'unexpected repository bytes' not in str(error):
        raise
else:
    raise SystemExit('unexpected signed commit result was accepted')
if unexpected.head != unexpected_base or set(unexpected.branches) != {'main'}:
    raise SystemExit('unexpected staging mutation changed main or leaked staging')

broken = FakeApi({'managed/old.bin': b'old'})
broken_base = broken.head
broken.fail_graphql_at = 2
try:
    publish(
        broken,
        full_name='Helvesec/rmux-packages',
        branch='main',
        updates=large_updates,
        message='fail chunked bytes',
        managed_prefixes=('managed',),
        expected_base=broken_base,
    )
except ValueError as error:
    if 'injected GraphQL failure' not in str(error):
        raise
else:
    raise SystemExit('chunked mutation failure was accepted')
if broken.head != broken_base or set(broken.branches) != {'main'}:
    raise SystemExit('failed chunked update changed main or leaked staging')

unsigned = FakeApi({'managed/old.bin': b'old'}, verified=False)
try:
    publish(
        unsigned,
        full_name='Helvesec/rmux-packages',
        branch='main',
        updates={'managed/new.bin': b'new'},
        message='unsigned',
        managed_prefixes=('managed',),
        expected_base=unsigned.head,
    )
except ValueError as error:
    if 'platform-signed commit' not in str(error):
        raise
else:
    raise SystemExit('unsigned repository commit was accepted')
"#,
    );
}
