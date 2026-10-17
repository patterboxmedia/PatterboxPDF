"""Capture focused native component behavior and exact source/program bindings."""
from pathlib import Path
import argparse, hashlib, json, platform, struct, subprocess

HERE=Path(__file__).resolve().parent
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--worker',type=Path,required=True)
parser.add_argument('--output',type=Path,required=True)
args=parser.parse_args()
worker=args.worker.resolve();out=args.output.resolve();out.mkdir(parents=True)
sha=lambda b:hashlib.sha256(b).hexdigest()
provenance=json.loads((HERE/'provenance.json').read_text())
results=[]
for name,control in provenance['controls'].items():
    source=HERE/name;data=source.read_bytes();gid=control['source_gid']
    assert sha(data)==control['sha256']
    target=out/(source.stem+'.font')
    request={'operation':'prepare','protocol_version':9,'font_path':str(source),'expected_source_sha256':sha(data),'prepared_output_path':str(target),'preparation_policy':'publication','face_index':0,'glyph_ids':[gid],'expected_glyph_set_sha256':sha(b'publisher-v2/font-glyph-set/v1\0'+struct.pack('>IH',1,gid))}
    proc=subprocess.run([str(worker)],input=json.dumps(request).encode(),capture_output=True)
    response=json.loads(proc.stdout)
    item={'control':name,'source_sha256':sha(data),'expected_outcome':control['expected_outcome'],'request':request,'exit':proc.returncode,'response':response}
    if control['expected_outcome']!='prepared':
        item['passed']=proc.returncode!=0 and response.get('error')==control['expected_outcome'] and not target.exists()
    else:
        assert proc.returncode==0,response
        prepared=target.read_bytes();report=response['report'];metrics=report['pdf_metrics']
        cid=next(m['cid'] for m in report['remapped_glyphs'] if m['source_glyph_id']==gid)
        item['passed']=sha(prepared)==report['prepared_sha256'] and report['preparation_kind']==control['expected_preparation_kind'] and (prepared==data if report['preparation_kind']=='full' else True) and metrics['bbox']==control['expected_pdf_bbox'] and metrics['widths'][cid]==control['expected_resolved_advance'] and metrics['cap_height']==control['expected_cap_height'] and report['requires_cap_height_fact']==(control['expected_cap_height'] is None)
        facts=control['source_facts_for_positive_print'];accented=facts['accented_source_glyph_ids'];cap=facts['cap_height_pdf_units']
        accent=sha(b'publisher-v2/font-ascent-facts/v1\0'+bytes.fromhex(sha(data))+struct.pack('>II',0,len(accented))+b''.join(struct.pack('>H',v) for v in accented))
        combined=sha(b'publisher-v2/font-design-facts/v1\0'+bytes.fromhex(sha(data))+struct.pack('>I',0)+b'\1'+struct.pack('>I',len(accented))+b''.join(struct.pack('>H',v) for v in accented)+(b'\0' if cap is None else b'\1'+struct.pack('>f',cap)))
        authored_target=out/(source.stem+'.facts.font')
        authored_request=dict(request,prepared_output_path=str(authored_target),accented_source_glyph_ids=accented,expected_ascent_facts_sha256=accent,cap_height_pdf_units=cap,expected_font_facts_sha256=combined)
        authored=subprocess.run([str(worker)],input=json.dumps(authored_request).encode(),capture_output=True)
        authored_response=json.loads(authored.stdout);ar=authored_response.get('report',{})
        item['authored']={'explicit_premise':facts,'request':authored_request,'exit':authored.returncode,'response':authored_response}
        item['passed']=item['passed'] and authored.returncode==0 and authored_target.read_bytes()==prepared and ar['ascent_facts_sha256']==accent and ar['font_facts_sha256']==combined and not ar['requires_ascent_facts'] and not ar['requires_cap_height_fact'] and ar['pdf_metrics']['cap_height']==(cap if cap is not None else control['expected_cap_height'])
    results.append(item)
receipt={'schema':'pdf-library-native-component-controls/v1','scope':'Native producer behavior and exact byte bindings against authored control premises. Independent source geometry/output evaluation is a separate gate.','worker_sha256':sha(worker.read_bytes()),'environment':platform.platform(),'generator_sha256':sha((HERE/'generate.py').read_bytes()),'provenance_sha256':sha((HERE/'provenance.json').read_bytes()),'probe_sha256':sha(Path(__file__).read_bytes()),'cases':results,'passed':all(v['passed'] for v in results)}
(out/'receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
print(json.dumps({'passed':receipt['passed'],'cases':len(results),'failed':[v['control'] for v in results if not v['passed']]}))
raise SystemExit(0 if receipt['passed'] else 1)
