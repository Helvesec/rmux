"""D570 evidence only: historical declarations never widen current bindings.

The recorded epochs have no runtime activation. These tests do not replace the
original relocation/descendant canaries or claim that their failures are fixed.
"""
import copy
import functools
import hashlib
import json
import os
from pathlib import Path
import re
import subprocess
import tempfile
import unittest
import unittest.mock
from collections import Counter

from inventory import scan
from model import (borrow_objects, canonical, git, read_tree, source_syntax_parser,
                   validate_declarations)
from relocation_chain import load_chain, semantic_key, validate_descendant

ROOT = Path(__file__).resolve().parents[2]
POLICY = 'scripts/source-inventory/declarations.json'
REGISTRY = 'scripts/source-inventory/proof-epochs-d570.json'
CANDIDATE = 'f7f744fb51e3f6bbfce1de475011a9b0efe85e20'
BASE = 'b373c8b9e37d0ae38477fb4ff43f64b668f61274'
PINS = ('scripts/pin_revisions.py', 'tests/pin_revisions_spec.py',
        'tests/release_policy_reachability_spec.py', 'scripts/pin_proof_publication.py',
        'tests/pin_proof_publication_spec.py')


SIX_SOURCE_BASE = '877f47bef3e97e3835886b32463958449e0014e2'
RELEASE_POLICY_REFRESH = '2bbad4bd41c9d350223c7e2f8336678b7a02fd2c'
COMPOSED = '7dfbf2664f6221fe313872be7c9203e739d03a2c'
CENTRAL = '988eac4e8ba7633e13fab69d090cbfccca135a28'
D570 = 'd77fd81aa46ef1678e6dce7b004efde436ae7df3'
BENCH_REVISION = '9eef8631b7858e6abcc72a8651aa997561584279'
PINNED_REVISIONS = (CANDIDATE, BASE, SIX_SOURCE_BASE, RELEASE_POLICY_REFRESH,
                    COMPOSED, CENTRAL, D570, BENCH_REVISION)
# The lists below name each recorded commit by its exact subject, resolved to
# the one commit reachable from HEAD that changed the declarations (or, for
# blob refreshes, the source). A rebase or cherry-pick keeps the subject, so a
# lot records its own commit and a sibling lot integrated first never forces a
# repin; the content proofs below are unchanged.
# Declaration refreshes recorded after SIX_SOURCE_BASE, oldest first.
POST_SIX_SOURCE_REFRESHES = (
    ('test(release): keep publication fixtures in the temporary directory', {
        'scripts/release/test_publication_inert_alias.py'}),
    ('fix(surface): project the assembled v1 frame ledger sections', {
        'scripts/surface_sources.py', 'scripts/surface_syntax.py',
        'scripts/test_surface_manifest_rejections.py'}),
    ('refactor(options): split table by responsibility', {
        'scripts/surface_sources.py', 'scripts/test_surface_manifest_rejections.py'}),
    ('fix(source-inventory): refresh the pin proof publication spec blob', {
        'tests/pin_proof_publication_spec.py'}),
    ('chore(source-inventory): admit the rewritten benchmark document blobs', {
        'benches/perf/fixtures/README.md', 'docs/benchmarks.md'}),
    ('fix(source-inventory): refresh the pin proof publication spec blob for POSIX object paths', {
        'tests/pin_proof_publication_spec.py'}),
    ('fix(ci): bind the rehearsal permit path in step env, where runner is allowed', {
        '.github/workflows/premerge-rehearsal.yml'}),
    ('fix(attach): report a server exit like tmux after kill-server', {
        'crates/rmux-proto/REGISTRY.md'}),
    ('test(source-inventory): declare the attach server-exit test source', {
        'crates/rmux-proto/REGISTRY.md'}),
)
# Role-only classifications recorded after SIX_SOURCE_BASE, oldest first.
POST_SIX_SOURCE_ROLE_ADDITIONS = (
    ('fix(tooling): classify Windows PowerShell environment helper', 'tooling', 'scripts/windows_powershell.py'),
    ('fix(inventory): classify tool snapshot regression tests', 'tests',
     'benches/perf/qualification/test_tool_snapshot.py'),
    ('chore(inventory): declare replayed L1 test surface', 'tests',
     'benches/perf/qualification/test_path_process_guard.py'),
    ('fix(queue-inventory): derive test-only sources from the module tree', 'tooling',
     'scripts/queue-inventory/module_tree.py'),
    ('fix(source-inventory): classify the HARNESS043 bundle fixture as tooling', 'tooling',
     'scripts/fixtures/HARNESS043.bundle'),
    ('fix(source-inventory): classify the gate policy parity test as a test', 'tests',
     'scripts/test_candidate_gate_policy_parity.py'),
    ('fix(source-inventory): classify the argument template probe as tooling', 'tooling',
     'scripts/probe-tmux-argument-templates.py'),
    ('test(source-inventory): declare new sources as union-merged rows', 'tests',
     'scripts/source-inventory/population-additions.jsonl'),
    ('test(source-inventory): declare the MSRV workflow', 'tooling',
     '.github/workflows/msrv.yml'),
)
# Document and document-group refreshes recorded after SIX_SOURCE_BASE, oldest
# first: (commit, refreshed documents, group members given one appended blob).
# A refreshed document holds the blob its commit wrote, alone or appended to
# its earlier reviewed blobs.
POST_SIX_SOURCE_GROUP_REFRESHES = (
    ('test: replace private host, address and agent names with neutral values', {
        'tests/tmux_compat_surface_matrix/copy_viewport/fixtures/macos-bash-top.json'}, {
        'docs/validation/legacy-scenarios.json', 'docs/validation/reference-population.json',
        'scripts/validation/contracts.py'}),
    ('fix(validation): re-baseline the poison witness inventory on the current tree', set(), {
        'tests/reference_contracts_spec.py'}),
    ('fix(validation): read cited reference bytes from the frozen source base', {
        'scripts/validation/reference_applicability.py'}, {
        'docs/validation/reference-contracts.md', 'scripts/validation/contracts.py',
        'tests/reference_contracts_spec.py'}),
    ('test(validation): name a checkout without frozen history as an environment limit', set(), {
        'tests/reference_contracts_spec.py'}),
    ('fix(validation): bind the C-D94 ledger entry as an exact v4 delta', {
        'docs/validation/reference-contracts.json',
        'scripts/validation/reference_applicability.py'}, set()),
    ('fix(validation): pin the C-D94 v4 binding to its trunk ledger commit', {
        'docs/validation/reference-contracts.json',
        'scripts/validation/reference_applicability.py'}, set()),
    ('fix(validation): bind the C-D44 ledger edit as an exact v5 delta', {
        'docs/validation/reference-contracts.json',
        'scripts/validation/reference_applicability.py'}, set()),
    ('fix(release): publish crates after the dev-dependencies Cargo keeps', set(), {
        'docs/release/publication-runbook.md'}),
    ('fix(validation): bind the C-D95 ledger entry as an exact v6 delta', {
        'docs/validation/reference-contracts.json',
        'scripts/validation/reference_applicability.py'}, set()),
    ('fix(validation): bind the C-D96 ledger entry as an exact v7 delta', {
        'docs/validation/reference-contracts.json',
        'scripts/validation/reference_applicability.py'}, set()),
    ('fix(validation): bind the C-D97 ledger entry as an exact v8 delta', {
        'docs/validation/reference-contracts.json',
        'scripts/validation/reference_applicability.py'}, set()),
    ("server: last-pane -d/-e toggles the last pane's input without selecting it", set(), {
        'docs/validation/reference-contracts.md', 'scripts/validation/contracts.py',
        'tests/reference_contracts_spec.py'}),
)
# Source blob refreshes of SIX_SOURCE_ROWS, pinned to the exact source commit.
SIX_SOURCE_BLOB_REFRESHES = (
    ('test(tiny-smoke): keep the repository __pycache__ rule in the fixture', 'scripts/test_candidate_tiny_smoke.py'),
)
SIX_SOURCE_ROWS = {
    'scripts/test_candidate_tiny_smoke.py': ('tests',
        'Regression coverage for candidate identity and package layout containment',
        ['python3 -B -m unittest discover -s scripts -p test_candidate_tiny_smoke.py']),
    'scripts/test_web_no_listener_preflight.py': ('tests',
        'Regression coverage for no-listener preflight ordering and identity qualification',
        ['python3 -B -m unittest discover -s scripts -p test_web_no_listener_preflight.py']),
    'scripts/tiny_smoke_identity.py': ('tooling',
        'Qualify tiny smoke package layout and installed candidate identity',
        ['scripts/smoke-tiny-release-review.sh', 'scripts/test_candidate_tiny_smoke.py']),
    'scripts/web_no_listener_preflight.py': ('tooling',
        'Qualify the exact CLI graph before the Linux no-listener runtime probe',
        ['scripts/regression_registry.py', 'scripts/test_web_no_listener_preflight.py']),
    'tests/options_bank_mutations_spec.py': ('tests',
        'In-memory falsifications of common-bank receipt canaries; no native proof',
        ['python3 -B tests/options_bank_mutations_spec.py']),
    'tests/options_bank_receipts_spec.py': ('tests',
        'Synthetic common-bank receipt acceptance regressions; no native proof',
        ['python3 -B tests/options_bank_receipts_spec.py', 'tests/options_bank_mutations_spec.py']),
}


def ancestor(repo, revision):
    """Return revision only once Git proves it is a commit reachable from HEAD.

    A replayed lot leaves its original commit readable in the object store;
    proving a declaration change against it would prove nothing about HEAD.
    """
    if not re.fullmatch('[0-9a-f]{40}', revision) or subprocess.run(
            ['git', '-C', str(repo), 'merge-base', '--is-ancestor', revision, 'HEAD'],
            stdout=subprocess.DEVNULL, stderr=subprocess.DEVNULL).returncode != 0:
        raise ValueError(f'pinned revision is not a commit reachable from HEAD: {revision}')
    return revision


@functools.lru_cache(maxsize=None)
def path_history(repo, path):
    """(commit, subject) of every commit reachable from HEAD that changed path.

    Cached per repository: HEAD must not move while one repository is tested.
    """
    rows = git(repo, 'log', '--full-history', '--format=%H%x00%s', 'HEAD', '--', path).decode()
    return tuple(tuple(row.split('\0', 1)) for row in rows.splitlines())


def recorded(repo, subject, path=POLICY):
    """The single commit reachable from HEAD that changed path with this subject."""
    matches = [revision for revision, title in path_history(repo, path) if title == subject]
    if len(matches) != 1:
        raise ValueError(f'expected exactly one commit reachable from HEAD changing {path} '
                         f'with subject {subject!r}, found {len(matches)}')
    return matches[0]


@functools.lru_cache(maxsize=None)
def pinned_ancestor(repo, revision):
    """ancestor(repo, revision), proven once per process; HEAD must not move."""
    return ancestor(repo, revision)


# The only reads the projections make. Any other command or option may follow
# refs (--all, --branches), read the repository layout or the index, or write.
PINNED_OPTIONS = {'show': frozenset(), 'rev-parse': frozenset(),
                  'log': frozenset({'--format=%H', '--diff-filter=A'}),
                  'diff-tree': frozenset({'--no-commit-id', '--name-only', '--diff-filter=A', '-r'})}


@functools.lru_cache(maxsize=None)
def pinned_git(command, *args, repo=ROOT):
    """Read an immutable object of a fixed commit of HEAD's history once per process.

    Each mutated projection repeats the same historical reads; a commit named by
    its full SHA yields the same bytes every time, and each caller still parses
    its own copy. At least one full SHA is named before any '--', and only the
    commands and options of PINNED_OPTIONS are accepted: HEAD, branches, other
    refs, abbreviations, the index and the worktree are refused, and so is a
    full SHA outside HEAD's history. Recorded subjects are resolved in HEAD's
    history by recorded() before their commit is read here."""
    revisions = args[:args.index('--')] if '--' in args else args
    options = {arg for arg in revisions if arg.startswith('-')}
    named = [arg for arg in revisions if not arg.startswith('-')]
    if command not in PINNED_OPTIONS or not options <= PINNED_OPTIONS[command]:
        raise ValueError('pinned_git refuses unused commands and options: '
                         + ' '.join((command,) + args))
    if not named or not all(re.fullmatch(r'[0-9a-f]{40}\^?(?::.+)?', arg) for arg in named):
        raise ValueError('pinned_git reads fixed commits only: ' + ' '.join((command,) + args))
    for arg in named:
        pinned_ancestor(repo, arg[:40])
    return git(repo, command, *args)


def setUpModule():
    # Fail in seconds with the exact cause, before any slow historical proof:
    # a missing parser, a pin outside HEAD's history or an unknown record.
    source_syntax_parser()
    registry = json.loads((ROOT / REGISTRY).read_bytes())
    for revision in PINNED_REVISIONS + tuple(row['revision'] for row in registry['entries']):
        ancestor(ROOT, revision)
    for subject, *_ in (POST_SIX_SOURCE_REFRESHES + POST_SIX_SOURCE_ROLE_ADDITIONS
                        + POST_SIX_SOURCE_GROUP_REFRESHES):
        recorded(ROOT, subject)
    for subject, path in SIX_SOURCE_BLOB_REFRESHES:
        recorded(ROOT, subject, path)


class RecordedEpochTests(unittest.TestCase):
    @classmethod
    def setUpClass(cls):
        cls.original = git(ROOT, 'show', CANDIDATE + ':' + POLICY)
        cls.policy = json.loads(cls.original)
        cls.registry = json.loads((ROOT / REGISTRY).read_bytes())

    def historical_policy(self, row):
        # Explicit proof input for this exact revision, not a runtime selector.
        policy = copy.deepcopy(self.policy)
        for supplement in self.registry['entries']:
            if supplement['revision'] == row['revision']:
                for path, blobs in supplement['declaration_additions'].items():
                    policy['documents'][path]['source_blobs'].extend(blobs)
        return policy

    def test_original_refusal_then_declared_epoch_then_withdrawal(self):
        for row in self.registry['entries'][:2]:
            with self.subTest(epoch=row['epoch']):
                files = read_tree(ROOT, row['revision'])
                with self.assertRaisesRegex(ValueError, 'stale document declaration: scripts/pin_revisions.py'):
                    validate_declarations(self.policy, files)
                declared = self.historical_policy(row)
                validate_declarations(declared, files)
                for path in PINS:
                    withdrawn = copy.deepcopy(declared)
                    withdrawn['documents'][path]['source_blobs'] = self.policy['documents'][path]['source_blobs']
                    with self.assertRaisesRegex(ValueError, 'stale document declaration: ' + path.replace('.', '[.]')):
                        validate_declarations(withdrawn, files)
                for path in PINS:
                    for bad in ('0' * 40, files[PINS[(PINS.index(path) + 1) % len(PINS)]]['blob']):
                        substituted = dict(files, **{path: dict(files[path], blob=bad)})
                        with self.subTest(path=path, substitution=bad), self.assertRaisesRegex(ValueError, 'stale document declaration:'):
                            validate_declarations(declared, substituted)

    def test_current_policy_keeps_old_blobs_refused(self):
        current = json.loads((ROOT / POLICY).read_bytes())
        historical = self.project_release_policy_refresh(
            self.project_recorded_refreshes(current, POST_SIX_SOURCE_REFRESHES))
        for row in self.registry['entries']:
            for path in row['declaration_additions']:
                self.assertEqual(historical['documents'][path], self.policy['documents'][path])
                for blob in row['declaration_additions'][path]:
                    with self.subTest(path=path, epoch=row['epoch']), self.assertRaisesRegex(ValueError, 'stale document declaration:'):
                        validate_declarations(current, {path: {'blob': blob}})
        for path in ('tests/release_policy_reachability_spec.py', 'tests/pin_proof_publication_spec.py'):
            for blob in self.policy['documents'][path]['source_blobs']:
                with self.subTest(replaced_blob=blob), self.assertRaisesRegex(ValueError, 'stale document declaration:'):
                    validate_declarations(current, {path: {'blob': blob}})

    def project_release_policy_refresh(self, current):
        # Admit only the exact source update, never an exception derived from HEAD
        # or current policy. All other metadata and historical evidence stay fixed.
        path = 'tests/release_policy_reachability_spec.py'
        revision = RELEASE_POLICY_REFRESH
        previous = self.policy['documents'][path]
        self.assertEqual(previous['source_blobs'],
                         [pinned_git('rev-parse', revision + '^:' + path).decode().strip()])
        refreshed = dict(previous,
                         source_blobs=[pinned_git('rev-parse', revision + ':' + path).decode().strip()],
                         freshness='Exact Git blob; maintained source introduced/last changed at '
                                   + revision + '; semantic responsibility remains manual')
        self.assertEqual(current['documents'].get(path), refreshed, path)
        projected = copy.deepcopy(current)
        projected['documents'][path] = copy.deepcopy(previous)
        return projected

    def test_existing_declarations_are_preserved_without_replacement(self):
        current = json.loads((ROOT / POLICY).read_bytes())
        self.assert_preserved_declarations(current)

    def project_six_source_classifications(self, current):
        # Authenticate each addition against fixed source commits and metadata
        # before removing it from the historical proof view. Later recorded
        # refreshes are restored first, since the baseline predates them.
        projected = self.project_recorded_group_refreshes(current, POST_SIX_SOURCE_GROUP_REFRESHES)
        projected = self.project_recorded_role_additions(projected, POST_SIX_SOURCE_ROLE_ADDITIONS)
        projected = self.project_recorded_refreshes(projected, POST_SIX_SOURCE_REFRESHES)
        projected = self.project_six_source_blob_refreshes(projected)
        baseline = json.loads(pinned_git('show', SIX_SOURCE_BASE + ':' + POLICY))
        for path, (role, description, consumers) in SIX_SOURCE_ROWS.items():
            self.assertTrue(all(path not in paths for paths in baseline['files'].values()))
            self.assertNotIn(path, baseline['documents'])
            expected = dict(
                basis='maintained declaration', consumers=consumers,
                freshness='Exact Git blob; git hash-object ' + path + '; semantic responsibility remains manual',
                owner='RMUX regression maintainers', role=description, source=[path],
                source_blobs=[pinned_git('rev-parse', SIX_SOURCE_BASE + ':' + path).decode().strip()])
            self.assertEqual(projected['documents'].get(path), expected, path)
            self.assertEqual(projected['files'][role].count(path), 1, path)
            projected['files'][role].remove(path)
            del projected['documents'][path]
        # Catch extra roles, paths, reordered lists or unrelated changes too.
        self.assertEqual(projected, baseline)
        return projected

    def project_six_source_blob_refreshes(self, current):
        # Admit only the blob written by each pinned source commit, restored to
        # its parent blob; every other field is still checked against the baseline.
        projected = copy.deepcopy(current)
        for subject, path in reversed(SIX_SOURCE_BLOB_REFRESHES):
            revision = recorded(ROOT, subject, path)
            row = projected['documents'].get(path)
            self.assertIsNotNone(row, path)
            self.assertEqual(row.get('source_blobs'),
                             [pinned_git('rev-parse', revision + ':' + path).decode().strip()], path)
            row['source_blobs'] = [pinned_git('rev-parse', revision + '^:' + path).decode().strip()]
        return projected

    def test_six_source_projection_refuses_unrecorded_changes(self):
        current = json.loads((ROOT / POLICY).read_bytes())
        self.project_six_source_classifications(current)
        for path, (role, _, _) in SIX_SOURCE_ROWS.items():
            for mutation in ('withdraw', 'duplicate', 'reclassify', 'extra_role',
                             'unknown_test', 'document', 'blob', 'append_blob',
                             'duplicate_blob', 'missing_blob', 'source', 'owner',
                             'consumers', 'freshness', 'role', 'basis'):
                changed = copy.deepcopy(current)
                row = changed['documents'][path]
                if mutation == 'withdraw':
                    changed['files'][role].remove(path)
                elif mutation == 'duplicate':
                    changed['files'][role].append(path)
                elif mutation in ('reclassify', 'extra_role'):
                    if mutation == 'reclassify':
                        changed['files'][role].remove(path)
                    changed['files']['generated'].append(path)
                elif mutation == 'unknown_test':
                    changed['files']['tests'].append(path + '.undeclared')
                elif mutation == 'document':
                    del changed['documents'][path]
                elif mutation == 'blob':
                    row['source_blobs'] = ['0' * 40]
                elif mutation == 'append_blob':
                    row['source_blobs'].append('0' * 40)
                elif mutation == 'duplicate_blob':
                    row['source_blobs'].append(row['source_blobs'][0])
                elif mutation == 'missing_blob':
                    del row['source_blobs']
                elif mutation in ('source', 'consumers'):
                    row[mutation] = [path + '.undeclared']
                else:
                    row[mutation] = 'unrecorded change'
                with self.subTest(path=path, mutation=mutation), self.assertRaises(AssertionError):
                    self.project_six_source_classifications(changed)
        self.assertEqual(current, json.loads((ROOT / POLICY).read_bytes()))

    def assert_preserved_declarations(self, current):
        # Pin the authorized additions, never derive an allowlist from current.
        admitted = json.loads(pinned_git('show', COMPOSED + ':' + POLICY))
        d570_policy = json.loads(pinned_git('show', D570 + ':' + POLICY))
        d570_paths = {'tooling': REGISTRY, 'tests': 'scripts/source-inventory/test_d570_epochs.py'}
        additions = [(role, path) for role, paths in admitted['files'].items()
                     for path in paths if path not in self.policy['files'][role]]
        documents = {p: row for p, row in admitted['documents'].items()
                     if p not in self.policy['documents']}
        self.assertEqual(Counter(role for role, _ in additions), {'tooling': 7, 'tests': 8})
        self.assertEqual(len(documents), 13)
        self.assertEqual({p for _, p in additions}, set(documents) | set(d570_paths.values()))

        projected = self.project_release_policy_refresh(
            self.project_six_source_classifications(current))
        # The reviewed bench classification is a role addition, not a document
        # refresh. Prove its exact delta before restoring the historical view.
        bench_path = 'scripts/test_bench_collect.py'
        before = json.loads(pinned_git('show', BENCH_REVISION + '^:' + POLICY))
        after = json.loads(pinned_git('show', BENCH_REVISION + ':' + POLICY))
        self.assertTrue(all(bench_path not in paths for paths in before['files'].values()))
        self.assertEqual(after['files']['tests'].count(bench_path), 1)
        self.assertEqual(projected['files']['tests'], after['files']['tests'])
        after['files']['tests'].remove(bench_path)
        self.assertEqual(after, before)
        projected['files']['tests'].remove(bench_path)
        # These immutable refreshes changed existing declarations after D576.
        # Authenticate the complete new row before restoring its old proof view;
        # deriving exceptions from current would hide unauthorized mutations.
        refreshes = (
            ('fix(inventory): admit exact S1290 reference source blobs', {
                'docs/validation/reference-contracts.json',
                'scripts/validation/reference_applicability.py'}),
            ('fix(source-inventory): bind source input documentation blobs', {
                'crates/rmux-proto/README.md', 'docs/release/0.11.0.md'}),
            ('fix(source-inventory): refresh relocated option source bindings', {
                'scripts/regression_registry_options.py',
                'scripts/test_regression_registry_options.py',
                'scripts/surface_sources.py', 'scripts/test_surface_manifest_rejections.py'}),
            ('fix: keep each platform behavior behind its single cfg selection', {
                'docs/platform-neutrality-budget.md'}),
        )
        projected = self.project_recorded_refreshes(projected, refreshes)
        for role, path in additions:
            self.assertEqual(projected['files'].get(role, []).count(path), 1, path)
            projected['files'][role].remove(path)
        for path, row in documents.items():
            self.assertEqual(projected['documents'].get(path), row, path)
            del projected['documents'][path]
        # Strict equality retains every historical role, binding and metadata field.
        self.assertEqual(projected, self.policy)

        for role, path in additions:
            if path in d570_paths.values():
                self.assertEqual(d570_paths[role], path)
                self.assertEqual(d570_policy['files'][role].count(path), 1)
                origin = D570
            else:
                origins = pinned_git('log', '--format=%H', '--diff-filter=A', CENTRAL, '--', path).decode().splitlines()
                self.assertEqual(len(origins), 1, path)
                origin = origins[0]
                # The admitted row, which the projection above proved current
                # holds apart from recorded refreshes, keeps the CENTRAL blob.
                self.assertEqual(documents[path]['source'], [path])
                self.assertEqual(documents[path]['source_blobs'],
                                 [pinned_git('rev-parse', CENTRAL + ':' + path).decode().strip()])
            self.assertEqual(pinned_git('diff-tree', '--no-commit-id', '--name-only',
                                 '--diff-filter=A', '-r', origin, '--', path).decode().strip(), path)

    def project_recorded_refreshes(self, current, refreshes):
        # Authenticate the complete new row of each pinned refresh before
        # restoring its old row; no exception is derived from current.
        projected = copy.deepcopy(current)
        for subject, paths in reversed(refreshes):
            revision = recorded(ROOT, subject)
            before = json.loads(pinned_git('show', revision + '^:' + POLICY))
            after = json.loads(pinned_git('show', revision + ':' + POLICY))
            self.assertEqual(set(before['documents']), set(after['documents']))
            self.assertEqual({path for path in before['documents']
                              if before['documents'][path] != after['documents'][path]}, paths)
            for path in paths:
                self.assertEqual(projected['documents'].get(path), after['documents'][path], path)
                projected['documents'][path] = before['documents'][path]
                after['documents'][path] = before['documents'][path]
            self.assertEqual(after, before)  # No role or other policy change is admitted.
        return projected

    def project_recorded_group_refreshes(self, current, refreshes):
        # Authenticate each pinned commit: its documents changed only as listed,
        # and each listed group member gained exactly the blob that commit wrote.
        projected = copy.deepcopy(current)
        for subject, documents, members in reversed(refreshes):
            revision = recorded(ROOT, subject)
            before = json.loads(pinned_git('show', revision + '^:' + POLICY))
            after = json.loads(pinned_git('show', revision + ':' + POLICY))
            self.assertEqual(set(before['documents']), set(after['documents']))
            self.assertEqual({path for path in before['documents']
                              if before['documents'][path] != after['documents'][path]}, documents)
            for path in documents:
                blob = pinned_git('rev-parse', revision + ':' + path).decode().strip()
                self.assertIn(after['documents'][path]['source_blobs'],
                              ([blob], before['documents'][path]['source_blobs'] + [blob]), path)
                self.assertEqual(projected['documents'].get(path), after['documents'][path], path)
                projected['documents'][path] = before['documents'][path]
                after['documents'][path] = before['documents'][path]
            groups = [(old['members'], new['members'], ours['members']) for old, new, ours
                      in zip(before['document_groups'], after['document_groups'],
                             projected['document_groups'])]
            self.assertEqual(len(groups), len(before['document_groups']))
            self.assertEqual(len(groups), len(projected['document_groups']))
            changed = set()
            for old, new, ours in groups:
                for path in set(old) | set(new):
                    if old.get(path) == new.get(path):
                        continue
                    changed.add(path)
                    blob = pinned_git('rev-parse', revision + ':' + path).decode().strip()
                    self.assertEqual(new.get(path), old.get(path, []) + [blob], path)
                    self.assertEqual(ours.get(path), new[path], path)
                    ours[path] = new[path] = old[path]
            self.assertEqual(changed, members)
            self.assertEqual(after, before)  # No role or other policy change is admitted.
        return projected

    def test_recorded_group_refresh_rejects_unrecorded_changes(self):
        current = json.loads((ROOT / POLICY).read_bytes())
        self.assert_preserved_declarations(current)
        for _, documents, members in POST_SIX_SOURCE_GROUP_REFRESHES:
            for path in sorted(members):
                for mutation in ('append', 'withdraw', 'replace', 'duplicate'):
                    changed = copy.deepcopy(current)
                    blobs = next(group['members'][path] for group in changed['document_groups']
                                 if path in group['members'])
                    if mutation == 'append':
                        blobs.append('0' * 40)
                    elif mutation == 'withdraw':
                        blobs.pop()
                    elif mutation == 'replace':
                        blobs[-1] = '0' * 40
                    else:
                        blobs.append(blobs[-1])
                    with self.subTest(path=path, mutation=mutation), self.assertRaises(AssertionError):
                        self.assert_preserved_declarations(changed)
            for path in sorted(documents):
                for mutation in ('stale', 'append', 'withdraw_last', 'withdraw_earlier', 'metadata'):
                    changed = copy.deepcopy(current)
                    row = changed['documents'][path]
                    if mutation == 'withdraw_earlier' and len(row['source_blobs']) < 2:
                        continue  # A row with only its commit's blob keeps nothing earlier.
                    if mutation == 'stale':
                        row['source_blobs'] = self.policy['documents'][path]['source_blobs']
                    elif mutation == 'append':
                        row['source_blobs'].append('0' * 40)
                    elif mutation == 'withdraw_last':
                        row['source_blobs'].pop()
                    elif mutation == 'withdraw_earlier':
                        del row['source_blobs'][:-1]
                    else:
                        row['freshness'] = 'unrecorded freshness'
                    with self.subTest(path=path, mutation=mutation), self.assertRaises(AssertionError):
                        self.assert_preserved_declarations(changed)
        self.assertEqual(current, json.loads((ROOT / POLICY).read_bytes()))

    def project_recorded_role_additions(self, current, additions):
        # Authenticate that each pinned commit added exactly one new path to one
        # role and changed nothing else, then withdraw it from the proof view.
        projected = copy.deepcopy(current)
        for subject, role, path in reversed(additions):
            revision = recorded(ROOT, subject)
            before = json.loads(pinned_git('show', revision + '^:' + POLICY))
            after = json.loads(pinned_git('show', revision + ':' + POLICY))
            self.assertTrue(all(path not in paths for paths in before['files'].values()), path)
            for policy in (after, projected):
                self.assertEqual([r for r, paths in policy['files'].items() if path in paths], [role], path)
                self.assertEqual(policy['files'][role].count(path), 1, path)
                policy['files'][role].remove(path)
            self.assertEqual(after, before)  # No document or other policy change is admitted.
        return projected

    def test_post_six_source_role_additions_reject_unrecorded_changes(self):
        current = json.loads((ROOT / POLICY).read_bytes())
        self.assert_preserved_declarations(current)
        for _, role, path in POST_SIX_SOURCE_ROLE_ADDITIONS:
            for mutation in ('withdraw', 'duplicate', 'reclassify', 'extra_role', 'document'):
                changed = copy.deepcopy(current)
                if mutation == 'withdraw':
                    changed['files'][role].remove(path)
                elif mutation == 'duplicate':
                    changed['files'][role].append(path)
                elif mutation in ('reclassify', 'extra_role'):
                    if mutation == 'reclassify':
                        changed['files'][role].remove(path)
                    changed['files']['generated'].append(path)
                else:
                    changed['documents'][path] = copy.deepcopy(current['documents'][PINS[0]])
                with self.subTest(path=path, mutation=mutation), self.assertRaises(AssertionError):
                    self.assert_preserved_declarations(changed)
        self.assertEqual(current, json.loads((ROOT / POLICY).read_bytes()))

    def test_mutated_projections_reuse_pinned_reads(self):
        # Every mutated copy used to repeat ~95 Git subprocesses for identical bytes.
        current = json.loads((ROOT / POLICY).read_bytes())
        self.assert_preserved_declarations(current)
        with unittest.mock.patch(__name__ + '.git', wraps=git) as spawned:
            self.assert_preserved_declarations(copy.deepcopy(current))
            self.project_six_source_classifications(copy.deepcopy(current))
        self.assertEqual(spawned.call_count, 0)
        for name in ('HEAD:' + POLICY, 'HEAD', CANDIDATE[:12] + ':' + POLICY, CANDIDATE + '~1'):
            with self.subTest(name=name), self.assertRaisesRegex(ValueError, 'fixed commits only'):
                pinned_git('show', name)
        # Without any named commit, Git reads from HEAD.
        with self.assertRaisesRegex(ValueError, 'fixed commits only'):
            pinned_git('log', '--format=%H', '--diff-filter=A', '--', POLICY)
        # Ref-dependent or repository-layout reads, then a read outside the four used.
        for args in (('log', '--format=%H', '--all', CANDIDATE),
                     ('log', '-1', '--format=%H', '--all'),
                     ('rev-parse', '--symbolic-full-name', '--branches'),
                     ('rev-parse', '--git-common-dir'),
                     ('ls-tree', CANDIDATE)):
            with self.subTest(args=args), self.assertRaisesRegex(ValueError, 'unused commands and options'):
                pinned_git(*args)

    def test_historical_declaration_replacement_is_rejected(self):
        current = json.loads((ROOT / POLICY).read_bytes())
        self.assert_preserved_declarations(current)
        current['documents'][PINS[0]]['source_blobs'] = ['0' * 40]
        with self.assertRaises(AssertionError):
            self.assert_preserved_declarations(current)

    def test_historical_declaration_reclassification_is_rejected(self):
        current = json.loads((ROOT / POLICY).read_bytes())
        self.assert_preserved_declarations(current)
        current['files']['tooling'].remove(PINS[0])
        current['files']['tests'].append(PINS[0])
        with self.assertRaises(AssertionError):
            self.assert_preserved_declarations(current)

    def test_historical_declaration_withdrawal_is_rejected(self):
        current = json.loads((ROOT / POLICY).read_bytes())
        self.assert_preserved_declarations(current)
        for section in ('files', 'documents'):
            withdrawn = copy.deepcopy(current)
            if section == 'files':
                withdrawn['files']['tooling'].remove(PINS[0])
            else:
                del withdrawn['documents'][PINS[0]]
            with self.subTest(section=section), self.assertRaises(AssertionError):
                self.assert_preserved_declarations(withdrawn)

    def test_declared_refresh_projection_rejects_unrecorded_changes(self):
        current = json.loads((ROOT / POLICY).read_bytes())
        self.assert_preserved_declarations(current)
        paths = (
            'docs/validation/reference-contracts.json', 'scripts/validation/reference_applicability.py',
            'crates/rmux-proto/README.md', 'docs/release/0.11.0.md',
            'scripts/regression_registry_options.py',
            'scripts/test_regression_registry_options.py',
            'scripts/surface_sources.py', 'scripts/test_surface_manifest_rejections.py',
            'scripts/surface_syntax.py',
            'tests/release_policy_reachability_spec.py',
            'scripts/release/test_publication_inert_alias.py',
            'tests/pin_proof_publication_spec.py',
            'crates/rmux-proto/REGISTRY.md',
        )
        for path in paths:
            for mutation in ('replace', 'append', 'withdraw_blob', 'duplicate',
                             'metadata', 'source', 'withdraw_document', 'reclassify', 'stale'):
                changed = copy.deepcopy(current)
                row = changed['documents'][path]
                if mutation == 'replace':
                    row['source_blobs'] = ['0' * 40]
                elif mutation == 'append':
                    row['source_blobs'].append('0' * 40)
                elif mutation == 'withdraw_blob':
                    row['source_blobs'].pop(0)
                elif mutation == 'duplicate':
                    row['source_blobs'].append(row['source_blobs'][-1])
                elif mutation == 'metadata':
                    row['freshness'] = 'unrecorded freshness'
                elif mutation == 'source':
                    row['source'] = [PINS[0]]
                elif mutation == 'withdraw_document':
                    del changed['documents'][path]
                elif mutation == 'stale':
                    row['source_blobs'] = self.policy['documents'][path]['source_blobs']
                else:
                    role = next(role for role, files in changed['files'].items() if path in files)
                    changed['files'][role].remove(path)
                    changed['files']['generated'].append(path)
                with self.subTest(path=path, mutation=mutation), self.assertRaises(AssertionError):
                    self.assert_preserved_declarations(changed)
        self.assertEqual(current, json.loads((ROOT / POLICY).read_bytes()))

    def test_bench_role_projection_rejects_unrecorded_changes(self):
        current = json.loads((ROOT / POLICY).read_bytes())
        self.assert_preserved_declarations(current)
        path = 'scripts/test_bench_collect.py'
        for mutation in ('withdraw', 'duplicate', 'reclassify', 'extra_role',
                         'unknown_test', 'substitute', 'document', 'reorder'):
            changed = copy.deepcopy(current)
            tests = changed['files']['tests']
            if mutation == 'withdraw':
                tests.remove(path)
            elif mutation == 'duplicate':
                tests.append(path)
            elif mutation == 'reclassify':
                tests.remove(path)
                changed['files']['tooling'].append(path)
            elif mutation == 'extra_role':
                changed['files']['tooling'].append(path)
            elif mutation == 'unknown_test':
                tests.append('scripts/test_bench_collect_undeclared.py')
            elif mutation == 'substitute':
                tests[tests.index(path)] = 'scripts/test_bench_collect_undeclared.py'
            elif mutation == 'document':
                changed['documents'][path] = copy.deepcopy(current['documents'][PINS[0]])
            else:
                tests.remove(path)
                tests.append(path)
            with self.subTest(mutation=mutation), self.assertRaises(AssertionError):
                self.assert_preserved_declarations(changed)
        self.assertEqual(current, json.loads((ROOT / POLICY).read_bytes()))

    def test_recorded_epochs_bind_exact_historical_blobs_and_prefix(self):
        self.assertIsNone(self.registry['runtime_consumer'])
        previous = None
        for row in self.registry['entries']:
            data = dict(row)
            expected = data.pop('sha256')
            self.assertEqual(hashlib.sha256(canonical(data).encode()).hexdigest(), expected)
            self.assertEqual(row['previous_sha256'], previous)
            self.assertEqual(row['declaration_base_sha256'], hashlib.sha256(self.original).hexdigest())
            previous = expected
            for path, blobs in row['declaration_additions'].items():
                self.assertIn(path, PINS)
                self.assertEqual(blobs, [git(ROOT, 'rev-parse', row['revision'] + ':' + path).decode().strip()])
        path = 'crates/rmux-pty/src/process_shell.rs'
        self.assertEqual(git(ROOT, 'show', BASE + ':' + path), git(ROOT, 'show', CANDIDATE + ':' + path))

    def test_unactivated_append_is_rejected_by_unchanged_runtime(self):
        audit = ROOT / '.rmux-audit/s1205/chain-canary'
        audit.mkdir(parents=True, exist_ok=True)
        old = json.loads((ROOT / 'scripts/source-inventory/relocation-revisions.json').read_bytes())
        with tempfile.TemporaryDirectory(dir=audit) as directory:
            path = Path(directory) / 'relocation-revisions.json'
            path.write_text(json.dumps(old + [self.registry['entries'][-1]]))
            with self.assertRaisesRegex(ValueError, 'altered or unsupported relocation chain'):
                load_chain(ROOT, path)

    def test_recorded_semantics_match_real_scan_without_activating_guard(self):
        report = scan(ROOT, CANDIDATE, source_syntax_parser(), self.policy, self.original)
        self.assertEqual(report['errors'], [])
        record = load_chain(ROOT)
        admitted = Counter(semantic_key(p, a, r['consumers'])
                           for p, r in record['after'].items() for a in r['attributes']
                           if not (a['kind'] == 'path' and a['classification'] == 'tests'))
        actual = Counter(semantic_key(r['path'], a, r['consumers'])
                         for r in report['files'] if r['path'].endswith('.rs')
                         for a in r['semantic_attributes']
                         if not (a['kind'] == 'path' and a['classification'] == 'tests'))
        row = next(row for row in self.registry['entries'] if row['revision'] == CANDIDATE)
        for name, delta in (('semantic_additions', actual - admitted), ('semantic_removals', admitted - actual)):
            self.assertEqual(Counter({canonical(item['key']): item['count'] for item in row[name]}), delta)
        with self.assertRaisesRegex(ValueError, '^descendant semantic attribute/cfg/consumer identity differs$'):
            validate_descendant(report, read_tree(ROOT, CANDIDATE), record)

class ExplicitEpochLoaderTests(unittest.TestCase):
    """D572 only selects proof inputs; original semantics remain authoritative."""
    epochs = ('D570-413e4026', 'D570-330141c0', 'D570-f7f744fb',
              'D570-413e4026-declaration-supplement',
              'D570-330141c0-declaration-supplement')

    def test_explicit_epoch_list_preserves_original_record_and_covers_all_entries(self):
        original = load_chain(ROOT)
        selected = load_chain(ROOT, epochs=self.epochs)
        entries = selected.pop('proof_epochs')
        self.assertEqual(selected, original)
        self.assertEqual(tuple(row['epoch'] for row in entries), self.epochs)
        self.assertEqual(entries, json.loads((ROOT / REGISTRY).read_bytes())['entries'])
        for epoch in self.epochs:
            with self.subTest(epoch=epoch):
                self.assertEqual([row['epoch'] for row in load_chain(ROOT, epochs=(epoch,))['proof_epochs']], [epoch])

    def test_original_refusal_explicit_loading_withdrawal_and_restoration(self):
        from relocation_chain import epoch_declarations
        original = json.loads(git(ROOT, 'show', CANDIDATE + ':' + POLICY))
        registry = json.loads((ROOT / REGISTRY).read_bytes())['entries']
        for row in registry[:2]:
            revision, epoch = row['revision'], row['epoch']
            files = read_tree(ROOT, revision)
            with self.subTest(epoch=epoch):
                with self.assertRaisesRegex(ValueError, 'stale document declaration'):
                    validate_declarations(original, files)
                names = (epoch, epoch + '-declaration-supplement')
                raw = epoch_declarations(ROOT, revision, epochs=names)
                declared = json.loads(raw)
                validate_declarations(declared, files)
                for withdrawn in ((epoch,), (epoch + '-declaration-supplement',)):
                    with self.subTest(withdrawn=withdrawn), self.assertRaisesRegex(ValueError, 'stale document declaration'):
                        validate_declarations(json.loads(epoch_declarations(ROOT, revision, epochs=withdrawn)), files)
                validate_declarations(json.loads(epoch_declarations(ROOT, revision, epochs=names)), files)
                for path in PINS:
                    for blob in ('0' * 40, files[PINS[(PINS.index(path) + 1) % len(PINS)]]['blob']):
                        with self.subTest(path=path, blob=blob), self.assertRaisesRegex(ValueError, 'stale document declaration'):
                            validate_declarations(declared, dict(files, **{path: dict(files[path], blob=blob)}))
        self.assertEqual(epoch_declarations(ROOT, CANDIDATE, epochs=('D570-f7f744fb',)),
                         git(ROOT, 'show', CANDIDATE + ':' + POLICY))

    def test_unknown_duplicate_empty_mixed_and_wrong_revision_selection_refuse(self):
        from relocation_chain import epoch_declarations
        for names in (('unknown',), (self.epochs[0], self.epochs[0])):
            with self.subTest(epochs=names), self.assertRaisesRegex(ValueError, 'epoch'):
                load_chain(ROOT, epochs=names)
        for revision, names in ((CANDIDATE, ()), (CANDIDATE, self.epochs[:2]),
                                (CANDIDATE, (self.epochs[0],)), ('HEAD', (self.epochs[2],))):
            with self.subTest(revision=revision, epochs=names), self.assertRaisesRegex(ValueError, 'epoch|revision'):
                epoch_declarations(ROOT, revision, epochs=names)

    def test_registry_withdrawal_substitution_reorder_append_and_reseal_refuse(self):
        import shutil
        from unittest.mock import patch
        import relocation_chain
        audit = ROOT / '.rmux-audit/s1208/registry-canaries'
        audit.mkdir(parents=True, exist_ok=True)
        original = json.loads((ROOT / REGISTRY).read_bytes())
        with tempfile.TemporaryDirectory(dir=audit) as directory:
            folder = Path(directory)
            shutil.copyfile(ROOT / 'scripts/source-inventory/relocation-revisions.json', folder / 'relocation-revisions.json')
            shutil.copytree(ROOT / 'scripts/source-inventory/relocation-proofs', folder / 'relocation-proofs')
            target = folder / Path(REGISTRY).name
            for change in ('missing', 'withdraw', 'substitute', 'reorder', 'append', 'reseal'):
                value = copy.deepcopy(original)
                if change == 'withdraw':
                    value['entries'].pop()
                elif change in ('substitute', 'reseal'):
                    value['entries'][0]['revision'] = CANDIDATE
                    if change == 'reseal':
                        row = value['entries'][0]
                        row['sha256'] = hashlib.sha256(canonical({k:v for k,v in row.items() if k != 'sha256'}).encode()).hexdigest()
                elif change == 'reorder':
                    value['entries'].reverse()
                elif change == 'append':
                    value['entries'].append(copy.deepcopy(value['entries'][-1]))
                if change != 'missing':
                    target.write_text(json.dumps(value))
                with self.subTest(change=change), patch.object(relocation_chain, 'TOOL', folder):
                    with self.assertRaisesRegex(ValueError, 'epoch'):
                        load_chain(ROOT, epochs=self.epochs)
                    self.assertEqual(load_chain(ROOT), load_chain(ROOT, ROOT / 'scripts/source-inventory/relocation-revisions.json'))
            target.write_bytes((ROOT / REGISTRY).read_bytes())
            with patch.object(relocation_chain, 'TOOL', folder):
                self.assertEqual(len(load_chain(ROOT, epochs=self.epochs)['proof_epochs']), 5)

    def test_registry_deletion_then_restoration_in_history_refuses(self):
        import subprocess
        audit = ROOT / '.rmux-audit/s1208/history-canary'
        audit.mkdir(parents=True, exist_ok=True)
        with tempfile.TemporaryDirectory(dir=audit) as directory:
            fixture = Path(directory)
            subprocess.run(['git', 'init', '-q', str(fixture)], check=True)
            common = git(ROOT, 'rev-parse', '--git-common-dir').decode().strip()
            borrow_objects(fixture / '.git', (ROOT / common / 'objects').resolve())
            parent = git(ROOT, 'rev-parse', 'HEAD').decode().strip()
            env = dict(os.environ, GIT_AUTHOR_NAME='Sidney Sissaoui', GIT_AUTHOR_EMAIL='shideneyu@gmail.com',
                       GIT_COMMITTER_NAME='Sidney Sissaoui', GIT_COMMITTER_EMAIL='shideneyu@gmail.com')
            for removed in (True, False):
                git(fixture, 'read-tree', git(ROOT, 'rev-parse', 'HEAD').decode().strip())
                if removed:
                    git(fixture, 'update-index', '--force-remove', REGISTRY)
                tree = git(fixture, 'write-tree').decode().strip()
                parent = subprocess.check_output(['git', '-C', str(fixture), 'commit-tree', tree,
                    '-p', parent, '-m', 'epoch history canary'], env=env).decode().strip()
            git(fixture, 'update-ref', 'HEAD', parent)
            with self.assertRaisesRegex(ValueError, 'epoch.*history'):
                load_chain(fixture, epochs=self.epochs)


class PinLocationTests(unittest.TestCase):
    """Pins resolve only inside HEAD's history and survive a replayed lot."""
    ENV = dict(os.environ, GIT_AUTHOR_NAME='Sidney Sissaoui', GIT_AUTHOR_EMAIL='shideneyu@gmail.com',
               GIT_COMMITTER_NAME='Sidney Sissaoui', GIT_COMMITTER_EMAIL='shideneyu@gmail.com')

    def setUp(self):
        audit = ROOT / '.rmux-audit/s2077/pin-location'
        audit.mkdir(parents=True, exist_ok=True)
        self.temp = tempfile.TemporaryDirectory(dir=audit)
        self.addCleanup(self.temp.cleanup)

    def fixture(self, name, head):
        """A repository reading ROOT's objects whose HEAD is the given commit."""
        repo = Path(self.temp.name) / name
        subprocess.run(['git', 'init', '-q', str(repo)], check=True)
        common = git(ROOT, 'rev-parse', '--git-common-dir').decode().strip()
        borrow_objects(repo / '.git', (ROOT / common / 'objects').resolve())
        git(repo, 'update-ref', 'HEAD', head)
        return repo

    def commit(self, repo, tree, parent, message):
        return subprocess.check_output(['git', '-C', str(repo), 'commit-tree', tree, '-p', parent,
                                        '-m', message], env=self.ENV).decode().strip()

    def test_non_ancestor_revision_is_refused(self):
        head = git(ROOT, 'rev-parse', 'HEAD').decode().strip()
        repo = self.fixture('sibling', head)
        # The pre-replay commit of a lot: readable, but beside HEAD's history.
        sibling = self.commit(repo, head + '^{tree}', head + '^', 'replayed lot original')
        self.assertEqual(git(repo, 'cat-file', '-t', sibling).decode().strip(), 'commit')
        parent = git(repo, 'rev-parse', 'HEAD^').decode().strip()
        for revision in (head, parent):
            self.assertEqual(ancestor(repo, revision), revision)
        tree = git(repo, 'rev-parse', 'HEAD^{tree}').decode().strip()
        for revision in (sibling, '0' * 40, tree, 'HEAD', head[:12], head.upper()):
            with self.subTest(revision=revision), self.assertRaisesRegex(
                    ValueError, '^pinned revision is not a commit reachable from HEAD: '):
                ancestor(repo, revision)
        # pinned_git reads the parent and refuses the readable sibling, never caching it.
        self.assertEqual(pinned_git('show', parent + ':' + POLICY, repo=repo), git(ROOT, 'show', parent + ':' + POLICY))
        with self.assertRaisesRegex(ValueError, '^pinned revision is not a commit reachable from HEAD: '):
            pinned_git('show', sibling + ':' + POLICY, repo=repo)

    def test_replayed_record_is_found_without_repin(self):
        subject, _, _ = POST_SIX_SOURCE_GROUP_REFRESHES[-1]
        original = recorded(ROOT, subject)
        # Replay the recorded commit on its own parent with a new date: a new
        # SHA, as when a sibling lot is integrated first and this one is rebased.
        repo = self.fixture('replayed', original)
        env = dict(self.ENV, GIT_COMMITTER_DATE='2001-01-01T00:00:00Z')
        message = git(ROOT, 'log', '-1', '--format=%B', original).decode()
        replayed = subprocess.check_output(['git', '-C', str(repo), 'commit-tree', original + '^{tree}',
                                            '-p', original + '^', '-m', message], env=env).decode().strip()
        self.assertNotEqual(replayed, original)
        git(repo, 'update-ref', 'HEAD', replayed)
        self.assertEqual(recorded(repo, subject), replayed)
        with self.assertRaisesRegex(ValueError, 'not a commit reachable from HEAD'):
            ancestor(repo, original)

    def test_unknown_or_ambiguous_subject_is_refused(self):
        subject, _, _ = POST_SIX_SOURCE_GROUP_REFRESHES[-1]
        head = git(ROOT, 'rev-parse', 'HEAD').decode().strip()
        with self.assertRaisesRegex(ValueError, 'expected exactly one commit .* found 0'):
            recorded(ROOT, subject + ' (unrecorded)')
        repo = self.fixture('ambiguous', head)
        # A later commit changing the declarations under the same subject.
        git(repo, 'read-tree', head)
        blob = subprocess.run(['git', '-C', str(repo), 'hash-object', '-w', '--stdin'], check=True,
                              input=git(ROOT, 'show', head + ':' + POLICY) + b'\n',
                              stdout=subprocess.PIPE).stdout.decode().strip()
        git(repo, 'update-index', '--cacheinfo', '100644,' + blob + ',' + POLICY)
        tree = git(repo, 'write-tree').decode().strip()
        git(repo, 'update-ref', 'HEAD', self.commit(repo, tree, head, subject))
        with self.assertRaisesRegex(ValueError, 'expected exactly one commit .* found 2'):
            recorded(repo, subject)


if __name__ == '__main__':
    unittest.main()
