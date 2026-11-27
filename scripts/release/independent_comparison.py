"""Fail-closed provenance and byte comparison for two independently built bundles."""
from __future__ import annotations

import importlib.util
import json
from pathlib import Path
import re
from types import SimpleNamespace

from canonical_contract import PLATFORMS
from independent_archive import digest, object_file, read_archive, regular
from canonical_payload import embedded_commit


def exact(value, keys, label):
    if not isinstance(value, dict) or set(value) != set(keys.split()):
        raise ValueError(label + ' fields differ')


def load_receipt(path, sha):
    path = regular(path)
    record = object_file(path)
    exact(record, 'schema_version source_sha platform_key toolchain options build archive binaries', 'receipt')
    if type(record['schema_version']) is not int or record['schema_version'] not in (1, 2):
        raise ValueError('unsupported independent build schema')
    if record['source_sha'] != sha:
        raise ValueError('source SHA differs')
    platform = record['platform_key']
    if not isinstance(platform, str) or platform not in PLATFORMS:
        raise ValueError('unknown canonical platform')
    exact(record['toolchain'], 'rustc cargo native_tools', 'toolchain')
    if any(not isinstance(v, str) or not v.strip() for v in (record['toolchain']['rustc'], record['toolchain']['cargo'])):
        raise ValueError('missing toolchain identity')
    tools = record['toolchain']['native_tools']
    if (not isinstance(tools, dict) or not set(tools) <= {'cc', 'strip', 'cargo-zigbuild', 'zig', 'codesign'}
            or any(not isinstance(v, str) or re.fullmatch('[0-9a-f]{64}', v) is None for v in tools.values())):
        raise ValueError('native toolchain digests differ')
    options = record['options']
    if (not isinstance(options, dict) or options.get('configuration') != 'release'
            or options.get('target') != PLATFORMS[platform][3]):
        raise ValueError('build options differ from native release contract')
    exact(options, 'configuration target normalizations cargo_locked cargo_incremental source_date_epoch '
          'features cargo_toml_sha256 cargo_lock_sha256 layout_sha256 notice_layout_sha256 packaging cargo_configs', 'options')
    if (options['cargo_locked'] is not True or options['cargo_incremental'] is not False
            or not isinstance(options['source_date_epoch'], str)
            or re.fullmatch('[0-9]+', options['source_date_epoch']) is None
            or options['features'] != {'cli': ['default', 'tiny-cli'], 'helper': ['default'], 'daemon': ['default']}):
        raise ValueError('release build options are incomplete or unapproved')
    exact(options['packaging'], 'linux_glibc_floor max_supported_glibc codesign_adhoc strip_tiny', 'packaging')
    for field, value in options['packaging'].items():
        if (not isinstance(value, str) or (field in {'codesign_adhoc', 'strip_tiny'} and value not in {'0', '1'})
                or (field in {'linux_glibc_floor', 'max_supported_glibc'} and value
                    and re.fullmatch(r'[0-9]+\.[0-9]+', value) is None)):
            raise ValueError('unapproved packaging option: ' + field)
    if not isinstance(options['cargo_configs'], dict) or any(
            not isinstance(k, str) or not isinstance(v, str) or re.fullmatch('[0-9a-f]{64}', v) is None
            for k, v in options['cargo_configs'].items()):
        raise ValueError('Cargo configuration binding differs')
    for field in ('cargo_toml_sha256', 'cargo_lock_sha256', 'layout_sha256', 'notice_layout_sha256'):
        if not isinstance(options[field], str) or re.fullmatch('[0-9a-f]{64}', options[field]) is None:
            raise ValueError('build input digest missing: ' + field)
    if 'host: ' + options['target'] not in record['toolchain']['rustc'].splitlines():
        raise ValueError('toolchain native host differs')
    if options.get('normalizations') != []:
        raise ValueError('unauthorized binary normalization')
    canonical = record['schema_version'] == 2
    exact(record['build'], 'id target_dir fresh_target object_cache_restored' + (' producers' if canonical else ''), 'build')
    build = record['build']
    if (build['fresh_target'] is not True or build['object_cache_restored'] is not False
            or not isinstance(build['id'], str) or not build['id']
            or not isinstance(build['target_dir'], str) or not build['target_dir']):
        raise ValueError('independent cold build provenance missing')
    archive = record['archive']
    exact(archive, 'path size sha256', 'archive')
    if not isinstance(archive['path'], str) or Path(archive['path']).name != archive['path']:
        raise ValueError('archive must be beside its receipt')
    archive_path = regular(path.parent / archive['path'])
    if (type(archive['size']) is not int or archive_path.stat().st_size != archive['size']
            or digest(archive_path.read_bytes()) != archive['sha256']):
        raise ValueError('archive digest or size differs')
    metadata, inventory, binaries = read_archive(archive_path, platform)
    if (metadata.get('git_commit') != sha or metadata.get('git_dirty') is not False
            or metadata.get('configuration') != 'release'
            or metadata.get('release_artifact') is not True
            or metadata.get('skip_build') is not False
            or metadata.get('target') != options['target']):
        raise ValueError('archive source or release provenance differs')
    if record['binaries'] != inventory:
        raise ValueError('exact critical binary set or digest differs')
    if canonical:
        producers = build['producers']
        exact(producers, 'cli helper daemon', 'canonical producers')
        os_name = platform.split('-')[0]
        for role, producer in producers.items():
            exact(producer, 'path sha256 size commit source os target', 'canonical producer')
            target = Path(build['target_dir'])
            source = producer['source']
            if (producer['commit'] != sha or producer['os'] != os_name
                    or producer['target'] != str(target) or not target.is_absolute()
                    or not isinstance(source, str) or not Path(source).is_absolute()
                    or target.parent != Path(source) / '.rmux-audit/candidate-target' / sha / os_name
                    or re.fullmatch('[0-9a-f]{32}', build['id']) is None
                    or target.name != build['id']
                    or not isinstance(producer['path'], str)
                    or not Path(producer['path']).is_relative_to(target)
                    or type(producer['size']) is not int or producer['size'] <= 0
                    or not isinstance(producer['sha256'], str)
                    or re.fullmatch('[0-9a-f]{64}', producer['sha256']) is None):
                raise ValueError('canonical producer provenance differs')
            embedded_commit(binaries[role], sha)
    return record, binaries, archive_path


def canonical_binding(path, record, expected_digest):
    # Reuse the producer's current asset/manifest checks; this is a byte join,
    # not a replacement for the existing hosted attestation verifier.
    spec = importlib.util.spec_from_file_location('canonical_record',
            Path(__file__).with_name('canonical-build-record.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    if not isinstance(expected_digest, str) or re.fullmatch('[0-9a-f]{64}', expected_digest) is None:
        raise ValueError('canonical producer record digest is required')
    if digest(regular(path).read_bytes()) != expected_digest:
        raise ValueError('canonical producer record digest differs')
    value = object_file(path)
    platform = record['platform_key']
    contract, platforms = module.load_contract(module.CONTRACT)
    module.verify_record_shape(value, contract, platforms[platform])
    if value.get('source_git_sha') != record['source_sha']:
        raise ValueError('canonical source/platform differs')
    if value['toolchain']['rustc_verbose_sha256'] != digest((record['toolchain']['rustc'] + '\n').encode()):
        raise ValueError('canonical toolchain identity differs')
    fields = {}
    for line in record['toolchain']['rustc'].splitlines():
        if ': ' in line:
            key, item = line.split(': ', 1)
            if key in fields:
                raise ValueError('duplicate native compiler identity field')
            fields[key] = item
    if any(fields.get(raw) != value['toolchain'][field] for raw, field in (
            ('host', 'host'), ('release', 'release'), ('commit-hash', 'commit_hash'))):
        raise ValueError('canonical compiler fields differ')
    assets = Path(path).parent / 'assets'
    module.verify(SimpleNamespace(record=Path(path), expected_record_sha256=expected_digest,
        contract=module.CONTRACT, platform_key=platform, assets_dir=assets,
        source_sha=record['source_sha'], fast_run_id=value['fast_run_id'],
        candidate_run_id=value['candidate_run_id'], release_intent_id=value['release_intent_id'],
        planned_release_ref=value['planned_release_ref'], release_kind=value['release_kind']))
    files = value['files']
    version = value['planned_release_ref'].removeprefix('v').split('-rc.')[0]
    if record['archive']['path'] != f"rmux-{version}-{platform}.{platforms[platform]['archive_format']}":
        raise ValueError('canonical release archive identity differs')
    expected = dict(path=record['archive']['path'], role='archive',
                    size=record['archive']['size'], sha256=record['archive']['sha256'])
    if expected not in files:
        raise ValueError('canonical archive binding differs')
    if digest(regular(path).read_bytes()) != expected_digest:
        raise ValueError('canonical record changed during comparison')
    return expected_digest


def invalidate_output(args):
    # Establish deletion safety before validating evidence. Broken receipts
    # protect every possible sibling archive, including hard-linked aliases.
    output = Path(args.output).absolute()
    inputs = [Path(args.left).absolute(), Path(args.right).absolute()]
    unknown_archives = []
    protected_assets = []
    if args.canonical_record:
        canonical = Path(args.canonical_record).absolute()
        inputs.append(canonical)
        protected_assets.append(canonical.parent / 'assets')
    for path in inputs[:2]:
        try:
            value = object_file(path)
        except (ValueError, OSError, UnicodeError):
            unknown_archives.append(path.parent)
            continue
        archive = value.get('archive', {}).get('path') if isinstance(value.get('archive'), dict) else None
        if isinstance(archive, str) and Path(archive).name == archive:
            inputs.append(path.parent / archive)
        else:
            unknown_archives.append(path.parent)
        producers = value.get('build', {}).get('producers') if isinstance(value.get('build'), dict) else None
        if isinstance(producers, dict):
            for producer in producers.values():
                if isinstance(producer, dict) and isinstance(producer.get('path'), str):
                    inputs.append(Path(producer['path']).absolute())
    if (output.is_symlink() or output.resolve() != output or output in inputs
            or any(output.is_relative_to(root) for root in protected_assets)):
        raise ValueError('comparison output would overwrite evidence')
    if output.exists():
        for root in unknown_archives + protected_assets:
            if root.is_dir():
                neighbors = root.rglob('*') if root in protected_assets else root.iterdir()
                # Exclude only this name; another name for the same file is
                # still protected evidence, even beside an unreadable receipt.
                inputs.extend(path for path in neighbors if path != output)
        if any(path.exists() and output.samefile(path) for path in inputs):
            raise ValueError('comparison output would overwrite evidence')
        regular(output)
        if unknown_archives:
            # Without a readable receipt, arbitrary files might be producer
            # evidence. Only an identifiable comparison report is safe to erase.
            try:
                previous = object_file(output)
            except (ValueError, OSError, UnicodeError) as error:
                raise ValueError('unknown evidence; output is not a comparison report') from error
            if (previous.get('schema_version') != 1 or type(previous.get('identical')) is not bool
                    or not isinstance(previous.get('left_receipt_sha256'), str)
                    or not isinstance(previous.get('right_receipt_sha256'), str)):
                raise ValueError('unknown evidence; output is not a comparison report')
        output.unlink()
    elif output.parent in unknown_archives:
        raise ValueError('comparison output would overwrite evidence')
    return output


def compare(args):
    output = invalidate_output(args)
    if args.expected_canonical_record_sha256 and not args.canonical_record:
        raise ValueError('canonical digest requires its producer record')
    if re.fullmatch('[0-9a-f]{40}', args.source_sha) is None:
        raise ValueError('full source SHA required')
    left, left_bytes, left_archive = load_receipt(args.left, args.source_sha)
    right, right_bytes, right_archive = load_receipt(args.right, args.source_sha)
    for field in ('schema_version', 'platform_key', 'toolchain', 'options'):
        if left[field] != right[field]:
            raise ValueError(field + ' differs between independent builds')
    if (left['build']['id'] == right['build']['id']
            or left['build']['target_dir'] == right['build']['target_dir']
            or left_archive.samefile(right_archive)):
        raise ValueError('independent builds must use distinct targets and archives')
    differences = []
    for role in sorted(left_bytes):
        a, b = left_bytes[role], right_bytes[role]
        if a != b:
            first = next((i for i, pair in enumerate(zip(a, b)) if pair[0] != pair[1]), min(len(a), len(b)))
            changed = sum(x != y for x, y in zip(a, b)) + abs(len(a) - len(b))
            differences.append(dict(role=role, first_difference=first, differing_bytes=changed,
                                    left_size=len(a), right_size=len(b),
                                    left_sha256=digest(a), right_sha256=digest(b)))
    report = dict(schema_version=1, source_sha=args.source_sha, platform_key=left['platform_key'],
                  identical=not differences, normalizations=[], differences=differences,
                  left_receipt_sha256=digest(regular(args.left).read_bytes()),
                  right_receipt_sha256=digest(regular(args.right).read_bytes()),
                  binaries=left['binaries'], hosted_attestation_verified=False, line_closed=False)
    if args.canonical_record:
        report['canonical_record_sha256'] = canonical_binding(args.canonical_record, left, args.expected_canonical_record_sha256)
    output.write_text(json.dumps(report, sort_keys=True, indent=2) + '\n')
    return 0 if report['identical'] else 1
