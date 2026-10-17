"""Original packets and CMaps exercise independent checks and readable mutations."""
import importlib.util
from pathlib import Path
import unittest


ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('text_metadata_rules', ROOT / 'scripts/pdf_text_metadata_rules.py')
rules = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(rules)


def packet(description=None):
    if description is None:
        description = '''<rdf:Description rdf:about="" xmlns:dc="http://purl.org/dc/elements/1.1/"
 xmlns:pdf="http://ns.adobe.com/pdf/1.3/">
 <dc:title><rdf:Alt><rdf:li xml:lang="x-default">Field &amp; Notes&#13;2026</rdf:li></rdf:Alt></dc:title>
 <pdf:Producer>Independent fixture</pdf:Producer>
 </rdf:Description>'''
    return ('''<?xpacket begin="\ufeff" id="W5M0MpCehiHzreSzNTczkc9d"?>
<x:xmpmeta xmlns:x="adobe:ns:meta/">
<rdf:RDF xmlns:rdf="http://www.w3.org/1999/02/22-rdf-syntax-ns#">'''
            + description + '''</rdf:RDF></x:xmpmeta>
<?xpacket end="w"?>''').encode('utf-8')


def cmap(blocks='1 beginbfchar\n<0001> <0041>\nendbfchar', spaces='1 begincodespacerange\n<0000> <FFFF>\nendcodespacerange'):
    return ('''/CIDInit /ProcSet findresource begin
12 dict begin
begincmap
/CIDSystemInfo << /Registry (Adobe) /Ordering (UCS) /Supplement 0 >> def
/CMapName /IndependentTextMap def
/CMapType 2 def
''' + spaces + '\n' + blocks + '''
endcmap
CMapName currentdict /CMap defineresource pop
end
end
''').encode('ascii')


class PacketRules(unittest.TestCase):
    def assert_rule(self, raw, rule):
        report = rules.inspect_packet(raw)
        self.assertIn(rule, [item['rule'] for item in report['findings']], report)

    def test_generated_shape_and_utf8_text_pass_without_changing_bytes(self):
        raw = bytearray(packet())
        before = bytes(raw)
        report = rules.inspect_packet(raw)
        self.assertEqual(report['findings'], [])
        self.assertEqual(report['observations']['description_count'], 1)
        self.assertEqual(report['observations']['subject'], '')
        self.assertEqual(bytes(raw), before)

    def test_empty_begin_and_read_only_packet_are_admitted(self):
        raw = packet().replace('\ufeff'.encode(), b'').replace(b'end="w"', b'end="r"')
        self.assertEqual(rules.inspect_packet(raw)['findings'], [])

    def test_unknown_pi_attributes_and_single_quotes_are_admitted(self):
        raw = packet().replace(b'id="W5M0MpCehiHzreSzNTczkc9d"', b"id='W5M0MpCehiHzreSzNTczkc9d' extra='ignored'")
        self.assertEqual(rules.inspect_packet(raw)['findings'], [])

    def test_deprecated_packet_length_and_encoding_need_explicit_analysis(self):
        for attribute in (b' bytes="2"', b' encoding="UTF-16"'):
            with self.subTest(attribute=attribute):
                raw = packet().replace(b'W5M0MpCehiHzreSzNTczkc9d"', b'W5M0MpCehiHzreSzNTczkc9d"' + attribute)
                self.assert_rule(raw, 'metadata-packet-unresolved')

    def test_header_required_attributes_order_and_values(self):
        header = b'begin="' + '\ufeff'.encode() + b'" id="W5M0MpCehiHzreSzNTczkc9d"'
        for replacement in (b'id="W5M0MpCehiHzreSzNTczkc9d" begin=""',
                            b'begin="wrong" id="W5M0MpCehiHzreSzNTczkc9d"',
                            b'begin="" id="wrong"', b'begin=""',
                            b'begin=""  id="W5M0MpCehiHzreSzNTczkc9d"',
                            b'begin=""\tid="W5M0MpCehiHzreSzNTczkc9d"'):
            with self.subTest(replacement=replacement):
                self.assert_rule(packet().replace(header, replacement), 'metadata-packet-header')

    def test_packet_trailer_and_nested_markers(self):
        for ending in (b'', b'<?xpacket end="q"?>', b'<?xpacket other="x" end="w"?>'):
            with self.subTest(ending=ending):
                self.assert_rule(packet().replace(b'<?xpacket end="w"?>', ending), 'metadata-packet-trailer')
        self.assert_rule(packet().replace(b'<rdf:RDF ', b'<?xpacket begin="" id="W5M0MpCehiHzreSzNTczkc9d"?><rdf:RDF '),
                         'metadata-packet-structure')
        self.assert_rule(packet() + b'<?xpacket end="w"?>', 'metadata-packet-structure')

    def test_xml_validity_encoding_and_unsupported_declarations(self):
        self.assert_rule(packet().replace(b'Field &amp;', b'Field &'), 'metadata-xml')
        self.assert_rule(packet().replace(b'Field', b'\xff'), 'metadata-encoding')
        self.assert_rule(packet().replace(b'<x:xmpmeta', b'<!DOCTYPE x [<!ENTITY word "field">]><x:xmpmeta'),
                         'metadata-xml-unresolved')

    def test_description_subject_is_required_and_shared(self):
        self.assert_rule(packet().replace(b' rdf:about=""', b''), 'metadata-rdf-subject')
        same = packet('<rdf:Description rdf:about="urn:one"/><rdf:Description rdf:about="urn:one"/>')
        self.assertEqual(rules.inspect_packet(same)['findings'], [])
        different = same.replace(b'about="urn:one"/>', b'about="urn:two"/>', 1)
        self.assert_rule(different, 'metadata-rdf-subject')

    def test_missing_rdf_and_top_level_typed_nodes_fail(self):
        self.assert_rule(packet().replace(b'rdf:RDF', b'rdf:Bag'), 'metadata-rdf-structure')
        self.assert_rule(packet('<rdf:Bag rdf:about=""/>'), 'metadata-rdf-structure')

    def test_rdf_container_cannot_hide_text_outside_properties(self):
        for old, new in ((b'<rdf:RDF ', b'outside<rdf:RDF '),
                         (b'<rdf:Description ', b'outside<rdf:Description '),
                         (b'</rdf:Description>', b'</rdf:Description>outside'),
                         (b'</rdf:RDF>', b'</rdf:RDF>outside')):
            with self.subTest(new=new):
                self.assert_rule(packet().replace(old, new), 'metadata-rdf-structure')


class ToUnicodeRules(unittest.TestCase):
    def assert_rule(self, raw, rule):
        report = rules.inspect_tounicode(raw)
        self.assertIn(rule, [item['rule'] for item in report['findings']], report)

    def test_bmp_surrogate_and_multiple_scalar_targets_pass(self):
        raw = bytearray(cmap('3 beginbfchar\n<0001> <0041>\n<0002> <D83DDE00>\n<0003> <00660069>\nendbfchar'))
        before = bytes(raw)
        report = rules.inspect_tounicode(raw)
        self.assertEqual(report['findings'], [])
        self.assertEqual(report['observations']['mappings'], {'0001': 'A', '0002': '😀', '0003': 'fi'})
        self.assertEqual(bytes(raw), before)

    def test_destination_512_byte_boundary_and_first_invalid_length(self):
        for target, valid in [('0041' * 256, True), ('D83DDE00' * 128, True), ('0041' * 257, False)]:
            with self.subTest(length=len(target) // 2):
                report = rules.inspect_tounicode(cmap('1 beginbfchar\n<0001> <' + target + '>\nendbfchar'))
                if valid:
                    self.assertEqual(report['findings'], [])
                else:
                    self.assertIn('tounicode-destination', [item['rule'] for item in report['findings']])

    def test_empty_odd_byte_and_unpaired_surrogates_fail(self):
        for target in ('', '00', 'D800', 'DC00', 'D8000041', '0041DC00', 'DC00D800'):
            with self.subTest(target=target):
                self.assert_rule(cmap('1 beginbfchar\n<0001> <' + target + '>\nendbfchar'), 'tounicode-destination')

    def test_bfchar_block_count_and_exact_record_grammar(self):
        for block in ('2 beginbfchar\n<0001> <0041>\nendbfchar',
                      '0 beginbfchar\n<0001> <0041>\nendbfchar',
                      '1 beginbfchar\n<0001> 65\nendbfchar',
                      '1 beginbfchar\n<0001> <0041> <0042>\nendbfchar',
                      '1 beginbfchar\n<0001> <0041>\nendbfrange'):
            with self.subTest(block=block):
                self.assertTrue(rules.inspect_tounicode(cmap(block))['findings'])

    def test_100_records_per_block_and_split_101_records(self):
        entries = ['<%04X> <0041>' % i for i in range(101)]
        first = '100 beginbfchar\n' + '\n'.join(entries[:100]) + '\nendbfchar'
        self.assertEqual(rules.inspect_tounicode(cmap(first))['findings'], [])
        split = first + '\n1 beginbfchar\n' + entries[100] + '\nendbfchar'
        self.assertEqual(rules.inspect_tounicode(cmap(split))['findings'], [])
        self.assert_rule(cmap('101 beginbfchar\n' + '\n'.join(entries) + '\nendbfchar'), 'tounicode-block-count')

    def test_codespace_shape_membership_and_nonoverlap(self):
        spaces = '1 begincodespacerange\n<8140> <9FFC>\nendcodespacerange'
        valid = cmap('1 beginbfchar\n<86A9> <0041>\nendbfchar', spaces)
        self.assertEqual(rules.inspect_tounicode(valid)['findings'], [])
        self.assert_rule(valid.replace(b'<86A9>', b'<8210>'), 'tounicode-source')
        self.assert_rule(cmap(spaces=spaces.replace('<9FFC>', '<9F30>')), 'tounicode-codespace')
        self.assert_rule(cmap(spaces='2 begincodespacerange\n<0000> <00FF>\n<0080> <0180>\nendcodespacerange'), 'tounicode-codespace')
        self.assert_rule(cmap().replace(b'<0001>', b'<01>'), 'tounicode-source')
        self.assert_rule(cmap().replace(b'<FFFF>', b'<FF>'), 'tounicode-codespace')

    def test_bfrange_array_and_sequential_destinations(self):
        block = '2 beginbfrange\n<0001> <0003> <0041>\n<0004> <0005> [<00660069> <D83DDE00>]\nendbfrange'
        report = rules.inspect_tounicode(cmap(block))
        self.assertEqual(report['findings'], [])
        self.assertEqual(report['observations']['mappings'], {'0001': 'A', '0002': 'B', '0003': 'C', '0004': 'fi', '0005': '😀'})
        for replacement in ('[<00660069>]', '[<00660069> <D800>]', '[<00660069> <0041> <0042>]'):
            with self.subTest(replacement=replacement):
                self.assertTrue(rules.inspect_tounicode(cmap(block.replace('[<00660069> <D83DDE00>]', replacement)))['findings'])

    def test_bfrange_increment_must_not_carry_or_enter_surrogate(self):
        self.assert_rule(cmap('1 beginbfrange\n<0001> <0002> <00FF>\nendbfrange'), 'tounicode-destination')
        self.assert_rule(cmap('1 beginbfrange\n<0001> <0002> <D83DDFFF>\nendbfrange'), 'tounicode-destination')
        self.assert_rule(cmap('1 beginbfrange\n<0002> <0001> <0041>\nendbfrange'), 'tounicode-source')

    def test_mapping_override_uses_later_definition(self):
        report = rules.inspect_tounicode(cmap('2 beginbfchar\n<0001> <0041>\n<0001> <0042>\nendbfchar'))
        self.assertEqual(report['findings'], [])
        self.assertEqual(report['observations']['mappings'], {'0001': 'B'})

    def test_unsupported_operators_and_inheritance_are_explicit(self):
        self.assert_rule(cmap().replace(b'1 beginbfchar', b'/OtherMap usecmap\n1 beginbfchar'), 'tounicode-unresolved')
        self.assert_rule(cmap().replace(b'beginbfchar', b'begincidchar').replace(b'endbfchar', b'endcidchar'), 'tounicode-unresolved')

    def test_cmap_type_order_envelope_and_lexical_errors(self):
        self.assert_rule(cmap().replace(b'/CMapType 2', b'/CMapType 0'), 'tounicode-type')
        self.assert_rule(cmap(spaces=''), 'tounicode-codespace')
        self.assert_rule(cmap().replace(b'<0041>', b'<00Q1>'), 'tounicode-structure')
        self.assert_rule(cmap().replace(b'endcmap', b''), 'tounicode-structure')
        self.assert_rule(cmap() + b'newoperator', 'tounicode-structure')


if __name__ == '__main__':
    unittest.main()
