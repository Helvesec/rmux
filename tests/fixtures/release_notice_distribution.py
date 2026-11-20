"""Acceptance fixture for notice transport, not evidence of a built Cargo graph."""
import copy
import hashlib
import importlib.util
from pathlib import Path
import sys
import tempfile
from types import SimpleNamespace

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT / "scripts/release"))


def module(name):
    spec = importlib.util.spec_from_file_location(name, ROOT / f"scripts/release/{name}.py")
    result = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(result)
    return result


def rejected(action):
    try:
        action()
    except ValueError:
        return
    raise AssertionError("invalid notice binding was accepted")


record = module("canonical-build-record")
candidate = module("candidate-manifest")
evidence = module("release_evidence")
_, platforms = record.load_contract(record.CONTRACT)
with tempfile.TemporaryDirectory(prefix="rmux-notice-release-") as temporary:
    root = Path(temporary)
    artifacts, public, notice_names = [], [], set()
    for key, platform in platforms.items():
        directory = root / key
        directory.mkdir()
        names = [f"rmux-0.9.0-{key}.{platform['archive_format']}"]
        if platform['linux_packages']:
            names += [f"rmux-0.9.0-{key}.deb", f"rmux-0.9.0-{key}.rpm"]
        supplemental = {
            "snap-amd64": f"rmux-{key}.snap", "snap-arm64": f"rmux-{key}.snap",
            "wasm-byte-set": "rmux-web-crypto-wasm-0.9.0.tar",
            "wasm-provenance": "rmux-web-crypto-wasm-0.9.0.provenance.json",
            "crate-package-set": "rmux-0.9.0-crate-package-set.tar",
            "chocolatey-package": "rmux.0.9.0.nupkg",
        }
        for name in names + [supplemental[role] for role in platform['supplemental_roles']]:
            (directory / name).write_bytes(f"fixture asset: {name}\n".encode())
        for name in names:
            digest = hashlib.sha256((directory / name).read_bytes()).hexdigest()
            notice = name + ".NOTICE.txt"
            (directory / notice).write_bytes(
                f"Package: {name}\nSHA256: {digest}\n\nRMUX third-party notices\nfixture attribution\n".encode("utf-8"))
            notice_names.add(notice)
        record.write_checksums(SimpleNamespace(assets_dir=directory))
        files = record.asset_files(directory, platform, "0.9.0")
        candidate.validate_files(files, key)
        artifacts.append(dict(platform_key=key, files=files))
        for item in files:
            if item['role'] in {'archive', 'debian', 'rpm', 'snap-amd64', 'snap-arm64'} or item['path'] in notice_names:
                public.append(f"{item['sha256']}  {item['path']}")
        # A notice must keep its precise package partner, name and digest.
        orphan = [item for item in files if item['path'] != names[0]]
        rejected(lambda: record.verify_asset_roles(orphan, platform))
        rejected(lambda: candidate.validate_files(orphan, key))
        wrong = copy.deepcopy(files)
        next(item for item in wrong if item['path'] == names[0] + '.NOTICE.txt')['path'] = 'unrelated.NOTICE.txt'
        wrong.sort(key=lambda item: item['path'])
        rejected(lambda: candidate.validate_files(wrong, key))
        notice = directory / (names[0] + '.NOTICE.txt')
        contents = notice.read_text()
        notice.write_text(contents.replace('SHA256: ', 'SHA256: 0'))
        record.write_checksums(SimpleNamespace(assets_dir=directory))
        rejected(lambda: record.asset_files(directory, platform, "0.9.0"))

    manifest = dict(schema_version=1, repository_id=1239918790, source_git_sha='a' * 40,
                    candidate_run_id=77, candidate_run_attempt=1, release_intent_id='notice-test',
                    planned_release_ref='v0.9.0', release_kind='stable', release_version='0.9.0',
                    package_version='0.9.0', is_prerelease=False,
                    release_policy={'sha256': 'b' * 64}, created_at='2026-09-29T00:00:00Z',
                    expires_at='2026-09-30T00:00:00Z', artifacts=artifacts)
    sums = root / 'SHA256SUMS'
    sums.write_text('\n'.join(sorted(public, key=lambda line: line.split('  ', 1)[1])) + '\n')
    assets = evidence.publishable_assets(manifest, sums)
    assert notice_names <= {item['name'] for item in assets}, 'distribution lost notices'
    # The legacy workflow must retain sidecars at upload, checksums, attestation and publication.
    workflow = (ROOT / '.github/workflows/release.yml').read_text()
    assert 'dist/*.NOTICE.txt' in workflow
    assert '*.NOTICE.txt' in workflow.split('assets=(', 1)[1].split(')', 1)[0]
    assert workflow.count('release-assets/*.NOTICE.txt') >= 2
    assert 'scripts/release/build-windows-package-binaries.ps1' in workflow
    windows = (ROOT / 'scripts/release/build-windows-package-binaries.ps1').read_text()
    assert 'notices.py' in windows and '"build"' in windows and '--binary-dir' in windows
print('notice sidecars: canonical record, candidate, distribution and Windows capture PASS')
