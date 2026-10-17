"""Exercise independent output rules with readable, targeted PDF mutations."""
import importlib.util
import io
from pathlib import Path
import tempfile
import unittest
import array
import zlib
import re

from pypdf import PdfReader, PdfWriter
from pypdf.errors import PdfReadError
from pypdf.generic import (ArrayObject, BooleanObject, DecodedStreamObject, EncodedStreamObject, DictionaryObject,
                          FloatObject, NameObject, NumberObject, TextStringObject)
from fontTools.fontBuilder import FontBuilder
from fontTools.pens.ttGlyphPen import TTGlyphPen
from fontTools.ttLib import newTable
from fontTools.ttLib.tables.ttProgram import Program

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('supported', ROOT / 'scripts/verify-pdfx-supported-output.py')
supported = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(supported)


class OutputChecks(unittest.TestCase):
    @staticmethod
    def multistage_profile():
        def lut(inputs, outputs, kind):
            count_b = outputs if kind == b'mAB ' else inputs
            count_a = inputs if kind == b'mAB ' else outputs
            curve = b'curv' + bytes(8)
            data = bytearray(kind + bytes(4) + bytes([inputs, outputs]) + bytes(22))
            data[12:16] = len(data).to_bytes(4, 'big'); data.extend(curve * count_b)
            data[24:28] = len(data).to_bytes(4, 'big')
            data.extend(bytes([2] * inputs) + bytes(16-inputs) + b'\x01\0\0\0' + bytes(2**inputs * outputs))
            while len(data) % 4: data.append(0)
            data[28:32] = len(data).to_bytes(4, 'big'); data.extend(curve * count_a)
            return bytes(data)
        mluc = b'mluc' + bytes(4) + (1).to_bytes(4,'big') + (12).to_bytes(4,'big') + b'enUS' + (2).to_bytes(4,'big') + (28).to_bytes(4,'big') + b'\0A'
        tags = {b'desc': mluc, b'cprt': mluc, b'wtpt': b'XYZ '+bytes(4)+bytes.fromhex('0000f6d6000100000000d32d')}
        tags.update({tag: lut(4,3,b'mAB ') for tag in (b'A2B0',b'A2B1',b'A2B2')})
        tags.update({tag: lut(3,4,b'mBA ') for tag in (b'B2A0',b'B2A1',b'B2A2')})
        tags[b'gamt'] = lut(3,1,b'mBA ')
        original = (ROOT/'crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc').read_bytes()
        result = bytearray(original[:128]); result[8:12] = b'\x04\x20\0\0'; result[84:128] = bytes(44)
        result.extend(len(tags).to_bytes(4,'big')); result.extend(bytes(12*len(tags)))
        for index, (name,payload) in enumerate(tags.items()):
            start = 132+12*index
            result[start:start+12] = name + len(result).to_bytes(4,'big') + len(payload).to_bytes(4,'big')
            result.extend(payload)
            while len(result)%4: result.append(0)
        result[:4] = len(result).to_bytes(4,'big')
        return bytes(result)

    def test_multistage_profile_stages_and_sample_extents_are_checked(self):
        profile = self.multistage_profile()
        self.assertEqual(supported.inspect_profile_bytes(profile), [])
        record = next(132+12*n for n in range(int.from_bytes(profile[128:132],'big')) if profile[132+12*n:136+12*n] == b'A2B0')
        offset = int.from_bytes(profile[record+4:record+8], 'big')
        clut = int.from_bytes(profile[offset+24:offset+28], 'big')
        mutations = [(offset+10, 1), (offset+15, 0), (offset+31, 0),
                     (offset+clut+16, 3), (offset+clut+4, 9), (offset+clut, 255), (offset+32, ord('x'))]
        for position,value in mutations:
            changed = bytearray(profile); changed[position] = value
            with self.subTest(position=position):
                self.assertTrue(supported.inspect_profile_bytes(bytes(changed)))

    def test_profile_header_reserved_domains_and_private_matrix_extent(self):
        original = (ROOT/'crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc').read_bytes()
        vendor = bytearray(original); vendor[44:48] = (1<<31).to_bytes(4,'big'); vendor[56:64] = (1<<63).to_bytes(8,'big')
        self.assertEqual(supported.inspect_profile_bytes(bytes(vendor)), [])
        for offset,size,value in ((44,4,4),(56,8,4),(64,4,4)):
            changed = bytearray(original); changed[offset:offset+size] = value.to_bytes(size,'big')
            self.assertTrue(supported.inspect_profile_bytes(bytes(changed)))
        changed = bytearray(original)
        record = next(n for n in range(132,132+12*int.from_bytes(original[128:132],'big'),12) if original[n:n+4] == b'arts')
        changed[record+8:record+12] = (40).to_bytes(4,'big')
        self.assertTrue(supported.inspect_profile_bytes(bytes(changed)))

    def fixture(self, mutation=None):
        writer = PdfWriter()
        writer.pdf_header = '%PDF-1.6'
        writer.add_blank_page(width=72, height=72)
        page = writer.pages[0]
        page[NameObject('/TrimBox')] = ArrayObject(map(NumberObject, [0, 0, 72, 72]))
        page[NameObject('/BleedBox')] = ArrayObject(map(NumberObject, [0, 0, 72, 72]))
        profile = DecodedStreamObject()
        profile.set_data((ROOT / 'crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc').read_bytes())
        profile[NameObject('/N')] = NumberObject(4)
        intent = DictionaryObject({NameObject('/S'): NameObject('/GTS_PDFX'),
            NameObject('/OutputConditionIdentifier'): TextStringObject('Synthetic test condition'),
            NameObject('/Info'): TextStringObject('Synthetic test condition'),
            NameObject('/DestOutputProfile'): writer._add_object(profile)})
        writer._root_object[NameObject('/OutputIntents')] = ArrayObject([writer._add_object(intent)])
        xmp = DecodedStreamObject()
        xmp.set_data(b'''<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/"><rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">
<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/" xmlns:pdf="http://ns.adobe.com/pdf/1.3/"
xmlns:xmp="http://ns.adobe.com/xap/1.0/" xmlns:xmpMM="http://ns.adobe.com/xap/1.0/mm/" xmlns:pdfxid="http://www.npes.org/pdfx/ns/id/">
<dc:title><rdf:Alt><rdf:li xml:lang="x-default">Test</rdf:li></rdf:Alt></dc:title>
<pdf:Producer>Independent fixture</pdf:Producer><pdf:Trapped>False</pdf:Trapped><xmp:CreatorTool>Independent fixture</xmp:CreatorTool>
<xmp:CreateDate>2026-10-16T00:00:00Z</xmp:CreateDate><xmp:ModifyDate>2026-10-16T00:00:00Z</xmp:ModifyDate>
<xmp:MetadataDate>2026-10-16T00:00:00Z</xmp:MetadataDate><xmpMM:DocumentID>uuid:12345678-1234-1234-1234-123456789abc</xmpMM:DocumentID>
<xmpMM:VersionID>1</xmpMM:VersionID><xmpMM:RenditionClass>default</xmpMM:RenditionClass>
<pdfxid:GTS_PDFXVersion>PDF/X-4</pdfxid:GTS_PDFXVersion></rdf:Description></rdf:RDF></x:xmpmeta><?xpacket end="w"?>''')
        writer._root_object[NameObject('/Metadata')] = writer._add_object(xmp)
        writer.add_metadata({'/Title': 'Test', '/Producer': 'Independent fixture', '/Creator': 'Independent fixture',
                             '/GTS_PDFXVersion': 'PDF/X-4', '/Trapped': '/False'})
        writer._info.get_object()[NameObject('/Trapped')] = NameObject('/False')
        writer.generate_file_identifiers()
        if mutation:
            mutation(writer, page, intent, profile, xmp)
        data = io.BytesIO()
        writer.write(data)
        return data.getvalue()

    def findings(self, data, mode='pdfx4'):
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / 'control.pdf'
            path.write_bytes(data)
            report = supported.inspect_pdf(path, mode=mode)
            self.assertEqual(path.read_bytes(), data)
            return {row['rule'] for row in report['findings']}

    def test_positive_independent_fixture(self):
        self.assertEqual(self.findings(self.fixture()), set())

    def test_ordinary_empty_title_preserves_the_public_metadata_contract(self):
        def empty_title(w, p, i, c, x):
            w._info.get_object()[NameObject('/Title')] = TextStringObject('')
            x.set_data(x.get_data().replace(b'>Test</rdf:li>', b'></rdf:li>'))
        data = self.fixture(empty_title)
        self.assertNotIn('metadata-values', self.findings(data, mode='ordinary'))
        self.assertNotIn('metadata-mapping', self.findings(data, mode='ordinary'))
        self.assertIn('metadata-values', self.findings(data))

    def test_targeted_readable_mutations(self):
        def put(key, value):
            return lambda w, p, i, c, x: p.__setitem__(NameObject(key), value)
        def content(data):
            def change(w, p, i, c, x):
                stream = DecodedStreamObject(); stream.set_data(data)
                p[NameObject('/Contents')] = w._add_object(stream)
            return change
        mutations = {
            'page-boxes': put('/TrimBox', ArrayObject(map(NumberObject, [0, 0, 100, 100]))),
            'no-actions': lambda w,p,i,c,x: w._root_object.__setitem__(NameObject('/OpenAction'), DictionaryObject()),
            'no-annotations': put('/Annots', ArrayObject([DictionaryObject({NameObject('/Subtype'): NameObject('/Text')})])),
            'output-intent': lambda w,p,i,c,x: w._root_object.pop('/OutputIntents'),
            'profile-integrity': lambda w,p,i,c,x: c.__setitem__(NameObject('/N'), NumberObject(3)),
            'metadata-identity': lambda w,p,i,c,x: x.set_data(x.get_data().replace(b'PDF/X-4', b'PDF/X-4p')),
            'metadata-mapping': lambda w,p,i,c,x: w._info.get_object().__setitem__(NameObject('/Title'), TextStringObject('Other')),
            'metadata-packet': lambda w,p,i,c,x: x.set_data(x.get_data().replace(b'begin=""', b'begin="" bytes="100"')),
            'graphics-state-depth': content(b'q '*29 + b'Q '*29),
            'known-operators': content(b'1 2 madeUpOperator'),
            'content-string-limit': content(b'(' + b'a'*32768 + b') Tj'),
            'resource-references': content(b'/Missing Do'),
            'device-color': content(b'1 0 0 rg 0 0 10 10 re f'),
        }
        for rule, mutation in mutations.items():
            with self.subTest(rule=rule):
                self.assertIn(rule, self.findings(self.fixture(mutation)))

    def test_inline_actual_text_decoded_byte_boundaries(self):
        def marked_content(raw):
            def change(w, p, i, c, x):
                stream = DecodedStreamObject()
                stream.set_data(b'/Span << /ActualText <' + raw.hex().encode('ascii')
                                + b'> >> BDC 0 0 m 1 1 l S EMC')
                p[NameObject('/Contents')] = w._add_object(stream)
            return change
        controls = [
            ('ascii-limit', b'a' * 32767, True),
            ('ascii-over', b'a' * 32768, False),
            ('utf16-bom-below', b'\xfe\xff' + ('\U0001f642' * 8191).encode('utf-16-be'), True),
            ('utf16-bom-over', b'\xfe\xff' + ('\U0001f642' * 8191 + 'a').encode('utf-16-be'), False),
        ]
        for label, raw, accepted in controls:
            with self.subTest(label=label, decoded_bytes=len(raw)):
                findings = self.findings(self.fixture(marked_content(raw)))
                self.assertEqual(findings, set() if accepted else {'content-string-limit'})

    def test_nested_form_is_inspected(self):
        def nested(w, p, i, c, x):
            form = DecodedStreamObject(); form.set_data(b'')
            form[NameObject('/Subtype')] = NameObject('/Form')
            form[NameObject('/BBox')] = ArrayObject(map(NumberObject, [0, 0, 10, 10]))
            form[NameObject('/Resources')] = DictionaryObject()
            form[NameObject('/OPI')] = DictionaryObject()
            p[NameObject('/Resources')][NameObject('/XObject')] = DictionaryObject({NameObject('/Nested'): w._add_object(form)})
        self.assertIn('embedded-resources', self.findings(self.fixture(nested)))

    def test_remaining_structure_mutations(self):
        def root(key, value):
            return lambda w,p,i,c,x: w._root_object.__setitem__(NameObject(key), value)
        mutations = {
            'base-format': lambda w,p,i,c,x: setattr(w, 'pdf_header', '%PDF-1.7'),
            'numeric-limits': root('/TestNumber', NumberObject(10**50)),
            'name-limits': root('/TestName', NameObject('/'+'N'*128)),
            'file-identity': lambda w,p,i,c,x: setattr(w, '_ID', ArrayObject([TextStringObject('short')]*2)),
            'no-encryption': root('/Perms', DictionaryObject()),
            'supported-streams': lambda w,p,i,c,x: c.__setitem__(NameObject('/Filter'), NameObject('/ASCIIHexDecode')),
            'no-optional-content': root('/OCProperties', DictionaryObject()),
            'no-interactive-forms': root('/AcroForm', DictionaryObject()),
            'no-postscript': root('/TestObject', DictionaryObject({NameObject('/Subtype'): NameObject('/PS')})),
            'no-halftones': root('/TestObject', DictionaryObject({NameObject('/HT'): NumberObject(1)})),
            'no-alternate-images': root('/TestObject', DictionaryObject({NameObject('/Alternates'): ArrayObject()})),
            'transparency': root('/TestObject', DictionaryObject({NameObject('/Type'): NameObject('/ExtGState'), NameObject('/ca'): NumberObject(2)})),
            'graphics-state': root('/TestObject', DictionaryObject({NameObject('/Type'): NameObject('/ExtGState'), NameObject('/RI'): NameObject('/Invalid')})),
            'image-data': root('/TestObject', self.image_stream()),
            'shading-functions': root('/TestObject', DictionaryObject({NameObject('/ShadingType'): NumberObject(2),
                NameObject('/ColorSpace'): NameObject('/DeviceCMYK'), NameObject('/Coords'): ArrayObject(map(NumberObject,[0,0,1,1])),
                NameObject('/Function'): DictionaryObject({NameObject('/FunctionType'): NumberObject(2),
                    NameObject('/Domain'): ArrayObject(map(NumberObject,[0,1])), NameObject('/N'): NumberObject(0)})})),
            'metadata-values': lambda w,p,i,c,x: x.set_data(x.get_data().replace(b'<pdf:Trapped>False', b'<pdf:Trapped>Unknown')),
        }
        for rule, mutation in mutations.items():
            with self.subTest(rule=rule):
                self.assertIn(rule, self.findings(self.fixture(mutation)))

    @staticmethod
    def image_stream():
        image = DecodedStreamObject(); image.set_data(b'wrong sample count')
        image.update({NameObject('/Subtype'): NameObject('/Image'), NameObject('/Width'): NumberObject(1),
                      NameObject('/Height'): NumberObject(1), NameObject('/BitsPerComponent'): NumberObject(8),
                      NameObject('/ColorSpace'): NameObject('/DeviceCMYK')})
        return image

    @staticmethod
    def add_font(writer, page, fault=None):
        # Original three-glyph font, independent of the Rust preparation code.
        fractional = fault == 'font-precision'
        builder = FontBuilder(2048 if fractional else 1000, isTTF=True)
        builder.setupGlyphOrder(['.notdef', 'space', 'A'])
        builder.setupCharacterMap({32:'space', 65:'A'})
        glyphs = {}
        for name in ['.notdef','space','A']:
            pen = TTGlyphPen(None)
            if name == 'A':
                pen.moveTo((0,0)); pen.lineTo((300,700)); pen.lineTo((600,0)); pen.closePath()
            glyphs[name] = pen.glyph()
        builder.setupGlyf(glyphs)
        builder.setupHorizontalMetrics({'.notdef':(600,0),'space':(300,0),'A':(600,0)})
        builder.setupHorizontalHeader(ascent=800,descent=-200)
        builder.setupNameTable({'familyName':'IndependentFixture','styleName':'Regular'})
        builder.setupOS2(); builder.setupPost(); builder.setupMaxp()
        for tag in ('prep', 'fpgm'):
            table = newTable(tag); table.program = Program(); table.program.fromBytecode(b'')
            builder.font[tag] = table
        table = newTable('cvt '); table.values = array.array('h'); builder.font['cvt '] = table
        program = io.BytesIO(); builder.save(program)
        stream = DecodedStreamObject(); stream.set_data(program.getvalue())
        stream[NameObject('/Length1')] = NumberObject(len(program.getvalue()))
        cid_set = DecodedStreamObject(); cid_set.set_data(b'\xe0')
        descriptor = DictionaryObject({NameObject('/Type'):NameObject('/FontDescriptor'),
            NameObject('/FontFile2'):writer._add_object(stream),
            NameObject('/CIDSet'):writer._add_object(cid_set),
            NameObject('/FontName'):NameObject('/IndependentFixture'),
            NameObject('/Flags'):NumberObject(4), NameObject('/ItalicAngle'):NumberObject(0),
            NameObject('/FontBBox'):ArrayObject(map(FloatObject, [0,0,600*1000/(2048 if fractional else 1000),700*1000/(2048 if fractional else 1000)])),
            NameObject('/Ascent'):NumberObject(800), NameObject('/Descent'):NumberObject(-200),
            NameObject('/CapHeight'):NumberObject(700), NameObject('/StemV'):NumberObject(0)})
        if fault == 'font-programs':
            descriptor.pop('/FontFile2')
        if fault == 'font-descriptors':
            descriptor[NameObject('/FontBBox')] = ArrayObject(map(NumberObject, [0,0,600,900]))
        scale = 1000/(2048 if fractional else 1000)
        child = DictionaryObject({NameObject('/Type'):NameObject('/Font'), NameObject('/Subtype'):NameObject('/CIDFontType2'),
            NameObject('/BaseFont'):NameObject('/IndependentFixture'),
            NameObject('/CIDSystemInfo'):DictionaryObject({NameObject('/Registry'):TextStringObject('Adobe'),
                NameObject('/Ordering'):TextStringObject('Identity'), NameObject('/Supplement'):NumberObject(0)}),
            NameObject('/FontDescriptor'):writer._add_object(descriptor), NameObject('/CIDToGIDMap'):NameObject('/Identity'),
            NameObject('/W'):ArrayObject([NumberObject(0),ArrayObject(map(FloatObject,[600*scale,300*scale,(700 if fault=='font-metrics' else 600)*scale]))])})
        cmap = DecodedStreamObject(); cmap.set_data(b'''/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def
/CMapName /IndependentFixture def
/CMapType 2 def
1 begincodespacerange
<0000> <FFFF>
endcodespacerange
1 beginbfchar
<0002> <0041>
endbfchar
endcmap
CMapName currentdict /CMap defineresource pop
end
end
''')
        font = DictionaryObject({NameObject('/Type'):NameObject('/Font'),NameObject('/Subtype'):NameObject('/Type0'),
            NameObject('/BaseFont'):NameObject('/IndependentFixture'),
            NameObject('/Encoding'):NameObject('/Identity-H'),NameObject('/DescendantFonts'):ArrayObject([writer._add_object(child)]),
            NameObject('/ToUnicode'):writer._add_object(cmap)})
        page['/Resources'][NameObject('/Font')] = DictionaryObject({NameObject('/F0'):writer._add_object(font)})
        content = DecodedStreamObject()
        content.set_data(b'BT /F0 12 Tf <0000> Tj ET' if fault=='font-glyphs' else b'BT /F0 12 Tf <0002> Tj ET')
        page[NameObject('/Contents')] = writer._add_object(content)

    def test_independent_embedded_font_controls(self):
        self.assertEqual(self.findings(self.fixture(lambda w,p,i,c,x:self.add_font(w,p))), set())
        for rule in ('font-programs','font-metrics','font-glyphs'):
            with self.subTest(rule=rule):
                self.assertIn(rule, self.findings(self.fixture(lambda w,p,i,c,x:self.add_font(w,p,rule))))

    def test_precise_advances_and_descriptor_bounds(self):
        self.assertEqual(self.findings(self.fixture(lambda w,p,i,c,x:self.add_font(w,p,'font-precision'))), set())
        self.assertIn('font-descriptors', self.findings(self.fixture(lambda w,p,i,c,x:self.add_font(w,p,'font-descriptors'))))

    def test_font_dictionary_roles_identity_cidset_and_width_grammar(self):
        def mutation(fault):
            def change(w,p,i,c,x):
                self.add_font(w,p)
                font = p['/Resources']['/Font']['/F0'].get_object()
                child = font['/DescendantFonts'][0].get_object(); descriptor = child['/FontDescriptor'].get_object()
                if fault == 'child-type': child[NameObject('/Type')] = NameObject('/Other')
                if fault == 'child-subtype': child[NameObject('/Subtype')] = NameObject('/CIDFontType0')
                if fault == 'length': descriptor['/FontFile2'][NameObject('/Length1')] = NumberObject(1)
                if fault == 'flags': descriptor[NameObject('/Flags')] = NumberObject(4|32)
                if fault == 'cidset': descriptor['/CIDSet'].set_data(b'\xa0')
                if fault == 'name': descriptor[NameObject('/FontName')] = NameObject('/Other')
                if fault == 'system': child['/CIDSystemInfo'][NameObject('/Ordering')] = TextStringObject('Other')
                if fault == 'width-order': child[NameObject('/W')] = ArrayObject([NumberObject(1),ArrayObject([NumberObject(300)]),NumberObject(0),ArrayObject([NumberObject(600)])])
                if fault == 'width-cid': child['/W'][0] = FloatObject(.5)
            return change
        for fault in ('child-type','child-subtype','length','flags','cidset','name','system','width-order','width-cid'):
            with self.subTest(fault=fault):
                self.assertTrue(self.findings(self.fixture(mutation(fault))) & {'font-programs','font-metrics','font-descriptors'})

    def test_required_profile_tags_and_payloads_are_independent_controls(self):
        def change(kind):
            def mutate(w,p,i,c,x):
                data = bytearray(c.get_data())
                if kind == 'missing-description':
                    data[132:136] = b'test'
                elif kind == 'reserved-payload':
                    offset = int.from_bytes(data[136:140], 'big')
                    data[offset+4] = 1
                elif kind == 'lut-layout':
                    count = int.from_bytes(data[128:132], 'big')
                    row = next(132+n*12 for n in range(count) if data[132+n*12:136+n*12] == b'A2B0')
                    offset = int.from_bytes(data[row+4:row+8], 'big')
                    data[offset+48:offset+50] = b'\x00\x01'
                elif kind in ('transform-type', 'negative-xyz'):
                    count = int.from_bytes(data[128:132], 'big')
                    tag = b'wtpt' if kind == 'negative-xyz' else b'A2B0'
                    row = next(132+n*12 for n in range(count) if data[132+n*12:136+n*12] == tag)
                    offset = int.from_bytes(data[row+4:row+8], 'big')
                    if kind == 'negative-xyz':
                        data[offset+8:offset+12] = b'\xff'*4
                    else:
                        data[offset:offset+4] = b'bad!'
                c.set_data(bytes(data))
            return mutate
        for kind in ('missing-description', 'reserved-payload', 'lut-layout', 'transform-type', 'negative-xyz'):
            with self.subTest(kind=kind):
                self.assertIn('profile-integrity', self.findings(self.fixture(change(kind))))

    def test_strict_compressed_stream_and_trapping_mapping(self):
        def compressed(w,p,i,c,x):
            stream = EncodedStreamObject(); stream._data = zlib.compress(b'control') + b'extra'
            stream[NameObject('/Filter')] = NameObject('/FlateDecode')
            w._root_object[NameObject('/Extra')] = w._add_object(stream)
        self.assertIn('supported-streams', self.findings(self.fixture(compressed)))
        self.assertIn('metadata-mapping', self.findings(self.fixture(lambda w,p,i,c,x:
            w._info.get_object().__setitem__(NameObject('/Trapped'), NameObject('/True')))))

    def test_soft_mask_dimension_condition_and_forbidden_entries(self):
        def masked(fault=None):
            def change(w,p,i,c,x):
                image = self.image_stream(); image.set_data(b'\0'*4)
                mask = DecodedStreamObject(); mask.set_data(b'\xff'*4)
                mask.update({NameObject('/Subtype'):NameObject('/Image'), NameObject('/ColorSpace'):NameObject('/DeviceGray'),
                             NameObject('/Width'):NumberObject(2), NameObject('/Height'):NumberObject(2), NameObject('/BitsPerComponent'):NumberObject(8)})
                if fault == 'matte': mask[NameObject('/Matte')] = ArrayObject(map(NumberObject,[0,0,0,0]))
                if fault == 'image-mask': mask[NameObject('/ImageMask')] = BooleanObject(True)
                if fault == 'mask': mask[NameObject('/Mask')] = ArrayObject(map(NumberObject,[0,0]))
                image[NameObject('/SMask')] = w._add_object(mask)
                w._root_object[NameObject('/Extra')] = w._add_object(image)
            return change
        self.assertEqual(self.findings(self.fixture(masked())), set())
        for fault in ('matte','image-mask','mask'):
            self.assertIn('image-data', self.findings(self.fixture(masked(fault))))

    def test_nonstream_physical_line_limit_excludes_stream_payloads(self):
        def long_title(w,p,i,c,x):
            w._info.get_object()[NameObject('/Title')] = TextStringObject('a'*260)
            x.set_data(x.get_data().replace(b'>Test<', b'>'+b'a'*260+b'<'))
        self.assertIn('nonstream-line-length', self.findings(self.fixture(long_title)))
        def long_stream(w,p,i,c,x):
            stream = DecodedStreamObject(); stream.set_data(b'a'*1000+b'\nstream\nendstream\n')
            w._root_object[NameObject('/Extra')] = w._add_object(stream)
        self.assertEqual(self.findings(self.fixture(long_stream)), set())

    def test_content_operator_context_path_and_state_values(self):
        def content(raw):
            def change(w,p,i,c,x):
                stream = DecodedStreamObject(); stream.set_data(raw)
                p[NameObject('/Contents')] = w._add_object(stream)
            return change
        for raw in (b'BT q Q ET', b'0 0 m 1 w n', b'0 0 m W 1 1 l n', b'1 1 l S',
                    b'-1 w', b'3 J', b'1.5 j', b'0 M', b'[0 0] 0 d', b'8 Tr', b'1 1 Td'):
            with self.subTest(raw=raw):
                self.assertIn('known-operators', self.findings(self.fixture(content(raw))))
        self.assertEqual(self.findings(self.fixture(content(b'1 w q 0 0 m 10 0 l 10 10 l h W n Q BT 1 w 0 Tr ET'))), set())

    def test_exact_classic_xref_records_and_raw_name_escapes(self):
        data = self.fixture()
        reader = PdfReader(io.BytesIO(data), strict=True)
        self.assertEqual(supported.inspect_serialized_file(data,reader), [])
        xref = int(re.search(rb'startxref\s+(\d+)',data)[1])
        records = xref+re.match(rb'xref\n0 \d+\n',data[xref:]).end()
        changed = bytearray(data); changed[records+17] = ord('n')
        self.assertTrue(supported.inspect_serialized_file(bytes(changed),reader))
        changed = bytearray(data); changed[records+20:records+30] = b'0000000001'
        self.assertTrue(supported.inspect_serialized_file(bytes(changed),reader))
        changed = bytearray(data); changed[records+30] = 9
        self.assertTrue(supported.inspect_serialized_file(bytes(changed),reader))
        for name in (b'/A#GG',b'/A#00',b'/A#FF'):
            changed = data.replace(b'/Title',name.ljust(6,b' '),1)
            self.assertTrue(supported.inspect_serialized_file(changed,reader))

    def test_page_tree_count_parent_and_duplicate_dictionary_recovery(self):
        def wrong_count(w,p,i,c,x):
            w._pages.get_object()[NameObject('/Count')] = NumberObject(2)
        self.assertIn('base-format', self.findings(self.fixture(wrong_count)))
        def wrong_parent(w,p,i,c,x):
            p[NameObject('/Parent')] = w._root_object
        self.assertIn('base-format', self.findings(self.fixture(wrong_parent)))
        original = self.fixture()
        # Same-size corruption keeps every xref offset intact.
        duplicate = original.replace(b'/Producer',b'/Title   ',1)
        with self.assertRaises(PdfReadError):
            reader = PdfReader(io.BytesIO(duplicate), strict=True)
            reader.metadata


if __name__ == '__main__':
    unittest.main()
