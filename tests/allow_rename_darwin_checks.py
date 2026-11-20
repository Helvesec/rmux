#!/usr/bin/env python3
"""Pure allow-rename controls; no bind_api, process query or product run."""
import copy
import json
from pathlib import Path
import sys
import types
sys.dont_write_bytecode=True
sys.path.insert(0,str(Path(__file__).resolve().parent))
import allow_rename_darwin as A

def codec_checks():
    """Codec-only controls: no config, acquired API, FFI or native process.

    Run directly with --codec-only. Historical codec records are checked separately
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

def checks():
    passed=codec_checks()['checks']
    def reject(name,fn):
        try: fn()
        except (A.Unproven,A.Failed,A.RuntimeMismatch): passed.append(name)
        else: raise AssertionError(name+' accepted')
    nonce='a'*48; name='L5_Unique_Name'; payload=A.emission_bytes(name,nonce)
    assert payload==b'\x1bkL5_Unique_Name\x1b\\\r\nL5-'+nonce.encode()+b'\r\n'; passed.append('exact_ESCk_ST_and_processing_marker')
    for bad in ['', 'x\x1b\\evil','x\n','a'*49,'é',7]: reject('unsafe_name_'+repr(bad),lambda bad=bad:A.emission_bytes(bad,nonce))
    for bad in ['a'*47,'a'*49,'g'*48]: reject('invalid_nonce_'+bad[:5]+str(len(bad)),lambda bad=bad:A.emission_bytes(name,bad))
    request={'name':name,'challenge':nonce}; public={'pane_tty':'/dev/ttys123'}
    ack={'pty':'/dev/ttys123','emission':{'name':name,'bytes_hex':payload.hex(),'bytes_written':len(payload),'marker':'L5-'+nonce}}
    A.check_emission(ack,request,public,'L5-'+nonce+'\n'); passed.append('valid_consumption_attribution')
    for field,value in [('name','wrong'),('bytes_hex','00'),('bytes_written',0),('marker','L5-'+'b'*48)]:
        mutant=copy.deepcopy(ack); mutant['emission'][field]=value; reject('emission_'+field,lambda mutant=mutant:A.check_emission(mutant,request,public,'L5-'+nonce))
    mutant=copy.deepcopy(ack); mutant['pty']='/dev/ttys999'; reject('wrong_output_PTY',lambda:A.check_emission(mutant,request,public,'L5-'+nonce))
    reject('missing_terminal_processing_marker',lambda:A.check_emission(ack,request,public,'not processed'))
    before={'session_id':'$0','window_id':'@0','pane_id':'%0','pane_pid':41,'pane_tty':'/dev/ttys123','window_name':'alpha'}
    after={**before,'window_name':name}; A.stable_public(before,after); passed.append('rename_preserves_public_identity')
    for field,value in [('session_id','$1'),('window_id','@1'),('pane_id','%1'),('pane_pid',42),('pane_tty','/dev/ttys999')]:
        mutant={**after,field:value}; reject('public_'+field,lambda mutant=mutant:A.stable_public(before,mutant))
    def fake():
        run=A.Run.__new__(A.Run); run.phase='candidate'; run.config={'names':{'candidate':{'R2':'fresh_alpha','R3on':'fresh_beta'}}}; run.targets={'alpha':'%0','beta':'%1'}
        run.children={k:{'public':{'session_id':'$0','window_id':'@'+str(i),'pane_id':'%'+str(i),'window_name':k}} for i,k in enumerate(A.KINDS)}
        run.record={'states':[]}; run.actual_names={'alpha':'fresh_alpha','beta':'beta'}
        run.actual_options={k:{'window_local':v,'pane_local':'','effective':v,'automatic_rename':'off'} for k,v in [('alpha','on'),('beta','off')]}
        run.oracle=[{'case':'R2','point':'after','names':{'alpha':'R2','beta':'initial_beta'},'options':copy.deepcopy(run.actual_options)}]
        run.options=lambda:copy.deepcopy(run.actual_options)
        run.public=lambda target:{'public':{**run.children[next(k for k,v in run.targets.items() if v==target)]['public'],'window_name':run.actual_names[next(k for k,v in run.targets.items() if v==target)]}}
        run.remaining=lambda:1
        return run
    run=fake(); run.state('R2','after',{'alpha':'on','beta':'off'}); passed.append('actual_state_matches_frozen_oracle')
    for label,value in [('alpha','alpha'),('beta','fresh_beta')]:
        run=fake(); run.actual_names[label]=value; reject('rename_or_neighbor_mismatch_'+label,lambda run=run:run.state('R2','after',{'alpha':'on','beta':'off'})); assert 'mismatch' in run.record
    for field,value in [('window_local','off'),('pane_local','on'),('effective','off'),('automatic_rename','on')]:
        run=fake(); run.actual_options['alpha'][field]=value; reject('scope_mismatch_'+field,lambda run=run:run.state('R2','after',{'alpha':'on','beta':'off'}))
    run=fake(); run.state('R2','after',{'alpha':'on','beta':'off'}); run.actual_options['alpha']['effective']='off'; assert run.record['states'][0]['options']['alpha']['effective']=='on'; passed.append('state_snapshot_no_alias')
    for path in (Path(A.__file__),Path(__file__)): compile(path.read_text(),str(path),'exec')
    return {'status':'PASS','checks':passed,'driver_sha256':A.digest(A.__file__),'product_runs':0,'PIDquery':0,'new_FFI':0}

if __name__=='__main__':
    result=checks(); A.write_once(sys.argv[1],result); print(json.dumps({'status':result['status'],'checks':len(result['checks'])}))
