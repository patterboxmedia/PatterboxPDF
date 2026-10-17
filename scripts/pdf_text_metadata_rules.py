"""Independent checks for generated metadata packets and text extraction maps.

These development predicates inspect exact bytes without rewriting them. They
cover the generated UTF-8 packet and two-byte Type 2 CMap subset; unsupported
constructs produce findings rather than implied general validation. Required
metadata values and their agreement with the PDF Info dictionary belong to the
caller that also inspects those PDF objects.
"""
import re
import xml.etree.ElementTree as ET


_RDF = '{http://www.w3.org/1999/02/22-rdf-syntax-ns#}'
_XML = '{http://www.w3.org/XML/1998/namespace}'
_TITLE = '{http://purl.org/dc/elements/1.1/}title'
_PACKET_ID = 'W5M0MpCehiHzreSzNTczkc9d'
_XML_SPACE = ' \t\r\n'


class _Invalid(ValueError):
    def __init__(self, rule, detail):
        self.rule = rule
        super().__init__(detail)


def _require(condition, rule, detail):
    if not condition:
        raise _Invalid(rule, detail)


def _report(raw):
    return {'findings': [], 'observations': {'byte_length': len(raw)}}


def _finding(report, error):
    report['findings'].append({'rule': error.rule, 'detail': str(error)})


def _packet_attributes(body, rule):
    # Processing-instruction data is not parsed as attributes by an XML parser.
    # Check the pseudo-attributes independently, including their separators.
    pattern = re.compile(r'''([A-Za-z_][A-Za-z0-9_.:-]*)[ \t\r\n]*=[ \t\r\n]*(["'])(.*?)\2''', re.S)
    _require(body.startswith(' '), rule, 'Packet attributes must follow one space')
    attributes = []
    at = 1
    while at < len(body):
        match = pattern.match(body, at)
        _require(match is not None, rule, 'Malformed packet attribute or separator')
        name, _, value = match.groups()
        _require(name not in dict(attributes), rule, 'Duplicate packet attribute: ' + name)
        attributes.append((name, value))
        at = match.end()
        if at < len(body):
            _require(body[at] == ' ', rule, 'Packet attributes must be separated by one space')
            at += 1
    return attributes


def _packet(raw, report):
    try:
        text = bytes(raw).decode('utf-8', errors='strict')
    except UnicodeDecodeError as error:
        raise _Invalid('metadata-encoding', 'Expected the generated UTF-8 packet encoding') from error
    report['observations']['encoding'] = 'utf-8'
    _require('<!DOCTYPE' not in text and '<!ENTITY' not in text,
             'metadata-xml-unresolved', 'DTD and custom entity declarations are outside the generated subset')
    markers = list(re.finditer(r'<\?xpacket\b(.*?)\?>', text, re.S))
    _require(len(markers) <= 2, 'metadata-packet-structure', 'Nested or extra packet markers')
    _require(markers and markers[0].start() == len(text) - len(text.lstrip(_XML_SPACE)),
             'metadata-packet-header', 'Missing packet header at the beginning of the stream')
    first = _packet_attributes(markers[0][1], 'metadata-packet-header')
    _require(len(first) >= 2 and [name for name, _ in first[:2]] == ['begin', 'id'],
             'metadata-packet-header', 'The first packet attributes must be begin followed by id')
    _require(first[0][1] in ('', '\ufeff') and first[1][1] == _PACKET_ID,
             'metadata-packet-header', 'Incorrect packet begin marker or identifier')
    _require(not {'bytes', 'encoding'} & dict(first).keys(), 'metadata-packet-unresolved',
             'Deprecated packet length and encoding fields are outside the generated subset')
    _require(len(markers) == 2 and not text[markers[-1].end():].strip(_XML_SPACE),
             'metadata-packet-trailer', 'Missing final packet trailer')
    last = _packet_attributes(markers[1][1], 'metadata-packet-trailer')
    _require(last and last[0][0] == 'end' and last[0][1] in ('w', 'r'),
             'metadata-packet-trailer', 'Packet trailer must start with end="w" or end="r"')
    try:
        root = ET.fromstring(text)
    except ET.ParseError as error:
        raise _Invalid('metadata-xml', 'Malformed XML: ' + str(error)) from error
    _require(root.tag == '{adobe:ns:meta/}xmpmeta' and len(root) == 1 and root[0].tag == _RDF + 'RDF',
             'metadata-rdf-structure', 'Expected one RDF container inside the metadata wrapper')
    rdf = root[0]
    _require(all(not (value or '').strip(_XML_SPACE) for value in (root.text, rdf.text, rdf.tail)),
             'metadata-rdf-structure', 'RDF containers may contain only element content and whitespace')
    descriptions = list(rdf)
    _require(descriptions and all(node.tag == _RDF + 'Description' for node in descriptions),
             'metadata-rdf-structure', 'Top-level RDF nodes must be descriptions')
    _require(not rdf.attrib and len(list(rdf.iter(_RDF + 'Description'))) == len(descriptions),
             'metadata-rdf-unresolved', 'Nested RDF descriptions and container attributes are outside the generated subset')
    subjects = []
    properties = 0
    for description in descriptions:
        _require(_RDF + 'about' in description.attrib,
                 'metadata-rdf-subject', 'Every description must identify its subject with rdf:about')
        subjects.append(description.attrib[_RDF + 'about'])
        _require(set(description.attrib) == {_RDF + 'about'},
                 'metadata-rdf-unresolved', 'Description shorthand attributes are outside the generated subset')
        _require(not (description.text or '').strip(_XML_SPACE)
                 and not (description.tail or '').strip(_XML_SPACE),
                 'metadata-rdf-structure', 'Text must occur in metadata properties')
        for prop in description:
            properties += 1
            _require(prop.tag.startswith('{') and not prop.tag.startswith(_RDF),
                     'metadata-rdf-structure', 'Metadata properties need a non-RDF namespace')
            _require(not prop.attrib, 'metadata-rdf-unresolved', 'Qualified or resource-valued properties need additional analysis')
            _require(not (prop.tail or '').strip(_XML_SPACE),
                     'metadata-rdf-structure', 'Text must occur in metadata properties')
            if prop.tag == _TITLE:
                _require(len(prop) == 1 and prop[0].tag == _RDF + 'Alt'
                         and not (prop.text or '').strip(_XML_SPACE),
                         'metadata-rdf-structure', 'Title must use a language alternative array')
                array = prop[0]
                _require(not array.attrib and not (array.text or '').strip(_XML_SPACE)
                         and not (array.tail or '').strip(_XML_SPACE),
                         'metadata-rdf-structure', 'Malformed language alternative array')
                languages = []
                for item in array:
                    _require(item.tag == _RDF + 'li' and not len(item)
                             and set(item.attrib) == {_XML + 'lang'}
                             and item.attrib[_XML + 'lang']
                             and not (item.tail or '').strip(_XML_SPACE),
                             'metadata-rdf-structure', 'Each language alternative needs a simple value and language')
                    languages.append(item.attrib[_XML + 'lang'].lower())
                _require(len(languages) == len(set(languages)),
                         'metadata-rdf-structure', 'Duplicate language alternative')
            else:
                _require(not len(prop), 'metadata-rdf-unresolved', 'Structured properties are outside the generated subset')
    _require(len(set(subjects)) == 1,
             'metadata-rdf-subject', 'Descriptions in one packet must refer to the same subject')
    report['observations'].update(description_count=len(descriptions), subject=subjects[0],
                                  property_count=properties, packet_end=last[0][1])


def inspect_packet(raw):
    """Return neutral findings and observations for one generated XMP packet."""
    report = _report(raw)
    try:
        _packet(raw, report)
    except _Invalid as error:
        _finding(report, error)
    return report


_TOKEN = re.compile(r'''[\0\t\n\f\r ]+|%[^\r\n]*|<<|>>|<[^>]*>|\((?:\\.|[^()\\])*\)|/[^\0\t\n\f\r ()<>\[\]{}/%]+|[\[\]]|[^\0\t\n\f\r ()<>\[\]{}/%]+''')


class _Tokens:
    def __init__(self, raw):
        try:
            text = bytes(raw).decode('ascii')
        except UnicodeDecodeError as error:
            raise _Invalid('tounicode-structure', 'Expected ASCII CMap syntax') from error
        self.tokens = []
        at = 0
        while at < len(text):
            match = _TOKEN.match(text, at)
            _require(match is not None, 'tounicode-structure', 'Malformed or unsupported CMap lexical element')
            token = match[0]
            _require(not token.startswith('<') or token == '<<'
                     or re.fullmatch(r'<[0-9a-fA-F\0\t\n\f\r ]*>', token) is not None,
                     'tounicode-structure', 'Malformed hexadecimal string')
            if not token.startswith('%') and not token[0] in '\0\t\n\f\r ':
                self.tokens.append(token)
            at = match.end()
        self.at = 0

    def peek(self):
        return self.tokens[self.at] if self.at < len(self.tokens) else None

    def take(self):
        token = self.peek()
        _require(token is not None, 'tounicode-structure', 'Unexpected end of CMap')
        self.at += 1
        return token

    def expect(self, expected, rule='tounicode-structure'):
        _require(self.take() == expected, rule, 'Expected ' + expected)


def _hex(token, rule):
    _require(re.fullmatch(r'<[0-9a-fA-F\0\t\n\f\r ]*>', token) is not None,
             rule, 'Expected a hexadecimal string')
    digits = re.sub(r'[\0\t\n\f\r ]', '', token[1:-1])
    # The generated subset uses complete bytes. Do not silently grant coverage
    # to alternate string spellings that this evaluator does not interpret.
    _require(len(digits) % 2 == 0, 'tounicode-unresolved', 'Odd-nibble hexadecimal strings need additional analysis')
    return bytes.fromhex(digits)


def _destination(raw):
    _require(0 < len(raw) <= 512 and len(raw) % 2 == 0,
             'tounicode-destination', 'Destination must contain 1 to 256 complete UTF-16 code units')
    try:
        return raw.decode('utf-16-be', errors='strict')
    except UnicodeDecodeError as error:
        raise _Invalid('tounicode-destination', 'Destination contains an unpaired UTF-16 surrogate') from error


def _cmap(raw, report):
    tokens = _Tokens(raw)
    for expected in ('/CIDInit', '/ProcSet', 'findresource', 'begin', '12', 'dict', 'begin', 'begincmap'):
        tokens.expect(expected)
    headers = set()
    while tokens.peek() and tokens.peek().startswith('/'):
        key = tokens.take()
        _require(key not in headers, 'tounicode-structure', 'Repeated CMap header key')
        headers.add(key)
        if key == '/CIDSystemInfo':
            for expected in ('<<', '/Registry', '(Adobe)', '/Ordering', '(UCS)', '/Supplement', '0', '>>', 'def'):
                tokens.expect(expected)
        elif key == '/CMapName':
            _require(tokens.take().startswith('/'), 'tounicode-structure', 'CMapName must be a literal name')
            tokens.expect('def')
        elif key == '/CMapType':
            tokens.expect('2', 'tounicode-type')
            tokens.expect('def')
        else:
            raise _Invalid('tounicode-unresolved', 'Unsupported CMap header construct: ' + key)
    _require(headers == {'/CIDSystemInfo', '/CMapName', '/CMapType'},
             'tounicode-structure', 'Missing generated CMap header entry')
    spaces = []
    mappings = {}
    block_counts = []
    maximum = 0
    mapping_started = False

    def source():
        code = _hex(tokens.take(), 'tounicode-source')
        _require(len(code) == 2, 'tounicode-source', 'Expected a two-byte source code')
        _require(any(all(lo[i] <= code[i] <= hi[i] for i in range(2)) for lo, hi in spaces),
                 'tounicode-source', 'Source code lies outside the declared codespace')
        return code

    def retain(code, destination):
        nonlocal maximum
        text = _destination(destination)
        maximum = max(maximum, len(destination))
        # Later mappings replace earlier ones, including overlapping blocks.
        mappings[code.hex().upper()] = text

    while tokens.peek() != 'endcmap':
        count_token = tokens.take()
        _require(re.fullmatch(r'[+-]?\d+', count_token) is not None,
                 'tounicode-unresolved' if count_token.startswith('/') else 'tounicode-structure',
                 'Expected a mapping block count; unsupported executable CMap construct')
        count = int(count_token)
        operator = tokens.take()
        _require(operator in ('begincodespacerange', 'beginbfchar', 'beginbfrange'),
                 'tounicode-unresolved', 'Unsupported CMap mapping operator: ' + operator)
        _require(0 <= count <= 100, 'tounicode-block-count', 'Each block must declare at most 100 entries')
        block_counts.append({'operator': operator, 'count': count})
        if operator == 'begincodespacerange':
            _require(not mapping_started, 'tounicode-codespace', 'Codespace must precede all mapping blocks')
            for _ in range(count):
                lo = _hex(tokens.take(), 'tounicode-codespace')
                hi = _hex(tokens.take(), 'tounicode-codespace')
                _require(len(lo) == len(hi) == 2 and all(a <= b for a, b in zip(lo, hi)),
                         'tounicode-codespace', 'Codespace bounds must define a two-byte rectangle')
                _require(not any(all(max(lo[i], other_lo[i]) <= min(hi[i], other_hi[i]) for i in range(2))
                                 for other_lo, other_hi in spaces),
                         'tounicode-codespace', 'Codespace regions must not overlap')
                spaces.append((lo, hi))
        else:
            _require(spaces, 'tounicode-codespace', 'Missing codespace before mapping block')
            mapping_started = True
            for _ in range(count):
                lo = source()
                if operator == 'beginbfchar':
                    retain(lo, _hex(tokens.take(), 'tounicode-destination'))
                else:
                    hi = source()
                    _require(lo <= hi, 'tounicode-source', 'Source range ends before it begins')
                    _require(lo[0] == hi[0], 'tounicode-unresolved', 'Source ranges crossing a first-byte boundary need additional analysis')
                    length = hi[1] - lo[1] + 1
                    destinations = []
                    if tokens.peek() == '[':
                        tokens.take()
                        for _ in range(length):
                            destinations.append(_hex(tokens.take(), 'tounicode-destination'))
                        tokens.expect(']', 'tounicode-destination')
                    else:
                        first = _hex(tokens.take(), 'tounicode-destination')
                        _destination(first)
                        _require(first[-1] <= 256 - length,
                                 'tounicode-destination', 'Sequential destination would overflow its last byte')
                        destinations = [first[:-1] + bytes([first[-1] + i]) for i in range(length)]
                    for i, destination in enumerate(destinations):
                        code = bytes([lo[0], lo[1] + i])
                        _require(any(all(a[j] <= code[j] <= b[j] for j in range(2)) for a, b in spaces),
                                 'tounicode-source', 'Source range includes an undeclared code')
                        retain(code, destination)
        tokens.expect('end' + operator[5:], 'tounicode-block-count')
    _require(spaces, 'tounicode-codespace', 'CMap contains no codespace')
    for expected in ('endcmap', 'CMapName', 'currentdict', '/CMap', 'defineresource', 'pop', 'end', 'end'):
        tokens.expect(expected)
    _require(tokens.peek() is None, 'tounicode-structure', 'Unexpected data after CMap resource definition')
    report['observations'].update(cmap_type=2, source_code_bytes=2,
                                  codespace_ranges=[{'low': lo.hex().upper(), 'high': hi.hex().upper()} for lo, hi in spaces],
                                  blocks=block_counts, mapping_count=len(mappings),
                                  max_destination_bytes=maximum, mappings=mappings)


def inspect_tounicode(raw):
    """Inspect one two-byte generated ToUnicode CMap without changing its bytes.

    bfchar blocks, same-first-byte bfrange arrays and sequential destinations are
    interpreted. Inheritance, arbitrary executable constructs and alternate
    source widths produce explicit findings. Findings ending in ``unresolved``
    describe missing analysis and must prevent a pass.
    """
    report = _report(raw)
    try:
        _cmap(raw, report)
    except _Invalid as error:
        _finding(report, error)
    return report
