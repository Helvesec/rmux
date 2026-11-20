"""Native persistence acceptance. Arguments: binary, pinned plugins, private output,
clean|kill, resurrect|continuum, optional restoring binary and same|upgrade|rollback.
Only the server at the newly created private socket is terminated.
"""
import json, os, pathlib, re, shlex, signal, subprocess, sys, time
from persistence_transition import validate_transition, verify_transition
binary, plugins, output = map(pathlib.Path, sys.argv[1:4])
mode, backend = sys.argv[4:6] if len(sys.argv) > 4 else ('clean', 'resurrect')
restore_binary=pathlib.Path(sys.argv[6]) if len(sys.argv)>6 else binary
if len(sys.argv) > 8:
    raise ValueError("unexpected persistence acceptance arguments")
transition = validate_transition(binary, restore_binary, sys.argv[7] if len(sys.argv) > 7 else None)
if transition is not None:
    binary = pathlib.Path(transition['saving']['path'])
    restore_binary = pathlib.Path(transition['restoring']['path'])
verify_transition(transition)
output.mkdir(parents=True, exist_ok=False)
(output/'bin').mkdir()
socket = output/'s'
wrapper = output/'bin/tmux'
def set_wrapper():
    guard = ''
    if transition is not None:
        helper = pathlib.Path(__file__).with_name('persistence_transition.py').resolve(strict=True)
        argv = [sys.executable, '-S', '-B', str(helper)]
        for image in (transition['saving'], transition['restoring']):
            argv.extend([image['path'], image['sha256']])
        guard = ' '.join(map(shlex.quote, argv)) + ' || exit $?\n'
    wrapper.write_text('#!/bin/sh\n'+guard+'if [ "$1" = -S ]; then exec '+shlex.quote(str(binary))+' "$@"; fi\nexec '+shlex.quote(str(binary))+' -S '+shlex.quote(str(socket))+' "$@"\n')
    wrapper.chmod(0o755)
set_wrapper()
env = dict(os.environ, PATH=str(output/'bin')+':'+os.environ['PATH'], RMUX_DISABLE_TMUX_FALLBACK='1')
env.pop('TMUX', None)
env.pop('RMUX_SOCKET', None)
log = open(output/'trace.log', 'w', buffering=1)
def run(*args, check=True):
    verify_transition(transition)
    result = subprocess.run([str(binary), '-S', str(socket), *args], env=env, text=True, capture_output=True, timeout=30)
    log.write(json.dumps([list(args), result.returncode, result.stdout, result.stderr])+'\n')
    if check and result.returncode: raise RuntimeError(result.stderr)
    return result.stdout.strip()
def plugin(name, script, *args):
    verify_transition(transition)
    result = subprocess.run(['/bin/bash',str(plugins/name/'scripts'/script),*args],env=env,text=True,capture_output=True,timeout=60)
    log.write(json.dumps([script,result.returncode,result.stdout,result.stderr])+'\n')
    assert result.returncode == 0, result.stderr
def configure():
    for key,value in [('@resurrect-dir',str(output/'save')),('@resurrect-processes','sleep'),('@resurrect-save-script-path',str(plugins/'tmux-resurrect/scripts/save.sh')),('@resurrect-restore-script-path',str(plugins/'tmux-resurrect/scripts/restore.sh')),('@resurrect-hook-post-save-all','touch '+shlex.quote(str(output/'save-done'))),('@continuum-restore','on')]:
        run('set-option','-g',key,value)
def canonical(text):
    rows=[]
    for row in text.splitlines():
        fields=row.split('|'); layout=fields.pop().split(',',1)[1]
        fields.append(re.sub(r'(\d+x\d+,\d+,\d+),\d+',r'\1',layout))
        rows.append(fields)
    return rows
try:
    run('-f', '/dev/null','new-session','-d','-s','restore-fixture','-n','first','-x','100','-y','30','-c',str(output),'/bin/bash --noprofile --norc')
    pid=run('display-message','-p','#{pid}')
    env['TMUX']=f'{socket},{pid},0'
    configure()
    run('set-option','-g','automatic-rename','off')
    run('split-window','-d','-h','-t','restore-fixture:0','-c',str(output),'/bin/bash --noprofile --norc')
    run('new-window','-d','-t','restore-fixture:1','-n','second','-c',str(output),'/bin/bash --noprofile --norc')
    run('select-pane','-t','restore-fixture:0.1','-T','saved-title')
    run('send-keys','-t','restore-fixture:0.0','sleep 300','C-m')
    for _ in range(100):
        if run('display-message','-pt','restore-fixture:0.0','#{pane_current_command}') == 'sleep': break
        time.sleep(.05)
    assert run('display-message','-pt','restore-fixture:0.0','#{pane_current_command}') == 'sleep'
    before=run('list-panes','-a','-F','#{session_name}:#{window_index}.#{pane_index}|#{pane_current_path}|#{pane_title}|#{window_layout}')
    if backend == 'continuum':
        plugin('tmux-continuum','continuum_save.sh')
    else:
        plugin('tmux-resurrect','save.sh','quiet')
    for _ in range(300):
        if (output/'save-done').exists(): break
        time.sleep(.05)
    assert (output/'save-done').exists(), 'save completion hook missing'
    saved=output/'save/last'
    assert saved.exists(), 'no saved snapshot'
    # The upstream ps strategy matches PID prefixes. Never replay a command
    # captured from another process when exercising a shared native host.
    for row in saved.read_text().splitlines():
        fields = row.split('\t')
        assert fields[0] in ('pane', 'window', 'state'), 'unexpected saved record'
        if fields[0] == 'pane':
            assert len(fields) == 11 and fields[-1] in (':', ':sleep 300'), 'foreign saved command'
    log.write(saved.read_text())
    if mode == 'kill':
        if sys.platform == 'darwin':
            def identity():
                return subprocess.check_output(['ps','-p',pid,'-o','lstart=','-o','command='])
            def alive():
                r=subprocess.run(['ps','-p',pid,'-o','stat='],capture_output=True,text=True)
                return r.returncode==0 and not r.stdout.strip().startswith('Z')
        else:
            proc=pathlib.Path('/proc')/pid
            def identity():
                return (proc/'stat').read_text().split()[21].encode()+b' '+(proc/'cmdline').read_bytes()
            def alive():
                return proc.exists() and (proc/'stat').read_text().split()[2] != 'Z'
        original=identity()
        assert str(socket).encode() in original and b'rmux' in original, original
        log.write(json.dumps(['fixture SIGKILL',pid,original.decode(errors='replace')])+'\n')
        assert identity() == original
        os.kill(int(pid),signal.SIGKILL)
        for _ in range(100):
            if not alive(): break
            time.sleep(.05)
        assert not alive()
    else:
        run('kill-server')
    for _ in range(100):
        if not socket.exists(): break
        time.sleep(.05)
    verify_transition(transition)
    binary=restore_binary
    set_wrapper()
    run('-f','/dev/null','new-session','-d','-s','0','-c',str(output),'/bin/bash --noprofile --norc')
    pid=run('display-message','-p','#{pid}')
    env['TMUX']=f'{socket},{pid},0'
    configure()
    if backend == 'continuum':
        plugin('tmux-continuum','continuum_restore.sh')
    else:
        plugin('tmux-resurrect','restore.sh')
    after=run('list-panes','-a','-F','#{session_name}:#{window_index}.#{pane_index}|#{pane_current_path}|#{pane_title}|#{window_layout}')
    assert canonical(before)==canonical(after), (before,after)
    for _ in range(100):
        if run('display-message','-pt','restore-fixture:0.0','#{pane_current_command}') == 'sleep': break
        time.sleep(.05)
    assert run('display-message','-pt','restore-fixture:0.0','#{pane_current_command}') == 'sleep', 'saved process was not restored'
    # Exercise the same layout importer used by resurrect, through a file.
    # The checksum is correct; the dimensions overflow internal arithmetic.
    body = '100x30,0,0{4294967295x30,0,0,0,50x30,50,0,1}'
    checksum = 0
    for byte in body.encode():
        checksum = ((checksum >> 1) + ((checksum & 1) << 15) + byte) & 65535
    corrupt = output / 'corrupt-layout.conf'
    corrupt.write_text("select-layout -t restore-fixture:0 '" + f'{checksum:04x},' + body + "'\n")
    previous = run('list-panes','-a','-F','#{session_name}:#{window_index}.#{pane_index}|#{pane_pid}|#{pane_current_path}|#{pane_current_command}|#{window_layout}')
    verify_transition(transition)
    result = subprocess.run([str(binary), '-S', str(socket), 'source-file', str(corrupt)], env=env, capture_output=True, text=True, timeout=30)
    log.write(json.dumps(['corrupt-layout-file', result.returncode, result.stdout, result.stderr])+'\n')
    assert result.returncode != 0 and 'layout' in result.stdout + result.stderr, 'corrupt layout must be diagnosed'
    assert run('list-panes','-a','-F','#{session_name}:#{window_index}.#{pane_index}|#{pane_pid}|#{pane_current_path}|#{pane_current_command}|#{window_layout}') == previous, 'corrupt layout mutated the restored session'
    result = {'mode':mode,'backend':backend,'transition':transition,'before':before,'after':after,'snapshot':saved.read_text()}
    verify_transition(transition)
    print(json.dumps(result,indent=2))
finally:
    try:
        run('kill-server',check=False)
    finally:
        log.close()
