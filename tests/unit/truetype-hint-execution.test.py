"""Execute original hinted fonts; font parsing alone accepts the bad programs."""
import importlib.util
import io
from pathlib import Path
import sys
import unittest
from unittest.mock import patch

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import TTFont, newTable
from fontTools.ttLib.tables.ttProgram import Program


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location(
    'hint_execution', ROOT / 'scripts/pdf_truetype_hint_execution.py')
hints = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(hints)
FUNCTION_ZERO = b'\xb0\x00\x2c\xb0\x07\x21\x2d'


def program(data):
    result = Program()
    result.fromBytecode(data)
    return result


def original_font(*, fpgm=None, prep=None, glyph=None, without_cmap=False):
    """Four self-authored glyphs, including a composite and a late hinted GID.

    Function zero pushes and pops one value. Both prep and the two simple
    outlines call it, so an absent/invalid definition is observable in the VM.
    This fixture uses no installed font or private project source.
    """
    builder = FontBuilder(1000, isTTF=True)
    order = ['.notdef', 'box', 'late', 'composite']
    builder.setupGlyphOrder(order)
    glyphs = {}
    for name in order[:3]:
        pen = TTGlyphPen(None)
        if name != '.notdef':
            pen.moveTo((10, 0))
            pen.lineTo((210, 0))
            pen.lineTo((210, 400))
            pen.lineTo((10, 400))
            pen.closePath()
        glyphs[name] = pen.glyph()
        if name != '.notdef':
            glyphs[name].program = program(b'\xb0\x00\x2b' if glyph is None else glyph)
    pen = TTGlyphPen(glyphs)
    pen.addComponent('box', (1, 0, 0, 1, 250, 0))
    glyphs['composite'] = pen.glyph()
    builder.setupGlyf(glyphs)
    builder.setupHorizontalMetrics({name: (500, 0) for name in order})
    builder.setupHorizontalHeader(ascent=800, descent=-200)
    builder.setupCharacterMap({65: 'box', 66: 'late'})
    builder.setupNameTable({'familyName': 'Original hint execution fixture', 'styleName': 'Regular'})
    builder.setupOS2()
    builder.setupPost()
    builder.setupMaxp()
    for name, data in (
        ('fpgm', FUNCTION_ZERO if fpgm is None else fpgm),
        ('prep', b'\xb0\x00\x2b' if prep is None else prep),
    ):
        table = newTable(name)
        table.program = program(data)
        builder.font[name] = table
    maximum = builder.font['maxp']
    maximum.maxZones = 2
    maximum.maxTwilightPoints = 0
    maximum.maxStorage = 2
    maximum.maxFunctionDefs = 1
    maximum.maxInstructionDefs = 0
    maximum.maxStackElements = 16
    if without_cmap:
        del builder.font['cmap']
    result = io.BytesIO()
    builder.save(result)
    return result.getvalue()


def edit_font(data, edit):
    font = TTFont(io.BytesIO(data), recalcTimestamp=False)
    edit(font)
    result = io.BytesIO()
    font.save(result)
    return result.getvalue()


class TrueTypeHintExecution(unittest.TestCase):
    def test_exact_bytecode_absence_does_not_depend_on_engine_phantom_support(self):
        for name in ('child-pp1','child-pp2'):
            data=(ROOT/'apps/font-worker/src/tests/components'/(name+'.ttf')).read_bytes()
            report=hints.inspect_truetype_hints(data)
            self.assertEqual(report['findings'],[])
            self.assertIs(report['observations'].get('hint_programs_absent'),True)
            self.assertEqual(report['observations']['glyph_loads_attempted'],0)

    def test_original_programs_execute_for_every_gid_and_size(self):
        data = original_font()
        report = hints.inspect_truetype_hints(data)
        self.assertEqual(report['findings'], [])
        observations = report['observations']
        self.assertEqual(observations['glyph_count'], 4)
        self.assertEqual(observations['programs']['fpgm_bytes'], 7)
        self.assertEqual(observations['programs']['prep_bytes'], 3)
        self.assertEqual(observations['programs']['glyphs_with_instructions'], 2)
        self.assertEqual(observations['conditions']['ppem'], [12, 72, 300])
        self.assertEqual(observations['conditions']['interpreter_version'], 35)
        self.assertEqual(observations['glyph_loads_attempted'], 12)
        self.assertEqual(observations['glyph_loads_succeeded'], 12)
        for size in observations['sizes']:
            self.assertEqual(size['glyph_loads_attempted'], 4)
            self.assertEqual(size['glyph_loads_succeeded'], 4)
        self.assertEqual(observations['input_sha256'], observations['input_sha256_after'])
        self.assertTrue(observations['dependencies']['freetype_binary_sha256'])
        self.assertIn('FreeType', observations['coverage_limit'])

    def test_parser_readable_stack_underflow_in_each_program_is_rejected(self):
        # POP has a valid opcode/encoding but needs an operand. The font
        # container and outline parser accept each independently bad program.
        for location in ('fpgm', 'prep', 'glyph'):
            with self.subTest(location=location):
                # Keep function zero present in fpgm so its failure is the
                # top-level underflow, rather than a later undefined CALL.
                bad_program = FUNCTION_ZERO + b'\x21' if location == 'fpgm' else b'\x21'
                data = original_font(**{location: bad_program})
                font = TTFont(io.BytesIO(data))
                self.assertEqual(font['maxp'].numGlyphs, 4)
                self.assertEqual(len(font['glyf']['late'].getCoordinates(font['glyf'])[0]), 4)
                report = hints.inspect_truetype_hints(data)
                errors = [f for f in report['findings'] if f['rule'] == 'truetype-hint-execution']
                self.assertTrue(errors, report)
                self.assertTrue(all(f['freetype_error'] != 0 for f in errors))

    def test_late_gid_and_size_conditional_programs_are_executed(self):
        # MPPEM; PUSHW 300; EQ; IF; POP; EIF fails only at 300 ppem.
        conditional = b'\x4b\xb8\x01\x2c\x54\x58\x21\x59'
        data = edit_font(original_font(),
                         lambda f: setattr(f['glyf']['late'], 'program', program(conditional)))
        report = hints.inspect_truetype_hints(data)
        errors = [f for f in report['findings'] if f['rule'] == 'truetype-hint-execution']
        self.assertEqual([(f['ppem'], f['gid']) for f in errors], [(300, 2)])
        self.assertEqual(report['observations']['glyph_loads_attempted'], 12)

    def test_cid_program_without_cmap_still_executes_by_gid(self):
        report = hints.inspect_truetype_hints(original_font(without_cmap=True))
        self.assertEqual(report['findings'], [])
        self.assertEqual(report['observations']['glyph_loads_succeeded'], 12)

    def test_missing_dependency_and_missing_vm_are_unresolved(self):
        data = original_font()
        with patch.dict(sys.modules, {'freetype': None}):
            report = hints.inspect_truetype_hints(data)
        self.assertEqual([f['rule'] for f in report['findings']], ['truetype-hint-unresolved'])
        import freetype
        with patch.object(freetype, 'FT_Get_TrueType_Engine_Type', return_value=0):
            report = hints.inspect_truetype_hints(data)
        self.assertEqual([f['rule'] for f in report['findings']], ['truetype-hint-unresolved'])
        self.assertEqual(report['observations']['glyph_loads_attempted'], 0)

    def test_non_sfnt_bytes_are_a_finding(self):
        report = hints.inspect_truetype_hints(b'not a TrueType font')
        self.assertTrue(report['findings'])
        self.assertEqual(report['observations']['glyph_loads_attempted'], 0)


if __name__ == '__main__':
    unittest.main()
