#!/usr/bin/env python3
"""Pure sensitivity controls for exit-empty identity, lifetime and stale socket evidence."""
import copy
import errno
import json
import os
from pathlib import Path
import stat
import struct
import sys
sys.dont_write_bytecode = True
sys.path.insert(0,str(Path(__file__).resolve().parent))
import exit_empty_darwin as A

def checks(config):
    A.bind_api(config); D=A.D
    expected={'exe':'/python','exe_sha256':'pin','argv':['/python','-I','/driver','witness','alpha','/markers','token','0'],
              'cwd':'/cwd','kind':'alpha','token':'token','environment':{'HOME':'/private'}}
    def fake(pid,parent):
        own={'pid':pid,'ppid':parent,'birth':[10,pid],'status':2}
        raw=struct.pack('=i',len(expected['argv']))+b'/python\0\0'+b'\0'.join(v.encode() for v in expected['argv'])+b'\0'
        return {'before':own,'after':dict(own),'image':{'executable':'/python','sha256':'pin'},'args':D.parse_procargs(raw),
                'cwd':{'path':'/cwd'},'environment':expected['environment'],'fd0':{'path':'/dev/ttys099','mode':stat.S_IFCHR}}
    child,daemon=fake(41,40),fake(40,1)
    public={'daemon_pid':40,'pane_pid':41,'pane_tty':'/dev/ttys099'}
    marker={**expected,'argv':expected['argv'][2:],'identity':child['before'],'challenge':'a'*48,'exit_code':0}
    A.C.check_child(child,copy.deepcopy(child),public,daemon,expected); A.C.check_marker(marker,child,expected,'a'*48)
    passed=['positive_child_marker']
    def reject(name,fn):
        try: fn()
        except (A.Unproven,A.Failed): passed.append(name)
        else: raise AssertionError('insensitive observer: '+name)
    for section,field,value in [('image','executable','/wrong'),('image','sha256','bad'),('args','argv',['wrong']),
                                ('args','argv_base64','AA=='),('cwd','path','/wrong'),('fd0','path','/pipe'),('before','ppid',99),('before','birth',[0,1])]:
        bad=copy.deepcopy(child); bad[section][field]=value
        reject('child_'+section+'_'+field,lambda bad=bad:A.C.check_child(bad,bad,public,daemon,expected))
    for field,value in [('identity',{'pid':41,'birth':[0,1]}),('challenge','stale'),('kind','beta'),('token','old'),('argv',['wrong']),('environment',{})]:
        reject('marker_'+field,lambda field=field,value=value:A.C.check_marker({**marker,field:value},child,expected,'a'*48))
    changed=copy.deepcopy(child); changed['before']['birth']=[12,41]; changed['after']['birth']=[12,41]
    reject('old_child_generation_replaced',lambda:A.require(D.stable(child,changed),'replaced'))
    endpoint={'path':'/s','device':1,'inode':2,'uid':os.getuid()}
    sample={'daemon':daemon,'daemon_after':copy.deepcopy(daemon),'endpoint':endpoint,'sessions':'','option':'off','exit_event':False}
    A.check_empty([sample,copy.deepcopy(sample)],daemon,endpoint); passed.append('positive_same_empty_daemon')
    reject('empty_one_sample',lambda:A.check_empty([sample],daemon,endpoint))
    for field,value in [('endpoint',{**endpoint,'inode':3}),('sessions','$1|beta'),('option','on'),('exit_event',True)]:
        reject('empty_'+field,lambda field=field,value=value:A.check_empty([sample,{**sample,field:value}],daemon,endpoint))
    changed=copy.deepcopy(daemon); changed['before']['birth']=[20,40]; changed['after']['birth']=[20,40]
    reject('empty_generation_changed',lambda:A.check_empty([sample,{**sample,'daemon':changed}],daemon,endpoint))
    reject('empty_generation_changed_during_query',lambda:A.check_empty([sample,{**sample,'daemon_after':changed}],daemon,endpoint))
    probe={'path_state':endpoint,'accepting':False,'errno':errno.ECONNREFUSED}
    A.check_probe(probe); A.check_probe({**probe,'errno':errno.ENOENT}); passed.append('positive_refusal_and_absence')
    for name,bad in [('accepting',{**probe,'accepting':True}),('timeout',{**probe,'errno':None}),('timedout',{**probe,'errno':errno.ETIMEDOUT}),('permission',{**probe,'errno':errno.EACCES})]:
        reject('probe_'+name,lambda bad=bad:A.check_probe(bad))
    identities=[{'before':daemon['before'],'after':None,'exit_event':True},{'before':child['before'],'after':None,'exit_event':True}]
    A.check_stale_removal(endpoint,endpoint,identities,probe); passed.append('positive_owned_stale_removal')
    for name,ep,cur,ids,pr in [('unknown_endpoint',None,endpoint,identities,probe),('changed_inode',endpoint,{**endpoint,'inode':8},identities,probe),
                             ('missing_identities',endpoint,endpoint,[],probe),('live_child',endpoint,endpoint,[identities[0],{**identities[1],'after':child['before']}],probe),
                             ('missing_NOTE_EXIT',endpoint,endpoint,[{**identities[0],'exit_event':False},identities[1]],probe),
                             ('stale_probe_inode',endpoint,endpoint,identities,{**probe,'path_state':None})]:
        reject('unlink_'+name,lambda ep=ep,cur=cur,ids=ids,pr=pr:A.check_stale_removal(ep,cur,ids,pr))
    other={**endpoint,'uid':os.getuid()+1}
    reject('unlink_other_uid',lambda:A.check_stale_removal(other,other,identities,{**probe,'path_state':other}))
    clean={'pass':True,'all_absent':True,'socket_absent':True,'fds_equal':True,'errors':[],'rescues':[],'processes':identities}
    owned=[daemon['before'],child['before']]; A.check_cleanup(clean,owned); passed.append('positive_cleanup_complete')
    for field,value in [('pass',False),('all_absent',False),('socket_absent',False),('fds_equal',False),('errors',['error']),('rescues',['kill']),('processes',identities[:1])]:
        reject('cleanup_'+field,lambda field=field,value=value:A.check_cleanup({**clean,field:value},owned))
    reject('cleanup_duplicate_birth',lambda:A.check_cleanup({**clean,'processes':identities+[identities[0]]},owned))
    reject('cleanup_missing_NOTE_EXIT',lambda:A.check_cleanup({**clean,'processes':[{**identities[0],'exit_event':False},identities[1]]},owned))
    reject('cleanup_live_birth',lambda:A.check_cleanup({**clean,'processes':[identities[0],{**identities[1],'after':child['before']}]},owned))
    ex={**identities[1],'numeric_status_claimed':False}; A.check_exit(ex)
    for field,value in [('exit_event',False),('after',child['before']),('numeric_status_claimed',True)]:
        reject('closure_'+field,lambda field=field,value=value:A.check_exit({**ex,field:value}))
    source={'empty':[copy.deepcopy(sample)],'child':copy.deepcopy(child),'probe':copy.deepcopy(probe)}
    frozen=A.saved(source); original=A.saved(source)
    source['empty'][0]['daemon']['before']['birth'][0]=999; source['child']['args']['argv'].append('later'); source['probe']['path_state']['inode']=99
    assert frozen==original and frozen!=source; passed.append('deep_nested_snapshot')
    for name,record in [('command_count',{'generations':[{'commands':[{}]*64}]}),('text_reserve',{'generations':[{'commands':[{'stdout':'x'*(128*1024)}]}]}),
                        ('record_reserve',{'generations':[{'commands':[]}],'large':'x'*A.RECORD_CAP})]:
        reject(name,lambda record=record:A.command_budget(record))
    path=Path(config['config_path']).parent/'PURE-MUST-NOT-EXIST.json'
    reject('file_cap_before_open',lambda:A.write_once(path,'x'*A.FILE_CAP,A.FILE_CAP)); assert not path.exists()
    bad=copy.deepcopy(config); bad['adapter_pin']['sha256']='bad'; reject('adapter_provenance',lambda:A.bind_api(bad))
    for phase in ('oracle','candidate'):
        for label in A.KINDS:
            argv=A.spawn_plan(config,phase,label); assert argv.count('new-session')==1 and '-c' in argv
            root=A.root_for(config,phase,A.generation(label)); assert len(os.fsencode(str(root/'s')))<104
        assert A.root_for(config,phase,'reuse')!=A.root_for(config,phase,'default')
        for gen in A.GENERATIONS: assert A.environment(config,phase,gen)['RMUX_DISABLE_TMUX_SHIM']=='1'
    passed.append('private_plan_socket_lengths')
    return {'at':A.now(),'status':'PASS','checks':passed,'driver_sha256':A.digest(A.__file__),'product_runs':0}

if __name__=='__main__':
    result=checks(json.loads(Path(sys.argv[1]).read_text())); A.write_once(sys.argv[2],result); print(json.dumps(result))
