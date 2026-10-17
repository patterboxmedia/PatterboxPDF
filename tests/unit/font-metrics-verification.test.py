"""Independent descriptor checks against original authored source geometry."""
import hashlib
import importlib.util
import io
import json
from pathlib import Path
import subprocess
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
        NameObject('/FontBBox'): ArrayObject([number(v) for v in (20, -180, 500, 920)]),
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


class FontMetricVerification(unittest.TestCase):
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
        self.assertTrue(self.inspect(upm=2048)['passed'])
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
        self.assertIn('font-metric-stem-unresolved', {f['rule'] for f in report['findings']})

    def test_capital_geometry_fallback_and_inconsistent_source_declaration(self):
        self.assertTrue(self.inspect(source_options={'declared_cap': 0})['passed'])
        report = self.inspect(source_options={'declared_cap': 900})
        self.assertFalse(report['passed'])
        self.assertIn('font-metric-cap-unresolved', {f['rule'] for f in report['findings']})

    def test_pdf_advance_mutation_is_rejected_against_decoded_font(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            source = root / 'source.ttf'; source.write_bytes(original_font())
            pdf = root / 'width.pdf'; pdf.write_bytes(fixture_pdf(source.read_bytes(), wrong_width=True))
            report = metrics.inspect_pdf_metrics(pdf, [source])
            self.assertFalse(report['passed'])
            self.assertIn('font-metric-width', {f['rule'] for f in report['findings']})

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
