#!/usr/bin/env python3
"""Pure sensitivity controls; no native product process or host mutation."""
import base64
import copy
import json
from pathlib import Path
import stat
import struct
import sys
sys.dont_write_bytecode = True
sys.path.insert(0, str(Path(__file__).resolve().parent))
import pane_base_index_darwin as A

def checks(config):
    A.bind_api(config); D = A.D
    expected = {'exe': '/python', 'exe_sha256': 'pin', 'argv': ['/python', '-I', '/driver', 'witness', 'alpha2', '/markers', 'token', '0'],
                'cwd': '/cwd', 'kind': 'alpha2', 'token': 'token', 'environment': {'HOME': '/private'}}
    def fake(pid, parent):
        own = {'pid': pid, 'ppid': parent, 'birth': [10, pid], 'status': 2}
        raw = struct.pack('=i', len(expected['argv']))+b'/python\0\0'+b'\0'.join(v.encode() for v in expected['argv'])+b'\0'
        return {'before': own, 'after': dict(own), 'image': {'executable': '/python', 'sha256': 'pin'},
                'args': D.parse_procargs(raw), 'cwd': {'path': '/cwd'}, 'environment': expected['environment'],
                'fd0': {'path': '/dev/ttys099', 'mode': stat.S_IFCHR}}
    child, daemon = fake(41, 40), fake(40, 1)
    public = dict(zip(A.FIELDS, ['40', '/s', '$0', '@0', '%1', '41', '/dev/ttys099', '0', '', '', '5', '0', '80', '24']))
    public.update(daemon_pid=40, pane_pid=41)
    marker = {**expected, 'argv': expected['argv'][2:], 'identity': child['before'], 'challenge': 'fresh',
              'exit_code': 0, 'tty': {'icanon': True, 'icrnl': True}, 'received': [], 'pending_bytes_base64': ''}
    A.C.check_child(child, copy.deepcopy(child), public, daemon, expected); A.check_marker(marker, child, expected, 'fresh')
    passed = ['positive_exact_child_and_marker']
    def rejected(name, fn):
        try: fn()
        except (A.Unproven, A.Failed): passed.append(name)
        else: raise AssertionError('insensitive canary: '+name)
    for section, field, value in [('image','executable','/wrong'), ('image','sha256','wrong'), ('args','argv',['bad']),
                                  ('args','argv_base64','AA=='), ('cwd','path','/wrong'), ('fd0','path','/pipe'),
                                  ('before','ppid',99), ('before','birth',[0,1])]:
        bad = copy.deepcopy(child); bad[section][field] = value
        rejected('child_'+section+'_'+field, lambda bad=bad: A.C.check_child(bad, bad, public, daemon, expected))
    changed = copy.deepcopy(child); changed['before']['birth'] = [12,41]; changed['after']['birth'] = [12,41]
    rejected('old_child_replaced', lambda: A.require(D.stable(child, changed), 'old birth'))
    for field, value in [('identity', {'pid':41,'birth':[0,1]}), ('challenge','old'), ('kind','alpha1'), ('token','old'),
                         ('argv',['wrong']), ('environment',{}), ('exit_code',7), ('tty',{'icanon':False,'icrnl':True})]:
        rejected('marker_'+field, lambda field=field, value=value: A.check_marker({**marker,field:value}, child, expected, 'fresh'))
    children = {'alpha1': {'public': {**public,'pane_id':'%0','pane_pid':42,'pane_index':'4'}}, 'alpha2': {'public':public}}
    rows = [v['public'] for v in children.values()]
    A.check_indices(rows, children, A.EXPECTED['P1']); passed.append('positive_indices')
    for name, bad in [('wrong_index',[rows[0],{**rows[1],'pane_index':'8'}]),
                      ('wrong_target_pid',[rows[0],{**rows[1],'pane_pid':99}]),
                      ('duplicate_pane',[rows[0],rows[0]]), ('extra_pane',rows+[rows[0]]),
                      ('wrong_geometry',[rows[0],{**rows[1],'window_width':'79'}]),
                      ('extra_window',[rows[0],{**rows[1],'window_id':'@1'}])]:
        rejected(name, lambda bad=bad: A.check_indices(bad, children, A.EXPECTED['P1']))
    nonce = 'a'*48; before = {'alpha1': [], 'alpha2': []}; after = {'alpha1': [], 'alpha2':[nonce]}
    A.check_delivery(after,before,'alpha2',nonce); passed.append('positive_exclusive_delivery')
    rejected('input_wrong_target',lambda:A.check_delivery({'alpha1':[nonce],'alpha2':[]},before,'alpha2',nonce))
    rejected('input_both_targets',lambda:A.check_delivery({'alpha1':[nonce],'alpha2':[nonce]},before,'alpha2',nonce))
    rejected('input_wrong_nonce',lambda:A.check_delivery({'alpha1':[],'alpha2':['b'*48]},before,'alpha2',nonce))
    rejected('input_stale_nonce',lambda:A.check_delivery(after,after,'alpha2',nonce))
    receipt = {**marker,'received':[nonce],'stdin_bytes_base64':base64.b64encode((nonce+'\n').encode()).decode()}
    A.check_stdin(receipt,nonce,[nonce]); passed.append('positive_raw_stdin')
    rejected('stdin_nonce_bytes',lambda:A.check_stdin(receipt,'b'*48,[nonce]))
    rejected('stdin_partial',lambda:A.check_stdin({**receipt,'pending_bytes_base64':'YQ=='},nonce,[nonce]))
    rejected('stdin_receipt_history',lambda:A.check_stdin({**receipt,'received':[]},nonce,[nonce]))
    shared = {'alpha2':[nonce], 'receipts':{'phase':{'received':[nonce]}}}
    frozen = A.delivery_snapshot(shared); copy2 = A.delivery_snapshot(shared)
    shared['alpha2'].append('later'); shared['receipts']['phase']['received'].append('later'); shared['alpha3']=[]
    assert frozen == copy2 and 'alpha3' not in frozen and len(frozen['receipts']['phase']['received']) == 1
    passed.append('nested_receipt_snapshot_survives_later_mutation')
    for name, record in [('command_count',{'commands':[{}]*158}), ('stdout_reserve',{'commands':[{'stdout':'x'*(A.STDOUT_CAP-A.FILE_CAP+1)}]}),
                         ('record_reserve',{'commands':[],'large':'x'*(720*1024)})]:
        rejected(name,lambda record=record:A.command_budget(record))
    clean = {'pass':True,'all_absent':True,'socket_absent':True,'fds_equal':True,'errors':[],'rescues':[],
             'processes':[{'before':child['before'],'after':None,'exit_event':True}]}
    owned = {41:child['before']}; A.check_cleanup(clean,owned); passed.append('positive_cleanup')
    for field,value in [('pass',False),('all_absent',False),('socket_absent',False),('fds_equal',False),('errors',['e']),('rescues',['kill']),('processes',[])]:
        rejected('cleanup_'+field,lambda field=field,value=value:A.check_cleanup({**clean,field:value},owned))
    rejected('cleanup_missing_NOTE_EXIT',lambda:A.check_cleanup({**clean,'processes':[{'before':child['before'],'after':None,'exit_event':False}]},owned))
    rejected('cleanup_live_birth',lambda:A.check_cleanup({**clean,'processes':[{'before':child['before'],'after':child['before'],'exit_event':True}]},owned))
    ex = {'before':child['before'],'after':None,'exit_event':True,'numeric_status_claimed':False}; A.check_exit(ex)
    rejected('exit_missing_event',lambda:A.check_exit({**ex,'exit_event':False}))
    rejected('exit_live_birth',lambda:A.check_exit({**ex,'after':child['before']}))
    rejected('nonparent_exit_status',lambda:A.check_exit({**ex,'numeric_status_claimed':True}))
    bad = copy.deepcopy(config); bad['adapter_pin']['sha256'] = 'wrong'
    rejected('adapter_provenance',lambda:A.bind_api(bad))
    path = Path(config['config_path']).parent/'PURE-MUST-NOT-EXIST.json'
    rejected('marker_cap_before_open',lambda:A.write_once(path,'x'*A.FILE_CAP,A.FILE_CAP)); assert not path.exists()
    for phase in ('oracle','candidate'):
        assert len(A.plan(config,phase)) == 5
        assert A.environment(config,phase)['PATH'] == '/usr/bin:/bin'
        assert A.environment(config,phase)['RMUX_DISABLE_TMUX_SHIM'] == '1'
    return {'at':A.now(),'status':'PASS','checks':passed,'driver_sha256':A.digest(A.__file__),'product_runs':0}

if __name__ == '__main__':
    result = checks(json.loads(Path(sys.argv[1]).read_text())); A.write_once(sys.argv[2], result)
    print(json.dumps(result))
