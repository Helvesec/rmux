#!/usr/bin/env python3
"""Retain the bounded, observed Linux inputs before Git or build consumers.

The executable collector requires Python -I -S -B so site startup does not run
before its observed .pth/.pyc inputs have been retained. This snapshot does not
claim retention of the collector interpreter's own earlier loader accesses.
"""
from pathlib import Path
import datetime
import hashlib
import importlib.util
import json
import os
import re
import secrets
import shutil
import subprocess
import sys


def locked_packages(lock):
    # Isolated collectors cannot import siblings through Python's search path.
    spec = importlib.util.spec_from_file_location(
        'toml_reader', Path(__file__).with_name('toml_reader.py'))
    reader = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(reader)
    return reader.loads(lock.read_text())['package']


SYSTEM_INPUTS = (
    '/etc/ld.so.cache', '/etc/ssl/certs/ca-certificates.crt',
    '/usr/lib/locale/locale-archive', '/etc/locale.alias',
    '/usr/lib/locale/C.utf8/LC_CTYPE',
    '/usr/lib/x86_64-linux-gnu/gconv/gconv-modules.cache',
    '/usr/share/zoneinfo/Europe/Zurich',
    '/usr/lib/python3/dist-packages/distutils-precedence.pth',
    '/usr/lib/python3/dist-packages/_distutils_hack/__pycache__/__init__.cpython-312.pyc',
    '/usr/lib/python3/dist-packages/__pycache__/apport_python_hook.cpython-312.pyc',
    '/etc/ssl/openssl.cnf',
    '/usr/lib/locale/C.utf8/LC_MESSAGES/SYS_LC_MESSAGES',
    '/usr/lib/locale/C.utf8/LC_TIME',
)
# Open paths measured in trace038; their canonical roles stay in the frozen profile.
OBSERVED_ALIASES = {
    '/usr/lib/ssl/cert.pem': '/etc/ssl/certs/ca-certificates.crt',
    '/usr/share/locale/locale.alias': '/etc/locale.alias',
    '/etc/localtime': '/usr/share/zoneinfo/Europe/Zurich',
    '/usr/lib/ssl/openssl.cnf': '/etc/ssl/openssl.cnf',
}
HOME_INPUTS = (
    '.gitconfig', '.config/git/ignore',
    '.local/lib/python3.12/site-packages/__editable__.json_parser-0.1.0.pth',
    '.local/lib/python3.12/site-packages/__pycache__/'
    '__editable___json_parser_0_1_0_finder.cpython-312.pyc',
)
GIT_INPUTS = ('HEAD', 'config', 'index', 'shallow', 'info/exclude',
              'logs/HEAD', 'FETCH_HEAD')
MUTABLE_CARGO = ('.package-cache', '.global-cache', '.package-cache-mutate')
TOOLS = ('git', 'python3', 'bash', 'sh', 'strace', 'cc', 'gcc', 'c++', 'g++',
         'ld', 'ldd', 'as', 'ar', 'ranlib', 'nm', 'objcopy', 'pkg-config',
         'env', 'date', 'uname', 'readlink', 'dirname', 'realpath')


def now():
    return datetime.datetime.now(datetime.timezone.utc).isoformat()


def sha(path):
    with Path(path).open('rb') as stream:
        digest = hashlib.sha256()
        for block in iter(lambda: stream.read(256 * 1024), b''):
            digest.update(block)
        return digest.hexdigest()


def stamp(info):
    return (info.st_dev, info.st_ino, info.st_mode, info.st_size,
            info.st_mtime_ns, info.st_ctime_ns)


class Snapshot:
    def __init__(self, audit, check_capacity=lambda: None):
        self.check_capacity = check_capacity
        self.check_capacity()
        self.objects = audit / 'before-objects'
        self.objects.mkdir(exist_ok=False, mode=0o700)
        if self.objects.resolve() != self.objects:
            raise RuntimeError('redirected native input object store')
        self.files = {}
        self.mutable_inputs = {}
        self.absent_observed_aliases = {}

    def retain(self, path):
        path = Path(path).absolute()
        key = str(path)
        if key in self.files:
            self.verify(key)
            return
        resolved = path.resolve(strict=True)
        if not resolved.is_file() or resolved.name in ('credentials', 'credentials.toml'):
            raise RuntimeError('native input is not an allowed regular file')
        before = stamp(resolved.stat())
        digest = sha(resolved)
        obj = self.objects / digest
        temporary = self.objects / ('.' + secrets.token_hex(16))
        try:
            with resolved.open('rb') as src, temporary.open('xb') as dst:
                shutil.copyfileobj(src, dst)
                dst.flush()
                os.fsync(dst.fileno())
            if os.name != 'nt':
                temporary.chmod(0o400)
            try:
                os.link(temporary, obj)
            except FileExistsError:
                pass
            if obj.is_symlink() or sha(obj) != digest:
                raise RuntimeError('native input object changed')
        finally:
            temporary.unlink(missing_ok=True)
        if os.name == 'nt':
            # Read-only attributes belong to the inode shared by both links.
            # Remove the temporary name before protecting the published object.
            obj.chmod(0o400)
        if (stamp(resolved.stat()) != before or path.resolve(strict=True) != resolved
                or sha(resolved) != digest):
            raise RuntimeError('native input changed during retention')
        self.files[key] = dict(resolved=str(resolved), sha256=digest,
                               size=before[3], object=str(obj), retained_at=now())
        if len(self.files) % 5000 == 0:
            self.check_capacity()

    def verify(self, key):
        path = Path(key)
        if str(key) in self.absent_observed_aliases:
            try:
                path.lstat()
            except FileNotFoundError:
                return
            raise RuntimeError('native input alias appeared before consumer: ' + str(key))
        row = self.files[str(key)]
        obj = Path(row['object'])
        if (str(path.resolve(strict=True)) != row['resolved'] or obj.is_symlink()
                or ('windows_stamp' in row and
                    list(stamp(path.stat())) != row['windows_stamp'])
                or sha(path) != row['sha256'] or sha(obj) != row['sha256']):
            raise RuntimeError('native input differs before consumer: ' + str(key))

    def tree(self, root):
        for path in sorted(Path(root).rglob('*')):
            if path.is_file():
                self.retain(path)


def observed_bootstrap(snapshot, source, home, profile):
    """Only the measured path roles are accepted; no site/Git tree discovery."""
    snapshot.retain(profile)
    entries = json.loads(Path(profile).read_text())
    expected = set(SYSTEM_INPUTS)
    expected.update('$HOME/' + name for name in HOME_INPUTS)
    expected.update('$SOURCE/.git/' + name for name in GIT_INPUTS)
    expected.update('$HOME/.cargo/' + name for name in MUTABLE_CARGO)
    packs = [path for path in entries if re.fullmatch(
        r'\$SOURCE/\.git/objects/pack/pack-[0-9a-f]{40}\.(?:idx|pack)', path)]
    if (len(entries) != 29 or len(set(entries)) != 29 or len(packs) != 2
            or {path.rsplit('.', 1)[1] for path in packs} != {'idx', 'pack'}
            or packs[0].rsplit('.', 1)[0] != packs[1].rsplit('.', 1)[0]
            or set(entries) != expected | set(packs)):
        raise RuntimeError('observed native input profile differs from bounded allowlist')
    stable = []
    mutable_names = {'$HOME/.cargo/' + item for item in MUTABLE_CARGO}
    for name in entries:
        path = Path(name.replace('$SOURCE', str(source)).replace('$HOME', str(home)))
        if name in mutable_names:
            resolved = path.resolve(strict=True)
            if not resolved.is_file():
                raise RuntimeError('observed mutable Cargo input is not a regular file')
            info = resolved.stat()
            snapshot.mutable_inputs[str(path)] = dict(
                resolved=str(resolved), observed_at=now(), size=info.st_size,
                role='mutable Cargo cache/lock; no immutable code-input credit')
        else:
            snapshot.retain(path)
            stable.append(str(path.absolute()))
    for name, target in OBSERVED_ALIASES.items():
        path = Path(name).absolute()
        key = str(path)
        expected = snapshot.files[str(Path(target).absolute())]['resolved']
        try:
            path.lstat()
        except FileNotFoundError:
            # An absent open path has no bytes to retain; bind its absence too,
            # without giving it immutable coverage or admitting its later creation.
            snapshot.absent_observed_aliases[key] = dict(
                expected_resolved=expected, observed_at=now())
        else:
            if str(path.resolve(strict=True)) != expected:
                raise RuntimeError('native input alias differs from observed binding: ' + key)
            snapshot.retain(path)
            if snapshot.files[key]['resolved'] != expected:
                raise RuntimeError('native input alias changed during retention: ' + key)
        stable.append(key)
    # Verify coverage before the first Git/ldd/build consumer can be reached.
    for key in stable:
        snapshot.verify(key)
    return stable


def observed_windows_bootstrap(snapshot, observation, bindings):
    from native_input_windows import observed_bootstrap as collect
    return collect(snapshot, observation, bindings)


def main():
    source = Path(os.environ['NATIVE_INPUT_SOURCE']).resolve(strict=True)
    audit = Path(os.environ['NATIVE_INPUT_AUDIT']).resolve(strict=True)
    head = os.environ['NATIVE_INPUT_HEAD']
    toolchain = Path(os.environ['NATIVE_INPUT_TOOLCHAIN']).resolve(strict=True)
    home = Path(os.environ.get('NATIVE_INPUT_HOME', '/home/example'))
    profile = Path(os.environ.get('NATIVE_INPUT_OBSERVED_OPEN_PROFILE', str(
        Path(__file__).with_name('fixtures') / 'native_input_observed_opens.json')))
    def floor():
        if shutil.disk_usage(source).free < 40 * 2**30 or shutil.disk_usage(home).free < 80 * 2**30:
            raise RuntimeError('native input retention disk floor violated')

    snapshot = Snapshot(audit, check_capacity=floor)
    bootstrap = observed_bootstrap(snapshot, source, home, profile)

    def git(*args):
        for path in bootstrap:
            snapshot.verify(path)
        env = dict(os.environ, GIT_OPTIONAL_LOCKS='0')
        return subprocess.check_output(['/usr/bin/git', '-C', str(source), *args],
                                       text=True, env=env).strip()

    if git('rev-parse', 'HEAD') != head or git('status', '--porcelain'):
        raise RuntimeError('native input source is not the clean pinned HEAD')
    floor()
    for rel in git('ls-files', '-z').split('\0'):
        if rel:
            snapshot.retain(source / rel)
    snapshot.tree(toolchain)
    for root in ('/usr/lib/python3.12', '/usr/lib/gcc/x86_64-linux-gnu',
                 '/usr/include', '/usr/lib/x86_64-linux-gnu/pkgconfig'):
        snapshot.tree(root)
    for path in Path('/usr/lib/x86_64-linux-gnu').glob('*'):
        if path.is_file() and ('.so' in path.name or path.suffix in ('.a', '.o')):
            snapshot.retain(path)
    for name in TOOLS:
        path = shutil.which(name)
        if not path:
            continue
        snapshot.retain(path)
        for key in bootstrap:
            snapshot.verify(key)
        libs = subprocess.run(['/usr/bin/ldd', path], capture_output=True, text=True).stdout
        for dep in re.findall(r'(/[^\s()]+)', libs):
            if Path(dep).is_file():
                snapshot.retain(dep)
    packages = locked_packages(source / 'Cargo.lock')
    registry, archives = home / '.cargo/registry', []
    for pkg in packages:
        if not pkg.get('source', '').startswith('registry+'):
            continue
        name = pkg['name'] + '-' + pkg['version']
        packed = list((registry / 'cache').glob('*/' + name + '.crate'))
        roots = list((registry / 'src').glob('*/' + name))
        if len(packed) != 1 or len(roots) != 1 or sha(packed[0]) != pkg['checksum']:
            raise RuntimeError('offline registry cache missing, ambiguous or different')
        snapshot.retain(packed[0])
        snapshot.tree(roots[0])
        archives.append(dict(name=pkg['name'], version=pkg['version'],
                             archive=str(packed[0]), sha256=pkg['checksum']))
    if (registry / 'index').exists():
        snapshot.tree(registry / 'index')
    for name in ('config', 'config.toml'):
        if (home / '.cargo' / name).exists():
            raise RuntimeError('Cargo global configuration requires explicit allowlisted qualification')
    argv = [str(toolchain / 'bin/cargo'), 'test', '--locked', '--offline', '-p', 'rmux-proto',
            '--test', 'wire_compat', '--test', 'wire_v8_v9', '--test', 'wire_ledger_v1',
            '--test', 'wire_runtime_v2']
    targets = {name: dict(path=str(source / 'crates/rmux-proto/tests' / (name + '.rs')),
                         sha256=sha(source / 'crates/rmux-proto/tests' / (name + '.rs')))
               for name in ('wire_compat', 'wire_v8_v9', 'wire_ledger_v1', 'wire_runtime_v2')}
    record = dict(at=now(), head=head, source=str(source), tree=git('rev-parse', 'HEAD^{tree}'),
                  status=git('status', '--porcelain'), branch=git('branch', '--show-current'),
                  worktrees=git('worktree', 'list', '--porcelain'), remotes=git('remote'),
                  files=snapshot.files, mutable_inputs=snapshot.mutable_inputs,
                  absent_observed_aliases=snapshot.absent_observed_aliases,
                  count=len(snapshot.files), bytes=sum(row['size'] for row in snapshot.files.values()),
                  unique_bytes=sum(p.stat().st_size for p in snapshot.objects.iterdir()),
                  archives=archives, argv=argv, static_targets=targets,
                  features='Cargo default features; no --features, no --no-default-features',
                  command_qualified_before_execution=False,
                  qualification='Exact argv retained; collection does not qualify or execute the command',
                  disks={str(p): shutil.disk_usage(p)._asdict() for p in (source, home)})
    for path in (*snapshot.files, *snapshot.absent_observed_aliases):
        snapshot.verify(path)
    floor()
    (audit / 'before-inputs.json').write_text(json.dumps(record, indent=2) + '\n')
    print('BEFORE COMPLETE', record['count'], record['bytes'], record['unique_bytes'],
          len(archives), record['at'], flush=True)


if __name__ == '__main__':
    if sys.platform == 'win32':
        if not sys.flags.isolated or not sys.flags.no_site:
            raise SystemExit('Windows collector requires Python -I -S -B before site consumers')
        sys.path.insert(0, str(Path(__file__).resolve().parent))
        from native_input_windows_command import main as windows_main
        windows_main()
    elif sys.platform != 'linux' or not sys.flags.isolated or not sys.flags.no_site:
        raise SystemExit('Linux collector requires Python -I -S -B before site consumers')
    else:
        main()
elif __name__ == '<run_path>':
    main()
