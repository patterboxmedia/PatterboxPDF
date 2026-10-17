"""Focused native worker / independent exact-program geometry control receipt."""
from pathlib import Path
from fractions import Fraction
import argparse, hashlib, importlib.util, io, json, math, os, platform, struct, subprocess, sys
HERE=Path(__file__).resolve().parent
ROOT=HERE.parents[3]
sys.path.insert(0,str(ROOT/'.tools/linux-verify-deps/site-packages'))
sys.path.insert(0,str(ROOT/'.tools/python'))
from fontTools.ttLib import TTFont
spec=importlib.util.spec_from_file_location('program_rules',ROOT/'scripts/pdf_font_program_rules.py')
rules=importlib.util.module_from_spec(spec);spec.loader.exec_module(rules)
parser=argparse.ArgumentParser(description=__doc__)
parser.add_argument('--worker',type=Path,required=True)
parser.add_argument('--output',type=Path,required=True)
args=parser.parse_args()
worker=args.worker.resolve()
out=args.output.resolve()
out.mkdir()
def sha(data):return hashlib.sha256(data).hexdigest()
def f32(v):return struct.unpack('>f',struct.pack('>f',v))[0]
def exact_dict(data):
    at=0;stack=[];result={}
    while at<len(data):
        b=data[at]
        if b==30:
            at+=1;text='';done=False
            while not done:
                byte=data[at];at+=1
                for n in (byte>>4,byte&15):
                    if n==15:done=True;break
                    text+=str(n) if n<10 else {10:'.',11:'E',12:'E-',14:'-'}[n]
            stack.append(Fraction(text));continue
        if b in (28,29) or 32<=b<=254:
            v,at=rules._number(data,at,True);stack.append(Fraction(v));continue
        at+=1;key=b
        if b==12:key=1200+data[at];at+=1
        result[key]=stack;stack=[]
    return result

def exact_matrices(raw):
    reader=rules._CFF(raw);_,at=reader.index(raw[2]);tops,_=reader.index(at);top=exact_dict(tops[0])
    fds,_=reader.index(int(top[1236][0])) if 1236 in top else ([tops[0]],0)
    identity=list(map(Fraction,[1,0,0,1,0,0]));default=[Fraction(1,1000),0,0,Fraction(1,1000),0,0]
    def concat(a,b):return [a[0]*b[0]+a[2]*b[1],a[1]*b[0]+a[3]*b[1],a[0]*b[2]+a[2]*b[3],a[1]*b[2]+a[3]*b[3],a[0]*b[4]+a[2]*b[5]+a[4],a[1]*b[4]+a[3]*b[5]+a[5]]
    result=[]
    for raw_fd in fds:
        fd=exact_dict(raw_fd)
        relative=fd.get(1207,identity) if 1236 in top else identity
        parent=top.get(1207,identity if 1236 in top and 1207 in fd else default)
        result.append([v*1000 for v in concat(parent,relative)])
    return result

ascent_input=HERE/'fixtures/ascent-facts.json'
ascent_controls=json.loads(ascent_input.read_text())['controls']
results=[]
for source in sorted((HERE/'fixtures').glob('*')):
    if source.suffix not in ('.otf','.ttf'):continue
    data=source.read_bytes()
    font=TTFont(io.BytesIO(data),recalcBBoxes=False,recalcTimestamp=False)
    gid=font.getGlyphID(font.getBestCmap()[ord('H')]);font.close()
    # Exercise the positioned, unencoded source GID without an encoded anchor.
    gids=[2] if source.name in ascent_controls else [0,gid]
    target=out/(source.stem+'.font')
    glyph_hash=sha(b'publisher-v2/font-glyph-set/v1\0'+struct.pack('>I',len(gids))+b''.join(struct.pack('>H',gid) for gid in gids))
    request={'operation':'prepare','protocol_version':9,'font_path':str(source),'expected_source_sha256':sha(data),'prepared_output_path':str(target),'preparation_policy':'publication','face_index':0,'glyph_ids':gids,'expected_glyph_set_sha256':glyph_hash}
    proc=subprocess.run([str(worker)],input=json.dumps(request).encode(),capture_output=True,check=False)
    response=json.loads(proc.stdout)
    item={'control':source.name,'source_sha256':sha(data),'worker_exit':proc.returncode,'response':response,'source_unchanged':sha(source.read_bytes())==sha(data)}
    rejected=source.stem in ('no-endchar','partial','subr-no-return')
    if rejected:
        item['passed']=proc.returncode!=0 and not target.exists()
    elif proc.returncode:
        item['passed']=False
    else:
        embedded=target.read_bytes();report=response['report'];kind=report['outline_kind']
        evaluation=rules.inspect_font_program(embedded,'truetype' if kind=='true-type' else kind)
        item.update(prepared_sha256=sha(embedded),independent_program=evaluation)
        metrics=report['pdf_metrics']
        if kind=='cff':
            geometry=evaluation['observations']['geometry'];boxes=[v['filled_bounds'] for v in geometry if v['filled_bounds'] is not None]
            widths=[f32(v['advance_displacement_x']) if v['advance_displacement_x'] is not None else metrics['widths'][i] for i,v in enumerate(geometry)]
        else:
            from fontTools.pens.boundsPen import BoundsPen
            spec=importlib.util.spec_from_file_location('metric_rules',ROOT/'scripts/verify-font-metrics.py')
            metric_rules=importlib.util.module_from_spec(spec);spec.loader.exec_module(metric_rules)
            tt=TTFont(io.BytesIO(embedded),recalcBBoxes=False);gs=tt.getGlyphSet();scale=1000/tt['head'].unitsPerEm;boxes=[]
            for name in tt.getGlyphOrder():
                box=metric_rules._bounds(gs,name,BoundsPen)
                if box is not None:boxes.append([v*scale for v in box])
            widths=[f32(tt['hmtx'][n][0]*scale) for n in tt.getGlyphOrder()]
            item['independent_filled_geometry']=boxes
        raw=[min(b[0] for b in boxes),min(b[1] for b in boxes),max(b[2] for b in boxes),max(b[3] for b in boxes)] if boxes else [0]*4
        expected=[math.floor(raw[0]),math.floor(raw[1]),math.ceil(raw[2]),math.ceil(raw[3])]
        uncertainty=['nonbinary effective decimal affine or retained nonlinear geometry'] if kind=='cff' and any(Fraction(str(v)).denominator & (Fraction(str(v)).denominator-1) for item_ in geometry for v in item_['effective_pdf_matrix']) else []
        enclosure=rules.inspect_font_bbox_enclosure(metrics['bbox'],raw,uncertainty)
        item.update(independent_raw_union=raw,expected_final_bbox=expected,bbox_enclosure=enclosure,bbox_agrees=enclosure['passed'],widths_agree=list(map(f32,metrics['widths']))==widths,exact_program_binding=sha(embedded)==report['prepared_sha256'])
        if kind=='cff':
            original=TTFont(io.BytesIO(data),recalcBBoxes=False,recalcTimestamp=False)
            source_raw=original.getTableData('CFF ')
            source_matrices=exact_matrices(source_raw);prepared_matrices=exact_matrices(embedded)
            cff=original['CFF '].cff.topDictIndex[0]
            prepared_cff=rules._CFF(embedded)
            from fontTools.cffLib import CFFFontSet
            dec=CFFFontSet();dec.decompile(io.BytesIO(embedded),None);prepared_top=dec.topDictIndex[0]
            bindings=[]
            for mapping in report['remapped_glyphs']:
                sgid=mapping['source_glyph_id'];pgid=mapping['cid']
                sm=source_matrices[cff.FDSelect[sgid] if hasattr(cff,'FDSelect') else 0]
                pm=prepared_matrices[prepared_top.FDSelect[pgid]]
                from fontTools.pens.recordingPen import RecordingPen
                source_pen=RecordingPen();prepared_pen=RecordingPen()
                source_char=cff.CharStrings[cff.charset[sgid]];prepared_char=prepared_top.CharStrings[prepared_top.charset[pgid]]
                source_char.draw(source_pen);prepared_char.draw(prepared_pen)
                advance=Fraction(str(source_char.width))*sm[0]
                width_same=f32(float(advance))==f32(metrics['widths'][pgid])
                same=sm==pm and source_pen.value==prepared_pen.value and width_same
                bindings.append({'source_gid':sgid,'prepared_gid':pgid,'source_effective_exact':list(map(str,sm)),'prepared_effective_exact':list(map(str,pm)),'source_untransformed_path_sha256':sha(repr(source_pen.value).encode()),'prepared_untransformed_path_sha256':sha(repr(prepared_pen.value).encode()),'source_advance_exact':str(advance),'native_advance':metrics['widths'][pgid],'width_same':width_same,'same':same})
            item['exact_source_prepared_matrices']=bindings
            item['source_geometry_preserved']=all(b['same'] for b in bindings)
            original.close()
        else:item['source_geometry_preserved']=embedded==data
        if source.stem=='top':
            ideal=list(map(Fraction,[104,-80,1204,1080]))
            item['exact_fraction_geometry']=list(map(str,ideal))
            item['exact_fraction_contained']=all(Fraction(metrics['bbox'][i])<=ideal[i] for i in (0,1)) and all(Fraction(metrics['bbox'][i])>=ideal[i] for i in (2,3))
        elif source.stem in ('tiny-positive','tiny-negative','integer-translation','default'):
            displacement={'tiny-positive':Fraction(1,10**15),'tiny-negative':-Fraction(1,10**15),'integer-translation':Fraction(1),'default':Fraction(0)}[source.stem]
            ideal=[100+displacement,Fraction(-120),900+displacement,Fraction(880)]
            item['exact_fraction_geometry']=list(map(str,ideal))
            item['exact_fraction_contained']=all(Fraction(metrics['bbox'][i])<=ideal[i] for i in (0,1)) and all(Fraction(metrics['bbox'][i])>=ideal[i] for i in (2,3))
        else:item['exact_fraction_contained']=True
        item['passed']=item['source_geometry_preserved'] and item['exact_fraction_contained'] and not evaluation['findings'] and item['bbox_agrees'] and item['widths_agree'] and item['exact_program_binding']
        if source.name in ascent_controls or source.stem=='default':
            role=ascent_controls.get(source.name)
            facts=role['accented_source_glyph_ids'] if role else []
            expected_auto_unresolved=source.stem=='ascent-unencoded-high'
            expected_auto_ascent=1100 if expected_auto_unresolved else 880
            item['automatic_ascent']={'requires_facts':report['requires_ascent_facts'],'ascent':metrics['ascent'],'expected_requires_facts':expected_auto_unresolved,'expected_ascent':expected_auto_ascent}
            item['passed']=item['passed'] and report['requires_ascent_facts']==expected_auto_unresolved and metrics['ascent']==expected_auto_ascent and report['ascent_facts_sha256'] is None
            identity=sha(b'publisher-v2/font-ascent-facts/v1\0'+bytes.fromhex(sha(data))+struct.pack('>I',0)+struct.pack('>I',len(facts))+b''.join(struct.pack('>H',g) for g in facts))
            combined_identity=sha(b'publisher-v2/font-design-facts/v1\0'+bytes.fromhex(sha(data))+struct.pack('>I',0)+b'\1'+struct.pack('>I',len(facts))+b''.join(struct.pack('>H',gid) for gid in facts)+b'\0')
            fact_request=dict(request,accented_source_glyph_ids=facts,expected_ascent_facts_sha256=identity,expected_font_facts_sha256=combined_identity,prepared_output_path=str(out/(source.stem+'.facts.font')))
            result=subprocess.run([str(worker)],input=json.dumps(fact_request).encode(),capture_output=True,check=False)
            fact_response=json.loads(result.stdout)
            assert result.returncode==0,fact_response
            fact_report=fact_response['report']
            mapping={entry['cid']:entry['source_glyph_id'] for entry in fact_report['remapped_glyphs']}
            independent_ascent=max([0]+[g['filled_bounds'][3] for g in geometry if g['filled_bounds'] is not None and mapping[g['gid']] not in facts])
            retained=(out/(source.stem+'.facts.font')).read_bytes()
            item['authored_ascent']={'explicit_facts':facts,'fact_input_file_sha256':sha(ascent_input.read_bytes()) if role else None,'semantic_premise':role['role'] if role else 'Retained missing glyph and plain H have no accented-only role','expected_fact_hash':identity,'independently_decoded_ascent':independent_ascent,'response':fact_response,'same_prepared_bytes':retained==embedded,'correct_metric':fact_report['pdf_metrics']['ascent']==independent_ascent,'authority_bound':not fact_report['requires_ascent_facts'] and fact_report['ascent_facts_sha256']==identity}
            item['passed']=item['passed'] and retained==embedded and item['authored_ascent']['correct_metric'] and item['authored_ascent']['authority_bound']

    if source.stem=='absent-top':item['qualification']='normalization characterization only; not a validated positive control';item['required_positive']=False
    else:item['required_positive']=True
    results.append(item)
receipt={'schema':'patterbox-u2-native-independent-program-controls/v1','worker_sha256':sha(worker.read_bytes()),'worker_engine':'subsetter0.2.6/ttf-parser0.25.1; '+sys.platform+' '+platform.machine(),'program_evaluator_sha256':sha((ROOT/'scripts/pdf_font_program_rules.py').read_bytes()),'probe_sha256':sha(Path(__file__).read_bytes()),'cases':results,'passed':all(v['passed'] for v in results if v['required_positive']),'scope':'Focused direct native worker and exact prepared-program geometry, not confinement or final PDF qualification'}
(out/'receipt.json').write_text(json.dumps(receipt,indent=2)+'\n')
print(json.dumps({'passed':receipt['passed'],'cases':len(results),'failures':[v['control'] for v in results if not v['passed']]}))
raise SystemExit(0 if receipt['passed'] else 1)



