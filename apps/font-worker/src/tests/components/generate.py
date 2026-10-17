"""Generate original tiny TrueType component controls, without private inputs."""
from pathlib import Path
from array import array
import hashlib, json, math, sys
ROOT = Path(__file__).resolve().parents[5]
sys.path.insert(0, str(ROOT / '.tools/python'))
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import newTable
from fontTools.ttLib.tables._g_l_y_f import Glyph, GlyphComponent
from fontTools.ttLib.tables.ttProgram import Program

OUT = Path(__file__).resolve().parent
ORDER = ['.notdef', 'base', 'mark', 'H']

def rectangle(x, y, width, height):
    pen = TTGlyphPen(None)
    pen.moveTo((x,y))
    for point in [(x+width,y),(x+width,y+height),(x,y+height)]: pen.lineTo(point)
    pen.closePath()
    glyph = pen.glyph(); glyph.recalcBounds(None)
    return glyph

def component(name, flags=0, xy=(0,0), points=None, scale=None, matrix=None):
    c=GlyphComponent(); c.glyphName=name; c.flags=flags
    if points is None: c.x,c.y=xy
    else: c.firstPt,c.secondPt=points
    if scale is not None: c.transform=[[scale,0],[0,scale]]
    if matrix is not None: c.transform=[list(matrix[:2]),list(matrix[2:])]
    return c

CASES = {
    'scaled-offset-clear': ([component('base',0x0800,xy=(1,1),scale=.5)], [5.5,.5,55.5,50.5], 5),
    'scaled-offset-round': ([component('base',0x0804,xy=(1,1),scale=.5)], [5.5,.5,55.5,50.5], 5),
    'point-match-clear': ([component('base',0x0800,xy=(1,1),scale=.5),component('mark',points=(0,0))], [5.5,.5,55.5,80.5], 5),
    'point-match-round': ([component('base',0x0800,xy=(1,1),scale=.5),component('mark',4,points=(0,0))], [5.5,.5,55.5,80.5], 5),
    'child-pp1': ([component('base'),component('mark',points=(1,4))], [10,0,155,100], 20),
    'child-pp2': ([component('base'),component('mark',points=(1,5))], [-185,0,110,100], 20),
    'metrics-unmarked': ([component('base')], [10,0,110,100], 50),
    'metrics-marked': ([component('base',0x200)], [10,0,110,100], 50),
    'metrics-marked-scaled': ([component('base',0x200,scale=.5)], [5,0,55,50], 50),
    'metrics-marked-translated': ([component('base',0x200,xy=(75,20))], [85,20,185,120], 50),
    'metrics-marked-nested': ([component('inner',0x200)], [10,0,110,100], 50),
    'child-pp2-scaled': ([component('base'),component('mark',points=(1,5),scale=.5)], [-37.5,0,110,100], 20),
    'parent-pp1': ([component('base'),component('mark',points=(8,0))], [10,0,110,100], 0),
    'parent-pp2': ([component('base'),component('mark',points=(9,0))], [10,0,900,100], 50),
    'first-point-match-invalid': ([component('mark',points=(4,0))], [0,0,40,80], 0),
    'metrics-marked-shear': ([component('base',0x200,matrix=(1,0,.25,1))], [10,0,135,100], 50),
    'metrics-marked-unequal': ([component('base',0x200,matrix=(.5,0,0,1.25))], [5,0,55,125], 50),
    'metrics-marked-rotated': ([component('base',0x200,matrix=(0,1,-1,0))], [-100,10,0,110], 50),
    'metrics-unmarked-rotated': ([component('base',matrix=(0,1,-1,0))], [-100,10,0,110], 50),
    'metrics-marked-negative-x': ([component('base',0x200,matrix=(-1,0,0,1))], [-110,0,-10,100], 50),
    'metrics-unmarked-negative-x': ([component('base',matrix=(-1,0,0,1))], [-110,0,-10,100], 50),
    'vertical-unresolved-absent': ([component('base'),component('mark',points=(1,6))], [10,-110,150,100], 20),
    'vertical-unresolved-paired': ([component('base'),component('mark',points=(1,6))], [10,-80,150,100], 20),
    'vertical-cancelled-x': ([component('base'),component('mark',points=(2,6),matrix=(0,0,0,1)),component('mark',points=(4,0))], [10,0,150,100], 20),
    'vertical-cancelled-both': ([component('base'),component('mark',points=(2,6),scale=0),component('mark',points=(4,0))], [10,0,150,180], 20),
    'vertical-empty-unused': ([component('base'),component('.notdef',points=(1,2))], [10,0,110,100], 20),
    'empty-invalid-anchor': ([component('base'),component('.notdef',points=(1,4))], [10,0,110,100], 20),
    'vhea-only-unused': ([component('base')], [10,0,110,100], 50),
    'vertical-pair-malformed': ([component('base')], [10,0,110,100], 50),
    'unused-unresolved-subset': ([component('base')], [10,0,110,100], 50),
    'unused-unresolved-full': ([component('base')], [10,0,110,100], 50),
    'unused-malformed-subset': ([component('base')], [10,0,110,100], 50),
    'optional-h-unknown-subset': ([component('base')], [10,0,110,100], 50),
    'optional-h-declared-subset': ([component('base')], [10,0,110,100], 50),
    'vertical-version-unused': ([component('base')], [10,0,110,100], 50),
    'vertical-version-needed': ([component('base'),component('mark',points=(2,6),matrix=(0,0,0,1)),component('mark',points=(4,0))], [10,0,150,100], 20),
}
controls={}
for label,(parts, bounds, lsb) in CASES.items():
    extra_unknown=label.startswith(('unused-','optional-h-'))
    order=ORDER[:-1]+['inner','H'] if label=='metrics-marked-nested' else ORDER+(['unknown'] if extra_unknown else [])
    fb=FontBuilder(1000,isTTF=True);fb.setupGlyphOrder(order)
    empty=TTGlyphPen(None).glyph()
    glyphs={'.notdef':empty,'base':rectangle(10,0,100,100),'mark':rectangle(0,0,40,80)}
    composite=Glyph();composite.numberOfContours=-1;composite.components=parts
    cache=[math.floor(bounds[0]),math.floor(bounds[1]),math.ceil(bounds[2]),math.ceil(bounds[3])]
    composite.xMin,composite.yMin,composite.xMax,composite.yMax=cache
    glyphs['H']=composite
    if label=='metrics-marked-nested':
        inner=Glyph();inner.numberOfContours=-1;inner.components=[component('base',0x200)]
        inner.xMin,inner.yMin,inner.xMax,inner.yMax=(10,0,110,100);glyphs['inner']=inner
    if extra_unknown:
        unknown=Glyph();unknown.numberOfContours=-1;unknown.components=[component('base',0x200,matrix=(0,1,-1,0))]
        if label=='unused-malformed-subset':unknown.components.append(component('mark',points=(999,0)))
        unknown.xMin,unknown.yMin,unknown.xMax,unknown.yMax=(-100,10,0,110);glyphs['unknown']=unknown
    fb.setupGlyf(glyphs,calcGlyphBounds=False)
    metrics={'.notdef':(500,0),'base':(600,20),'mark':(300,5),'H':(900,lsb)}
    if label=='metrics-marked-nested':metrics['inner']=(800,70)
    if extra_unknown:metrics['unknown']=(700,30)
    fb.setupHorizontalMetrics(metrics);fb.setupHorizontalHeader(ascent=800,descent=-200)
    fb.setupCharacterMap({ord('H'):'unknown',ord('Q'):'base'} if label.startswith('optional-h-') else {ord('H'):'H'})
    name='PDFLibraryComponent'+label.replace('-','').title()
    fb.setupNameTable({'familyName':name,'styleName':'Regular','uniqueFontIdentifier':name,'fullName':name,'psName':name,'version':'Version 1.0','copyright':'Original synthetic geometry; MIT OR Apache-2.0, as the existing original.ttf control.'})
    subset_allowed=label.endswith('subset')
    fb.setupOS2(sTypoAscender=800,sTypoDescender=-200,usWinAscent=800,usWinDescent=200,fsType=0 if subset_allowed else 0x0108,sCapHeight=700 if label=='optional-h-declared-subset' else 0)
    fb.setupPost();fb.setupMaxp()
    paired_vertical=label in ('vertical-unresolved-paired','vertical-cancelled-x','vertical-pair-malformed','vertical-version-unused','vertical-version-needed')
    if paired_vertical:
        fb.setupVerticalMetrics({n:(400 if n=='mark' else 500,0) for n in order})
    if paired_vertical or label=='vhea-only-unused':
        fb.setupVerticalHeader(ascent=800,descent=-200,lineGap=0)
        vhea=fb.font['vhea'];vhea.advanceHeightMax=500;vhea.minTopSideBearing=0
        vhea.minBottomSideBearing=320;vhea.yMaxExtent=100
        if label=='vertical-pair-malformed':vhea.tableVersion=0x10000;vhea.lineGap=1
        if label.startswith('vertical-version-'):vhea.tableVersion=0x20000
    for tag in ('fpgm','prep'):
        table=newTable(tag);table.program=Program();table.program.fromBytecode([]);fb.font[tag]=table
    cvt=newTable('cvt ');cvt.values=array('h');fb.font['cvt ']=cvt
    head=fb.font['head'];head.flags=1;head.created=head.modified=2082844800
    all_boxes=[[10,0,110,100],[0,0,40,80]]+([[10,0,110,100]] if label=='metrics-marked-nested' else [])+[cache]+([[-100,10,0,110]] if extra_unknown else [])
    head.xMin=min(b[0] for b in all_boxes);head.yMin=min(b[1] for b in all_boxes)
    head.xMax=max(b[2] for b in all_boxes);head.yMax=max(b[3] for b in all_boxes)
    hhea=fb.font['hhea'];hhea.advanceWidthMax=900
    values=[(metrics[n][1],metrics[n][0]-metrics[n][1]-(b[2]-b[0]),metrics[n][1]+b[2]-b[0]) for n,b in zip(order[1:],all_boxes)]
    hhea.minLeftSideBearing=min(v[0] for v in values);hhea.minRightSideBearing=min(v[1] for v in values);hhea.xMaxExtent=max(v[2] for v in values)
    component_limit=max(len(g.components) for g in glyphs.values() if g.numberOfContours<0)
    maxp=fb.font['maxp'];maxp.maxPoints=4;maxp.maxContours=1;maxp.maxCompositePoints=4*component_limit;maxp.maxCompositeContours=component_limit
    maxp.maxComponentElements=component_limit;maxp.maxComponentDepth=2 if label=='metrics-marked-nested' else 1;maxp.maxZones=1
    fb.font.recalcBBoxes=False
    path=OUT/(label+'.ttf');fb.save(path)
    selected = label.startswith('metrics-marked')
    origin=-10 if selected or label.startswith('optional-h-') else cache[0]-lsb
    outcome=('malformed-font' if label in ('first-point-match-invalid','empty-invalid-anchor','vertical-pair-malformed','unused-malformed-subset') else
        'unsupported-vertical-phantom-context' if label.startswith('vertical-unresolved') else
        'unsupported-component-metric-context' if label in ('metrics-marked-rotated','metrics-marked-negative-x','unused-unresolved-full') else
        'unsupported-vertical-metrics-version' if label=='vertical-version-needed' else 'prepared')
    requested_box=[bounds[0]-origin,bounds[1],bounds[2]-origin,bounds[3]]
    retained_boxes=([[20,0,120,100]] if label.startswith('optional-h-') else [requested_box,[20,0,120,100]] if subset_allowed else [requested_box,[20,0,120,100],[5,0,45,80]])
    expected_bbox=[math.floor(min(b[0] for b in retained_boxes)),math.floor(min(b[1] for b in retained_boxes)),math.ceil(max(b[2] for b in retained_boxes)),math.ceil(max(b[3] for b in retained_boxes))]
    controls[path.name]={'sha256':hashlib.sha256(path.read_bytes()).hexdigest(),'source_gid':1 if label.startswith('optional-h-') else order.index('H'),'raw_hmtx':[600,20] if label.startswith('optional-h-') else [900,lsb], 'raw_bounds_exact':bounds if not label.startswith('vertical-unresolved') else None,'cache_bounds':cache,'expected_resolved_advance':600 if selected or label.startswith('optional-h-') else 900,'expected_resolved_pp1_x':origin,'expected_outline_bounds':[bounds[0]-origin,bounds[1],bounds[2]-origin,bounds[3]] if not label.startswith('vertical-unresolved') else None,'expected_cap_height':(700 if label=='optional-h-declared-subset' else None if label=='optional-h-unknown-subset' else bounds[3]) if outcome=='prepared' else None,'expected_outcome':outcome,'expected_preparation_kind':'subset' if subset_allowed else 'full','vertical_metric_pair':paired_vertical,'vertical_context_note':'Vertical X is unestablished; absent pair also leaves Y unestablished. Exact-zero local coefficients/empty geometry can make those coordinates irrelevant.','source_facts_for_positive_print':{'accented_source_glyph_ids':[],'cap_height_pdf_units':700 if label=='optional-h-unknown-subset' else None}}
    controls[path.name]['expected_pdf_bbox']=expected_bbox if outcome=='prepared' else None
(OUT/'provenance.json').write_text(json.dumps({
    'schema':'pdf-library-original-component-controls/v1',
    'provenance':'Original simple rectangles and component records, generated here without source font assets or private artifacts; same MIT OR Apache-2.0 original-control terms as ../fixtures/original.ttf. No source font art or hint programs.',
    'scope':'Unhinted coordinates, transformed child horizontal phantom attachments, original recursively inherited component horizontal metrics, and parent horizontal phantom numbering at final composite n/n+1 (source inference; no normative worked example).',
    'interpretation_boundaries':{
        'selected_horizontal_metrics':'A selected USE_MY_METRICS component with b != 0 or a < 0 has unsupported component metric context. This is the documented project interpretation boundary, not a normative definition of rotation. Positive scales, translations and c-only x shear preserve original component metrics.',
        'vertical_anchors':'Supported paired vhea/vmtx establishes vertical Y only; X is unspecified. Missing pairs leave both coordinates unestablished. Exact-zero transform coefficients or empty geometry can eliminate an unknown coordinate dependency. Only an actual unresolved geometry dependency is refused.',
        'source_and_retained_scope':'Only typed interpretation-context errors may be deferred for an unused source glyph. Structural errors are never deferred. All retained prepared glyphs, including unused full-font glyphs, must resolve.'
    },
    'controls':controls
},indent=2)+'\n',encoding='utf-8',newline='\n')
