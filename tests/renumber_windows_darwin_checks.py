#!/usr/bin/env python3
"""Pure sensitivity checks for native attribution, mapping and input evidence."""
import base64
import copy
import json
from pathlib import Path
import stat
import struct
import sys
sys.dont_write_bytecode = True
sys.path.insert(0,str(Path(__file__).resolve().parent))
import renumber_windows_darwin as A

def checks(config):
    A.bind_api(config); D = A.D
    expected = {'exe':'/python','exe_sha256':'pin','argv':['/python','-I','/driver','witness','a10','/markers','token','0'],
                'cwd':'/cwd','kind':'a10','token':'token','environment':{'HOME':'/private'}}
    def fake(pid,parent):
        own = {'pid':pid,'ppid':parent,'birth':[10,pid],'status':2}
        raw = struct.pack('=i',len(expected['argv']))+b'/python\0\0'+b'\0'.join(v.encode() for v in expected['argv'])+b'\0'
        return {'before':own,'after':dict(own),'image':{'executable':'/python','sha256':'pin'},'args':D.parse_procargs(raw),
                'cwd':{'path':'/cwd'},'environment':expected['environment'],'fd0':{'path':'/dev/ttys099','mode':stat.S_IFCHR}}
    child,daemon = fake(41,40),fake(40,1)
    public = {'daemon_pid':40,'socket':'/s','session_id':'$0','window_id':'@1','pane_id':'%1','pane_pid':41,
              'pane_tty':'/dev/ttys099','pane_dead':'0','pane_dead_status':'','pane_dead_signal':'',
              'session_name':'alpha','window_index':5,'window_name':'a10','window_width':'80','window_height':'24'}
    marker = {**expected,'argv':expected['argv'][2:],'identity':child['before'],'challenge':'fresh','exit_code':0,
              'tty':{'icanon':True,'icrnl':True},'received':[],'pending_bytes_base64':''}
    A.C.check_child(child,copy.deepcopy(child),public,daemon,expected); A.check_marker(marker,child,expected,'fresh')
    passed = ['positive_exact_child_marker']
    def reject(name,fn):
        try: fn()
        except (A.Unproven,A.Failed): passed.append(name)
        else: raise AssertionError('insensitive observer: '+name)
    for section,field,value in [('image','executable','/wrong'),('image','sha256','bad'),('args','argv',['wrong']),
                                ('args','argv_base64','AA=='),('cwd','path','/wrong'),('fd0','path','/pipe'),('before','ppid',99),('before','birth',[0,1])]:
        bad = copy.deepcopy(child); bad[section][field] = value
        reject('child_'+section+'_'+field,lambda bad=bad:A.C.check_child(bad,bad,public,daemon,expected))
    changed = copy.deepcopy(child); changed['before']['birth'] = [12,41]; changed['after']['birth'] = [12,41]
    reject('old_birth_replaced',lambda:A.require(D.stable(child,changed),'old birth replaced'))
    for field,value in [('identity',{'pid':41,'birth':[0,1]}),('challenge','stale'),('kind','a8'),('token','old'),
                        ('argv',['wrong']),('environment',{}),('exit_code',7),('tty',{'icanon':False,'icrnl':True})]:
        reject('marker_'+field,lambda field=field,value=value:A.check_marker({**marker,field:value},child,expected,'fresh'))
    children = {'a8':{'public':{**public,'window_name':'a8','window_id':'@0','pane_id':'%0','pane_pid':42,'window_index':4}},
                'a10':{'public':public},'b4':{'public':{**public,'window_name':'b4','session_name':'beta','session_id':'$1','window_id':'@2','pane_id':'%2','pane_pid':43,'window_index':4}}}
    rows = [v['public'] for v in children.values()]
    def windows(panes): return [{**{k:r[k] for k in A.WINDOW_FIELDS if k!='window_panes'},'window_panes':1} for r in panes]
    listed = windows(rows); mapping = A.check_layout(rows,listed,children); A.check_mapping(mapping,copy.deepcopy(mapping)); passed.append('positive_layout_and_mapping')
    for name,bad in [('missing_pane',rows[:-1]),('extra_pane',rows+[rows[0]]),('duplicate_ID',[rows[0],rows[0],rows[2]]),
                     ('wrong_target_PID',[rows[0],{**rows[1],'pane_pid':99},rows[2]]),
                     ('wrong_geometry',[rows[0],{**rows[1],'window_height':'23'},rows[2]]),
                     ('beta_renumbered',[rows[0],rows[1],{**rows[2],'window_index':5}])]:
        reject(name,lambda bad=bad:A.check_layout(bad,windows(bad),children))
    reject('missing_window',lambda:A.check_layout(rows,listed[:-1],children))
    reject('extra_window',lambda:A.check_layout(rows,listed+[listed[0]],children))
    reject('duplicate_index',lambda:A.check_layout([rows[0],{**rows[1],'window_index':4},rows[2]],windows([rows[0],{**rows[1],'window_index':4},rows[2]]),children))
    wrong = copy.deepcopy(mapping); wrong[1]['window_index'] = 8
    reject('wrong_intermediate_oracle_index',lambda:A.check_mapping(wrong,mapping))
    reject('wrong_survivor_order',lambda:A.check_mapping(list(reversed(mapping)),mapping))
    reject('unintended_immediate_reindex',lambda:A.check_mapping([dict(r,window_index=r['window_index']+1) for r in mapping],mapping))
    nonce = 'a'*48; before = {'a8':[],'a10':[],'b4':[]}; after = {**before,'a10':[nonce]}
    A.check_delivery(before,after,'a10',nonce); passed.append('positive_exclusive_input')
    reject('wrong_input_target',lambda:A.check_delivery(before,{**before,'a8':[nonce]},'a10',nonce))
    reject('duplicated_input',lambda:A.check_delivery(before,{**after,'b4':[nonce]},'a10',nonce))
    reject('wrong_input_nonce',lambda:A.check_delivery(before,{**before,'a10':['b'*48]},'a10',nonce))
    reject('stale_input_nonce',lambda:A.check_delivery(after,after,'a10',nonce))
    receipt = {**marker,'received':[nonce],'stdin_bytes_base64':base64.b64encode((nonce+'\n').encode()).decode()}
    A.check_stdin(receipt,nonce,[nonce]); passed.append('positive_raw_stdin')
    reject('stdin_wrong_bytes',lambda:A.check_stdin(receipt,'b'*48,[nonce]))
    reject('stdin_partial',lambda:A.check_stdin({**receipt,'pending_bytes_base64':'YQ=='},nonce,[nonce]))
    reject('stdin_history',lambda:A.check_stdin({**receipt,'received':[]},nonce,[nonce]))
    shared = {'after':{'a10':[nonce]},'receipt':{'nested':[{'received':[nonce]}]},'mapping':copy.deepcopy(mapping)}
    frozen = A.saved(shared); original = A.saved(shared)
    shared['after']['a12'] = []; shared['after']['a10'].append('later'); shared['receipt']['nested'][0]['received'].append('later'); shared['mapping'][0]['window_index'] = 99
    assert frozen == original and frozen != shared; passed.append('deep_snapshot_nested_receipts_and_mapping')
    for name,record in [('command_count',{'commands':[{}]*218}),('text_reserve',{'commands':[{'stdout':'x'*(A.TEXT_CAP-2*A.FILE_CAP+1)}]}),
                        ('record_reserve',{'commands':[],'large':'x'*(A.RECORD_CAP-96*1024)})]:
        reject(name,lambda record=record:A.command_budget(record,A.TEXT_CAP))
    clean = {'pass':True,'all_absent':True,'socket_absent':True,'fds_equal':True,'errors':[],'rescues':[],
             'processes':[{'before':child['before'],'after':None,'exit_event':True}]}; owned = {41:child['before']}
    A.check_cleanup(clean,owned); passed.append('positive_cleanup')
    for field,value in [('pass',False),('all_absent',False),('socket_absent',False),('fds_equal',False),('errors',['error']),('rescues',['kill']),('processes',[])]:
        reject('cleanup_'+field,lambda field=field,value=value:A.check_cleanup({**clean,field:value},owned))
    reject('cleanup_missing_NOTE_EXIT',lambda:A.check_cleanup({**clean,'processes':[{'before':child['before'],'after':None,'exit_event':False}]},owned))
    reject('cleanup_live_birth',lambda:A.check_cleanup({**clean,'processes':[{'before':child['before'],'after':child['before'],'exit_event':True}]},owned))
    ex = {'before':child['before'],'after':None,'exit_event':True,'numeric_status_claimed':False}; A.check_exit(ex)
    reject('closure_missing_NOTE_EXIT',lambda:A.check_exit({**ex,'exit_event':False}))
    reject('closure_live_birth',lambda:A.check_exit({**ex,'after':child['before']}))
    reject('closure_fabricated_numeric_status',lambda:A.check_exit({**ex,'numeric_status_claimed':True}))
    bad = copy.deepcopy(config); bad['adapter_pin']['sha256'] = 'wrong'; reject('adapter_provenance',lambda:A.bind_api(bad))
    path = Path(config['config_path']).parent/'PURE-MUST-NOT-EXIST.json'
    reject('marker_cap_before_open',lambda:A.write_once(path,'x'*A.FILE_CAP,A.FILE_CAP)); assert not path.exists()
    for phase in ('oracle','candidate'):
        assert len(A.plan(config,phase)) == 7 and len(A.STAGES) == 9
        assert A.environment(config,phase)['PATH']=='/usr/bin:/bin' and A.environment(config,phase)['RMUX_DISABLE_TMUX_SHIM']=='1'
    return {'at':A.now(),'status':'PASS','checks':passed,'driver_sha256':A.digest(A.__file__),'product_runs':0}

if __name__=='__main__':
    result = checks(json.loads(Path(sys.argv[1]).read_text())); A.write_once(sys.argv[2],result); print(json.dumps(result))
