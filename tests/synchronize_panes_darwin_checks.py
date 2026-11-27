#!/usr/bin/env python3
"""Pure parser/prefix/identity/ordering/sentinel checks. No API bind or native."""
import copy
import json
from pathlib import Path
import sys
sys.dont_write_bytecode=True
sys.path.insert(0,str(Path(__file__).resolve().parent))
import synchronize_panes_darwin as A

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
    passed=codec_checks()['checks']; token='a'*48; nonces={p:format(i,'048x') for i,p in enumerate((*A.CASES,'SA','SB'),1)}
    own={'pid':41,'birth':[10,20],'ppid':40,'pgid':40,'status':2}; other={**own,'pid':42}
    def reject(name,fn):
        try: fn()
        except (A.Unproven,A.Failed): passed.append(name)
        else: raise AssertionError(name+' accepted')
    def row(phase,counter=1,previous=None,label='alpha',identity=None):
        return {'at':'frozen','identity':copy.deepcopy(identity or own),'label':label,'token':token,'phase':phase,'nonce':nonces[phase],'counter':counter,
                'line_hex':A.wire_line(token,phase,nonces[phase]).hex(),'previous_sha256':A.receipt_sha(previous) if previous else None}
    line=A.wire_line(token,'P1',nonces['P1']); assert A.parse_line(line,token,nonces)==('P1',nonces['P1']); passed.append('complete_line_roundtrip')
    for name,raw in [('partial',line[:-1]),('duplicate_line',line+line),('carriage',line[:-1]+b'\r\n'),('wrong_token',line.replace(token.encode(),b'b'*48)),('wrong_phase',line.replace(b'P1',b'P2')),('unknown_phase',line.replace(b'P1',b'XX')),('wrong_nonce',line.replace(nonces['P1'].encode(),b'f'*48)),('nonascii',b'\xff\n'),('oversize',b'x'*129+b'\n'),('empty',b'\n')]:
        reject('parser_'+name,lambda raw=raw:A.parse_line(raw,token,nonces))
    first=row('P1'); second=row('P2',2,first); rows=[first,second]
    assert A.validate_prefix(rows,[first],own,'alpha',token,nonces)==['P1','P2']; passed.append('append_only_prefix_and_counter')
    for field,value in [('identity',other),('label','beta'),('token','b'*48),('phase','P3'),('nonce',nonces['P3']),('counter',True),('previous_sha256','bad'),('line_hex','xx')]:
        bad=copy.deepcopy(first); bad[field]=value; reject('receipt_'+field,lambda bad=bad:A.validate_prefix([bad],[],own,'alpha',token,nonces))
    bad=copy.deepcopy(first); bad['extra']=1; reject('receipt_extra_key',lambda:A.validate_prefix([bad],[],own,'alpha',token,nonces))
    born=copy.deepcopy(own); born['birth'][1]+=1; reject('same_PID_other_birth',lambda:A.validate_prefix([first],[],born,'alpha',token,nonces))
    reject('prefix_shortened',lambda:A.validate_prefix([first],rows,own,'alpha',token,nonces))
    bad=copy.deepcopy(first); bad['at']='rewritten'; reject('prefix_rewritten',lambda:A.validate_prefix([bad], [first],own,'alpha',token,nonces))
    reject('counter_gap',lambda:A.validate_prefix([first,{**second,'counter':3}],[],own,'alpha',token,nonces))
    duplicate=row('P1',2,first); reject('duplicate_nonce',lambda:A.validate_prefix([first,duplicate],[],own,'alpha',token,nonces))
    reject('out_of_order',lambda:A.validate_prefix([second,first],[],own,'alpha',token,nonces))
    copied=A.saved(rows); copied[0]['identity']['birth'][0]=99; assert rows[0]['identity']['birth'][0]==10; passed.append('receipt_snapshots_no_alias')
    def chain(phases,label,identity):
        result=[]
        for p in phases: result.append(row(p,len(result)+1,result[-1] if result else None,label,identity))
        return result
    identities={'alpha':own,'beta':other}; injections=[{'phase':p,'index':i} for i,p in enumerate((*A.CASES,'SA','SB'))]
    prefixes={'alpha':chain(['P1','P2','SA'],'alpha',own),'beta':chain(['P2','P3','SB'],'beta',other)}
    before={k:v[:-1] for k,v in prefixes.items()}
    got=A.check_sealed(prefixes,before,copy.deepcopy(prefixes),identities,token,nonces,injections)
    assert got=={'P1':['alpha'],'P2':['alpha','beta'],'P3':['beta']}; passed.append('ordered_two_sentinels_prove_exact_recipient_sets')
    no_rep={'alpha':chain(['P1','P2','SA'],'alpha',own),'beta':chain(['SB'],'beta',other)}
    cut=[{'phase':p,'index':i} for i,p in enumerate(('P1','P2','SA','SB'))]
    assert A.check_sealed(no_rep,{k:v[:-1] for k,v in no_rep.items()},no_rep,identities,token,nonces,cut)=={'P1':['alpha'],'P2':['alpha']}; passed.append('oracle_nonreplication_remains_visible_not_PASS')
    for name,phases in [('missing_sentinel',['P1','P2']),('after_sentinel',['SA','P1']),('foreign_sentinel',['P1','SB']),('wrong_order',['P2','P1','SA'])]:
        mutant=copy.deepcopy(prefixes); mutant['alpha']=chain(phases,'alpha',own)
        reject(name,lambda mutant=mutant:A.check_sealed(mutant,{k:[] for k in A.KINDS},mutant,identities,token,nonces,injections))
    mutant=copy.deepcopy(prefixes); mutant['beta'][0]['label']='alpha'; reject('foreign_helper_receipt',lambda:A.check_sealed(mutant,before,mutant,identities,token,nonces,injections))
    reject('barrier_snapshot_changed',lambda:A.check_sealed(prefixes,before,{**prefixes,'beta':[]},identities,token,nonces,injections))
    reject('uninjected_phase',lambda:A.check_sealed(prefixes,before,prefixes,identities,token,nonces,[x for x in injections if x['phase']!='P3']))
    for value in ['a'*47,'a'*49,'g'*48,None,True]: reject('invalid_nonce_'+repr(value)[:8],lambda value=value:A.wire_line(token,'P1',value))
    # Endpoint closure guards: no native API call, pure stable identity predicate.
    import types
    saved_D=A.D; A.D=types.SimpleNamespace(same=lambda a,b:bool(a and b and a['pid']==b['pid'] and a['birth']==b['birth']))
    try:
        endpoint={'uid':A.os.getuid(),'path':'/private/s','device':1,'inode':2}; process={'before':own,'after':None,'exit_event':True}
        probe={'accepting':False,'errno':A.errno.ECONNREFUSED,'path_state':endpoint}
        A.check_unlink(endpoint,endpoint,[process],probe); passed.append('qualified_private_endpoint_unlink')
        for name,ep,ps,pr in [('inode',{**endpoint,'inode':3},[process],probe),('birth_alive',endpoint,[{**process,'after':own}],probe),('no_event',endpoint,[{**process,'exit_event':False}],probe),('timeout',endpoint,[process],{**probe,'errno':A.errno.ETIMEDOUT}),('listener',endpoint,[process],{**probe,'accepting':True})]:
            reject('closure_'+name,lambda ep=ep,ps=ps,pr=pr:A.check_unlink(endpoint,ep,ps,pr))
    finally: A.D=saved_D
    source=Path(A.__file__).read_text(); compile(source,A.__file__,'exec')
    return {'status':'PASS','checks':passed,'driver_sha256':A.digest(A.__file__),'native_runs':0,'PIDquery':0,'FFI':0}

if __name__=='__main__':
    result=checks(); A.write_once(sys.argv[1],result); print(json.dumps({'status':result['status'],'checks':len(result['checks'])}))
