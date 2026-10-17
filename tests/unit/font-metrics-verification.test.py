"""Independent descriptor checks against original authored source geometry."""
import hashlib
import importlib.util
import io
import math
import json
from pathlib import Path
import subprocess
import struct
import sys
import tempfile
import unittest

from fontTools import subset
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from pypdf import PdfWriter
from pypdf.generic import (ArrayObject, DecodedStreamObject, DictionaryObject,
                           FloatObject, NameObject, NumberObject)


ROOT = Path(__file__).resolve().parents[2]
CHECKER = ROOT / 'scripts/verify-font-metrics.py'
SPEC = importlib.util.spec_from_file_location('font_metrics', CHECKER)
metrics = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(metrics)


def original_font(*, upm=1000, with_h=True, declared_cap=700):
    """A known 80-unit H stem, accented high glyph and unused taller glyph.

    Deliberately generous hhea metrics are paragraph line metrics, whereas the
    retained unaccented outline ascent is 700. All outlines are self-authored.
    """
    order = ['.notdef', 'H', 'g', 'Aacute', 'Omega']
    builder = FontBuilder(upm, isTTF=True)
    builder.setupGlyphOrder(order)
    glyphs = {}
    for name, rectangles in {
        '.notdef': [(20, 0, 120, 500)],
        'H': [(40, 0, 120, 700), (420, 0, 500, 700), (120, 300, 420, 380)],
        'g': [(40, -180, 360, 500)],
        'Aacute': [(40, 0, 420, 700), (200, 800, 280, 920)],
        'Omega': [(40, 0, 420, 1100)],
    }.items():
        pen = TTGlyphPen(None)
        for left, bottom, right, top in rectangles:
            pen.moveTo((left, bottom)); pen.lineTo((left, top))
            pen.lineTo((right, top)); pen.lineTo((right, bottom)); pen.closePath()
        glyphs[name] = pen.glyph()
    builder.setupGlyf(glyphs)
    builder.setupHorizontalMetrics({name: (600, 20 if name == '.notdef' else 40) for name in order})
    builder.setupHorizontalHeader(ascent=1250, descent=-350)
    cmap = {103: 'g', 193: 'Aacute', 937: 'Omega'}
    if with_h:
        cmap[72] = 'H'
    builder.setupCharacterMap(cmap)
    builder.setupNameTable({'familyName': 'Authored metric fixture', 'styleName': 'Regular',
                            'psName': 'AuthoredMetric-Regular'})
    builder.setupOS2(version=4, sCapHeight=declared_cap)
    builder.setupPost(); builder.setupMaxp()
    output = io.BytesIO(); builder.save(output)
    return output.getvalue()


def fixture_pdf(source, *, upm=1000, changes=None, wrong_width=False):
    # The expected descriptor numbers follow the authored coordinates above;
    # none is calculated by the checker or copied from a production receipt.
    from fontTools.ttLib import TTFont
    font = TTFont(io.BytesIO(source), recalcTimestamp=False)
    options = subset.Options()
    options.notdef_outline = True
    sub = subset.Subsetter(options=options)
    sub.populate(glyphs=['.notdef', 'H', 'g', 'Aacute'])
    sub.subset(font)
    program_bytes = io.BytesIO(); font.save(program_bytes)
    writer = PdfWriter(); page = writer.add_blank_page(200, 200)
    program = DecodedStreamObject(); program.set_data(program_bytes.getvalue())
    def number(value):
        return FloatObject(value * 1000 / upm)
    descriptor = DictionaryObject({
        NameObject('/Type'): NameObject('/FontDescriptor'),
        NameObject('/FontName'): NameObject('/ABCDEF+AuthoredMetric-Regular'),
        NameObject('/Flags'): NumberObject(4),
        NameObject('/FontBBox'): ArrayObject([NumberObject((math.floor if i < 2 else math.ceil)(v * 1000 / upm))
                                            for i, v in enumerate((20, -180, 500, 920))]),
        NameObject('/Ascent'): number(700), NameObject('/Descent'): number(-180),
        NameObject('/CapHeight'): number(700), NameObject('/StemV'): number(80),
        NameObject('/ItalicAngle'): NumberObject(0),
        NameObject('/FontFile2'): writer._add_object(program),
    })
    for key, value in (changes or {}).items():
        descriptor[NameObject('/' + key)] = (ArrayObject([FloatObject(v) for v in value])
                                            if isinstance(value, list) else FloatObject(value))
    descendant = DictionaryObject({
        NameObject('/Type'): NameObject('/Font'), NameObject('/Subtype'): NameObject('/CIDFontType2'),
        NameObject('/FontDescriptor'): writer._add_object(descriptor),
        NameObject('/CIDToGIDMap'): NameObject('/Identity'),
        NameObject('/W'): ArrayObject([NumberObject(0), ArrayObject([number(600), number(700 if wrong_width else 600), number(600), number(600)])]),
    })
    cmap = DecodedStreamObject()
    cmap.set_data(b'''/CIDInit /ProcSet findresource begin
12 dict begin begincmap
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def
/CMapName /Authored def /CMapType 2 def
1 begincodespacerange <0000> <FFFF> endcodespacerange
3 beginbfchar <0001> <0048> <0002> <0067> <0003> <00C1> endbfchar
endcmap CMapName currentdict /CMap defineresource pop end end''')
    composite = DictionaryObject({
        NameObject('/Type'): NameObject('/Font'), NameObject('/Subtype'): NameObject('/Type0'),
        NameObject('/Encoding'): NameObject('/Identity-H'),
        NameObject('/DescendantFonts'): ArrayObject([writer._add_object(descendant)]),
        NameObject('/ToUnicode'): writer._add_object(cmap),
    })
    page[NameObject('/Resources')] = DictionaryObject({NameObject('/Font'): DictionaryObject({
        NameObject('/F1'): writer._add_object(composite)})})
    output = io.BytesIO(); writer.write(output)
    return output.getvalue()


def preparation_witness(source, program, **changes):
    witness = {'source_sha256': hashlib.sha256(source).hexdigest(),
               'prepared_sha256': hashlib.sha256(program).hexdigest(), 'face_index': 0,
               'preparation_kind': 'Subset', 'outline_kind': 'TrueType',
               'remapping': [{'source_gid': 0, 'cid': 0}, {'source_gid': 8, 'cid': 1}]}
    witness.update(changes)
    return witness


def component_fonts(*, point_attachment=False):
    """Authored nested/shared graph with a retained empty dependency and shuffled GIDs."""
    from fontTools.ttLib import TTFont
    from fontTools.ttLib.reorderGlyphs import reorderGlyphs
    font = TTFont(io.BytesIO(original_font()), recalcTimestamp=False)
    glyphs = font['glyf'].glyphs
    order = font.getGlyphOrder() + ['empty', 'inner', 'shared', 'root']
    pen = TTGlyphPen(glyphs); glyphs['empty'] = pen.glyph()
    for name, children in [('inner', [('H', 0), ('empty', 0)]),
                           ('shared', [('inner', 0), ('H', 600)]),
                           ('root', [('shared', 0), ('inner', 0)])]:
        pen = TTGlyphPen(glyphs)
        for child, x in children:
            pen.addComponent(child, (1, 0, 0, 1, x, 0))
        glyphs[name] = pen.glyph()
    font.setGlyphOrder(order)
    for name in order[5:]: font['hmtx'][name] = (600, 0 if name == 'empty' else 40)
    for table in font['cmap'].tables:
        if table.isUnicode(): table.cmap.update({0x2600: 'inner', 0x2601: 'shared', 0x2602: 'root'})
    if point_attachment:
        component = glyphs['root'].components[1]
        del component.x, component.y
        component.firstPt = 0; component.secondPt = 0
    source = io.BytesIO(); font.save(source)
    options = subset.Options(); options.notdef_outline = True
    sub = subset.Subsetter(options=options); sub.populate(glyphs=['root']); sub.subset(font)
    reorderGlyphs(font, ['.notdef', 'root', 'shared', 'empty', 'inner', 'H'])
    font['post'].formatType = 3.0
    prepared = io.BytesIO(); font.save(prepared); font.close()
    return source.getvalue(), prepared.getvalue()


def cff_fonts():
    """Self-authored CFF with a CID charset deliberately unlike GID order."""
    from fontTools.cffLib import FDArrayIndex, FDSelect, FontDict
    from fontTools.pens.t2CharStringPen import T2CharStringPen
    from fontTools.ttLib import TTFont
    original = TTFont(io.BytesIO(original_font()), recalcTimestamp=False)
    glyphs = original.getGlyphSet()
    def build(order, originals):
        builder = FontBuilder(1000, isTTF=False)
        builder.setupGlyphOrder(order)
        builder.setupCharacterMap({72: order[originals.index('H')], 103: order[originals.index('g')]})
        builder.setupHorizontalMetrics({name: (600, 20 if i == 0 else 40) for i, name in enumerate(order)})
        builder.setupHorizontalHeader(ascent=1250, descent=-350)
        builder.setupNameTable({'familyName': 'Authored metric fixture', 'styleName': 'Regular',
                                'psName': 'AuthoredMetric-Regular'})
        builder.setupOS2(version=4, sCapHeight=700); builder.setupPost()
        chars = {}
        for name, source_name in zip(order, originals):
            pen = T2CharStringPen(600, glyphs); glyphs[source_name].draw(pen)
            chars[name] = pen.getCharString()
        builder.setupCFF('AuthoredMetric-Regular', {}, chars, {})
        builder.setupMaxp()
        return builder
    builder = build(['.notdef', 'H', 'g'], ['.notdef', 'H', 'g'])
    output = io.BytesIO(); builder.save(output); source = output.getvalue(); builder.font.close()
    builder = build(['.notdef', 'cid00002', 'cid00001'], ['.notdef', 'g', 'H'])
    top = builder.font['CFF '].cff.topDictIndex[0]
    top.ROS = ('Adobe', 'Identity', 0); top.CIDCount = 3
    top.FDArray = FDArrayIndex(); fd = FontDict(); fd.Private = top.Private; top.FDArray.append(fd)
    top.FDSelect = FDSelect(format=3); top.FDSelect.gidArray = [0, 0, 0]
    del top.Private
    output = io.BytesIO(); builder.font['CFF '].cff.compile(output, builder.font); raw = output.getvalue()
    output = io.BytesIO(); builder.save(output); wrapper = output.getvalue(); builder.font.close(); original.close()
    return source, raw, wrapper


def program_pdf(program, *, bbox=(20, 0, 1100, 700), count=6, descent=0, cff=False):
    from pypdf import PdfReader
    reader = PdfReader(io.BytesIO(fixture_pdf(original_font())))
    writer = PdfWriter(); writer.clone_document_from_reader(reader)
    composite = writer.pages[0]['/Resources']['/Font']['/F1'].get_object()
    child = composite['/DescendantFonts'][0].get_object()
    descriptor = child['/FontDescriptor'].get_object()
    if cff:
        del descriptor['/FontFile2']
        stream = DecodedStreamObject(); stream.set_data(program)
        stream[NameObject('/Subtype')] = NameObject('/CIDFontType0C')
        descriptor[NameObject('/FontFile3')] = writer._add_object(stream)
        child[NameObject('/Subtype')] = NameObject('/CIDFontType0')
        del child['/CIDToGIDMap']
    else:
        descriptor['/FontFile2'].set_data(program)
    descriptor[NameObject('/FontBBox')] = ArrayObject([NumberObject(v) for v in bbox])
    descriptor[NameObject('/Descent')] = NumberObject(descent)
    child[NameObject('/W')] = ArrayObject([NumberObject(0), ArrayObject([NumberObject(600)] * count)])
    composite['/ToUnicode'].set_data(composite['/ToUnicode'].get_data().replace(
        b'3 beginbfchar <0001> <0048> <0002> <0067> <0003> <00C1>',
        b'1 beginbfchar <0001> <0067>'))
    output = io.BytesIO(); writer.write(output)
    return output.getvalue()


class FontMetricVerification(unittest.TestCase):
    def test_source_identity_ignores_other_name_encodings_without_repairing_selected_names(self):
        from fontTools.ttLib import TTFont
        from fontTools.ttLib.tables._n_a_m_e import makeName

        def identity(records):
            font = TTFont(io.BytesIO(original_font()), recalcTimestamp=False)
            try:
                font['name'].names = records
                return metrics._postscript_name(font)
            finally:
                font.close()

        chosen = 'AuthoredMetric-Regular'
        for platform, encoding in ((0, 4), (3, 0), (3, 1), (3, 10), (1, 0)):
            with self.subTest(platform=platform, encoding=encoding):
                raw = chosen.encode('mac_roman' if platform == 1 else 'utf-16-be')
                selected = makeName(raw, 6, platform, encoding, 0)
                # A different name in an ignored encoding is not an ambiguity.
                ignored = makeName(b'IgnoredLegacyIdentity', 6, 1, 1, 0)
                self.assertEqual(identity([selected, ignored]), chosen)
                conflicting = makeName(b'DifferentIdentity', 6, 1, 0, 0)
                with self.assertRaisesRegex(ValueError, 'unambiguous'):
                    identity([selected, conflicting])

        for raw in (b'\x00A\x00', b'\xd8\x00'):
            with self.subTest(malformed=raw), self.assertRaises(UnicodeError):
                identity([makeName(raw, 6, 3, 1, 0)])
        for text in ('', 'Contains Space', 'Bad/Delimiter', 'A' * 64, '\u00e9'):
            with self.subTest(invalid=text), self.assertRaisesRegex(ValueError, 'Invalid source'):
                identity([makeName(text.encode('utf-16-be'), 6, 3, 1, 0)])
        with self.assertRaisesRegex(ValueError, 'unambiguous'):
            identity([makeName(b'IgnoredLegacyIdentity', 6, 1, 1, 0)])

    def test_component_geometry_and_effective_advance_use_separate_phantom_roles(self):
        controls=ROOT/'apps/font-worker/src/tests/components'
        cases={
            'scaled-offset-clear':([5.5,.5,55.5,50.5],900),
            'scaled-offset-round':([5.5,.5,55.5,50.5],900),
            'point-match-clear':([5.5,.5,55.5,80.5],900),
            'point-match-round':([5.5,.5,55.5,80.5],900),
            'child-pp1':([20,0,165,100],900),
            'child-pp2':([20,0,315,100],900),
            'metrics-unmarked':([50,0,150,100],900),
            'metrics-marked':([20,0,120,100],600),
        }
        for name,(bounds,advance) in cases.items():
            with self.subTest(name=name):
                decoded=metrics._program((controls/(name+'.ttf')).read_bytes(),'/FontFile2')
                try:
                    self.assertEqual(decoded['bounds'][3],bounds)
                    self.assertEqual(decoded['advances'][3],advance)
                finally:decoded['font'].close()

    def test_linear_phantom_geometry_remains_measurable_when_engine_cannot_load_it(self):
        for name in ('child-pp1','child-pp2'):
            source=metrics._source((ROOT/'apps/font-worker/src/tests/components'/(name+'.ttf')).read_bytes())
            try:
                measured=metrics._stem(source)
                self.assertEqual(measured['measured_pdf_units'],100)
                self.assertIn('Native outline/raster unavailable',measured['raster_limitation'])
                self.assertEqual(measured['uncertainty_pdf_units'],0)
            finally:source['font'].close()

    def test_unused_source_context_and_optional_reference_are_not_false_empty(self):
        controls=ROOT/'apps/font-worker/src/tests/components'
        for name,expected in (('unused-unresolved-subset',100),('optional-h-unknown-subset',None),('optional-h-declared-subset',700)):
            source=metrics._source((controls/(name+'.ttf')).read_bytes())
            try:
                cap,observed,_=metrics._capital_height(source)
                self.assertEqual(cap,expected)
                if name.startswith('optional-h'):
                    self.assertEqual(observed['source_flat_cap_geometry_unresolved'],['H'])
            finally:source['font'].close()

    def test_raster_stem_samples_true_curve_bounds_instead_of_control_padding(self):
        from fontTools.pens.t2CharStringPen import T2CharStringPen
        builder = FontBuilder(1000, isTTF=False)
        builder.setupGlyphOrder(['.notdef', 'H'])
        builder.setupCharacterMap({72: 'H'})
        builder.setupHorizontalMetrics({name: (1100, 0) for name in ('.notdef', 'H')})
        builder.setupHorizontalHeader(ascent=1000, descent=0)
        builder.setupNameTable({'familyName': 'Authored cubic', 'styleName': 'Regular', 'psName': 'AuthoredCubic'})
        builder.setupOS2(); builder.setupPost()
        chars = {}
        for name in ('.notdef', 'H'):
            pen = T2CharStringPen(1100, None)
            if name == 'H':
                pen.moveTo((0, 0)); pen.curveTo((0, 1000), (1000, 1000), (1000, 0)); pen.closePath()
            chars[name] = pen.getCharString()
        builder.setupCFF('AuthoredCubic', {}, chars, {}); builder.setupMaxp()
        data = io.BytesIO(); builder.save(data); builder.font.close()
        source = metrics._source(data.getvalue())
        try:
            measured = metrics._stem(source)
            # y(t)=3000t(1-t) has actual top750; the median quarter-height
            # horizontal span at y375 is 1000*5*sqrt(2)/8, not a control-box slice.
            self.assertAlmostEqual(measured['measured_pdf_units'], 1000*5*math.sqrt(2)/8,
                                   delta=2*1000/4096)
        finally:
            source['font'].close()

    def test_curved_stem_accounts_for_actual_vertical_pixel_quantization(self):
        source = metrics._source((ROOT/'apps/font-worker/src/tests/fixtures/flex1_top.otf').read_bytes())
        try:
            measured = metrics._stem(source)
            lower, upper = measured.get('observed_median_interval_pdf_units',
                                        [measured['measured_pdf_units']]*2)
            # Independent 60-digit monotone-curve intersection calculation on
            # the authored effective curves; no native metric is read here.
            exact_curve_estimate = 119.66160827931705
            self.assertLessEqual(lower-measured['uncertainty_pdf_units'], exact_curve_estimate)
            self.assertGreaterEqual(upper+measured['uncertainty_pdf_units'], exact_curve_estimate)
        finally:
            source['font'].close()

    def test_linear_subpixel_stem_uses_decoded_geometry_when_freetype_loses_deltas(self):
        from fontTools.ttLib import TTFont
        from pypdf import PdfReader
        source = (ROOT/'apps/font-worker/src/tests/fixtures/accumulated-fraction.otf').read_bytes()
        font = TTFont(io.BytesIO(source), recalcBBoxes=False, recalcTimestamp=False)
        top = font['CFF '].cff.topDictIndex[0]
        old = top.charset[1]
        top.CharStrings.charStrings['cid00001'] = top.CharStrings.charStrings.pop(old)
        top.charset[1] = 'cid00001'; top.CIDCount = 2
        font.setGlyphOrder(list(top.charset))
        raw = io.BytesIO(); font['CFF '].cff.compile(raw,font); program = raw.getvalue(); font.close()
        writer = PdfWriter(); writer.clone_document_from_reader(PdfReader(io.BytesIO(
            program_pdf(program,bbox=(100,-120,1001,880),descent=-120,count=2,cff=True))))
        child = writer.pages[0]['/Resources']['/Font']['/F1'].get_object()['/DescendantFonts'][0].get_object()
        descriptor = child['/FontDescriptor'].get_object()
        descriptor[NameObject('/FontName')] = NameObject('/ABCDEF+PatterboxU2Accumulatedfraction')
        descriptor[NameObject('/Ascent')] = NumberObject(880)
        descriptor[NameObject('/CapHeight')] = NumberObject(200)
        descriptor[NameObject('/StemV')] = FloatObject(5000/65536)
        child[NameObject('/W')] = ArrayObject([NumberObject(0),ArrayObject([NumberObject(1000),NumberObject(728)])])
        witness = preparation_witness(source,program,outline_kind='Cff',
            remapping=[{'source_gid':0,'cid':0},{'source_gid':1,'cid':1}])
        def emitted():
            output = io.BytesIO(); writer.write(output); return output.getvalue()
        report = self.inspect_witness(source,program,[witness],pdf_bytes=emitted())
        self.assertTrue(report['passed'],report['findings'])
        self.assertEqual(report['observations']['fonts'][0]['stem_measurement']['measured_pdf_units'],5000/65536)
        descriptor[NameObject('/StemV')] = NumberObject(0)
        report = self.inspect_witness(source,program,[witness],pdf_bytes=emitted())
        self.assertIn('font-metric-stem-v',{row['rule'] for row in report['findings']})

    def test_linear_filled_intervals_group_exact_coincident_crossings(self):
        from fontTools.ttLib import TTFont
        from fontTools.pens.t2CharStringPen import T2CharStringPen
        original,_,_=cff_fonts()
        for reverse in (False,True):
            for gap,expected in ((0,200),(1/65536,100)):
                with self.subTest(reverse=reverse,gap=gap):
                    font=TTFont(io.BytesIO(original),recalcTimestamp=False)
                    top=font['CFF '].cff.topDictIndex[0]
                    pen=T2CharStringPen(600,None,roundTolerance=0)
                    boxes=[(0,100),(100+gap,200+gap)]
                    for left,right in reversed(boxes) if reverse else boxes:
                        pen.moveTo((left,0));pen.lineTo((left,100));pen.lineTo((right,100))
                        pen.lineTo((right,0));pen.closePath()
                    top.CharStrings['H']=pen.getCharString(private=top.Private)
                    output=io.BytesIO();font.save(output);font.close()
                    source=metrics._source(output.getvalue())
                    try:
                        observed=metrics._linear_stem(source,'H','Authored line-only control')
                        self.assertEqual(observed['measured_pdf_units'],expected)
                        self.assertEqual(len(observed['samples'][0]['filled_intervals_pdf_units']),1 if gap==0 else 2)
                    finally: source['font'].close()

    def inspect_authored_ascent(self, *, facts_change=None, witness_change=None, ascent=700, bind_facts=False, facts_count=1):
        from fontTools.ttLib import TTFont
        from pypdf import PdfReader
        # The third glyph is an authored accented design whose cmap identity is
        # absent. The separate facts describe that role; the producer does not.
        font = TTFont(io.BytesIO(original_font()), recalcTimestamp=False)
        for table in font['cmap'].tables:
            table.cmap = {code: name for code, name in table.cmap.items() if name != 'Aacute'}
        stream = io.BytesIO(); font.save(stream); font.close(); source = stream.getvalue()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_path = root/'source.ttf'; source_path.write_bytes(source)
            pdf = root/'example.pdf'; pdf.write_bytes(fixture_pdf(source, changes={'Ascent': ascent}))
            font_object = PdfReader(pdf).pages[0]['/Resources']['/Font']['/F1'].get_object()
            program = font_object['/DescendantFonts'][0].get_object()['/FontDescriptor']['/FontFile2'].get_data()
            facts = {'source_sha256': hashlib.sha256(source).hexdigest(), 'face_index': 0,
                     'accented_source_glyph_ids': [3]}
            facts.update(facts_change or {})
            witness = preparation_witness(source, program,
                remapping=[{'source_gid': gid, 'cid': gid} for gid in range(4)])
            if bind_facts:
                # Independent encoding of the documented authority identity.
                digest_input = (b'publisher-v2/font-ascent-facts/v1\0' + hashlib.sha256(source).digest() +
                                (0).to_bytes(4, 'big') + (1).to_bytes(4, 'big') + (3).to_bytes(2, 'big'))
                witness['ascent_facts_sha256'] = hashlib.sha256(digest_input).hexdigest()
            witness.update(witness_change or {})
            facts_path = root/'authored-facts.json'; facts_path.write_text(json.dumps(facts))
            witness_path = root/'prepared.json'; witness_path.write_text(json.dumps(witness))
            return metrics.inspect_pdf_metrics(pdf, [source_path], preparation_reports=[witness_path],
                                               ascent_facts=[facts_path] * facts_count)

    def test_authored_ascent_facts_measure_unencoded_role_independently(self):
        report = self.inspect_authored_ascent()
        self.assertTrue(report['passed'], report['findings'])
        observed = report['observations']['fonts'][0]
        self.assertEqual(observed['expected']['Ascent'], 700)
        self.assertEqual(observed['accented_gids_excluded_from_ascent'], [3])
        self.assertEqual(observed['ascent_classification'], 'separate-authored-source-gid-facts')
        wrong = self.inspect_authored_ascent(ascent=920)
        self.assertIn('font-metric-ascent', {row['rule'] for row in wrong['findings']})

    def test_authored_ascent_facts_require_canonical_scoped_source_gids(self):
        controls = [
            {'source_sha256': '0'*64}, {'face_index': 1},
            {'accented_source_glyph_ids': [5]}, {'accented_source_glyph_ids': [0]},
            {'accented_source_glyph_ids': [3, 3]}, {'accented_source_glyph_ids': [3, 2]},
            {'accented_source_glyph_ids': [True]}, {'accented_source_glyph_ids': ['3']},
            {'unexpected': 'producer-approved'},
        ]
        for changed in controls:
            with self.subTest(changed=changed):
                result = self.inspect_authored_ascent(facts_change=changed)
                self.assertFalse(result['passed'])
                self.assertIn('font-metric-ascent-facts', {row['rule'] for row in result['findings']})
        # Valid unretained source roles are permitted; no requested-subset cap.
        result = self.inspect_authored_ascent(facts_change={'accented_source_glyph_ids': [3, 4]})
        self.assertTrue(result['passed'], result['findings'])

    def test_authored_ascent_identity_is_separate_from_producer_metrics(self):
        result = self.inspect_authored_ascent(bind_facts=True,
            witness_change={'metrics': {'ascent': -999}, 'requires_ascent_facts': True})
        self.assertTrue(result['passed'], result['findings'])
        wrong = self.inspect_authored_ascent(witness_change={'ascent_facts_sha256': '0'*64})
        self.assertIn('font-metric-ascent-facts', {row['rule'] for row in wrong['findings']})
        for facts_count in (0, 2):
            with self.subTest(facts_count=facts_count):
                wrong = self.inspect_authored_ascent(bind_facts=True, facts_count=facts_count)
                self.assertIn('font-metric-ascent-facts', {row['rule'] for row in wrong['findings']})
        # A syntactically valid assertion has no runtime semantic-truth oracle.
        # The supplied empty set changes the independently computed maximum;
        # it is not mislabeled malformed merely because the authored set is false.
        wrong = self.inspect_authored_ascent(facts_change={'accented_source_glyph_ids': []})
        self.assertNotIn('font-metric-ascent-facts', {row['rule'] for row in wrong['findings']})
        self.assertIn('font-metric-ascent', {row['rule'] for row in wrong['findings']})
        self.assertEqual(wrong['observations']['fonts'][0]['expected']['Ascent'], 920)

    def test_authored_ascent_cli_accepts_separate_facts_and_rejects_duplicate_scope(self):
        source, program = component_fonts()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_path = root/'source.ttf'; source_path.write_bytes(source)
            pdf = root/'example.pdf'; pdf.write_bytes(program_pdf(program))
            witness = root/'prepared.json'; witness.write_text(json.dumps(preparation_witness(source, program)))
            facts = root/'facts.json'; facts.write_text(json.dumps({'source_sha256': hashlib.sha256(source).hexdigest(),
                'face_index': 0, 'accented_source_glyph_ids': []}))
            command = [sys.executable, str(CHECKER), '--pdf', str(pdf), '--source-font', str(source_path),
                       '--preparation-report', str(witness), '--ascent-facts', str(facts), '--output', str(root/'report.json')]
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            command[-1] = str(root/'duplicate.json')
            result = subprocess.run(command + ['--ascent-facts', str(facts)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn('font-metric-ascent-facts', result.stdout)

    def inspect(self, changes=None, *, upm=1000, source_options=None):
        source = original_font(upm=upm, **(source_options or {}))
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            pdf = root / 'example.pdf'; pdf.write_bytes(fixture_pdf(source, upm=upm, changes=changes))
            path = root / 'source.ttf'; path.write_bytes(source)
            before = hashlib.sha256(pdf.read_bytes()).hexdigest()
            report = metrics.inspect_pdf_metrics(pdf, [path])
            self.assertEqual(before, hashlib.sha256(pdf.read_bytes()).hexdigest())
            self.assertEqual(before, report['observations']['input_sha256_after'])
            return report

    def test_actual_retained_geometry_and_native_stem_measurement_pass(self):
        report = self.inspect()
        self.assertEqual(report['findings'], [])
        self.assertTrue(report['passed'])
        font = report['observations']['fonts'][0]
        self.assertEqual(font['expected']['Ascent'], 700)
        self.assertEqual(font['emitted']['Ascent'],700)
        self.assertEqual(font['emitted']['CapHeight'],700)
        self.assertTrue(font['width_comparisons'])
        self.assertTrue(all(row['emitted']==row['expected'] for row in font['width_comparisons']))
        self.assertEqual(font['expected']['FontBBox'], [20, -180, 500, 920])
        self.assertEqual(font['accented_gids_excluded_from_ascent'], [3])
        self.assertEqual(font['unmapped_gids'], [0])
        self.assertAlmostEqual(font['stem_measurement']['measured_pdf_units'], 80, delta=0.5)
        self.assertEqual(font['stem_measurement']['ppem'], 4096)
        self.assertTrue(report['evaluator_sha256'])

    def test_descriptor_style_flags_agree_with_the_explicit_source_face(self):
        for flags in (5,68):
            with self.subTest(flags=flags):
                report = self.inspect({'Flags':flags})
                self.assertIn('font-metric-flags',{row['rule'] for row in report['findings']})

    def test_wrong_bbox_ascent_descent_capital_and_stem_are_rejected(self):
        for key, wrong, rule in (
            ('FontBBox', [20, -180, 500, 1250], 'font-metric-bbox'),
            ('Ascent', 1250, 'font-metric-ascent'),
            ('Descent', -350, 'font-metric-descent'),
            ('CapHeight', 920, 'font-metric-cap-height'),
            ('StemV', 0, 'font-metric-stem-v'),
            ('StemV', 180, 'font-metric-stem-v'),
        ):
            with self.subTest(key=key, wrong=wrong):
                report = self.inspect({key: wrong})
                self.assertFalse(report['passed'])
                self.assertIn(rule, {f['rule'] for f in report['findings']})

    def test_non_integer_metrics_use_exact_binary32_roundtrip(self):
        for upm in (2048,1001):
            with self.subTest(upm=upm):
                report = self.inspect(upm=upm)
                self.assertTrue(report['passed'],report['findings'])
        self.assertEqual(metrics.scaled(600,1000/1001),metrics.f32(600000/1001))
        report = self.inspect({'CapHeight': 341.8}, upm=2048)
        self.assertIn('font-metric-cap-height', {f['rule'] for f in report['findings']})

    def test_missing_source_and_missing_measurement_cannot_pass(self):
        with tempfile.TemporaryDirectory() as temporary:
            pdf = Path(temporary) / 'input.pdf'; pdf.write_bytes(fixture_pdf(original_font()))
            report = metrics.inspect_pdf_metrics(pdf, [])
            self.assertFalse(report['passed'])
            self.assertIn('font-metric-source-unresolved', {f['rule'] for f in report['findings']})
        report = self.inspect(source_options={'with_h': False})
        self.assertFalse(report['passed'])
        self.assertIn('font-metric-stem-v', {f['rule'] for f in report['findings']})
        self.assertEqual(report['observations']['fonts'][0]['stem_measurement']['source_glyph'], '.notdef')

    def test_capital_geometry_fallback_and_source_designer_declaration(self):
        self.assertTrue(self.inspect(source_options={'declared_cap': 0})['passed'])
        report = self.inspect(source_options={'declared_cap': 900})
        self.assertFalse(report['passed'])
        self.assertIn('font-metric-cap-height', {f['rule'] for f in report['findings']})

    def test_source_designer_capital_units_are_global_and_independent_of_cff_grid(self):
        from fontTools.ttLib import TTFont
        source,_,_ = cff_fonts()
        for upm,declaration,matrix in ((1000,700,[.0005,0,0,.0005,0,0]),
                                      (2000,1400,[.001,0,0,.002,15,20])):
            with self.subTest(upm=upm,matrix=matrix):
                font = TTFont(io.BytesIO(source),recalcTimestamp=False)
                font['head'].unitsPerEm = upm
                font['OS/2'].sCapHeight = declaration
                font['CFF '].cff.topDictIndex[0].FontMatrix = matrix
                # Aliased or absent reference labels do not change a global declaration.
                for table in font['cmap'].tables:
                    if table.isUnicode(): table.cmap = {81:'H',72:'H'}
                stream=io.BytesIO(); font.save(stream); font.close()
                face=metrics._source(stream.getvalue())
                try:
                    expected,observations,_=metrics._capital_height(face)
                    self.assertEqual(expected,700)
                    self.assertIsNone(observations['capital_reference'])
                finally: face['font'].close()

    def test_pdf_advance_mutation_is_rejected_against_decoded_font(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'source.ttf'; source.write_bytes(original_font())
            pdf = root / 'width.pdf'; pdf.write_bytes(fixture_pdf(source.read_bytes(), wrong_width=True))
            report = metrics.inspect_pdf_metrics(pdf, [source])
            self.assertFalse(report['passed'])
            self.assertIn('font-metric-width', {f['rule'] for f in report['findings']})

    def test_exact_preparation_witness_keeps_unicode_mapping_independent_of_source_glyph(self):
        from pypdf import PdfReader
        source = original_font()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root/'source.ttf'; path.write_bytes(source)
            # The caller may map H's shape to logical g. Text is not a source-GID oracle.
            pdf = root/'logical.pdf'
            pdf.write_bytes(fixture_pdf(source).replace(b'<0001> <0048>', b'<0001> <0067>'))
            reader = PdfReader(pdf)
            font = reader.pages[0]['/Resources']['/Font']['/F1'].get_object()
            program = font['/DescendantFonts'][0].get_object()['/FontDescriptor']['/FontFile2'].get_data()
            witness = {'source_sha256': hashlib.sha256(source).hexdigest(),
                       'prepared_sha256': hashlib.sha256(program).hexdigest(), 'face_index': 0,
                       'preparation_kind': 'Subset', 'outline_kind': 'TrueType',
                       'remapping': [{'source_gid': gid, 'cid': gid} for gid in range(4)]}
            report_path = root/'prepared.json'; report_path.write_text(json.dumps(witness))
            report = metrics.inspect_pdf_metrics(pdf, [path], preparation_reports=[report_path])
            self.assertTrue(report['passed'], report['findings'])
            self.assertEqual(report['observations']['fonts'][0]['source_gid_map'], {'0': 0, '1': 1, '2': 2, '3': 3})
            for mutation in ('source', 'program', 'mapping', 'duplicate'):
                with self.subTest(mutation=mutation):
                    changed = json.loads(json.dumps(witness))
                    if mutation == 'source': changed['source_sha256'] = '0'*64
                    elif mutation == 'program': changed['prepared_sha256'] = '0'*64
                    elif mutation == 'mapping': changed['remapping'][1]['source_gid'] = 2
                    else: changed['remapping'].append(changed['remapping'][0])
                    report_path.write_text(json.dumps(changed))
                    result = metrics.inspect_pdf_metrics(pdf, [path], preparation_reports=[report_path])
                    self.assertFalse(result['passed'])
                    self.assertTrue(any(row['rule'] in ('font-metric-preparation-identity', 'font-metric-source-outline', 'font-metric-unresolved')
                                        for row in result['findings']), result['findings'])

    def inspect_witness(self, source, program, witnesses, *, pdf_bytes=None, font_facts=None):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            path = root/'source.ttf'; path.write_bytes(source)
            pdf = root/'example.pdf'; pdf.write_bytes(pdf_bytes or program_pdf(program))
            reports = []
            for index, witness in enumerate(witnesses):
                report_path = root/f'prepared-{index}.json'
                report_path.write_text(json.dumps(witness)); reports.append(report_path)
            options = {}
            if font_facts is not None:
                fact_paths = []
                for index, facts in enumerate(font_facts):
                    fact_path = root/f'font-facts-{index}.json'
                    fact_path.write_text(json.dumps(facts)); fact_paths.append(fact_path)
                options['font_facts'] = fact_paths
            return metrics.inspect_pdf_metrics(pdf, [path], preparation_reports=reports, **options)

    def test_separate_capital_design_fact_fills_unknown_and_preserves_computed_metrics(self):
        from fontTools.ttLib import TTFont
        from pypdf import PdfReader
        font = TTFont(io.BytesIO(original_font(with_h=False)), recalcTimestamp=False)
        font['OS/2'].version = 0
        encoded = io.BytesIO(); font.save(encoded); font.close(); source = encoded.getvalue()
        pdf = fixture_pdf(source,changes={'StemV':100})
        reader = PdfReader(io.BytesIO(pdf))
        program = reader.pages[0]['/Resources']['/Font']['/F1']['/DescendantFonts'][0]['/FontDescriptor']['/FontFile2'].get_data()
        witness = preparation_witness(source,program,
            remapping=[{'source_gid':gid,'cid':gid} for gid in range(4)])
        facts = {'source_sha256':hashlib.sha256(source).hexdigest(),'face_index':0,
                 'accented_source_glyph_ids':[3],'cap_height_pdf_units':700}
        report = self.inspect_witness(source,program,[witness],pdf_bytes=pdf,font_facts=[facts])
        self.assertTrue(report['passed'],report['findings'])
        self.assertEqual(report['observations']['fonts'][0]['expected']['CapHeight'],700)
        for wrong in (0,701):
            changed = fixture_pdf(source,changes={'CapHeight':wrong,'StemV':100})
            report = self.inspect_witness(source,program,[witness],pdf_bytes=changed,font_facts=[facts])
            self.assertIn('font-metric-cap-height',{row['rule'] for row in report['findings']})
        known = original_font(); known_pdf = fixture_pdf(known)
        embedded = PdfReader(io.BytesIO(known_pdf)).pages[0]['/Resources']['/Font']['/F1']['/DescendantFonts'][0]['/FontDescriptor']['/FontFile2'].get_data()
        conflict = dict(facts,source_sha256=hashlib.sha256(known).hexdigest(),cap_height_pdf_units=701)
        report = self.inspect_witness(known,embedded,[preparation_witness(known,embedded,
            remapping=[{'source_gid':gid,'cid':gid} for gid in range(4)])],pdf_bytes=known_pdf,font_facts=[conflict])
        self.assertIn('font-metric-cap-height-facts',{row['rule'] for row in report['findings']})

    def test_combined_design_facts_reject_invalid_fields_and_stale_identity(self):
        source = original_font(); pdf = fixture_pdf(source)
        from pypdf import PdfReader
        program = PdfReader(io.BytesIO(pdf)).pages[0]['/Resources']['/Font']['/F1']['/DescendantFonts'][0]['/FontDescriptor']['/FontFile2'].get_data()
        witness = preparation_witness(source,program,
            remapping=[{'source_gid':gid,'cid':gid} for gid in range(4)])
        facts = {'source_sha256':hashlib.sha256(source).hexdigest(),'face_index':0,
                 'cap_height_pdf_units':700}
        for change in ({'cap_height_pdf_units':True},{'cap_height_pdf_units':float('nan')},
                       {'cap_height_pdf_units':10000001},{'cap_height_pdf_units':None},
                       {'source_sha256':'0'*64},{'face_index':1},{'unexpected':True}):
            with self.subTest(change=change):
                report = self.inspect_witness(source,program,[witness],pdf_bytes=pdf,
                                              font_facts=[dict(facts,**change)])
                self.assertFalse(report['passed'])
        expected = hashlib.sha256(b'publisher-v2/font-design-facts/v1\0'+hashlib.sha256(source).digest()+
            bytes(4)+b'\0\1'+struct.pack('>f',700)).hexdigest()
        report = self.inspect_witness(source,program,[dict(witness,font_facts_sha256=expected)],
                                      pdf_bytes=pdf,font_facts=[facts])
        self.assertTrue(report['passed'],report['findings'])
        stale = self.inspect_witness(source,program,[dict(witness,font_facts_sha256='0'*64)],
                                     pdf_bytes=pdf,font_facts=[facts])
        self.assertIn('font-metric-font-facts',{row['rule'] for row in stale['findings']})
        zero = dict(facts,cap_height_pdf_units=0.0)
        self.assertEqual(metrics._font_facts_digest(zero),metrics._font_facts_digest(dict(zero,cap_height_pdf_units=-0.0)))
        self.assertNotEqual(metrics._font_facts_digest(zero),metrics._font_facts_digest(dict(zero,accented_source_glyph_ids=[])))

    def test_old_design_table_l_and_empty_h_have_required_measurable_capital_height(self):
        from pypdf import PdfReader
        for letter,capital,ascent,stem in [('L',700,700,80),('H',0,500,100)]:
            with self.subTest(letter=letter):
                builder = FontBuilder(1000,isTTF=True)
                builder.setupGlyphOrder(['.notdef',letter]); builder.setupCharacterMap({ord(letter):letter})
                glyphs = {}
                for name in ('.notdef',letter):
                    pen = TTGlyphPen(None)
                    points = ([(20,0),(20,500),(120,500),(120,0)] if name=='.notdef' else
                              [(40,0),(40,700),(120,700),(120,80),(500,80),(500,0)] if letter=='L' else [])
                    if points:
                        pen.moveTo(points[0])
                        for point in points[1:]: pen.lineTo(point)
                        pen.closePath()
                    glyphs[name] = pen.glyph()
                builder.setupGlyf(glyphs)
                builder.setupHorizontalMetrics({'.notdef':(600,20),letter:(600,40 if letter=='L' else 0)})
                builder.setupHorizontalHeader(ascent=800,descent=0)
                builder.setupNameTable({'familyName':'Authored metric fixture','styleName':'Regular','psName':'AuthoredMetric-Regular'})
                builder.setupOS2(version=0); builder.setupPost(); builder.setupMaxp()
                encoded=io.BytesIO(); builder.save(encoded); builder.font.close(); source=encoded.getvalue()
                pdf = program_pdf(source,bbox=(20,0,500 if letter=='L' else 120,ascent),count=2)
                writer=PdfWriter(); writer.clone_document_from_reader(PdfReader(io.BytesIO(pdf)))
                descriptor=writer.pages[0]['/Resources']['/Font']['/F1']['/DescendantFonts'][0]['/FontDescriptor']
                descriptor[NameObject('/CapHeight')]=NumberObject(capital)
                descriptor[NameObject('/Ascent')]=NumberObject(ascent); descriptor[NameObject('/StemV')]=NumberObject(stem)
                witness=preparation_witness(source,source,preparation_kind='Full',remapping=[{'source_gid':1,'cid':1}])
                def inspect():
                    output=io.BytesIO(); writer.write(output)
                    return self.inspect_witness(source,source,[witness],pdf_bytes=output.getvalue())
                report=inspect(); self.assertTrue(report['passed'],report['findings'])
                self.assertEqual(report['observations']['fonts'][0]['expected']['CapHeight'],capital)
                del descriptor['/CapHeight']
                self.assertIn('font-metric-cap-height',{row['rule'] for row in inspect()['findings']})

    def test_witness_reconstructs_nested_shared_and_empty_component_tails(self):
        source, program = component_fonts()
        report = self.inspect_witness(source, program, [preparation_witness(source, program)])
        self.assertTrue(report['passed'], report['findings'])
        self.assertEqual(report['observations']['fonts'][0]['source_gid_map'],
                         {'0': 0, '1': 8, '2': 7, '3': 5, '4': 6, '5': 1})
        self.assertTrue(report['observations']['preparation_reports_unchanged'])
        observed=report['observations']['preparation_reports'][0]
        self.assertEqual(observed['source_sha256'],metrics.sha(source))
        self.assertEqual(observed['prepared_sha256'],metrics.sha(program))
        self.assertEqual(observed['face_index'],0)

    def test_witness_missing_conflicting_or_duplicate_anchors_are_findings(self):
        source, program = component_fonts()
        original = preparation_witness(source, program)
        controls = [[], [original['remapping'][1]], [original['remapping'][0]],
                    original['remapping'] + [{'source_gid': 2, 'cid': 2}],
                    original['remapping'] + [original['remapping'][0]]]
        for anchors in controls:
            with self.subTest(anchors=anchors):
                witness = preparation_witness(source, program, remapping=anchors)
                report = self.inspect_witness(source, program, [witness])
                self.assertFalse(report['passed'])
                self.assertIn('font-metric-preparation-identity', {f['rule'] for f in report['findings']})
        for witnesses in ([], [original, original]):
            with self.subTest(witness_count=len(witnesses)):
                report = self.inspect_witness(source, program, witnesses)
                self.assertIn('font-metric-preparation-identity', {f['rule'] for f in report['findings']})

    def test_point_attachment_records_are_matched_without_converting_to_xy(self):
        from fontTools.ttLib import TTFont
        source, program = component_fonts(point_attachment=True)
        witness = preparation_witness(source, program)
        report = self.inspect_witness(source, program, [witness])
        self.assertTrue(report['passed'], report['findings'])
        font = TTFont(io.BytesIO(program), recalcTimestamp=False)
        font['glyf'][font.getGlyphOrder()[1]].components[1].secondPt = 1
        output = io.BytesIO(); font.save(output); font.close(); changed = output.getvalue()
        report = self.inspect_witness(source, changed, [preparation_witness(source, changed)])
        self.assertIn('font-metric-preparation-identity', {f['rule'] for f in report['findings']})

    def test_unrequested_geometry_and_empty_advance_still_receive_source_comparison(self):
        from fontTools.ttLib import TTFont
        source, original = component_fonts()
        for mutation, gid, rule in (('geometry', 5, 'font-metric-source-outline'),
                                    ('advance', 3, 'font-metric-source-width')):
            with self.subTest(mutation=mutation):
                font = TTFont(io.BytesIO(original), recalcTimestamp=False)
                name = font.getGlyphOrder()[gid]
                if mutation == 'geometry':
                    glyph = font['glyf'][name]
                    glyph.coordinates[0] = (glyph.coordinates[0][0] - 1, glyph.coordinates[0][1])
                else: font['hmtx'][name] = (601, 0)
                output = io.BytesIO(); font.save(output); font.close(); program = output.getvalue()
                report = self.inspect_witness(source, program, [preparation_witness(source, program)])
                self.assertTrue(any(f['rule'] == rule and f.get('gid') == gid for f in report['findings']), report['findings'])

    def test_cff_witness_uses_charset_cids_and_exact_raw_program_hash(self):
        source, program, wrapper = cff_fonts()
        witness = preparation_witness(source, program, outline_kind='Cff',
            remapping=[{'source_gid': gid, 'cid': gid} for gid in range(3)])
        pdf = program_pdf(program, bbox=(20, -180, 500, 700), count=3, descent=-180, cff=True)
        report = self.inspect_witness(source, program, [witness], pdf_bytes=pdf)
        self.assertTrue(report['passed'], report['findings'])
        self.assertEqual(report['observations']['fonts'][0]['source_gid_map'], {'0': 0, '1': 2, '2': 1})
        for mutation in ('wrapper_hash', 'missing_cid', 'outline_kind'):
            with self.subTest(mutation=mutation):
                changed = json.loads(json.dumps(witness))
                if mutation == 'wrapper_hash': changed['prepared_sha256'] = hashlib.sha256(wrapper).hexdigest()
                elif mutation == 'missing_cid': changed['remapping'].pop()
                else: changed['outline_kind'] = 'TrueType'
                result = self.inspect_witness(source, program, [changed], pdf_bytes=pdf)
                self.assertIn('font-metric-preparation-identity', {f['rule'] for f in result['findings']})

    def test_full_program_uses_exact_bytes_and_complete_identity_despite_sparse_requests(self):
        source = original_font()
        witness = preparation_witness(source, source, preparation_kind='Full',
            remapping=[{'source_gid': 3, 'cid': 3}])
        pdf = program_pdf(source, bbox=(20, -180, 500, 1100), count=5)
        # Full embedding includes the unused taller ordinary glyph and descended g.
        from pypdf import PdfReader
        reader = PdfReader(io.BytesIO(pdf)); writer = PdfWriter(); writer.clone_document_from_reader(reader)
        descriptor = writer.pages[0]['/Resources']['/Font']['/F1']['/DescendantFonts'][0]['/FontDescriptor']
        descriptor[NameObject('/Ascent')] = NumberObject(1100)
        descriptor[NameObject('/Descent')] = NumberObject(-180)
        output = io.BytesIO(); writer.write(output)
        report = self.inspect_witness(source, source, [witness], pdf_bytes=output.getvalue())
        self.assertTrue(report['passed'], report['findings'])
        self.assertEqual(report['observations']['fonts'][0]['source_gid_map'], {str(gid): gid for gid in range(5)})
        subset_source, subset_program = component_fonts()
        result = self.inspect_witness(subset_source, subset_program, [preparation_witness(
            subset_source, subset_program, preparation_kind='Full')])
        self.assertIn('font-metric-preparation-identity', {f['rule'] for f in result['findings']})

    def test_collection_requires_the_exact_explicit_source_face(self):
        from fontTools.ttLib import TTCollection, TTFont
        source, program = component_fonts()
        collection = TTCollection()
        collection.fonts = [TTFont(io.BytesIO(original_font()), recalcTimestamp=False),
                            TTFont(io.BytesIO(source), recalcTimestamp=False)]
        output = io.BytesIO(); collection.save(output); collection.close(); data = output.getvalue()
        witness = preparation_witness(data, program, face_index=1)
        report = self.inspect_witness(data, program, [witness])
        self.assertTrue(report['passed'], report['findings'])
        self.assertEqual(report['observations']['fonts'][0]['source_face_index'], 1)
        for face in (0, 2):
            with self.subTest(face=face):
                result = self.inspect_witness(data, program, [dict(witness, face_index=face)])
                self.assertIn('font-metric-preparation-identity', {f['rule'] for f in result['findings']})
        result = self.inspect_witness(source, program, [preparation_witness(source, program, face_index=1)])
        self.assertIn('font-metric-preparation-identity', {f['rule'] for f in result['findings']})

    def test_witness_ignores_producer_metrics_and_preserves_text_validation(self):
        source, program = component_fonts()
        witness = preparation_witness(source, program, metrics={'bbox': [0, 0, 0, 0]}, passed=False)
        report = self.inspect_witness(source, program, [witness])
        self.assertTrue(report['passed'], report['findings'])
        invalid = program_pdf(program).replace(b'<0001> <0067>', b'<0001> <D800>')
        report = self.inspect_witness(source, program, [witness], pdf_bytes=invalid)
        self.assertFalse(report['passed'])
        self.assertTrue(any('ToUnicode' in f['detail'] for f in report['findings']), report['findings'])

    def test_witness_cli_accepts_repeated_reports_and_checks_both(self):
        source, program = component_fonts()
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source_path = root/'source.ttf'; source_path.write_bytes(source)
            pdf = root/'example.pdf'; pdf.write_bytes(program_pdf(program))
            witness = root/'prepared.json'; witness.write_text(json.dumps(preparation_witness(source, program)))
            command = [sys.executable, str(CHECKER), '--pdf', str(pdf), '--source-font', str(source_path),
                       '--preparation-report', str(witness), '--output', str(root/'report.json')]
            result = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(result.returncode, 0, result.stdout + result.stderr)
            command[-1] = str(root/'duplicate.json')
            result = subprocess.run(command + ['--preparation-report', str(witness)], capture_output=True, text=True)
            self.assertEqual(result.returncode, 1, result.stdout + result.stderr)
            self.assertIn('font-metric-preparation-identity', result.stdout)

    def test_component_record_changes_and_unreachable_tail_are_identity_findings(self):
        from fontTools.ttLib import TTFont
        source, original = component_fonts()
        for mutation in ('argument', 'transform', 'flags', 'child', 'unreachable'):
            with self.subTest(mutation=mutation):
                font = TTFont(io.BytesIO(original), recalcTimestamp=False)
                root = font['glyf'][font.getGlyphOrder()[1]]
                if mutation == 'argument': root.components[0].x += 1
                elif mutation == 'transform': root.components[0].transform = [[0.5, 0], [0, 0.5]]
                elif mutation == 'flags': root.components[0].flags ^= 4
                elif mutation == 'child': root.components[1].glyphName = font.getGlyphOrder()[2]
                else:
                    name = 'extra'; font.setGlyphOrder(font.getGlyphOrder() + [name])
                    font['glyf'].glyphs[name] = TTGlyphPen(None).glyph(); font['hmtx'][name] = (600, 0)
                output = io.BytesIO(); font.save(output); font.close(); program = output.getvalue()
                report = self.inspect_witness(source, program, [preparation_witness(source, program)])
                self.assertIn('font-metric-preparation-identity', {f['rule'] for f in report['findings']})

    def test_cli_preserves_input_and_refuses_to_overwrite_receipt(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'source.ttf'; source.write_bytes(original_font())
            pdf = root / 'example.pdf'; pdf.write_bytes(fixture_pdf(source.read_bytes()))
            output = root / 'report.json'
            command = [sys.executable, str(CHECKER), '--pdf', str(pdf), '--source-font', str(source),
                       '--output', str(output)]
            first = subprocess.run(command, capture_output=True, text=True)
            self.assertEqual(first.returncode, 0, first.stderr)
            receipt = output.read_bytes()
            self.assertTrue(json.loads(receipt)['passed'])
            second = subprocess.run(command, capture_output=True, text=True)
            self.assertNotEqual(second.returncode, 0)
            self.assertEqual(output.read_bytes(), receipt)


if __name__ == '__main__':
    unittest.main()
