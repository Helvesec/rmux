"""Passive evidence: audit events only; no monkeypatch, build flag or test change."""
import atexit
import datetime
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import sys

ROOT = AUDIT = NATURAL = GIT = PARENT = None
BUSY = False
LAST_BUILD = None
SEEN = set()
ORIGINS = {}
WINDOWS = os.name == 'nt'

def natural(path):
    text = str(path)
    if os.name == 'nt' and text.startswith('\\\\?\\UNC\\'):
        return Path('\\\\' + text[8:])
    if os.name == 'nt' and text.startswith('\\\\?\\'):
        return Path(text[4:])
    return Path(path)

def access(path):
    path = natural(path).absolute()
    if os.name != 'nt':
        return path
    text = str(path)
    return Path('\\\\?\\UNC\\' + text[2:] if text.startswith('\\\\') else '\\\\?\\' + text)

def files_under(path):
    return sorted(natural(p) for p in access(path).rglob('*') if p.is_file())

def windows_argv(command):
    """Decode the Windows quoting emitted by subprocess.list2cmdline."""
    result, word = [], []
    quoted = started = False
    index = 0
    while index < len(command):
        char = command[index]
        if char in ' \t' and not quoted:
            if started:
                result.append(''.join(word))
                word, started = [], False
            index += 1
            continue
        started = True
        slashes = 0
        while index < len(command) and command[index] == '\\':
            slashes += 1
            index += 1
        if index < len(command) and command[index] == '"':
            word.extend('\\' * (slashes // 2))
            if slashes % 2:
                word.append('"')
            else:
                quoted = not quoted
            index += 1
        else:
            word.extend('\\' * slashes)
            if index < len(command):
                if command[index] in ' \t' and not quoted:
                    continue
                word.append(command[index])
                index += 1
    if quoted:
        raise ValueError('Unterminated Windows argument quote')
    if started:
        result.append(''.join(word))
    return result

def argv_tokens(argv):
    if isinstance(argv, str):
        return windows_argv(argv) if WINDOWS else [argv]
    if not isinstance(argv, (list, tuple)) or any(not isinstance(a, str) for a in argv):
        raise ValueError('Unsupported Popen argv')
    return list(argv)

def append(name, value):
    value.update(at=datetime.datetime.now(datetime.timezone.utc).isoformat(), pid=os.getpid(), ppid=os.getppid())
    with (AUDIT/name).open('a',encoding='utf-8') as stream:
        stream.write(json.dumps(value,default=str)+'\n')

def record(file):
    file = natural(file)
    readable = access(file)
    before = readable.stat()
    with readable.open('rb') as stream:
        magic = stream.read(2)
    digest = hashlib.sha256(readable.read_bytes()).hexdigest()
    after = readable.stat()
    assert (before.st_size,before.st_mtime_ns,before.st_ino) == (after.st_size,after.st_mtime_ns,after.st_ino), str(file)
    return dict(path=str(file),physical=str(natural(readable.resolve())),bytes=before.st_size,
        mtime_ns=before.st_mtime_ns,mtime_after_hash_ns=after.st_mtime_ns,
        inode=before.st_ino,nlink=before.st_nlink,sha256=digest,pe=magic==b'MZ')

def retained(file, bucket):
    item = record(file)
    dest = AUDIT / bucket / item['sha256']
    dest.parent.mkdir(exist_ok=True)
    if not dest.exists():
        shutil.copy2(access(file), access(dest))
    assert hashlib.sha256(dest.read_bytes()).hexdigest() == item['sha256'], str(dest)
    assert record(file) == item, str(file)
    item['retained'] = str(dest)
    return item

def source_git(source):
    facts = {}
    for label, args in [('head', ['rev-parse', 'HEAD']), ('tree', ['rev-parse', 'HEAD^{tree}']),
                        ('tracked_status', ['status', '--porcelain', '--untracked-files=no'])]:
        p = subprocess.run([GIT, *args], cwd=source, capture_output=True, text=True, check=True)
        facts[label] = p.stdout.strip()
    assert all(len(facts[k]) == 40 and set(facts[k]) <= set('0123456789abcdef') for k in ('head', 'tree'))
    assert not facts['tracked_status'], str(source)
    return facts

def artifact_units(artifact, units):
    name = artifact['target']['name']
    names = {name, name.replace('-', '_')}
    deps = [unit for unit in units if Path(unit['path']).suffix == '.d' and any(
        Path(unit['path']).stem == n or Path(unit['path']).stem.startswith(n + '-') for n in names)]
    fingerprints = [unit for unit in units if '.fingerprint' in Path(unit['path']).parts
                    and Path(unit['path']).name in {f'bin-{name}', f'bin-{name}.json'}]
    assert deps and fingerprints, 'Missing .d/.fingerprint for ' + name
    return deps + fingerprints

def snapshot_build(build):
    source = Path(build['cwd'])
    target = source / 'target'
    images, units = [], []
    for file in files_under(target):
        assert access(file).resolve().is_relative_to(access(source).resolve()), str(file)
        with access(file).open('rb') as stream:
            magic = stream.read(4)
        if magic[:2] == b'MZ' or magic == b'\x7fELF' or magic in (
                b'\xcf\xfa\xed\xfe', b'\xce\xfa\xed\xfe', b'\xfe\xed\xfa\xcf',
                b'\xfe\xed\xfa\xce', b'\xca\xfe\xba\xbe', b'\xbe\xba\xfe\xca'):
            images.append(retained(file, 'images'))
        if file.suffix == '.d' or '.fingerprint' in file.parts:
            units.append(retained(file, 'objects'))
    git = source_git(source)
    assert git == build['source_git'], 'Fixture HEAD/tree/status changed during build'
    logs = {p.name: retained(p, 'objects') for p in sorted(source.glob('*.jsonl'))}
    changed = {name: item for name, item in logs.items() if name not in build['logs_before']
               or any(item[k] != build['logs_before'][name][k]
                      for k in ('sha256', 'bytes', 'mtime_ns', 'inode'))}
    origins, issues = [], []
    for name, log in changed.items():
        rows = [json.loads(line) for line in Path(log['retained']).read_text().splitlines()]
        if not rows or rows[-1].get('reason') != 'build-finished' or rows[-1].get('success') is not True:
            issues.append('Build not proven successful: ' + name)
            continue
        for line, row in enumerate(rows, 1):
            if row.get('reason') != 'compiler-artifact':
                continue
            manifest = Path(row['manifest_path'])
            assert manifest.resolve().is_relative_to(source.resolve()), str(manifest)
            paths = [*row.get('filenames', []), row.get('executable')]
            for image in images:
                if image['path'] in paths:
                    origins.append(dict(sha256=image['sha256'], image_path=image['path'],
                        log=log, line=line, artifact=row, build=build, source_git=git,
                        manifest=retained(manifest, 'objects'), units=artifact_units(row, units)))
    for image in images:
        key = (str(source), git['head'], image['sha256'])
        matches = [origin for origin in origins if origin['sha256'] == image['sha256']]
        if matches:
            ORIGINS[key] = matches
        image['origins'] = ORIGINS.get(key, [])
        if not image['origins']:
            issues.append('Image without compilation origin: ' + image['path'])
    if not changed or not images or not units:
        issues.append('Missing changed Cargo log, image or compilation unit evidence')
    append('build-snapshots.jsonl', dict(build=build, source_git=git, images=images,
        units=units, fingerprints=[str(natural(p)) for p in access(target).rglob('.fingerprint')],
        logs=logs, complete=not issues, issues=issues))
    if issues:
        raise ValueError('; '.join(issues))

def preserve(source):
    if source in SEEN or not source.is_relative_to(NATURAL) or not (source/'Cargo.toml').is_file(): return
    SEEN.add(source)
    facts=dict(source=str(source),physical_source=str(source.resolve()),parent_sha=PARENT,git={})
    for label,args in [('head',['rev-parse','HEAD']),('tree',['rev-parse','HEAD^{tree}']),
                       ('tracked_status',['status','--porcelain','--untracked-files=no'])]:
        p=subprocess.run([GIT,*args],cwd=source,capture_output=True,text=True)
        facts['git'][label]=dict(rc=p.returncode,stdout=p.stdout.strip(),stderr=p.stderr.strip())
    # Hash originals before cleanup; verify their mtimes again after the copy.
    facts['files']=[record(p) for p in files_under(source)]
    facts['fingerprints']=[str(natural(p)) for p in access(source).rglob('.fingerprint')]
    dest=AUDIT/'retained'/source.name
    shutil.copytree(access(source),access(dest))
    for item in facts['files']:
        p=Path(item['path']);copy=dest/p.relative_to(source)
        assert access(p).stat().st_mtime_ns==item['mtime_ns']
        assert hashlib.sha256(access(copy).read_bytes()).hexdigest()==item['sha256']
        item['retained']=str(copy)
    facts['retained']=str(dest)
    append('fixtures.jsonl',facts)

def observe(event,args):
    global BUSY,LAST_BUILD
    if BUSY or event not in ('subprocess.Popen','shutil.rmtree'): return
    BUSY=True
    try:
        if LAST_BUILD:
            build, LAST_BUILD = LAST_BUILD, None
            try:
                snapshot_build(build)
            except Exception as exc:
                append('observer-errors.jsonl', dict(event=event, error=repr(exc), build=build))
        if event=='subprocess.Popen':
            exe,argv,cwd,env=args
            tokens = argv_tokens(argv)
            item=dict(event=event,executable=exe,argv=argv,argv_tokens=tokens,cwd=str(Path(cwd or Path.cwd()).absolute()),
                resolved=shutil.which(str(exe or tokens[0])),parent_sha=PARENT)
            append('subprocess.jsonl',item)
            if len(tokens)>1 and tokens[1]=='build' and Path(item['cwd']).is_relative_to(NATURAL):
                source = Path(item['cwd'])
                assert source.resolve().is_relative_to(NATURAL.resolve()), str(source)
                item['source_git'] = source_git(source)
                item['logs_before'] = {p.name:record(p) for p in source.glob('*.jsonl')}
                LAST_BUILD=item
        else:
            preserve(natural(args[0]).absolute())
    except Exception as exc:
        append('observer-errors.jsonl',dict(event=event,error=repr(exc)))
    finally:
        BUSY=False

def finish():
    global BUSY, LAST_BUILD
    if BUSY or LAST_BUILD is None:
        return
    BUSY = True
    build, LAST_BUILD = LAST_BUILD, None
    try:
        snapshot_build(build)
    except Exception as exc:
        append('observer-errors.jsonl', dict(event='process-exit', error=repr(exc), build=build))
    finally:
        BUSY = False

def install(root, parent_sha, git):
    """Install the passive audit hook in the test process, with private evidence."""
    global ROOT, AUDIT, NATURAL, GIT, PARENT
    ROOT = Path(root).absolute()
    AUDIT = ROOT / '.rmux-audit/d144'
    NATURAL = ROOT / '.rmux-audit/local-sbom-tests'
    GIT, PARENT = str(git), parent_sha
    assert source_git(ROOT)['head'] == parent_sha, 'Parent HEAD must be real and exact'
    AUDIT.mkdir(parents=True, exist_ok=True)
    append('observer-starts.jsonl', dict(argv=sys.argv, parent_sha=PARENT,
        observer_sha256=hashlib.sha256(Path(__file__).read_bytes()).hexdigest()))
    sys.addaudithook(observe)
    atexit.register(finish)
