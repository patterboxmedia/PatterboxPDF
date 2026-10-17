"""Generate tiny renamed OFL CFF controls and original MIT/Apache TT controls."""
from pathlib import Path
import hashlib, io, json, sys
sys.path.insert(0, str(Path(__file__).resolve().parents[4] / '.tools/python'))
from fontTools.ttLib import TTFont
from fontTools import subset
from fontTools.misc.psCharStrings import T2CharString
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib.tables.ttProgram import Program
HERE = Path(__file__).resolve().parent
ROOT = HERE.parents[3]
OUT = HERE / 'fixtures'
SOURCE = ROOT / 'crates/layout-core/tests/fixtures/NotoSansCJKsc-Regular.otf'
assert hashlib.sha256(SOURCE.read_bytes()).hexdigest() == '2c76254f6fc379fddfce0a7e84fb5385bb135d3e399294f6eeb6680d0365b74b'
# Reproduce the tiny parent from the approved source asset; no private evidence
# file is an input. Keep the original OFL copyright and license metadata.
parent_font=TTFont(SOURCE,recalcBBoxes=True,recalcTimestamp=False)
options=subset.Options();options.name_IDs=['*'];options.name_languages=['*']
options.name_legacy=True;options.notdef_outline=True;options.recalc_timestamp=False
sub=subset.Subsetter(options=options);sub.populate(unicodes=[ord('H')]);sub.subset(parent_font)
parent=io.BytesIO();parent_font.save(parent);parent_bytes=parent.getvalue()
(OUT/'LICENSE-OFL').write_bytes((SOURCE.parent/'LICENSE-NotoSansCJK').read_bytes())
controls = {}
for case in ['default', 'top', 'fd', 'absent-top', 'reflected', 'fractional', 'hflex', 'flex', 'hflex1', 'flex1', 'empty', 'no-endchar', 'partial', 'subr-no-return', 'width-diff', 'accumulated-fraction','tiny-positive','tiny-negative','integer-translation','implicit-default','explicit-default','implicit-scaled','explicit-scaled','implicit-translated','explicit-translated'] + [flex + '_' + matrix for flex in ('hflex','flex','hflex1','flex1') for matrix in ('top','fd')]:
    base, _, mode = case.partition('_')
    font = TTFont(io.BytesIO(parent_bytes), recalcBBoxes=False, recalcTimestamp=False)
    name = 'PatterboxU2' + case.replace('-', '').title()
    for entry in font['name'].names:
        if entry.nameID in (1,3,4,6,16,18,21,25): entry.string = name.encode(entry.getEncoding())
    top = font['CFF '].cff.topDictIndex[0]
    font['CFF '].cff.fontNames = [name]
    top.FullName = top.FamilyName = name
    for fd in top.FDArray:
        fd.FontName = name + 'FD'
    if case != 'absent-top': top.FontMatrix = [.001,0,0,.001,0,0]
    if case == 'top' or mode == 'top': top.FontMatrix = [.001,.0002,.0003,.001,.04,.02]
    if case == 'fd' or mode == 'fd':
        top.FontMatrix = [.0012,0,0,.001,0,0]
    if case in ('fd','absent-top') or mode == 'fd':
        for fd in top.FDArray: fd.FontMatrix = [1,.2,.3,1,40,20]
    if case.startswith(('implicit-','explicit-')):
        top.__dict__.pop('FontMatrix',None);top.rawDict.pop('FontMatrix',None)
        top.defaults={k:v for k,v in top.defaults.items() if k!='FontMatrix'}
        for fd in top.FDArray:
            fd.__dict__.pop('FontMatrix',None);fd.rawDict.pop('FontMatrix',None)
            fd.defaults={k:v for k,v in fd.defaults.items() if k!='FontMatrix'}
        explicit=case.startswith('explicit-')
        if explicit: top.FontMatrix=[.001,0,0,.001,0,0]
        if case.endswith(('scaled','translated')):
            translated=case.endswith('translated')
            matrix=([1,0,0,1.25,20 if translated else 0,10 if translated else 0] if explicit else [.001,0,0,.00125,.02 if translated else 0,.01 if translated else 0])
            for fd in top.FDArray:fd.FontMatrix=matrix
    if case in ('tiny-positive','tiny-negative','integer-translation'): top.FontMatrix=[.001,0,0,.001,{'tiny-positive':1e-18,'tiny-negative':-1e-18,'integer-translation':.001}[case],0]
    if case == 'reflected': top.FontMatrix = [-.001,.0002,.0003,.001,.04,.02]
    glyph = font.getBestCmap()[ord('H')]
    char = top.CharStrings[glyph]
    width = font['hmtx'][glyph][0] - getattr(char.private,'nominalWidthX',0)
    programs = {
        'fractional': [width, 50.25,.125,'rmoveto',.125,1400.25,400.5,.125,.25,-1400.125,'rrcurveto','endchar'],
        'hflex': [width,1000,1000,'rmoveto',50,50,100,50,50,50,50,'hflex','endchar'],
        'flex': [width,1000,1000,'rmoveto',50,20,50,100,50,-20,50,-20,50,-100,50,20,50,'flex','endchar'],
        'hflex1': [width,1000,1000,'rmoveto',50,20,50,100,50,50,50,-100,50,'hflex1','endchar'],
        'flex1': [width,1000,1000,'rmoveto',50,20,50,100,50,-20,50,-20,50,-100,50,'flex1','endchar'],
        'accumulated-fraction': [width,1000,100,'rmoveto'] + [v for _ in range(5000) for v in (1/65536,0,'rlineto')] + [0,100,-5000/65536,0,0,-100,'rlineto','endchar'],
        'empty': [width,'endchar'],
        'no-endchar': [width,50,0,'rmoveto'],
        'partial': [width,50,0,'rmoveto',100,200,'rlineto',100,'rrcurveto','endchar'],
    }
    if base in programs and hasattr(font['OS/2'], 'sCapHeight'): font['OS/2'].sCapHeight = 0
    if base in programs:
        top.CharStrings[glyph] = T2CharString(program=programs[base],private=char.private,globalSubrs=char.globalSubrs)
    if case == 'subr-no-return':
        subrs = font['CFF '].cff.GlobalSubrs
        subrs.append(T2CharString(program=[100,100,'rlineto'],globalSubrs=subrs))
        bias = 107 if len(subrs) < 1240 else 1131 if len(subrs) < 33900 else 32768
        top.CharStrings[glyph] = T2CharString(program=[width,0,0,'rmoveto',len(subrs)-1-bias,'callgsubr','endchar'],private=char.private,globalSubrs=subrs)
    if case == 'width-diff':
        font['hmtx'][glyph] = (123, font['hmtx'][glyph][1])
    output = OUT / (case + '.otf')
    font.save(output)
    controls[output.name] = hashlib.sha256(output.read_bytes()).hexdigest()
# A tiny original full-font-required TrueType with empty/simple/quadratic and
# a component attached by point indices. No borrowed artwork or bytecode.
fb = FontBuilder(1000,isTTF=True)
order = ['.notdef','H','curve','attached']
fb.setupGlyphOrder(order)
glyphs = {}
for name, coords in {'.notdef':[], 'H':[(10,0),(10,700),(90,700),(90,0)], 'curve':[]}.items():
    pen = TTGlyphPen(None)
    if coords:
        pen.moveTo(coords[0])
        for p in coords[1:]: pen.lineTo(p)
        pen.closePath()
    elif name == 'curve':
        pen.moveTo((0,0));pen.qCurveTo((50,101),(100,0));pen.closePath()
    glyphs[name] = pen.glyph()
pen=TTGlyphPen(glyphs);pen.addComponent('H',(1,0,0,1,0,0));pen.addComponent('curve',(1,0,0,1,0,0));glyphs['attached']=pen.glyph()
from fontTools.ttLib.tables._g_l_y_f import GlyphComponent
second = glyphs['attached'].components[1]
del second.x; del second.y
second.firstPt = 2; second.secondPt = 0
fb.setupGlyf(glyphs);fb.setupHorizontalMetrics({n:(500,200 if n=='attached' else 10 if n=='H' else 0) for n in order})
fb.setupHorizontalHeader(ascent=800,descent=-200)
fb.setupCharacterMap({ord('H'):'H',ord('Q'):'curve',ord('A'):'attached'})
fb.setupNameTable({'familyName':'Patterbox U2 Original','styleName':'Regular','uniqueFontIdentifier':'PatterboxU2Original','fullName':'Patterbox U2 Original','psName':'PatterboxU2Original','version':'Version 1.0'})
fb.setupOS2(sTypoAscender=800,sTypoDescender=-200,usWinAscent=800,usWinDescent=200,fsType=0x0108)
fb.setupPost();fb.setupMaxp()
from fontTools.ttLib import newTable
for tag in ('fpgm','prep'):
    t=newTable(tag);t.program=Program();t.program.fromBytecode([]);fb.font[tag]=t
from array import array
cvt=newTable('cvt ');cvt.values=array('h');fb.font['cvt ']=cvt
fb.font['head'].flags = 1  # lsb can differ from xMin in this positive control
fb.font['head'].created = fb.font['head'].modified = 2082844800
fb.save(OUT / 'original.ttf')
controls['original.ttf']=hashlib.sha256((OUT/'original.ttf').read_bytes()).hexdigest()
# Append three actual H-plus-circumflex roles; regenerate directly from the
# approved source subset above. The authored exclusion set is independent of cmap.
from fontTools.pens.t2CharStringPen import T2CharStringPen
from fontTools.pens.boundsPen import BoundsPen
ascent_roles={}
for case,coded,height in [('ascent-unencoded-high',False,1100),('ascent-coded-high',True,1100),('ascent-unencoded-low',False,800)]:
    font=TTFont(io.BytesIO(parent_bytes),recalcBBoxes=True,recalcTimestamp=False)
    family='Patterbox Authored '+case.replace('-',' ').title()
    name='PatterboxAuthored'+case.replace('-','').title()
    for entry in font['name'].names:
        if entry.nameID in (1,3,4,6,16,18,21,25):entry.string=(name if entry.nameID==6 else family).encode(entry.getEncoding())
    top=font['CFF '].cff.topDictIndex[0];font['CFF '].cff.fontNames=[name]
    top.FullName=top.FamilyName=family;top.FontMatrix=[.001,0,0,.001,0,0]
    for fd in top.FDArray:fd.FontName=name+'FD'
    original_name=font.getBestCmap()[ord('H')];original=top.CharStrings[original_name]
    vertical=font['vmtx'].metrics[original_name] if 'vmtx' in font else None
    original.draw(BoundsPen(top.CharStrings));width=original.width;private=original.private
    pen=T2CharStringPen(width-getattr(private,'nominalWidthX',0),None,roundTolerance=0)
    original.draw(pen);bottom=height-200
    pen.moveTo((200,bottom))
    for point in [(364,height),(528,bottom),(468,bottom-40),(364,height-120),(260,bottom-40)]:pen.lineTo(point)
    pen.closePath();alternate=pen.getCharString(private=private,globalSubrs=original.globalSubrs)
    alternate_name='cid00042';gid=len(top.charset);assert gid==2
    strings=top.CharStrings;index=len(strings.charStringsIndex);strings.charStringsIndex.append(alternate);strings.charStrings[alternate_name]=index
    fd=top.FDSelect[font.getGlyphID(original_name)];top.charset.append(alternate_name);top.FDSelect.gidArray.append(fd)
    font.setGlyphOrder(list(top.charset));font['hmtx'].metrics[alternate_name]=(int(width),101);font['maxp'].numGlyphs=len(top.charset)
    if vertical is not None:font['vmtx'].metrics[alternate_name]=vertical
    for table in font['cmap'].tables:
        if table.isUnicode() and coded:table.cmap[0x0124]=alternate_name
    font['OS/2'].fsType=0;output=OUT/(case+'.otf');font.save(output);font.close()
    controls[output.name]=hashlib.sha256(output.read_bytes()).hexdigest()
    with TTFont(output,recalcBBoxes=False,recalcTimestamp=False) as check:
        assert set(check.getGlyphSet())==set(check['vmtx'].metrics)
    ascent_roles[output.name]={'source_sha256':controls[output.name],'accented_source_glyph_ids':[2],'role':'H plus authored circumflex; accented-only role','cmap_identity':'U+0124' if coded else None,'authored_glyph_height':height,'expected_ascent':880}
(OUT/'ascent-facts.json').write_text(json.dumps({'schema':'patterbox-authored-ascent-controls/v1','scope':'Complete authored role set for retained source GIDs0,1,2; semantic truth is the authored premise','controls':ascent_roles},indent=2)+'\n',encoding='utf-8',newline='\n')
(OUT/'provenance.json').write_text(json.dumps({'schema':'patterbox-u2-font-controls/v1','cff_original_source_asset':SOURCE.relative_to(ROOT).as_posix(),'cff_original_source_sha256':hashlib.sha256(SOURCE.read_bytes()).hexdigest(),'cff_parent_subset_sha256':hashlib.sha256(parent_bytes).hexdigest(),'cff_license':'OFL-1.1; renamed derivatives; copyright metadata retained','truetype_license':'MIT OR Apache-2.0; original project control','control_scope':{'positive':'All controls except the explicitly malformed and normalization-only cases','malformed':['no-endchar.otf','partial.otf','subr-no-return.otf'],'normalization_characterization_only':['absent-top.otf'],'absent_top_limit':'Retained to inspect source/prepared normalization. It is excluded from positive qualification, including any large effective geometry or renderer overflow.'},'controls':controls},indent=2)+'\n',encoding='utf-8',newline='\n')





