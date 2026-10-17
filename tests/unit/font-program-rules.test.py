"""Original small fonts and binary mutations for embedded-program predicates."""
import importlib.util
import io
from pathlib import Path
import struct
import array
import unittest

from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import TTFont, newTable
from fontTools.ttLib.sfnt import calcChecksum
from fontTools.ttLib.tables.ttProgram import Program

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('font_rules', ROOT / 'scripts/pdf_font_program_rules.py')
rules = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(rules)


def number(value):
    # Fixed-width DICT operands make authored offsets independent of rebuilding.
    return b'\x1d' + struct.pack('>i', value)


def t2(*items):
    result = bytearray()
    for item in items:
        if isinstance(item, bytes):
            result.extend(item)
        elif -107 <= item <= 107:
            result.append(item + 139)
        else:
            result.extend(b'\x1c' + struct.pack('>h', item))
    return bytes(result)


def index(items):
    if not items:
        return b'\0\0'
    offsets = [1]
    for item in items:
        offsets.append(offsets[-1] + len(item))
    size = 1 if offsets[-1] < 256 else (2 if offsets[-1] < 65536 else 3)
    return struct.pack('>H', len(items)) + bytes([size]) + b''.join(x.to_bytes(size, 'big') for x in offsets) + b''.join(items)


def cff(program=None, private=b'', local=(), global_subrs=(), charset=(1,), top_extra=b'', fd_extra=b'', strings=(b'Example', b'Identity'), missing_private=False, private_offset_delta=0):
    """Two original glyphs, one FD, explicit CID charset and empty defaults."""
    if program is None:
        program = t2(0, 0, b'\x15', 50, 0, 0, 70, -50, 0, b'\x05\x0e')
    name = index([b'IndependentFixture'])
    string_index = index(list(strings))
    global_index = index(list(global_subrs))
    # Fixed-width operands preserve Top/FD length while the addresses settle.
    def top(cs, ch, fd, sel):
        return number(391)+number(392)+number(0)+b'\x0c\x1e'+number(ch)+b'\x0f'+number(cs)+b'\x11'+number(fd)+b'\x0c\x24'+number(sel)+b'\x0c\x25'+top_extra
    def fontdict(p):
        entry = b'' if missing_private else number(len(private))+number(p+private_offset_delta)+b'\x12'
        return entry+fd_extra
    top_index = index([top(0, 0, 0, 0)])
    prefix = b'\x01\x00\x04\x04'+name+top_index+string_index+global_index
    charset_at = len(prefix)
    charset_data = b'\0'+b''.join(struct.pack('>H', x) for x in charset)
    select_at = charset_at+len(charset_data)
    select_data = b'\0\0\0'
    fd_at = select_at+len(select_data)
    private_at = fd_at+len(index([fontdict(0)]))
    cs_at = private_at+len(private)+(len(index(list(local))) if local else 0)
    prefix = b'\x01\x00\x04\x04'+name+index([top(cs_at, charset_at, fd_at, select_at)])+string_index+global_index
    return prefix+charset_data+select_data+index([fontdict(private_at)])+private+(index(list(local)) if local else b'')+index([b'\x0e', program])


def truetype(composite=False, empty_advance=600):
    builder = FontBuilder(1000, isTTF=True)
    names = ['.notdef', 'box', 'space'] + (['scaled'] if composite else [])
    builder.setupGlyphOrder(names)
    glyphs = {}
    for name in names[:3]:
        pen = TTGlyphPen(None)
        if name == 'box':
            pen.moveTo((10, -20)); pen.lineTo((110, -20)); pen.lineTo((110, 180)); pen.lineTo((10, 180)); pen.closePath()
        glyphs[name] = pen.glyph()
    if composite:
        pen = TTGlyphPen(glyphs)
        pen.addComponent('box', (0.5, 0, 0, 0.5, 200, 100))
        glyphs['scaled'] = pen.glyph()
    builder.setupGlyf(glyphs)
    metrics = {'.notdef': (500, 0), 'box': (300, 10), 'space': (empty_advance, -999)}
    if composite:
        metrics['scaled'] = (400, 205)
    builder.setupHorizontalMetrics(metrics)
    builder.setupHorizontalHeader(ascent=800, descent=-200)
    builder.setupCharacterMap({65: 'box', 32: 'space'})
    builder.setupNameTable({'familyName': 'Original fixture', 'styleName': 'Regular'})
    builder.setupOS2(); builder.setupPost(); builder.setupMaxp()
    for tag in ('fpgm', 'prep'):
        table = newTable(tag); table.program = Program(); table.program.fromBytecode(b'')
        builder.font[tag] = table
    table = newTable('cvt '); table.values = array.array('h'); builder.font['cvt '] = table
    data = io.BytesIO(); builder.save(data)
    return data.getvalue()


def edit_ttf(data, edit):
    font = TTFont(io.BytesIO(data)); edit(font)
    result = io.BytesIO(); font.save(result, reorderTables=False)
    return result.getvalue()


def name_v1(data, tag=None, language_index=0):
    """Rebuild only the naming table; SFNT compiler updates every checksum."""
    from fontTools.ttLib.tables.DefaultTable import DefaultTable
    font=TTFont(io.BytesIO(data))
    raw=font.getTableData('name')
    _,count,base=struct.unpack_from('>3H',raw)
    records=[list(struct.unpack_from('>6H',raw,6+12*i)) for i in range(count)]
    tags=[] if tag is None else [tag.encode('utf-16-be') if isinstance(tag,str) else tag]
    if tags:
        for row in records:
            if row[0]==3: row[2]=0x8000+language_index
    records.sort(key=lambda row:row[:4])
    storage=raw[base:]; language_records=[]
    for value in tags:
        language_records.append(struct.pack('>2H',len(value),len(storage)))
        storage+=value
    header=struct.pack('>3H',1,count,6+12*count+2+4*len(tags))
    table=DefaultTable('name')
    table.data=header+b''.join(struct.pack('>6H',*row) for row in records)+struct.pack('>H',len(tags))+b''.join(language_records)+storage
    font['name']=table
    output=io.BytesIO(); font.save(output,reorderTables=False)
    return output.getvalue()


def extra_name_record(data, platform, encoding, language, name, value, version=None):
    from fontTools.ttLib.tables.DefaultTable import DefaultTable
    font=TTFont(io.BytesIO(data)); raw=font.getTableData('name')
    current,count,base=struct.unpack_from('>3H',raw)
    records=[struct.unpack_from('>6H',raw,6+12*i) for i in range(count)]
    storage=raw[base:]
    records.append((platform,encoding,language,name,len(value),len(storage)))
    table=DefaultTable('name')
    table.data=(struct.pack('>3H',current if version is None else version,count+1,base+12)
                +b''.join(struct.pack('>6H',*row) for row in sorted(records))
                +raw[6+12*count:base]+storage+value)
    font['name']=table
    output=io.BytesIO(); font.save(output,reorderTables=False)
    return output.getvalue()


def directory(data):
    return {tag.decode('latin1'): (12 + i * 16, offset, length)
            for i in range(struct.unpack_from('>H', data, 4)[0])
            for tag, checksum, offset, length in [struct.unpack_from('>4sIII', data, 12 + i * 16)]}


def binary_edit(data, edit):
    """Keep checksums valid so each malformed control reaches its byte rule."""
    data = bytearray(data)
    edit(data, directory(data))
    tables = directory(data)
    head = tables['head'][1]
    struct.pack_into('>I', data, head + 8, 0)
    for entry, offset, length in tables.values():
        struct.pack_into('>I', data, entry + 4, calcChecksum(bytes(data[offset:offset + length])))
    struct.pack_into('>I', data, head + 8, (0xb1b0afba - calcChecksum(bytes(data))) & 0xffffffff)
    return bytes(data)


def field(data, tag, at, fmt, value):
    return binary_edit(data, lambda raw, tables: struct.pack_into(fmt, raw, tables[tag][1] + at, value))


class FontProgramRules(unittest.TestCase):
    def findings(self, data, kind='cff'):
        return {x['rule'] for x in rules.inspect_font_program(data, kind)['findings']}

    def test_long_loca_uses_byte_offsets_without_short_form_alignment(self):
        font = TTFont(io.BytesIO(truetype()))
        font['glyf'].padding = 0
        encoded = io.BytesIO()
        font.save(encoded)
        raw = encoded.getvalue()
        decoded = TTFont(io.BytesIO(raw))
        self.assertEqual(decoded['head'].indexToLocFormat, 1)
        self.assertTrue(any(offset % 2 for offset in decoded['loca'].locations))
        self.assertEqual(self.findings(raw, 'truetype'), set())

    def test_original_component_programs_include_horizontal_phantom_anchors(self):
        controls = ROOT/'apps/font-worker/src/tests/components'
        for name in ('scaled-offset-clear','scaled-offset-round','point-match-clear','point-match-round',
                     'child-pp1','child-pp2','metrics-unmarked','metrics-marked'):
            with self.subTest(name=name):
                self.assertEqual(self.findings((controls/(name+'.ttf')).read_bytes(),'truetype'),set())

    def test_supported_vertical_metric_pair_validates_declared_structure(self):
        data=(ROOT/'apps/font-worker/src/tests/components/vertical-pair-malformed.ttf').read_bytes()
        self.assertIn('truetype-vertical-metrics',self.findings(data,'truetype'))

    def test_extended_component_neighbors_keep_supported_full_programs(self):
        import json
        controls=ROOT/'apps/font-worker/src/tests/components'
        rows=json.loads((controls/'provenance.json').read_bytes())['controls']
        for name,row in rows.items():
            if row.get('expected_outcome','prepared')=='prepared' and row['expected_preparation_kind']=='full':
                with self.subTest(name=name):
                    self.assertEqual(self.findings((controls/name).read_bytes(),'truetype'),set())

    def test_unhinted_truetype_zone_count_neighbors(self):
        # This original control has no instructions and does not use zone0.
        # Range checking does not prove conditional twilight use in other VMs.
        original = truetype()
        for count in (1, 2):
            self.assertEqual(self.findings(field(original, 'maxp', 14, '>H', count), 'truetype'), set())
        for count in (0, 3):
            self.assertIn('truetype-maxp', self.findings(field(original, 'maxp', 14, '>H', count), 'truetype'))

    def test_bbox_representation_expansion_is_conditional_and_outward(self):
        inspect=rules.inspect_font_bbox_enclosure
        self.assertTrue(inspect([99,-121,901,881],[100,-120,900,880],['nonbinary affine'])['passed'])
        self.assertFalse(inspect([99,-120,900,880],[100,-120,900,880],[])['passed'])
        self.assertFalse(inspect([98,-120,900,880],[100,-120,900,880],['nonbinary affine'])['passed'])
        self.assertFalse(inspect([100,-120,900,880],[100,-120,900.01,880],['nonbinary affine'])['passed'])
        self.assertFalse(inspect([99,-120,901,880],[100.25,-120,900.25,880],['nonbinary affine'])['passed'])

    def test_original_cid_cff(self):
        self.assertEqual(self.findings(cff()), set())

    def test_empty_cff_private_is_required_and_permitted(self):
        self.assertEqual(self.findings(cff(private=b'')), set())
        self.assertIn('cff-private', self.findings(cff(missing_private=True)))

    def test_index_first_offset_and_extent(self):
        data = bytearray(cff()); data[7] = 0
        self.assertIn('cff-index', self.findings(bytes(data)))
        self.assertIn('cff-index', self.findings(cff()[:-1]))

    def test_dictionary_stack_reserved_and_real_termination(self):
        for extra in (number(0)*49+b'\x0e', b'\x16', b'\x1e\x12', b'\x1e\xdf'):
            with self.subTest(extra=extra):
                expected = 'cff-dict-stack' if len(extra) > 200 else 'cff-dict'
                self.assertIn(expected, self.findings(cff(top_extra=extra)))

    def test_strings_sids_and_cid_charset(self):
        self.assertIn('cff-strings', self.findings(cff(strings=(b'Example', b'Example'))))
        self.assertIn('cff-sid', self.findings(cff(top_extra=number(65000)+b'\x00')))
        self.assertIn('cff-charset', self.findings(cff(charset=(0,))))
        self.assertIn('cff-charset', self.findings(cff(top_extra=number(0)+b'\x0f')))

    def test_private_hint_pairs_separation_scale_and_snap(self):
        controls = [
            (number(0)+b'\x06', 'cff-blue-zones'),
            (number(10)+number(-5)+b'\x06', 'cff-blue-zones'),
            (number(0)+number(10)+number(1)+number(10)+b'\x06', 'cff-blue-zones'),
            (number(0)+number(30)+b'\x06', 'cff-blue-scale'),
            (number(40)+number(-5)+b'\x0c\x0c', 'cff-stem-snap'),
            (number(40)+b'\x0a'+number(41)+b'\x0c\x0c', 'cff-stem-snap'),
        ]
        for private, expected in controls:
            with self.subTest(expected=expected, private=private):
                self.assertIn(expected, self.findings(cff(private=private)))
        valid = number(-10)+number(10)+number(490)+number(10)+b'\x06'+number(40)+b'\x0a'+number(40)+number(10)+b'\x0c\x0c'
        self.assertEqual(self.findings(cff(private=valid)), set())

    def test_type2_initial_width_and_complete_path_arguments(self):
        for program, expected in [
            (t2(*([0]*49), b'\x15\x0e'), 'type2-stack'),
            (t2(0, 0, b'\x15', 1, b'\x05\x0e'), 'type2-arity'),
            (t2(0, 0, b'\x15', 1, 2, 3, b'\x15\x0e'), 'type2-arity'),
            (t2(1, 2, b'\x05\x0e'), 'type2-path'),
            (t2(0, 0, b'\x15'), 'type2-termination'),
            (b'\x0e\x0e', 'type2-termination'),
            (b'\x00\x0e', 'type2-operator'),
            (t2(0, 0, 1, 2, b'\x0e'), 'type2-composite'),
        ]:
            with self.subTest(expected=expected):
                self.assertIn(expected, self.findings(cff(program)))
        self.assertEqual(self.findings(cff(t2(50, 0, 0, b'\x15\x0e'))), set())

    def test_type2_masks_stem_order_and_overlap(self):
        cases = [
            (t2(0, 20, b'\x12\x13\x81\x0e'), 'type2-mask'),
            (t2(20, 5, -30, 5, b'\x12\x0e'), 'type2-hints'),
            (t2(0, 20, -10, 20, b'\x12\x0e'), 'type2-hints'),
            (t2(0, 20, -10, 20, b'\x12\x13\xc0\x0e'), 'type2-hints'),
            (t2(0, 20, b'\x01\x13\x80\x0e'), 'type2-hints'),
            (t2(0, 0, b'\x15', 0, 20, b'\x12\x0e'), 'type2-hints'),
        ]
        for program, expected in cases:
            with self.subTest(expected=expected, program=program):
                self.assertIn(expected, self.findings(cff(program)))
        selected = t2(0, 20, -10, 20, b'\x12\x13\x80', 0, 0, b'\x15\x0e')
        self.assertEqual(self.findings(cff(selected)), set())

    def test_type2_transient_subroutines_and_depth(self):
        self.assertIn('type2-transient', self.findings(cff(t2(0, b'\x0c\x15\x0c\x12\x0e'))))
        self.assertIn('type2-transient', self.findings(cff(t2(0, 32, b'\x0c\x14\x0e'))))
        self.assertIn('type2-subroutine', self.findings(cff(t2(-107, b'\x0a\x0e'))))
        local = [t2(-107, b'\x0a\x0b')]
        private = number(6)+b'\x13'
        self.assertIn('type2-subroutine-depth', self.findings(cff(t2(-107, b'\x0a\x0e'), private=private, local=local)))
        self.assertEqual(self.findings(cff(t2(-107, b'\x0a\x0e'), private=private, local=[b'\x0b'])), set())
        self.assertIn('cff-private-subrs', self.findings(cff(private=number(-1)+b'\x13')))

    def test_type2_length_and_hint_limits(self):
        self.assertIn('type2-length', self.findings(cff(b'\x0c\x00'*32768+b'\x0e')))
        stems = b''.join(t2(group*100, 1, *([1, 1]*23), b'\x12') for group in range(4))+t2(1000, 1, b'\x12\x0e')
        self.assertIn('type2-hint-limit', self.findings(cff(stems)))

    def test_original_truetype_including_transformed_composite(self):
        self.assertEqual(self.findings(truetype(), 'truetype'), set())
        self.assertEqual(self.findings(truetype(composite=True), 'truetype'), set())
        observations = rules.inspect_font_program(truetype(True), 'truetype')['observations']
        self.assertEqual(observations['hhea_expected'], [600, 10, 145, 255])
        self.assertEqual(observations['head_expected'], [10, -20, 255, 190])
        def fraction(font):
            font['glyf']['scaled'].components[0].transform = [[0.25, 0], [0, 0.25]]
        self.assertEqual(self.findings(edit_ttf(truetype(True), fraction), 'truetype'), set())

    def test_type2_boundaries_are_admitted(self):
        # Four declarations of 24 disjoint stems reach 96 hints and a 48-value
        # operand stack; their authored positions advance between declarations.
        stems = b''.join(t2(group*100, 1, *([1, 1]*23), b'\x12') for group in range(4))
        self.assertEqual(self.findings(cff(stems+b'\x0e')), set())
        self.assertEqual(self.findings(cff(b'\x0c\x00'*32767+b'\x0e')), set())
        self.assertEqual(self.findings(cff(t2(7, 31, b'\x0c\x14', 31, b'\x0c\x15\x0c\x12\x0e'))), set())
        local = [t2(-106+i, b'\x0a\x0b') for i in range(9)]+[b'\x0b']
        self.assertEqual(self.findings(cff(t2(-107, b'\x0a\x0e'), private=number(6)+b'\x13', local=local)), set())
        self.assertEqual(self.findings(cff(t2(32766, b'\x1d\x0e'), global_subrs=[b'\x0b']*65535)), set())

    def test_binary_index_rejects_descending_offsets(self):
        # The String INDEX is the first authored two-object INDEX. Decrease its
        # middle offset below one without changing the font's byte extents.
        data = bytearray(cff()); marker = index([b'Example', b'Identity'])
        at = bytes(data).index(marker)
        data[at+4] = 0
        self.assertIn('cff-index', self.findings(bytes(data)))

    def test_reachable_global_subroutine_and_unresolved_random(self):
        self.assertEqual(self.findings(cff(t2(-107, b'\x1d\x0e'), global_subrs=[b'\x0b'])), set())
        self.assertIn('type2-arity', self.findings(cff(t2(0, 0, b'\x15', -107, b'\x1d\x0e'), global_subrs=[b'\x05\x0b'])))
        self.assertIn('type2-unresolved', self.findings(cff(b'\x0c\x17\x0c\x12\x0e')))

    def test_pdf_truetype_without_cmap_is_accepted(self):
        data = edit_ttf(truetype(), lambda f: f.__delitem__('cmap'))
        self.assertEqual(self.findings(data, 'truetype'), set())

    def test_truetype_aggregate_metrics_use_only_retained_outlines(self):
        def change(font):
            font.recalcBBoxes = False
            font['hhea'].advanceWidthMax = 999
            font['hhea'].minLeftSideBearing = -999
            font['hhea'].minRightSideBearing = -999
            font['hhea'].xMaxExtent = 999
            font['head'].xMax = 999
        data = edit_ttf(truetype(), change)
        self.assertIn('truetype-hhea-extrema', self.findings(data, 'truetype'))
        self.assertIn('truetype-head-bounds', self.findings(data, 'truetype'))

    def test_truetype_checksums_and_required_structure(self):
        data = bytearray(truetype()); data[-1] ^= 1
        self.assertIn('truetype-checksum', self.findings(bytes(data), 'truetype'))
        data = edit_ttf(truetype(), lambda f: f.__delitem__('hmtx'))
        self.assertIn('truetype-structure', self.findings(data, 'truetype'))
        for tag in ('cvt ', 'prep', 'fpgm'):
            with self.subTest(tag=tag):
                data = edit_ttf(truetype(), lambda f: f.__delitem__(tag))
                self.assertIn('truetype-structure', self.findings(data, 'truetype'))

    def test_directory_order_and_actual_zero_padding(self):
        def swap(raw, tables):
            raw[12:28], raw[28:44] = raw[28:44], raw[12:28]
        self.assertIn('truetype-directory', self.findings(binary_edit(truetype(), swap), 'truetype'))
        def nonzero(raw, tables):
            _, offset, length = next(row for row in tables.values() if row[2] % 4)
            raw[offset + length] = 1
        self.assertIn('truetype-padding', self.findings(binary_edit(truetype(), nonzero), 'truetype'))

    def test_raw_head_caret_and_cvt_fields(self):
        for tag, at, fmt, value, rule in (
            ('head', 18, '>H', 32, 'truetype-structure'),
            ('head', 16, '>H', 64, 'truetype-head-fields'),
            ('head', 48, '>h', 3, 'truetype-head-fields'),
        ):
            with self.subTest(tag=tag, at=at):
                self.assertIn(rule, self.findings(field(truetype(), tag, at, fmt, value), 'truetype'))
        data = binary_edit(truetype(), lambda raw, tables: struct.pack_into('>hh', raw, tables['hhea'][1] + 18, 0, 0))
        self.assertIn('truetype-hhea-format', self.findings(data, 'truetype'))
        data = edit_ttf(truetype(), lambda font: setattr(font['cvt '], 'values', array.array('h', [0])))
        data = binary_edit(data, lambda raw, tables: struct.pack_into('>I', raw, tables['cvt '][0] + 12, 1))
        self.assertIn('truetype-cvt', self.findings(data, 'truetype'))

    def test_raw_glyph_extents_and_component_capacity(self):
        data = truetype(True)
        # The authored box is GID1 at glyf offset0, one contour/four points.
        for at, fmt, value in ((10, '>H', 65535), (12, '>H', 65535), (14, '>H', 0x09ff)):
            with self.subTest(at=at):
                self.assertIn('truetype-glyf', self.findings(field(data, 'glyf', at, fmt, value), 'truetype'))
        for at in (28, 30):
            self.assertIn('truetype-maxp', self.findings(field(data, 'maxp', at, '>H', 0), 'truetype'))
        def instructions(font):
            program = Program(); program.fromBytecode(b'\xb0\x00\x21')
            font['glyf']['box'].program = program
        hinted = edit_ttf(data, instructions)
        self.assertIn('truetype-maxp', self.findings(field(hinted, 'maxp', 26, '>H', 0), 'truetype'))

    def test_name_record_order_extents_unicode_and_postscript_name(self):
        data = truetype()
        def swap(raw, tables):
            at = tables['name'][1] + 6
            raw[at:at+12], raw[at+12:at+24] = raw[at+12:at+24], raw[at:at+12]
        self.assertIn('truetype-name', self.findings(binary_edit(data, swap), 'truetype'))
        self.assertIn('truetype-name', self.findings(field(data, 'name', 14, '>H', 65535), 'truetype'))
        def bad_name(font):
            font['name'].setName('bad name', 6, 3, 1, 0x409)
        self.assertIn('truetype-name', self.findings(edit_ttf(data, bad_name), 'truetype'))
        def bad_unicode(raw, tables):
            _, at, length = tables['name']
            base = struct.unpack_from('>H', raw, at + 4)[0]
            for i in range(struct.unpack_from('>H', raw, at + 2)[0]):
                p, e, language, name, size, offset = struct.unpack_from('>6H', raw, at + 6 + 12 * i)
                if p == 3 and e == 1:
                    raw[at + base + offset:at + base + offset + 2] = b'\xd8\x00'
                    break
        self.assertIn('truetype-name', self.findings(binary_edit(data, bad_unicode), 'truetype'))

    def test_name_version_one_language_records_and_syntax(self):
        data=truetype()
        for tag in (None,'en-US','zh-cmn-Hans-CN','sl-rozaj-biske-1994',
                    'en-a-foo-b-bar-x-baz','i-klingon','x-private'):
            with self.subTest(valid=tag):
                self.assertEqual(self.findings(name_v1(data,tag),'truetype'),set())
        for tag in ('en_US','-en','en-','en-a','en-a-foo-a-bar',
                    'de-1901-1901','en-å',b'\xd8\x00'):
            with self.subTest(invalid=repr(tag)):
                self.assertIn('truetype-name',self.findings(name_v1(data,tag),'truetype'))
        self.assertIn('truetype-name',self.findings(name_v1(data,'en-US',1),'truetype'))

    def test_name_encoding_context_and_mac_postscript_cid_sentinel(self):
        data=truetype()
        for base in (data,name_v1(data)):
            with self.subTest(version=struct.unpack_from('>H',TTFont(io.BytesIO(base)).getTableData('name'))[0]):
                sentinel=extra_name_record(base,1,0,0xffff,20,b'Identity')
                self.assertEqual(self.findings(sentinel,'truetype'),set())
        legacy=extra_name_record(data,1,0,0x8000,1,b'Legacy language context')
        self.assertEqual(self.findings(legacy,'truetype'),set())
        opaque=extra_name_record(data,240,7,0,6,b'\xff\x00')
        report=rules.inspect_font_program(opaque,'truetype')
        self.assertEqual(report['findings'],[])
        self.assertIn({'platform':240,'encoding':7,'language':0,'name_id':6,
                       'interpretation':'not interpreted'},report['observations']['name_uninterpreted_records'])
        known_bad=extra_name_record(data,0,4,0,1,b'\xd8\x00')
        self.assertIn('truetype-name',self.findings(known_bad,'truetype'))

    def test_windows_naming_encodings_differ_from_cmap_codec_mappings(self):
        data=truetype()
        for encoding in (0,1,2,6,10,99):
            with self.subTest(encoding=encoding):
                self.assertEqual(self.findings(extra_name_record(data,3,encoding,0x409,1,b'\x00A'),'truetype'),set())
                self.assertIn('truetype-name',self.findings(extra_name_record(data,3,encoding,0x409,1,b'\xd8\x00'),'truetype'))
        # This is an expressly documented alternative to CP950 for this field.
        alternative=extra_name_record(data,3,4,0x409,2,'Regular'.encode('utf-16-be'))
        self.assertEqual(self.findings(alternative,'truetype'),set())
        opaque_legacy=extra_name_record(data,3,3,0x409,1,b'\xff')
        self.assertEqual(self.findings(opaque_legacy,'truetype'),set())

    def test_post_header_glyph_counts_and_fixed_pitch(self):
        data = truetype()
        for at, fmt, value in ((0, '>I', 0x12340000), (32, '>H', 65535), (34, '>H', 65535), (12, '>I', 7)):
            with self.subTest(at=at):
                self.assertIn('truetype-post', self.findings(field(data, 'post', at, fmt, value), 'truetype'))

    def test_cff_duplicate_scalar_boolean_and_language_group(self):
        for extra in (number(0)*4+b'\x05'+number(0)*4+b'\x05', number(2)+b'\x0c\x01', b'\x1e\x1a\x5f\x0c\x01'):
            with self.subTest(extra=extra):
                self.assertTrue(any(rule.startswith('cff-dict') for rule in self.findings(cff(top_extra=extra))))
        for private in (number(2)+b'\x0c\x11', b'\x1e\x1a\x5f\x0c\x11', number(2)+b'\x0c\x0e'):
            self.assertTrue(any(rule.startswith('cff-dict') for rule in self.findings(cff(private=private))))
        self.assertEqual(self.findings(cff(private=number(1)+b'\x0c\x11')), set())

    def test_zero_count_roll_and_conditional_flex_matrix_analysis(self):
        self.assertEqual(self.findings(cff(t2(0, 7, b'\x0c\x1e\x0e'))), set())
        flex = t2(0, 0, b'\x15', *([1] * 7), b'\x0c\x22\x0e')
        self.assertEqual(self.findings(cff(flex)), set())
        matrix = b''.join(number(x) for x in (2, 0, 0, 2, 0, 0)) + b'\x0c\x07'
        self.assertEqual(self.findings(cff(fd_extra=matrix)), set())
        report = rules.inspect_font_program(cff(fd_extra=matrix), 'cff')
        self.assertEqual(report['observations']['geometry'][1]['filled_bounds'], [0, 0, 100, 140])

    def test_valid_boundaries_and_declared_absence_observations(self):
        data = truetype()
        for units in (64, 1000, 16384):
            self.assertEqual(self.findings(field(data, 'head', 18, '>H', units), 'truetype'), set())
        # Bit6 alone is forbidden; Adobe-defined bits and last-resort bit14
        # are not accidentally rejected by a blanket reserved-bit mask.
        self.assertEqual(self.findings(field(data, 'head', 16, '>H', 0x7800), 'truetype'), set())
        horizontal = binary_edit(data, lambda raw, tables: struct.pack_into('>hh', raw, tables['hhea'][1] + 18, 0, 1))
        self.assertEqual(self.findings(horizontal, 'truetype'), set())
        self.assertIn('truetype-hhea-format', self.findings(field(data, 'hhea', 22, '>h', 1), 'truetype'))
        report = rules.inspect_font_program(cff(), 'cff')
        self.assertTrue(report['observations']['reachable_flex_absent'])
        self.assertEqual(report['observations']['fd_font_matrices'], [[1, 0, 0, 1, 0, 0]])
        self.assertEqual(report['observations']['reachable_type2_escaped_operators'], [])

    def test_instruction_push_extents_and_post_pascal_strings(self):
        for program in (b'\x40', b'\x41\x01\x00', b'\xb1\x00', b'\xb8\x00'):
            with self.subTest(program=program):
                with self.assertRaisesRegex(ValueError, 'PUSH'):
                    rules._instruction_tokens(program)
        self.assertEqual(rules._instruction_tokens(b'\x40\x02\x2c\x2d\x21'), ([0x40, 0x21], 2))
        data = truetype()
        self.assertIn('truetype-post', self.findings(field(data, 'post', 40, '>B', 255), 'truetype'))
        def disagreement(font):
            font['name'].setName('NameA', 6, 1, 0, 0)
            font['name'].setName('NameB', 6, 3, 1, 0x409)
        self.assertIn('truetype-name', self.findings(edit_ttf(data, disagreement), 'truetype'))

    def test_contextual_scaler_unreachable_push_and_internal_padding(self):
        data = truetype()
        apple_scaler = binary_edit(data, lambda raw, tables: raw.__setitem__(slice(0, 4), b'true'))
        with self.subTest(case='contextual-scaler'):
            self.assertIn('truetype-scaler-unresolved', self.findings(apple_scaler, 'truetype'))
        def unreachable(font):
            program = Program(); program.fromBytecode(b'\xb0\x00\x58\xb3\x01\x02\x03\x04\x59')
            font['fpgm'].program = program
        conditional = field(edit_ttf(data, unreachable), 'maxp', 24, '>H', 1)
        report = rules.inspect_font_program(conditional, 'truetype')
        with self.subTest(case='unreachable-push'):
            self.assertEqual(report['findings'], [])
            self.assertEqual(report['observations']['largest_single_push'], 4)
            self.assertEqual(report['observations']['maxp_capacities']['maxStackElements'], 1)
        noncanonical = field(data, 'glyf', 23, '>B', 1)
        with self.subTest(case='internal-padding'):
            self.assertIn('truetype-glyf-padding-unresolved', self.findings(noncanonical, 'truetype'))


if __name__ == '__main__':
    unittest.main()
