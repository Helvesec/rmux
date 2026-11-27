#!/usr/bin/env python3
"""Pure identity, lifetime, mismatch stopping and evidence encoding controls."""
import copy
import errno
import json
import os
from pathlib import Path
import stat
import struct
import sys
sys.dont_write_bytecode=True
sys.path.insert(0,str(Path(__file__).resolve().parent))
import exit_unattached_darwin as A

def codec_checks():
    """Codec-only controls: no config, acquired API, FFI or native process.

    Run directly with --codec-only. Historical019 records are checked separately
    against their immutable byte pins and an independent reconstruction.
    """
    passed=[]
    def doc(values,root):
        return {'encoding':'shared-json-v1','root':root,'values':values}
    def reject(name,value):
        try: A.unpack(value)
        except (A.Unproven,A.Failed): passed.append(name)
        else: raise AssertionError(name+' accepted malformed shared JSON')
    # Exact R1-047-F1 examples: these previously decoded to x, payload and 7.
    reject('F1_values_string',doc('x',{'$ref':0}))
    reject('F1_values_object',doc({'payload':{}},{'$ref':0}))
    reject('F1_scalar_table_entry',doc([7],{'$ref':0}))
    for name,value in [('null',None),('bool',True),('number',7),('string','encoding'),('list',['encoding','root','values'])]:
        reject('envelope_'+name,value)
    reject('missing_key',{'encoding':'shared-json-v1','root':{}})
    reject('extra_key',{**doc([],{}),'extra':1})
    reject('wrong_encoding',{'encoding':'other','root':{},'values':[]})
    for name,value in [('null',None),('bool',False),('number',3)]:
        reject('values_'+name,doc(value,{}))
    for name,value in [('null',None),('bool',True),('string','x'),('float',1.5)]:
        reject('entry_'+name,doc([value],{'$ref':0}))
    for name,value in [('negative',-1),('dangling',1),('bool',True),('string','0'),('float',0.0),('null',None)]:
        reject('reference_'+name,doc([{}],{'$ref':value}))
    reject('empty_table_reference',doc([],{'$ref':0}))
    reject('self_cycle',doc([{'$ref':0}],{'$ref':0}))
    reject('forward_cycle',doc([{'$ref':1},{'$ref':0}],{'$ref':0}))
    reject('nested_self_reference',doc([{'child':[{'$ref':0}]}],{}))
    reject('nested_forward_reference',doc([{'child':{'$ref':1}},{}],{}))
    reject('invalid_unused_entry',doc([7],None))
    for value in [None,False,7,1.5,'x',[],{},[1,{'a':2}]]:
        assert A.unpack(A.pack(value))==value
    passed.append('scalar_and_inline_container_roots')
    table=doc([{'child':[1,2]},[{'$ref':0},{'$ref':0}],{'$ref':1}],{'left':{'$ref':2},'right':{'$ref':2}})
    snapshot=copy.deepcopy(table); decoded=A.unpack(table)
    assert decoded=={'left':[{'child':[1,2]},{'child':[1,2]}],'right':[{'child':[1,2]},{'child':[1,2]}]}
    decoded['left'][0]['child'].append(3)
    assert decoded['left'][1]['child']==[1,2] and decoded['right'][0]['child']==[1,2]
    assert table==snapshot
    passed.append('nested_backward_refs_aliases_and_deepcopy')
    assert A.unpack(doc([{},[]],[{'$ref':0},{'$ref':1}]))==[{},[]]
    passed.append('empty_container_entries')
    literal={'$ref':0,'sibling':'ordinary data'}
    assert A.unpack(A.pack(literal))==literal
    passed.append('ref_key_with_sibling_is_ordinary_data')
    sample={'nested':{'list':[1,2],'text':'x'*300}}
    original=[sample,copy.deepcopy(sample)]; packed=A.pack(original); restored=A.unpack(packed)
    assert restored==original
    restored[0]['nested']['list'].append(3)
    assert restored[1]['nested']['list']==[1,2] and original[0]['nested']['list']==[1,2]
    assert A.unpack(packed)==original
    passed.append('encoder_roundtrip_and_independent_copies')
    left={'a':'x'*300,'b':[1,2]}; right={'b':[1,2],'a':'x'*300}
    original=[left,right]; restored=A.unpack(A.pack(original))
    assert json.dumps(original,sort_keys=True)==json.dumps(restored,sort_keys=True)
    assert A.encoded(original)!=A.encoded(restored)
    passed.append('canonical_dictionary_equality_despite_insertion_order')
    return {'status':'PASS','checks':passed,'product_runs':0,'FFI':0,'PIDquery':0}

def checks(config):
    A.bind_api(config); D=A.D
    expected={'exe':'/python','exe_sha256':'pin','argv':['/python','-I','/driver','witness','alpha','/markers','token','0'],
              'cwd':'/cwd','kind':'alpha','token':'token','environment':{'HOME':'/private'}}
    def fake(pid,parent):
        own={'pid':pid,'ppid':parent,'birth':[10,pid],'status':2}
        raw=struct.pack('=i',len(expected['argv']))+b'/python\0\0'+b'\0'.join(v.encode() for v in expected['argv'])+b'\0'
        return {'at':'now','before':own,'after':dict(own),'image':{'executable':'/python','sha256':'pin'},'args':D.parse_procargs(raw),
                'cwd':{'path':'/cwd'},'environment':expected['environment'],'fd0':{'path':'/dev/ttys099','mode':stat.S_IFCHR}}
    child,daemon=fake(41,40),fake(40,1); public={'daemon_pid':40,'pane_pid':41,'pane_tty':'/dev/ttys099','session_id':'$0'}
    marker={**expected,'argv':expected['argv'][2:],'identity':child['before'],'challenge':'a'*48,'exit_code':0}
    A.C.check_child(child,copy.deepcopy(child),public,daemon,expected); A.C.check_marker(marker,child,expected,'a'*48)
    passed=['positive_child_marker']
    def reject(name,fn,errors=None):
        try: fn()
        except errors or (A.Unproven,A.Failed): passed.append(name)
        else: raise AssertionError('insensitive observer: '+name)
    for section,field,value in [('image','executable','/wrong'),('image','sha256','bad'),('args','argv',['wrong']),('args','argv_base64','AA=='),
                                ('cwd','path','/wrong'),('fd0','path','/pipe'),('before','ppid',99),('before','birth',[0,1])]:
        bad=copy.deepcopy(child); bad[section][field]=value
        reject('child_'+section+'_'+field,lambda bad=bad:A.C.check_child(bad,bad,public,daemon,expected))
    for field,value in [('identity',{'pid':41,'birth':[0,1]}),('challenge','stale'),('kind','beta'),('token','old'),('argv',['wrong']),('environment',{})]:
        reject('marker_'+field,lambda field=field,value=value:A.C.check_marker({**marker,field:value},child,expected,'a'*48))
    changed=copy.deepcopy(child); changed['before']['birth']=[12,41]; changed['after']['birth']=[12,41]
    reject('old_birth_replaced',lambda:A.require(D.stable(child,changed),'replaced'))
    live={'daemon':{'before':daemon['before'],'after':daemon['before'],'exit_event':False,'numeric_status_claimed':False},
          'helper':{'before':child['before'],'after':child['before'],'exit_event':False,'numeric_status_claimed':False}}
    dead={role:{**r,'after':None,'exit_event':True} for role,r in live.items()}
    assert A.classify(live,20)=='survived_20s' and A.classify(dead,.01)=='exited'; passed.append('positive20s_survival_and_attributed_exit')
    reject('short_window_not_survival',lambda:A.classify(live,19.99))
    reject('missing_effect_population',lambda:A.classify({'daemon':live['daemon']},20))
    mixed={'daemon':dead['daemon'],'helper':live['helper']}; assert A.classify(mixed,20)=='mixed_at_20s'; passed.append('mixed_not_complete_exit')
    reject('short_mixed_window',lambda:A.classify(mixed,1))
    unproved={role:{**r,'exit_event':False} for role,r in dead.items()}; assert A.classify(unproved,20)=='mixed_at_20s'; passed.append('absence_without_NOTE_EXIT_not_exit')
    still_present={role:{**r,'exit_event':True} for role,r in live.items()}; assert A.classify(still_present,20)=='mixed_at_20s'; passed.append('event_without_absence_not_exit')
    for field,value in [('exit_event',False),('after',child['before']),('numeric_status_claimed',True)]:
        reject('exit_'+field,lambda field=field,value=value:A.check_exit({**dead['helper'],field:value}))
    projected={'generation':'A','before_options':{'exit-empty':'on','exit-unattached':'off'},'mutation':{'args':['set-option','-s','exit-unattached','on'],'returncode':0,'stdout':'','stderr':''},
               'lifecycle':'exited','daemon_exit_event':True,'helper_exit_event':True,'service':'absent','after_options':None}
    A.compare(projected,copy.deepcopy(projected)); passed.append('positive_exact_oracle_projection')
    for field,value in [('lifecycle','survived_20s'),('daemon_exit_event',False),('helper_exit_event',False),('service','responding'),('before_options',{'exit-empty':'off','exit-unattached':'off'})]:
        reject('oracle_mismatch_'+field,lambda field=field,value=value:A.compare({**projected,field:value},projected),(A.RuntimeMismatch,))
    for field,value in [('returncode',1),('stdout','changed'),('stderr','changed'),('args',['wrong'])]:
        bad=copy.deepcopy(projected); bad['mutation'][field]=value
        reject('mutation_'+field,lambda bad=bad:A.compare(bad,projected),(A.RuntimeMismatch,))
    created=[]; real_gen=A.Gen
    class FakeGen:
        def __init__(self,lot,name): self.gen=name; self.record={'projection':{**projected,'lifecycle':'survived_20s'}}; created.append(name)
        def prepare(self): pass
        def mutate(self): pass
        def effect(self): pass
        def post_observe(self): pass
    lot=object.__new__(A.Lot); lot.gens={}; lot.phase='candidate'; lot.frozen={'oracle_projection':{'A':projected,'B':projected}}; lot.record={}
    lot.transition=lambda g,name,fn:fn(); lot.checkpoint=lambda:None
    try:
        A.Gen=FakeGen; reject('first_mismatch_terminates',lot.execute,(A.RuntimeMismatch,)); assert created==['A'] and lot.record['mismatch']['generation']=='A'
    finally: A.Gen=real_gen
    passed.append('generationB_never_created_after_A_mismatch')
    endpoint={'path':'/s','device':1,'inode':2,'uid':os.getuid()}; identities=list(dead.values())
    probe={'path_state':endpoint,'accepting':False,'errno':errno.ECONNREFUSED}; A.check_probe(probe); A.check_probe({**probe,'errno':errno.ENOENT})
    A.check_unlink(endpoint,endpoint,identities,probe); passed.append('positive_owned_stale_unlink')
    for name,bad in [('accepting',{**probe,'accepting':True}),('timeout',{**probe,'errno':None}),('timedout',{**probe,'errno':errno.ETIMEDOUT}),('permission',{**probe,'errno':errno.EACCES})]:
        reject('probe_'+name,lambda bad=bad:A.check_probe(bad))
    for name,ep,cur,ids,pr in [('unknown_endpoint',None,endpoint,identities,probe),('changed_inode',endpoint,{**endpoint,'inode':8},identities,probe),
                             ('missing_identities',endpoint,endpoint,[],probe),('live_helper',endpoint,endpoint,[dead['daemon'],live['helper']],probe),
                             ('missing_NOTE_EXIT',endpoint,endpoint,[{**dead['daemon'],'exit_event':False},dead['helper']],probe),
                             ('stale_probe_inode',endpoint,endpoint,identities,{**probe,'path_state':None}),('ENOENT_not_stale_refusal',endpoint,endpoint,identities,{**probe,'errno':errno.ENOENT})]:
        reject('unlink_'+name,lambda ep=ep,cur=cur,ids=ids,pr=pr:A.check_unlink(ep,cur,ids,pr))
    other={**endpoint,'uid':os.getuid()+1}; reject('unlink_other_uid',lambda:A.check_unlink(other,other,identities,{**probe,'path_state':other}))
    clean={'pass':True,'all_absent':True,'socket_absent':True,'fds_equal':True,'errors':[],'rescues':[],'processes':identities}; owned=[daemon['before'],child['before']]
    A.check_cleanup(clean,owned); passed.append('positive_cleanup')
    for field,value in [('pass',False),('all_absent',False),('socket_absent',False),('fds_equal',False),('errors',['error']),('rescues',['kill']),('processes',identities[:1])]:
        reject('cleanup_'+field,lambda field=field,value=value:A.check_cleanup({**clean,field:value},owned))
    reject('cleanup_duplicate_birth',lambda:A.check_cleanup({**clean,'processes':identities+[identities[0]]},owned))
    raw={'effect':copy.deepcopy(live),'cleanup':copy.deepcopy(dead),'snapshots':[copy.deepcopy(child)]*4,'nonce':'a'*48,'polls':[[0.0,False,False],[20.001,False,False]]}
    original=copy.deepcopy(raw); document=A.pack(raw); decoded=A.unpack(document); assert decoded==raw and len(A.encoded(document))<len(A.encoded(raw))
    decoded['cleanup']['helper']['exit_event']=False; decoded['snapshots'][0]['before']['birth'][0]=999; raw['effect']['helper']['exit_event']=True
    assert A.unpack(document)==original and decoded['effect']['helper']['exit_event'] is False
    assert decoded['snapshots'][1]['before']['birth']==original['snapshots'][1]['before']['birth']; passed.append('lossless_all_polls_snapshots_and_cause_separation')
    reject('sharing_reserved_key',lambda:A.pack({'$ref':0})); reject('sharing_bad_encoding',lambda:A.unpack({**document,'encoding':'unknown'}))
    reject('sharing_dangling_ref',lambda:A.unpack({'encoding':'shared-json-v1','root':{'$ref':0},'values':[]}))
    reject('sharing_cycle',lambda:A.unpack({'encoding':'shared-json-v1','root':{'$ref':0},'values':[{'$ref':0}]}))
    for name,record in [('command_count',{'generations':[{'commands':[{}]*64}]}),('text_reserve',{'generations':[{'commands':[{'stdout':'x'*(64*1024)}]}]}),
                        ('raw_memory_bound',{'generations':[{'commands':[]}],'large':'x'*(1024*1024)}),('shared_record_reserve',{'generations':[{'commands':[]}],'large':'x'*A.RECORD_CAP})]:
        reject(name,lambda record=record:A.check_budget(record))
    path=Path(config['config_path']).parent/'PURE-MUST-NOT-EXIST.json'
    reject('file_cap_before_open',lambda:A.write_once(path,'x'*A.FILE_CAP,A.FILE_CAP)); assert not path.exists()
    bad=copy.deepcopy(config); bad['adapter_pin']['sha256']='bad'; reject('adapter_provenance',lambda:A.bind_api(bad))
    for phase in ('oracle','candidate'):
        for gen in A.GENERATIONS:
            assert len(str(A.root_for(config,phase,gen)/'s').encode())<104
            assert A.spawn_plan(config,phase,gen).count('new-session')==1
            assert A.environment(config,phase,gen)['RMUX_DISABLE_TMUX_SHIM']=='1'
    passed.append('private_two_generation_plan')
    return {'at':A.now(),'status':'PASS','checks':passed,'driver_sha256':A.digest(A.__file__),'product_runs':0}

if __name__=='__main__':
    if sys.argv[1:]==['--codec-only']:
        print(json.dumps(codec_checks()))
    else:
        result=checks(json.loads(Path(sys.argv[1]).read_text())); A.write_once(sys.argv[2],result); print(json.dumps(result))
