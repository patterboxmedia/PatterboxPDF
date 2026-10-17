"""Independently inspect the library's supported generated-output rules.

This checker accepts the generated subset described by its rule catalog. It is
not a general validator for arbitrary PDFs. pypdf parses objects/content,
fontTools parses embedded font programs and LittleCMS parses color profiles.
Input bytes are never changed. Findings retain neutral rule names so this
implementation and its factual receipts can be distributed on their own.
"""
import argparse
import datetime
import hashlib
import importlib.metadata
import importlib.util
import io
import json
import math
from pathlib import Path
import re
import subprocess
import struct
import xml.etree.ElementTree as ET
import zlib

from fontTools.cffLib import CFFFontSet
from fontTools.pens.basePen import NullPen
from fontTools.pens.boundsPen import BoundsPen
from fontTools.ttLib import TTFont
from PIL import ImageCms
from pypdf import PdfReader
from pypdf.generic import (ArrayObject, BooleanObject, ByteStringObject,
                          ContentStream, DictionaryObject, FloatObject,
                          IndirectObject, NameObject, NumberObject,
                          StreamObject, TextStringObject)

RULES = {
    'base-format': 'Readable complete PDF with supported version, objects and cross references.',
    'numeric-limits': 'Integers and real values stay inside the supported architectural range.',
    'nonstream-line-length': 'Physical lines outside stream payloads contain no more than 255 bytes.',
    'name-limits': 'Names use valid UTF-8 and at most 127 decoded bytes.',
    'file-identity': 'Two direct, nonempty file identifier strings are present.',
    'no-encryption': 'Encryption and permissions dictionaries are absent.',
    'embedded-resources': 'External streams, OPI, referenced Forms and file specifications are absent.',
    'supported-streams': 'Only plain and Flate streams occur in this generated subset.',
    'no-actions': 'Actions, JavaScript and additional action dictionaries are absent.',
    'no-annotations': 'Annotations are absent from the supported output.',
    'no-optional-content': 'Optional content and presentation variants are absent.',
    'no-interactive-forms': 'Interactive forms and rich-media/XFA content are absent.',
    'no-postscript': 'PostScript XObjects and functions are absent.',
    'no-halftones': 'Halftones, transfer functions and black-generation adjustments are absent.',
    'no-alternate-images': 'Alternate image dictionaries are absent.',
    'page-boxes': 'Page boxes are positive, correctly nested and contain exactly one trim/art box.',
    'output-intent': 'One embedded CMYK print condition has a meaningful identifier and description.',
    'profile-integrity': 'Embedded profiles are parseable printer profiles with matching size/channels/version.',
    'device-color': 'Print content uses only the declared device model and gray.',
    'transparency': 'Transparency groups, blend modes, masks and opacity use the supported definitions.',
    'graphics-state': 'Graphics state uses supported rendering intents and overprint values.',
    'graphics-state-depth': 'Executed saved graphics states never exceed 28 and are balanced.',
    'known-operators': 'All content operators have known signatures and legal operands.',
    'resource-references': 'Every content resource reference resolves in the active resource dictionary.',
    'content-string-limit': 'Content strings contain no more than 32767 bytes.',
    'font-programs': 'Every font has a parseable embedded program and the supported composite encoding.',
    'font-metrics': 'All emitted glyph widths agree with independently decoded program metrics.',
    'font-program-layout': 'Embedded program structures, bounds, checksums and reachable compact-outline instructions are consistent.',
    'font-descriptors': 'Font descriptors have required metrics and bounds for their embedded glyph outlines.',
    'font-hint-execution': 'Embedded TrueType instructions execute at the declared independent interpreter settings.',
    'font-glyphs': 'Text references existing nonzero glyphs with embedded outlines and a Unicode mapping.',
    'image-data': 'Decoded image and mask samples have the declared dimensions and supported components.',
    'shading-functions': 'Shadings have valid geometry and bounded continuous/stitching component functions.',
    'metadata-packet': 'All metadata packets are unfiltered, well-formed supported RDF/XML packets.',
    'metadata-values': 'Required title, dates, producer, creator, identifiers and trapping state are present.',
    'metadata-mapping': 'Present Info fields agree with their mapped metadata values.',
    'metadata-identity': 'Print identity has the selected value and ordinary output makes no print claim.',
}
NS = {'rdf': 'http://www.w3.org/1999/02/22-rdf-syntax-ns#',
      'dc': 'http://purl.org/dc/elements/1.1/', 'pdf': 'http://ns.adobe.com/pdf/1.3/',
      'xmp': 'http://ns.adobe.com/xap/1.0/', 'xmpMM': 'http://ns.adobe.com/xap/1.0/mm/',
      'pdfxid': 'http://www.npes.org/pdfx/ns/id/'}
INTENTS = {'/AbsoluteColorimetric', '/RelativeColorimetric', '/Perceptual', '/Saturation'}
BLENDS = {'/Normal', '/Multiply', '/Screen', '/Overlay', '/Darken', '/Lighten',
          '/ColorDodge', '/ColorBurn', '/HardLight', '/SoftLight', '/Difference',
          '/Exclusion', '/Hue', '/Saturation', '/Color', '/Luminosity'}
# Public inputs produce this finite operator set. Rejecting an otherwise valid
# unsupported feature is deliberate and reported as such.
ARITY = {b'q': 0, b'Q': 0, b'cm': 6, b'w': 1, b'J': 1, b'j': 1, b'M': 1,
         b'd': 2, b'ri': 1, b'gs': 1, b'm': 2, b'l': 2, b'c': 6, b'v': 4,
         b'y': 4, b'h': 0, b're': 4, b'S': 0, b's': 0, b'f': 0, b'F': 0,
         b'f*': 0, b'B': 0, b'B*': 0, b'b': 0, b'b*': 0, b'n': 0,
         b'W': 0, b'W*': 0, b'G': 1, b'g': 1, b'RG': 3, b'rg': 3,
         b'K': 4, b'k': 4, b'Do': 1, b'sh': 1, b'BT': 0, b'ET': 0,
         b'Tc': 1, b'Tw': 1, b'Tz': 1, b'TL': 1, b'Tf': 2, b'Tr': 1,
         b'Ts': 1, b'Td': 2, b'TD': 2, b'Tm': 6, b'T*': 0, b'Tj': 1,
         b'TJ': 1, b"'": 1, b'"': 3, b'BMC': 1, b'BDC': 2, b'EMC': 0}


def sha(data):
    return hashlib.sha256(data).hexdigest()


def font_evaluator(name):
    spec = importlib.util.spec_from_file_location(name, Path(__file__).with_name(name + '.py'))
    module = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(module)
    return module


def f32(value):
    return struct.unpack('>f', struct.pack('>f', value))[0]


def nonstream_bytes(data, reader):
    # Locate exact encoded payloads through the independently parsed xref and
    # StreamObject bytes. Delimiter-looking bytes inside a payload are data.
    offsets = sorted((offset, number, generation) for generation, entries in reader.xref.items()
                     for number, offset in entries.items() if number and offset)
    spans = []
    for index, (offset, number, generation) in enumerate(offsets):
        value = reader.get_object(IndirectObject(number, generation, reader))
        if not isinstance(value, StreamObject):
            continue
        stop = offsets[index+1][0] if index+1 < len(offsets) else len(data)
        encoded = value._data
        candidates = []
        for marker in re.finditer(rb'(?<![A-Za-z])stream(?:\r\n|\n)', data[offset:stop]):
            start = offset+marker.end(); end = start+len(encoded)
            if data[start:end] == encoded and re.match(rb'[\r\n]*endstream\b', data[end:stop]):
                candidates.append((start,end))
        if len(candidates) != 1:
            raise ValueError('Cannot uniquely locate an exact stream payload')
        spans.extend(candidates)
    pieces = []; at = 0
    for start,end in sorted(spans):
        pieces.extend((data[at:start],b'\n')); at=end
    pieces.append(data[at:])
    return b''.join(pieces)


def inspect_nonstream_lines(data, reader):
    return max(map(len,re.split(rb'\r\n|\r|\n',nonstream_bytes(data,reader))),default=0)


def inspect_serialized_file(data, reader):
    problems = []
    def require(condition, detail):
        if not condition: raise ValueError(detail)
    try:
        end = re.search(rb'startxref\s+(\d+)\s+%%EOF\s*$',data)
        require(end is not None, 'Missing final startxref/EOF')
        offset = int(end[1])
        header = re.match(rb'xref\r?\n0 (\d+)\r?\n',data[offset:])
        require(header is not None, 'Unsupported classic xref subsection')
        count = int(header[1]); cursor = offset+header.end()
        require(count == reader.trailer.get('/Size') and count > 1, 'Trailer Size differs from xref')
        for number in range(count):
            record = data[cursor:cursor+20]; cursor += 20
            require(re.fullmatch(rb'\d{10} \d{5} [nf](?: \n|\r\n)',record) is not None, 'xref record is not exactly twenty bytes')
            position,generation = int(record[:10]),int(record[11:16])
            if number == 0:
                require(position == 0 and generation == 65535 and record[17:18] == b'f', 'Invalid free object zero')
            else:
                require(generation == 0 and record[17:18] == b'n' and
                        re.match(str(number).encode()+rb' 0 obj\b', data[position:]) is not None, 'xref points to the wrong indirect object')
        require(data[cursor:].startswith(b'trailer'), 'xref extent does not end at Trailer')
        require(set(reader.xref) <= {0,65535} and set(reader.xref.get(0,{})) == set(range(1,count)), 'xref object inventory differs')
        for key in ('/Root','/Info'):
            reference = reader.trailer.raw_get(key)
            require(isinstance(reference,IndirectObject) and reference.generation == 0 and 0 < reference.idnum < count,
                    'Trailer role lacks a valid indirect reference')
        require(reader.trailer['/Root'].get('/Type') == '/Catalog' and isinstance(reader.trailer['/Info'],DictionaryObject), 'Trailer object roles differ')
        source = nonstream_bytes(data,reader); cursor = 0
        delimiters = b'\0\t\n\x0c\r ()<>[]{}/%'
        while cursor < len(source):
            byte = source[cursor]
            if byte == 37:
                match = re.search(rb'[\r\n]', source[cursor:]); cursor = cursor+match.start() if match else len(source)
            elif byte == 40:
                nesting = 1; cursor += 1
                while cursor < len(source) and nesting:
                    if source[cursor] == 92: cursor += 2; continue
                    if source[cursor] == 40: nesting += 1
                    elif source[cursor] == 41: nesting -= 1
                    cursor += 1
                require(nesting == 0, 'Unterminated literal string')
            elif source[cursor:cursor+2] == b'<<':
                cursor += 2
            elif byte == 60:
                stop = source.find(b'>',cursor+1); require(stop >= 0,'Unterminated hexadecimal string'); cursor = stop+1
            elif byte == 47:
                cursor += 1; name = bytearray()
                while cursor < len(source) and source[cursor] not in delimiters:
                    if source[cursor] == 35:
                        pair = source[cursor+1:cursor+3]
                        require(re.fullmatch(rb'[0-9a-fA-F]{2}',pair) is not None, 'Invalid name escape')
                        name.append(int(pair,16)); cursor += 3
                    else: name.append(source[cursor]); cursor += 1
                require(0 not in name and len(name) <= 127, 'Invalid decoded name length or NUL')
                name.decode('utf-8',errors='strict')
            else: cursor += 1
    except (ValueError,KeyError,TypeError) as error:
        problems.append(str(error))
    return problems


def object_value(value):
    return value.get_object() if isinstance(value, IndirectObject) else value


def string_bytes(value):
    if isinstance(value, ByteStringObject):
        return bytes(value)
    if isinstance(value, TextStringObject):
        return value.original_bytes
    raise ValueError('Expected a PDF string')


def inspect_multistage_lut(payload, expected_channels):
    """Decode embedded curves, matrix and CLUT using tag-relative offsets."""
    problems, spans = [], []
    def require(condition, detail):
        if not condition: raise ValueError(detail)
    def u32(start):
        require(start+4 <= len(payload), 'Truncated multistage integer')
        return int.from_bytes(payload[start:start+4], 'big')
    def span(start, size, kind):
        end = start+size
        require(start >= 32 and start%4 == 0 and end <= len(payload), 'Multistage element bounds')
        spans.append((start,end,kind))
        return end
    def curves(start, count):
        for _ in range(count):
            require(start+12 <= len(payload) and not any(payload[start+4:start+8]), 'Invalid embedded curve header')
            kind = payload[start:start+4]
            if kind == b'curv':
                size = 12+2*u32(start+8)
            elif kind == b'para':
                function = int.from_bytes(payload[start+8:start+10], 'big')
                require(function <= 4 and payload[start+10:start+12] == bytes(2), 'Invalid parametric curve function')
                size = 12+4*(1,3,4,5,7)[function]
            else:
                raise ValueError('Invalid embedded curve signature')
            end = span(start,size,'curve')
            padded = (end+3)&~3
            require(padded <= len(payload) and not any(payload[end:padded]), 'Nonzero embedded curve padding')
            start = padded
    try:
        require(len(payload) >= 32 and not any(payload[4:8]+payload[10:12]), 'Invalid multistage header')
        inputs, outputs = payload[8:10]
        require(1 <= inputs <= 15 and 1 <= outputs <= 15 and
                (expected_channels is None or (inputs,outputs) == expected_channels), 'Multistage channel count')
        b, matrix, m, clut, a = (u32(start) for start in (12,16,20,24,28))
        require(b and bool(a) == bool(clut) and bool(m) == bool(matrix)
                and (inputs == outputs or clut), 'Invalid multistage stage combination')
        forward = payload[:4] == b'mAB '
        side = outputs if forward else inputs
        curves(b,side)
        if a: curves(a,inputs if forward else outputs)
        if m:
            require(side == 3, 'Matrix stage requires three channels')
            curves(m,side); span(matrix,48,'matrix')
        if clut:
            span(clut,20,'CLUT header')
            grids = payload[clut:clut+16]
            precision = payload[clut+16]
            require(all(n >= 2 for n in grids[:inputs]) and not any(grids[inputs:])
                    and precision in (1,2) and not any(payload[clut+17:clut+20]), 'Invalid multistage CLUT header')
            end = span(clut,20+math.prod(grids[:inputs])*outputs*precision,'CLUT')
            require(not any(payload[end:min((end+3)&~3,len(payload))]), 'Nonzero multistage CLUT padding')
            spans = [item for item in spans if item[2] != 'CLUT header']
        unique = sorted(set(spans))
        for left,right in zip(unique,unique[1:]):
            require(left[1] <= right[0], 'Overlapping multistage elements')
    except (ValueError,IndexError) as error:
        problems.append(str(error))
    return problems


def inspect_profile_bytes(data):
    """Check header, required printer tags and present binary payload layouts.

    This byte decoder is independent of the Rust admission code and native
    color engine. Unknown private types are retained as opaque private data.
    """
    problems = []
    def check(condition, detail):
        if not condition:
            problems.append(detail)
        return bool(condition)
    def uint(offset, size=4, source=data):
        if offset < 0 or offset+size > len(source):
            raise ValueError('Truncated profile integer')
        return int.from_bytes(source[offset:offset+size], 'big')
    if not check(len(data) >= 132, 'Truncated profile'):
        return problems
    check(uint(0) == len(data), 'Profile size differs from exact bytes')
    check(data[8:12] in (b'\x02\x20\x00\x00',b'\x04\x00\x00\x00',b'\x04\x10\x00\x00',b'\x04\x20\x00\x00'), 'Unsupported profile version')
    check(data[12:20] == b'prtrCMYK' and data[20:24] in (b'XYZ ',b'Lab ') and data[36:40] == b'acsp', 'Profile class/color/PCS/signature differs')
    try:
        datetime.datetime(*(uint(offset,2) for offset in range(24,36,2)))
    except ValueError:
        check(False, 'Invalid profile creation date')
    check(data[68:80] == bytes.fromhex('0000f6d6000100000000d32d'), 'Header illuminant differs from D50')
    check(not any(data[84 if data[8] == 2 else 100:128]), 'Nonzero reserved header bytes')
    check(uint(44)&0xffff&~3 == 0, 'Unsupported reserved profile flag semantics')
    check(uint(56,8)&0xffffffff&~(3 if data[8] == 2 else 15) == 0, 'Unsupported reserved device attribute semantics')
    check(uint(64) <= 3, 'Invalid rendering intent')
    check(data[40:44] in (bytes(4),b'APPL',b'MSFT',b'SGI ',b'SUNW',b'TGNT'), 'Unsupported platform identifier')
    check(not any(data[48:56]), 'Nonzero manufacturer/model needs independent registry evidence outside these control profiles')
    if data[8] == 4 and any(data[84:100]):
        identity = bytearray(data)
        identity[64:68] = bytes(4); identity[84:100] = bytes(16)
        if data[9] == 0:
            identity[56:64] = bytes(8)
        else:
            identity[44:48] = bytes(4)
        check(hashlib.md5(identity).digest() == data[84:100], 'Profile ID differs from edition-specific digest')
    count = uint(128)
    if not check(132+12*count <= len(data), 'Truncated tag table'):
        return problems
    tags, ranges = {}, []
    for index in range(count):
        record = 132+index*12
        name, offset, size = data[record:record+4],uint(record+4),uint(record+8)
        check(name not in tags, 'Duplicate tag signature')
        tags[name] = (offset,size)
        if not check(offset >= 132+12*count and offset % 4 == 0 and size >= 8 and offset+size <= len(data), 'Tag range is invalid'):
            continue
        ranges.append((offset,offset+size))
        payload = data[offset:offset+size]; kind = payload[:4]
        check(not any(payload[4:8]), 'Nonzero reserved tag bytes')
        expected = ({b'desc':b'desc',b'cprt':b'text',b'wtpt':b'XYZ ',b'bkpt':b'XYZ '} if data[8] == 2
                    else {b'desc':b'mluc',b'cprt':b'mluc',b'wtpt':b'XYZ ',b'bkpt':b'XYZ '})
        check(name not in expected or kind == expected[name], 'Incorrect required tag data type')
        if name in (b'A2B0',b'A2B1',b'A2B2',b'B2A0',b'B2A1',b'B2A2',b'gamt'):
            allowed = {b'mft1', b'mft2'}
            if data[8] == 4:
                allowed.add(b'mAB ' if name.startswith(b'A2B') else b'mBA ')
            check(kind in allowed, 'Incorrect transform tag data type')
        if kind == b'XYZ ':
            check(size >= 20 and (size-8)%12 == 0 and (name not in (b'wtpt',b'bkpt') or size == 20), 'Invalid XYZ tag size')
            check(all(int.from_bytes(payload[start:start+4], 'big', signed=True) >= 0 for start in range(8,size,4)), 'Negative XYZ component')
        elif kind == b'text':
            check(size >= 9 and payload[-1] == 0 and payload[8:].isascii(), 'Invalid ASCII text payload')
        elif kind == b'desc':
            try:
                ascii_count = uint(8,source=payload)
                unicode_start = 12+ascii_count
                unicode_count = uint(unicode_start+4,source=payload)
                script_start = unicode_start+8+unicode_count*2
                check(ascii_count > 0 and unicode_start <= size and payload[unicode_start-1] == 0
                      and payload[12:unicode_start].isascii() and script_start+70 == size
                      and payload[script_start+2] <= 67, 'Invalid text description layout')
                if unicode_count:
                    text = payload[unicode_start+8:script_start]
                    check(text[-2:] == bytes(2), 'Unicode description lacks a terminator')
                    text.decode('utf-16-be',errors='strict')
                script_count = payload[script_start+2]
                if script_count:
                    check(payload[script_start+3+script_count-1] == 0, 'Script description lacks a terminator')
                    check(payload[script_start:script_start+2] == bytes(2), 'Non-Roman ScriptCode needs separate decoding evidence')
                    payload[script_start+3:script_start+3+script_count].decode('mac-roman',errors='strict')
            except (ValueError,IndexError):
                check(False,'Truncated text description')
        elif kind == b'mluc':
            try:
                records, stride = uint(8,source=payload),uint(12,source=payload)
                if check(stride == 12 and 16+records*stride <= size, 'Invalid localized text records'):
                    locales = set()
                    for row in range(records):
                        start = 16+row*stride; locale = payload[start:start+4]
                        length, text_offset = uint(start+4,source=payload),uint(start+8,source=payload)
                        check(locale not in locales and re.fullmatch(rb'[a-z]{2}[A-Z]{2}',locale) is not None, 'Invalid or unsupported localized text locale');locales.add(locale)
                        if check(length%2 == 0 and text_offset >= 16+records*stride and text_offset+length <= size,'Invalid localized text bounds'):
                            payload[text_offset:text_offset+length].decode('utf-16-be',errors='strict')
            except (ValueError,UnicodeError):
                check(False,'Invalid localized text payload')
        elif kind == b'mft2':
            if not check(size >= 52,'Truncated LUT16 header'):
                continue
            inputs,outputs,grid = payload[8:11]
            entries_in,entries_out = uint(48,2,payload),uint(50,2,payload)
            if check(1 <= inputs <= 15 and 1 <= outputs <= 15 and grid >= 2
                     and payload[11] == 0 and 2 <= entries_in <= 4096 and 2 <= entries_out <= 4096,'Invalid LUT16 dimensions'):
                check(size == 52+2*(inputs*entries_in+grid**inputs*outputs+outputs*entries_out),'Invalid LUT16 sample layout')
                expected_channels = ((4,3) if name.startswith(b'A2B') else (3,4) if name.startswith(b'B2A') else (3,1) if name == b'gamt' else None)
                check(expected_channels is None or expected_channels == (inputs,outputs),'LUT16 channels differ from device/PCS role')
                if data[20:24] != b'XYZ ' or name.startswith(b'A2B'):
                    check(payload[12:48] == b''.join((65536 if n%4 == 0 else 0).to_bytes(4,'big') for n in range(9)), 'Non-XYZ LUT16 matrix is not identity')
        elif kind in (b'mAB ', b'mBA '):
            expected_channels = ((4,3) if name.startswith(b'A2B') else (3,4) if name.startswith(b'B2A') else (3,1) if name == b'gamt' else None)
            problems.extend(inspect_multistage_lut(payload, expected_channels))
        elif kind == b'sf32':
            check(size >= 12 and (size-8)%4 == 0,'Invalid fixed-point array')
            check(name != b'chad' or size == 44, 'Adaptation matrix must have nine components')
            check(name != b'arts' or size >= 44, 'Private transform matrix needs at least nine components')
    check({b'desc',b'cprt',b'wtpt',b'gamt',b'A2B0',b'A2B1',b'A2B2',b'B2A0',b'B2A1',b'B2A2'} <= set(tags),'Required printer tag absent')
    unique = sorted(set(ranges))
    check(all(left[1] <= right[0] for left,right in zip(unique,unique[1:])),'Partially overlapping tag payloads')
    if data[8] == 4 and unique:
        cursor = 132+12*count
        for start,end in unique:
            check(start == cursor,'Noncontiguous v4 tag payload')
            cursor = (end+3)&~3
            check(not any(data[end:cursor]),'Nonzero v4 tag padding')
        check(cursor == len(data),'v4 trailing padding/size differs')
    return problems


class Inspection:
    def __init__(self, reader, mode):
        self.reader, self.mode = reader, mode
        self.findings, self.observations = [], {'objects': 0, 'streams': 0, 'fonts': [], 'profiles': [], 'dictionary_keys': {}, 'names': {}, 'resource_roles': []}
        self.seen, self.fonts = set(), {}

    def require(self, condition, rule, path, detail):
        if not condition:
            row = {'rule': rule, 'path': path, 'detail': detail}
            if row not in self.findings:
                self.findings.append(row)
        return bool(condition)

    def page_tree(self):
        seen = set()
        def visit(reference, parent):
            if not self.require(isinstance(reference, IndirectObject), 'base-format', '/Pages', 'Page tree child is not indirect'):
                return 0
            identity = (reference.idnum, reference.generation)
            if not self.require(identity not in seen, 'base-format', '/Pages', 'Repeated or cyclic page-tree node'):
                return 0
            seen.add(identity); node = reference.get_object()
            actual_parent = node.raw_get('/Parent') if '/Parent' in node else None
            self.require(actual_parent == parent, 'base-format', '/Pages', 'Page-tree Parent disagrees with its owner')
            if node.get('/Type') == '/Page':
                return 1
            if not self.require(node.get('/Type') == '/Pages' and isinstance(node.get('/Kids'), ArrayObject),
                                'base-format', '/Pages', 'Invalid page-tree node type or Kids'):
                return 0
            count = sum(visit(child,reference) for child in node['/Kids'])
            self.require(isinstance(node.get('/Count'),NumberObject) and node['/Count'] == count,
                         'base-format', '/Pages', 'Page-tree Count differs from descendant pages')
            return count
        root = self.reader.trailer['/Root']
        count = visit(root.raw_get('/Pages'),None)
        self.require(count > 0, 'base-format', '/Pages', 'Empty page tree')
        return not any(row['rule'] == 'base-format' and row['path'] == '/Pages' for row in self.findings)

    def walk(self, value, path):
        if isinstance(value, IndirectObject):
            key = (value.idnum, value.generation)
            if key in self.seen:
                return
            self.seen.add(key)
            self.require(0 < value.idnum <= 8388607, 'base-format', path, 'Object number exceeds the supported range')
            value = value.get_object()
            self.observations['objects'] += 1
        if isinstance(value, NameObject):
            counts = self.observations['names']; counts[str(value)] = counts.get(str(value),0)+1
            try:
                raw = str(value)[1:].encode('utf-8', errors='strict')
                valid = len(raw) <= 127 and '\x00' not in str(value)
            except UnicodeError:
                valid = False
            self.require(valid, 'name-limits', path, 'Invalid UTF-8 or oversized name')
        elif isinstance(value, (NumberObject, FloatObject)) and not isinstance(value, BooleanObject):
            number = float(value)
            # Integer-looking tokens outside the signed integer range are
            # interpreted as reals. Their spelling alone does not make them
            # invalid integers; dictionary-specific integer fields are checked
            # where they are consumed.
            valid = math.isfinite(number) and abs(number) <= 3.403e38 and (number == 0 or abs(number) >= 1.175e-38)
            self.require(valid, 'numeric-limits', path, 'Numeric value exceeds the supported architectural range')
        elif isinstance(value, (ArrayObject, list)):
            for index, item in enumerate(value):
                self.walk(item, f'{path}/{index}')
        elif isinstance(value, DictionaryObject):
            self.dictionary(value, path)
            for name, item in value.items():
                self.walk(name, path + '/<key>')
                self.walk(item, path + str(name))

    def dictionary(self, value, path):
        for key in value:
            counts = self.observations['dictionary_keys']; counts[str(key)] = counts.get(str(key),0)+1
        if '/Resources' in value:
            resources = object_value(value['/Resources'])
            if self.require(isinstance(resources,DictionaryObject), 'resource-references', path, 'Resources must be a dictionary'):
                for category in ('/Font','/XObject','/ExtGState','/Shading'):
                    if category not in resources: continue
                    entries = object_value(resources[category])
                    if not self.require(isinstance(entries,DictionaryObject), 'resource-references', path, 'Named resource category is not a dictionary'): continue
                    for name,target in entries.items():
                        target = object_value(target)
                        valid = isinstance(name,NameObject) and isinstance(target,DictionaryObject)
                        if valid and category == '/Font':
                            valid = target.get('/Type') == '/Font' and target.get('/Subtype') == '/Type0'
                            if valid: self.font(target,path+'/Resources/Font/'+str(name))
                        elif valid and category == '/XObject':
                            valid = isinstance(target,StreamObject) and target.get('/Type') == '/XObject' and target.get('/Subtype') in ('/Form','/Image')
                        elif valid and category == '/ExtGState':
                            valid = target.get('/Type','/ExtGState') == '/ExtGState'
                            if valid: self.graphics_state(target,path+'/Resources/ExtGState/'+str(name))
                        elif valid and category == '/Shading': valid = target.get('/ShadingType') in (2,3)
                        self.require(valid, 'resource-references', path, 'Invalid named '+category+' resource role')
                        self.observations['resource_roles'].append({'path':path,'category':category,'name':str(name),'valid':bool(valid)})
                if '/ProcSet' in resources:
                    names = resources['/ProcSet']
                    self.require(isinstance(names,ArrayObject) and all(isinstance(name,NameObject) and name in
                                 ('/PDF','/Text','/ImageB','/ImageC','/ImageI') for name in names), 'resource-references', path, 'Invalid ProcSet member')
        def absent(keys, rule):
            self.require(not any(key in value for key in keys), rule, path, 'Unsupported entries: ' + ', '.join(key for key in keys if key in value))
        absent(['/OPI', '/Ref', '/F', '/EF', '/FS'], 'embedded-resources')
        self.require(value.get('/Type') != '/Filespec', 'embedded-resources', path, 'File specification is outside the supported model')
        absent(['/AA', '/OpenAction', '/JS', '/JavaScript', '/A'], 'no-actions')
        self.require(value.get('/Type') != '/Action' and value.get('/S') not in {'/JavaScript', '/Launch', '/GoToR', '/SubmitForm'}, 'no-actions', path, 'Action dictionary')
        absent(['/Annots'], 'no-annotations')
        self.require(value.get('/Type') != '/Annot', 'no-annotations', path, 'Annotation dictionary')
        absent(['/OCProperties', '/OC', '/AlternatePresentations', '/PresSteps', '/Collection'], 'no-optional-content')
        absent(['/AcroForm', '/XFA', '/RichMediaContent'], 'no-interactive-forms')
        absent(['/HT', '/HTP', '/BG', '/BG2', '/UCR', '/UCR2'] + ([] if value.get('/S') in ('/Luminosity','/Alpha') else ['/TR']), 'no-halftones')
        self.require(value.get('/TR2', '/Default') == '/Default', 'no-halftones', path, 'Non-default transfer function')
        absent(['/Alternates'], 'no-alternate-images')
        self.require(value.get('/Subtype') != '/PS' and value.get('/Subtype2') != '/PS'
                     and value.get('/FunctionType') != 4, 'no-postscript', path, 'PostScript object or calculator function')
        if self.mode == 'pdfx4':
            absent(['/Perms', '/Encrypt'], 'no-encryption')
        if isinstance(value, StreamObject):
            self.observations['streams'] += 1
            filters = object_value(value.get('/Filter', []))
            if not isinstance(filters, (list, ArrayObject)):
                filters = [filters]
            self.require(all(item == '/FlateDecode' for item in filters), 'supported-streams', path, 'Filter outside plain/Flate generated subset')
            try:
                decoded = value.get_data()
                if filters == ['/FlateDecode']:
                    decoder = zlib.decompressobj()
                    independent = decoder.decompress(value._data) + decoder.flush()
                    self.require(decoder.eof and not decoder.unused_data and not decoder.unconsumed_tail
                                 and independent == decoded, 'supported-streams', path, 'Incomplete, trailing or inconsistent compressed stream')
            except Exception as error:
                self.require(False, 'supported-streams', path, 'Stream decode failed: ' + type(error).__name__)
            if value.get('/Type') == '/Metadata' or value.get('/Subtype') == '/XML':
                self.require('/Filter' not in value, 'metadata-packet', path, 'Metadata stream is filtered')
                self.metadata(value, path, document=False)
            if value.get('/Subtype') == '/Image':
                self.image(value, path)
            if value.get('/Subtype') == '/Form':
                self.form(value, path)
                self.content(value, value.get('/Resources'), path+'/SerializedForm')
        if value.get('/Type') == '/ExtGState':
            self.graphics_state(value, path)
        if value.get('/S') == '/Transparency':
            allowed = {'/DeviceGray', '/DeviceCMYK'} if self.mode == 'pdfx4' else {'/DeviceGray', '/DeviceRGB', '/DeviceCMYK'}
            self.require(value.get('/Type','/Group') == '/Group' and value.get('/CS') in allowed and isinstance(value.get('/I'),BooleanObject)
                         and ('/K' not in value or isinstance(value['/K'],BooleanObject)),
                         'transparency', path, 'Unsupported or implicit group color space')
        if '/ColorSpace' in value:
            color = object_value(value['/ColorSpace'])
            if isinstance(color, NameObject):
                allowed = {'/DeviceGray', '/DeviceCMYK'} if self.mode == 'pdfx4' else {'/DeviceGray', '/DeviceRGB', '/DeviceCMYK'}
                self.require(color in allowed, 'device-color', path, 'Uncharacterized device color space')
            elif isinstance(color, ArrayObject):
                self.require(False, 'device-color', path, 'Color-space array is outside current generated subset')
        if value.get('/ShadingType') is not None:
            self.shading(value, path)
        if value.get('/Type') == '/Font' and value.get('/Subtype') == '/Type0':
            self.font(value, path)

    def graphics_state(self, state, path):
        for key in ('/ca', '/CA'):
            if key in state:
                self.require(isinstance(state[key],(NumberObject,FloatObject)) and 0 <= float(state[key]) <= 1, 'transparency', path, 'Opacity outside zero to one')
        blend = state.get('/BM', '/Normal')
        self.require(isinstance(blend, NameObject) and blend in BLENDS or blend == '/Normal', 'transparency', path, 'Unsupported blend mode')
        self.require(state.get('/RI', '/RelativeColorimetric') in INTENTS, 'graphics-state', path, 'Unknown rendering intent')
        self.require(state.get('/OPM', 0) in (0, 1), 'graphics-state', path, 'Invalid overprint mode')
        mask = object_value(state.get('/SMask', '/None'))
        self.require(mask == '/None' or isinstance(mask,DictionaryObject), 'transparency', path, 'Invalid SMask object kind')
        if isinstance(mask, DictionaryObject):
            form = object_value(mask.get('/G')); group = object_value(form.get('/Group')) if isinstance(form,DictionaryObject) else None
            self.require(mask.get('/S') in {'/Luminosity', '/Alpha'} and isinstance(form,StreamObject)
                         and form.get('/Subtype') == '/Form' and isinstance(group,DictionaryObject) and group.get('/S') == '/Transparency',
                         'transparency', path, 'Invalid soft mask')
            channels = {'/DeviceGray':1,'/DeviceRGB':3,'/DeviceCMYK':4}.get(group.get('/CS')) if isinstance(group,DictionaryObject) else None
            if '/BC' in mask:
                backdrop = mask['/BC']
                self.require(isinstance(backdrop,ArrayObject) and len(backdrop) == channels and all(isinstance(n,(NumberObject,FloatObject)) and 0 <= n <= 1 for n in backdrop),
                             'transparency', path, 'Invalid soft-mask backdrop')
            self.require(mask.get('/TR','/Identity') == '/Identity', 'transparency', path, 'Custom soft-mask transfer is outside the current subset')

    def form(self, form, path):
        bbox = list(form.get('/BBox', [])); matrix = list(form.get('/Matrix', [1,0,0,1,0,0]))
        self.require(form.get('/Type', '/XObject') == '/XObject' and form.get('/FormType',1) == 1
                     and len(bbox) == 4 and all(isinstance(n,(NumberObject,FloatObject)) and math.isfinite(float(n)) for n in bbox)
                     and bbox[0] < bbox[2] and bbox[1] < bbox[3]
                     and len(matrix) == 6 and ('/Matrix' not in form or all(isinstance(n,(NumberObject,FloatObject)) and math.isfinite(float(n)) for n in matrix))
                     and isinstance(object_value(form.get('/Resources')),DictionaryObject), 'embedded-resources', path, 'Invalid independent Form fields')

    def image(self, image, path):
        channels = {'/DeviceGray': 1, '/DeviceRGB': 3, '/DeviceCMYK': 4}.get(image.get('/ColorSpace'))
        width, height, bits = image.get('/Width', 0), image.get('/Height', 0), image.get('/BitsPerComponent', 0)
        valid = channels and image.get('/Type','/XObject') == '/XObject' and isinstance(width, NumberObject) and isinstance(height, NumberObject) and width > 0 and height > 0 and isinstance(bits,NumberObject) and bits == 8
        self.require(valid, 'image-data', path, 'Unsupported image dimensions, samples or color space')
        self.require('/Mask' not in image and ('/ImageMask' not in image or isinstance(image['/ImageMask'],BooleanObject) and image['/ImageMask'].value is False),
                     'image-data', path, 'Stencil and color-key masks are outside the current subset')
        self.require('/Interpolate' not in image or isinstance(image['/Interpolate'],BooleanObject), 'image-data', path, 'Interpolate must be boolean')
        if '/Decode' in image:
            self.require(isinstance(image['/Decode'],ArrayObject) and list(image['/Decode']) == [0,1]*(channels or 0)
                         and all(isinstance(n,(NumberObject,FloatObject)) for n in image['/Decode']), 'image-data', path, 'Custom Decode is outside the current subset')
        if valid:
            self.require(len(image.get_data()) == width * height * channels, 'image-data', path, 'Decoded image sample count differs')
        if '/SMask' in image:
            mask = image['/SMask'].get_object()
            self.require(mask.get('/ColorSpace') == '/DeviceGray' and not bool(mask.get('/ImageMask', False))
                         and '/Mask' not in mask and '/SMask' not in mask, 'image-data', path, 'Invalid soft-mask image entries')
            self.require('/Matte' not in mask or mask.get('/Width') == width and mask.get('/Height') == height,
                         'image-data', path, 'A matte soft mask must match its parent dimensions')
            if '/Matte' in mask:
                matte = mask['/Matte']
                self.require(isinstance(matte,ArrayObject) and len(matte) == channels and all(isinstance(n,(NumberObject,FloatObject)) and 0 <= n <= 1 for n in matte),
                             'image-data', path, 'Invalid parent-color matte components')

    def function(self, function, count, path):
        function = object_value(function)
        self.require(isinstance(function, DictionaryObject), 'shading-functions', path, 'Function dictionary absent')
        if not isinstance(function, DictionaryObject):
            return
        domain = list(function.get('/Domain', []))
        self.require(domain == [0, 1], 'shading-functions', path, 'Unsupported function domain')
        if '/Range' in function:
            ranges = function['/Range']
            self.require(isinstance(ranges,ArrayObject) and len(ranges) == 2*count and all(isinstance(n,(NumberObject,FloatObject)) for n in ranges)
                         and all(a <= b for a,b in zip(ranges[::2],ranges[1::2])), 'shading-functions', path, 'Invalid function Range')
        kind = function.get('/FunctionType')
        if kind == 2:
            a, b = function.get('/C0', []), function.get('/C1', [])
            self.require(len(a) == len(b) == count and all(0 <= float(n) <= 1 for n in [*a, *b])
                         and float(function.get('/N', 0)) == 1, 'shading-functions', path, 'Invalid linear component function')
        elif kind == 3:
            children, bounds, encode = function.get('/Functions', []), function.get('/Bounds', []), function.get('/Encode', [])
            self.require(len(children) >= 1 and len(bounds) == len(children)-1 and len(encode) == 2*len(children)
                         and [0, *bounds, 1] == sorted(set([0, *bounds, 1]))
                         and list(encode) == [0, 1]*len(children), 'shading-functions', path, 'Invalid stitching intervals')
            for index, child in enumerate(children):
                self.function(child, count, f'{path}/Functions/{index}')
        else:
            self.require(False, 'shading-functions', path, 'Unsupported function type')

    def shading(self, shading, path):
        count = {'/DeviceGray': 1, '/DeviceRGB': 3, '/DeviceCMYK': 4}.get(shading.get('/ColorSpace'))
        kind, coords = shading.get('/ShadingType'), shading.get('/Coords', [])
        self.require(isinstance(kind,NumberObject) and all(isinstance(n,(NumberObject,FloatObject)) for n in coords)
                     and ('/Domain' not in shading or list(shading['/Domain']) == [0,1])
                     and ('/Extend' not in shading or isinstance(shading['/Extend'],ArrayObject) and len(shading['/Extend']) == 2 and all(isinstance(n,BooleanObject) for n in shading['/Extend']))
                     and ('/AntiAlias' not in shading or isinstance(shading['/AntiAlias'],BooleanObject)), 'shading-functions', path, 'Invalid shading field types/defaults')
        if '/Background' in shading:
            background = shading['/Background']
            self.require(isinstance(background,ArrayObject) and len(background) == count and all(isinstance(n,(NumberObject,FloatObject)) and 0 <= n <= 1 for n in background), 'shading-functions', path, 'Invalid shading background')
        if '/BBox' in shading:
            box = shading['/BBox']
            self.require(isinstance(box,ArrayObject) and len(box) == 4 and all(isinstance(n,(NumberObject,FloatObject)) for n in box)
                         and box[0] < box[2] and box[1] < box[3], 'shading-functions', path, 'Invalid shading BBox')
        self.require(kind in (2, 3) and len(coords) == (4 if kind == 2 else 6) and bool(count),
                     'shading-functions', path, 'Unsupported shading geometry or color space')
        if kind == 3 and len(coords) == 6:
            self.require(coords[2] >= 0 and coords[5] >= 0, 'shading-functions', path, 'Negative radial radius')
        if count:
            self.function(shading.get('/Function'), count, path + '/Function')

    def font(self, font, path):
        key = id(font)
        if key in self.fonts:
            return self.fonts[key]
        parsed = None
        try:
            if font.get('/Type') != '/Font' or font.get('/Subtype') != '/Type0' or font.get('/Encoding') != '/Identity-H' or len(font['/DescendantFonts']) != 1:
                raise ValueError('Unsupported composite encoding')
            child = font['/DescendantFonts'][0].get_object()
            descriptor = child['/FontDescriptor'].get_object()
            programs = [name for name in ('/FontFile2', '/FontFile3') if name in descriptor]
            if len(programs) != 1 or not font['/ToUnicode'].get_object().get_data():
                raise ValueError('Missing embedded program or Unicode map')
            data = descriptor[programs[0]].get_object().get_data()
            kind = 'truetype' if programs[0] == '/FontFile2' else 'cff'
            self.require(child.get('/Type') == '/Font' and child.get('/Subtype') == ('/CIDFontType2' if kind == 'truetype' else '/CIDFontType0')
                         and descriptor.get('/Type') == '/FontDescriptor', 'font-programs', path, 'Font dictionary types disagree with embedded program')
            program_stream = descriptor[programs[0]].get_object()
            self.require(program_stream.get('/Length1') == len(data) if kind == 'truetype' else program_stream.get('/Subtype') == '/CIDFontType0C',
                         'font-programs', path, 'Embedded program stream role or decoded length differs')
            name = child.get('/BaseFont')
            self.require(isinstance(name, NameObject) and descriptor.get('/FontName') == name
                         and font.get('/BaseFont') == (str(name) + '-Identity-H' if kind == 'cff' else name),
                         'font-programs', path, 'Parent, descendant and descriptor names disagree')
            system = child.get('/CIDSystemInfo', {})
            self.require(isinstance(system.get('/Registry'),(TextStringObject,ByteStringObject)) and string_bytes(system['/Registry']) == b'Adobe'
                         and isinstance(system.get('/Ordering'),(TextStringObject,ByteStringObject)) and string_bytes(system['/Ordering']) == b'Identity'
                         and isinstance(system.get('/Supplement'),NumberObject) and system['/Supplement'] == 0,
                         'font-programs', path, 'Unexpected CID system information')
            flags = descriptor.get('/Flags')
            self.require(isinstance(flags, NumberObject) and flags >= 0 and not flags & ~0x7006f
                         and bool(flags & 4) != bool(flags & 32), 'font-descriptors', path, 'Invalid or contradictory descriptor flags')
            unicode_data = font['/ToUnicode'].get_object().get_data()
            unicode_map = font_evaluator('pdf_text_metadata_rules').inspect_tounicode(unicode_data)
            self.require(not unicode_map['findings'], 'font-glyphs', path, json.dumps(unicode_map['findings']))
            binary = font_evaluator('pdf_font_program_rules').inspect_font_program(data, kind)
            self.require(not binary['findings'], 'font-program-layout', path, json.dumps(binary['findings']))
            hint = None
            advances = {}
            if programs[0] == '/FontFile2':
                program = TTFont(io.BytesIO(data), lazy=False)
                upm = program['head'].unitsPerEm
                scale = f32(1000 / upm)
                for gid, name in enumerate(program.getGlyphOrder()):
                    advances[gid] = f32(program['hmtx'][name][0] * scale)
                head = program['head']
                bounds = [getattr(head, key) for key in ('xMin', 'yMin', 'xMax', 'yMax')]
                hint = font_evaluator('pdf_truetype_hint_execution').inspect_truetype_hints(data)
                self.require(not hint['findings'], 'font-hint-execution', path, json.dumps(hint['findings']))
                self.require(child.get('/CIDToGIDMap') == '/Identity', 'font-programs', path, 'Unsupported CID to GID map')
            else:
                program = CFFFontSet(); program.decompile(io.BytesIO(data), None)
                top = program.topDictIndex[0]
                matrix = getattr(top, 'FontMatrix', [0.001, 0, 0, 0.001, 0, 0])
                scale = matrix[0] * 1000
                boxes = []
                for gid, name in enumerate(top.charset):
                    charstring = top.CharStrings[name]
                    pen = BoundsPen(top.CharStrings); charstring.draw(pen)
                    if pen.bounds:
                        boxes.append(pen.bounds)
                    cid = binary['observations']['cid_charset'][gid]
                    advances[cid] = f32(charstring.width * f32(scale))
                bounds = [choose((box[index] for box in boxes), default=0)
                          for index, choose in enumerate((min, min, max, max))]
            required = ('/FontName', '/Flags', '/FontBBox', '/ItalicAngle', '/Ascent', '/Descent', '/StemV')
            self.require(all(key in descriptor for key in required), 'font-descriptors', path, 'Missing required descriptor field')
            expected_bbox = [f32(value * f32(scale)) for value in bounds]
            declared_bbox = list(map(float, descriptor.get('/FontBBox', [])))
            self.require(len(declared_bbox) == 4 and all(f32(a) == b for a,b in zip(declared_bbox, expected_bbox)),
                         'font-descriptors', path, 'Descriptor bounding box differs from embedded outlines')
            self.require(all(math.isfinite(float(descriptor.get(key, math.nan))) for key in required[3:])
                         and descriptor.get('/StemV', -1) >= 0, 'font-descriptors', path, 'Invalid descriptor metric')
            destinations = re.findall(rb'<[0-9A-Fa-f]{4}>\s*<([0-9A-Fa-f]+)>', font['/ToUnicode'].get_object().get_data())
            latin = any(any('A' <= ch <= 'Z' or 'a' <= ch <= 'z' for ch in bytes.fromhex(value.decode()).decode('utf-16be'))
                        for value in destinations)
            self.require(not latin or math.isfinite(float(descriptor.get('/CapHeight', math.nan)))
                         and descriptor.get('/CapHeight', 0) > 0, 'font-descriptors', path, 'Latin glyph mapping requires a positive capital height')
            widths, values, index = {}, list(child.get('/W', [])), 0
            previous = -1
            while index < len(values):
                if not isinstance(values[index], NumberObject) or not 0 <= values[index] <= 65535 or values[index] <= previous:
                    raise ValueError('Widths require ascending nonoverlapping integer CIDs')
                start = int(values[index]); following = object_value(values[index+1]); index += 2
                if isinstance(following, ArrayObject):
                    if not following or start+len(following) > 65536:
                        raise ValueError('Invalid consecutive CID width range')
                    for offset, width in enumerate(following):
                        if not isinstance(width,(NumberObject,FloatObject)): raise ValueError('Width must be numeric')
                        widths[start+offset] = float(width)
                    previous = start+len(following)-1
                else:
                    if not isinstance(following, NumberObject) or not start <= following <= 65535:
                        raise ValueError('Invalid constant CID width range')
                    if not isinstance(values[index],(NumberObject,FloatObject)): raise ValueError('Width must be numeric')
                    width = float(values[index]); index += 1
                    for cid in range(start, int(following)+1):
                        widths[cid] = width
                    previous = int(following)
            self.require(('/DW' not in child or isinstance(child['/DW'],(NumberObject,FloatObject)))
                         and math.isfinite(float(child.get('/DW',1000))) and all(math.isfinite(width) for width in widths.values()),
                         'font-metrics', path, 'Nonfinite default or explicit width')
            cidset = descriptor.get('/CIDSet')
            expected_set = bytes(sum(128 >> bit for bit in range(8) if byte*8+bit in advances) for byte in range((max(advances,default=0)+8)//8))
            self.require(cidset is not None and cidset.get_object().get_data() == expected_set,
                         'font-programs', path, 'CIDSet does not identify exactly the embedded glyphs')
            # The public emitter writes binary32 metrics using round-tripping
            # decimals. Compare that exact representable value, without an
            # integer-rounding rule or an arbitrary metric tolerance.
            errors = [abs(width-advances.get(cid, math.inf)) for cid,width in widths.items()]
            self.require(bool(widths) and all(cid in advances and f32(width) == advances[cid]
                         for cid,width in widths.items()), 'font-metrics', path, 'Dictionary widths differ from decoded prepared-program metrics')
            parsed = {'advances': advances, 'widths': widths, 'unicode': unicode_data,
                      'unicode_map': unicode_map['observations'].get('mappings', {})}
            self.observations['fonts'].append({'path': path, 'program_sha256': sha(data), 'glyphs': len(advances),
                                               'widths': len(widths), 'maximum_decimal_width_error': max(errors, default=0),
                                               'program_layout': binary['observations'],
                                               'unicode_map': unicode_map['observations'],
                                               'hint_execution': hint['observations'] if hint else None})
        except Exception as error:
            self.require(False, 'font-programs', path, type(error).__name__ + ': ' + str(error))
        self.fonts[key] = parsed
        return parsed

    def content(self, stream, resources, path, depth=0, active=frozenset()):
        resources = object_value(resources)
        if not isinstance(resources, DictionaryObject):
            resources = DictionaryObject()
        identity = id(object_value(stream))
        if not self.require(identity not in active, 'resource-references', path, 'Recursive Form invocation'):
            return
        active = active | {identity}
        try:
            operations = ContentStream(stream, self.reader).operations
        except Exception as error:
            self.require(False, 'known-operators', path, 'Content parse failed: ' + type(error).__name__)
            return
        current_font, initial, text, marked = None, depth, False, 0
        state, font_stack = 'page', []
        self.require(self.mode != 'pdfx4' or depth <= 28, 'graphics-state-depth', path, 'Implicit Form save exceeds 28')
        for index, (args, op) in enumerate(operations):
            here = f'{path}/op/{index}'
            if not self.require(op in ARITY and len(args) == ARITY.get(op), 'known-operators', here,
                                'Unknown operator or operand count: ' + repr(op)):
                continue
            numeric = lambda value: isinstance(value, (NumberObject, FloatObject))
            if op in (b'Tj', b"'"):
                valid_args = isinstance(args[0], (ByteStringObject, TextStringObject))
            elif op == b'"':
                valid_args = all(numeric(value) for value in args[:2]) and isinstance(args[2], (ByteStringObject, TextStringObject))
            elif op == b'TJ':
                valid_args = isinstance(args[0], ArrayObject) and all(numeric(value) or isinstance(value, (ByteStringObject, TextStringObject)) for value in args[0])
            elif op == b'd':
                valid_args = isinstance(args[0], ArrayObject) and all(numeric(value) and value >= 0 for value in args[0]) and numeric(args[1])
            elif op == b'Tf':
                valid_args = isinstance(args[0], NameObject) and numeric(args[1])
            elif op in (b'gs', b'Do', b'sh', b'ri', b'BMC'):
                valid_args = isinstance(args[0], NameObject)
            elif op == b'BDC':
                valid_args = isinstance(args[0], NameObject) and isinstance(args[1], DictionaryObject) and args[0] != '/OC'
            else:
                valid_args = all(numeric(value) for value in args)
            if not self.require(valid_args, 'known-operators', here, 'Invalid operand types'):
                continue
            construction = {b'm',b'l',b'c',b'v',b'y',b'h',b're'}
            painting = {b'S',b's',b'f',b'F',b'f*',b'B',b'B*',b'b',b'b*',b'n'}
            positioning = {b'Td',b'TD',b'Tm',b'T*',b'Tj',b'TJ',b"'",b'"'}
            if state == 'clip':
                context_valid = op in painting
            elif op in construction:
                context_valid = state in ('page','path') if op in (b'm',b're') else state == 'path'
            elif op in painting or op in (b'W',b'W*'):
                context_valid = state == 'path'
            elif op == b'BT':
                context_valid = state == 'page'
            elif op == b'ET' or op in positioning:
                context_valid = state == 'text'
            elif op in (b'q',b'Q',b'cm',b'Do',b'sh'):
                context_valid = state == 'page'
            else:
                context_valid = state in ('page','text')
            self.require(context_valid, 'known-operators', here, 'Operator is illegal in '+state+' context')
            if op in construction: state = 'path'
            elif op in painting: state = 'page'
            elif op in (b'W',b'W*'): state = 'clip'
            elif op == b'BT': state = 'text'
            elif op == b'ET': state = 'page'
            restricted = ((op == b'w' and args[0] < 0) or (op == b'M' and args[0] < 1)
                          or (op in (b'J',b'j') and (not isinstance(args[0], NumberObject) or args[0] not in (0,1,2)))
                          or (op == b'Tr' and (not isinstance(args[0], NumberObject) or not 0 <= args[0] <= 7))
                          or (op == b'd' and bool(args[0]) and not any(args[0])))
            self.require(not restricted, 'known-operators', here, 'Invalid graphics-state operand value')
            def strings(value):
                if isinstance(value, (ByteStringObject, TextStringObject)):
                    yield value
                elif isinstance(value, ArrayObject):
                    for item in value:
                        yield from strings(item)
                elif isinstance(value, DictionaryObject):
                    for item in value.values():
                        yield from strings(item)
            for arg in args:
                self.walk(arg, here)
                for string in strings(arg):
                    self.require(len(string_bytes(string)) <= 32767, 'content-string-limit', here, 'Content string exceeds 32767 bytes')
            if op == b'q':
                depth += 1
                font_stack.append(current_font)
                self.require(self.mode != 'pdfx4' or depth <= 28, 'graphics-state-depth', here, 'Saved state depth exceeds 28')
            elif op == b'Q':
                depth -= 1
                current_font = font_stack.pop() if font_stack else None
                self.require(depth >= initial, 'graphics-state-depth', here, 'Unmatched restore')
            elif op in (b'BT', b'ET'):
                self.require(text == (op == b'ET'), 'known-operators', here, 'Unbalanced text object')
                text = op == b'BT'
            elif op in (b'BMC', b'BDC'):
                marked += 1
            elif op == b'EMC':
                marked -= 1
                self.require(marked >= 0, 'known-operators', here, 'Unmatched marked-content end')
            elif op in (b'rg', b'RG') and self.mode == 'pdfx4':
                self.require(False, 'device-color', here, 'Uncharacterized RGB print paint')
            elif op == b'ri':
                self.require(args[0] in INTENTS, 'graphics-state', here, 'Unknown rendering intent')
            if op in (b'gs', b'Tf', b'Do', b'sh'):
                category = {b'gs': '/ExtGState', b'Tf': '/Font', b'Do': '/XObject', b'sh': '/Shading'}[op]
                entries = object_value(resources.get(category, {}))
                target = object_value(entries.get(args[0]))
                if not self.require(target is not None, 'resource-references', here, 'Missing ' + category + ' ' + str(args[0])):
                    continue
                if op == b'Tf':
                    current_font = self.font(target, here)
                elif op == b'Do' and target.get('/Subtype') == '/Form':
                    self.content(target, target.get('/Resources', resources), here + '/Form', depth+1, active)
                elif op == b'gs':
                    mask = object_value(target.get('/SMask'))
                    if isinstance(mask, DictionaryObject) and '/G' in mask:
                        form = object_value(mask['/G'])
                        self.content(form, form.get('/Resources', resources), here + '/Mask', depth+1, active)
            if op in (b'Tj', b'TJ', b"'", b'"'):
                self.require(text and current_font is not None, 'font-glyphs', here, 'Text outside a text object or without valid font')
                if current_font:
                    for value in strings(args[-1]):
                        raw = string_bytes(value)
                        valid = len(raw) % 2 == 0
                        for offset in range(0, len(raw)-1, 2):
                            cid = int.from_bytes(raw[offset:offset+2], 'big')
                            valid = valid and 0 < cid <= 65535 and cid in current_font['advances'] and cid in current_font['widths']
                            valid = valid and f'{cid:04X}' in current_font['unicode_map']
                        self.require(valid, 'font-glyphs', here, 'Missing, zero or unmapped drawn CID')
        self.require(depth == initial, 'graphics-state-depth', path, 'Unbalanced saved graphics states')
        self.require(not text, 'known-operators', path, 'Unclosed text object')
        self.require(state == 'page', 'known-operators', path, 'Unterminated path or clip')
        self.require(marked == 0, 'known-operators', path, 'Unbalanced marked content')

    def metadata(self, stream, path, document):
        try:
            raw = stream.get_data()
            packet = font_evaluator('pdf_text_metadata_rules').inspect_packet(raw)
            self.require(not packet['findings'], 'metadata-packet', path, json.dumps(packet['findings']))
            self.require('/Filter' not in stream, 'metadata-packet', path, 'Metadata must be unfiltered')
            self.require(not re.search(br'<\?xpacket[^?]*\b(?:bytes|encoding)\s*=', raw),
                         'metadata-packet', path, 'Deprecated packet attribute')
            self.require(b'<!DOCTYPE' not in raw and b'<!ENTITY' not in raw, 'metadata-packet', path, 'Unsupported XML declaration')
            tree = ET.fromstring(raw)
            rdf = tree.find('.//rdf:RDF', NS)
            self.require(rdf is not None and all(child.tag == '{'+NS['rdf']+'}Description' for child in rdf),
                         'metadata-packet', path, 'Unsupported RDF/XML subject structure')
            if not document:
                return
            def value(tag):
                element = tree.find('.//' + tag, NS)
                return ''.join(element.itertext()) if element is not None else None
            title = tree.find('.//dc:title/rdf:Alt/rdf:li[@xml:lang="x-default"]', {**NS, 'xml': 'http://www.w3.org/XML/1998/namespace'})
            title = (title.text or '') if title is not None else None
            required = [value(tag) for tag in ('pdf:Producer', 'xmp:CreatorTool', 'xmpMM:DocumentID', 'xmpMM:VersionID', 'xmpMM:RenditionClass')]
            title_valid = title is not None and (self.mode != 'pdfx4' or bool(title.strip()))
            self.require(title_valid and all(required), 'metadata-values', path, 'Missing required descriptive value')
            self.require(re.fullmatch(r'uuid:[0-9a-fA-F]{8}(?:-[0-9a-fA-F]{4}){3}-[0-9a-fA-F]{12}', value('xmpMM:DocumentID') or '') is not None,
                         'metadata-values', path, 'Invalid document UUID')
            dates = {}
            for tag in ('xmp:CreateDate', 'xmp:ModifyDate', 'xmp:MetadataDate'):
                try:
                    dates[tag] = datetime.datetime.fromisoformat(value(tag).replace('Z', '+00:00'))
                    self.require(dates[tag].tzinfo is not None, 'metadata-values', path, 'Date lacks UTC offset')
                except (TypeError, ValueError, AttributeError):
                    self.require(False, 'metadata-values', path, 'Invalid required date: ' + tag)
            self.require(value('pdf:Trapped') == 'False', 'metadata-values', path, 'Supported writer has no trapping operation')
            info = object_value(self.reader.trailer.get('/Info', {}))
            self.require('/Trapped' in info and str(info['/Trapped']).removeprefix('/') == value('pdf:Trapped'),
                         'metadata-mapping', path, 'Info/XMP trapping state differs')
            mappings = {'/Title': title, '/Producer': value('pdf:Producer'), '/Creator': value('xmp:CreatorTool'),
                        '/Subject': value('dc:description'), '/Keywords': value('pdf:Keywords')}
            for key, mapped in mappings.items():
                if key in info:
                    self.require(str(info[key]) == mapped, 'metadata-mapping', path, 'Info/XMP mismatch: ' + key)
            for key, tag in (('/CreationDate', 'xmp:CreateDate'), ('/ModDate', 'xmp:ModifyDate')):
                if key in info:
                    parsed = getattr(self.reader.metadata, 'creation_date' if key == '/CreationDate' else 'modification_date')
                    self.require(parsed == dates.get(tag), 'metadata-mapping', path, 'Info/XMP date mismatch')
            if '/Author' in info:
                creators = tree.findall('.//dc:creator/rdf:Seq/rdf:li', NS)
                self.require(len(creators) == 1 and creators[0].text == info['/Author'], 'metadata-mapping', path, 'Author sequence mismatch')
            identity = value('pdfxid:GTS_PDFXVersion')
            if self.mode == 'pdfx4':
                self.require(identity == 'PDF/X-4' and str(info.get('/GTS_PDFXVersion')) == 'PDF/X-4'
                             and '/GTS_PDFXConformance' not in info,
                             'metadata-identity', path, 'Missing or conflicting print identity')
                for prefix, uri in NS.items():
                    if prefix != 'rdf':
                        self.require(re.search(rb'xmlns:'+prefix.encode()+rb'\s*=\s*[\"\']'+re.escape(uri.encode())+rb'[\"\']', raw) is not None,
                                     'metadata-packet', path, 'Required namespace prefix absent: '+prefix)
                self.require(all(element.tag == '{'+NS['pdfxid']+'}GTS_PDFXVersion'
                                 for element in tree.iter() if element.tag.startswith('{'+NS['pdfxid']+'}')),
                             'metadata-identity', path, 'Unsupported identity property')
                if '/GTS_PDFXVersion' in info:
                    self.require(string_bytes(info['/GTS_PDFXVersion']) == b'PDF/X-4', 'metadata-identity', path, 'Identity string is not the direct byte encoding')
            else:
                self.require(identity is None and '/GTS_PDFXVersion' not in info, 'metadata-identity', path, 'Ordinary output has a print identity')
        except (ET.ParseError, ValueError, TypeError, KeyError) as error:
            self.require(False, 'metadata-packet', path, type(error).__name__ + ': ' + str(error))

    def envelope(self):
        root = self.reader.trailer['/Root']
        self.require(not self.reader.is_encrypted, 'no-encryption', '/Trailer', 'Encrypted file')
        ids = object_value(self.reader.trailer.get('/ID', []))
        self.require(len(ids) == 2 and all(not isinstance(value, IndirectObject)
                     and isinstance(value, (ByteStringObject, TextStringObject)) and len(string_bytes(value)) >= 16 for value in ids),
                     'file-identity', '/Trailer/ID', 'Missing direct identifiers')
        metadata = object_value(root.get('/Metadata'))
        if self.require(isinstance(metadata, StreamObject), 'metadata-packet', '/Root/Metadata', 'Document metadata absent'):
            self.metadata(metadata, '/Root/Metadata', True)
        intents = object_value(root.get('/OutputIntents', []))
        selected = [object_value(item) for item in intents if object_value(item).get('/S') == '/GTS_PDFX']
        if self.mode == 'ordinary':
            self.require(not selected, 'metadata-identity', '/Root/OutputIntents', 'Ordinary output has a print output intent')
            return
        self.require(len(selected) == 1, 'output-intent', '/Root/OutputIntents', 'Exactly one print output intent required')
        for intent in selected:
            self.require(isinstance(intent.get('/OutputConditionIdentifier'), TextStringObject)
                         and bool(str(intent['/OutputConditionIdentifier']).strip())
                         and bool(str(intent.get('/Info', '')).strip()) and '/DestOutputProfileRef' not in intent,
                         'output-intent', '/Root/OutputIntents', 'Missing description or unsupported external profile')
            registry = intent.get('/RegistryName')
            if registry:
                self.require(str(registry) == 'http://www.color.org' and bool(intent.get('/OutputCondition')),
                             'output-intent', '/Root/OutputIntents', 'Unsupported registry declaration')
            profile = object_value(intent.get('/DestOutputProfile'))
            if not self.require(isinstance(profile, StreamObject), 'output-intent', '/Root/OutputIntents', 'Embedded output profile absent'):
                continue
            try:
                data = profile.get_data()
                for problem in inspect_profile_bytes(data):
                    self.require(False, 'profile-integrity', '/Root/OutputIntents', problem)
                version = data[8:12].hex()
                valid = len(data) >= 132 and int.from_bytes(data[:4], 'big') == len(data)
                valid = valid and data[12:16] == b'prtr' and data[16:20] == b'CMYK' and data[36:40] == b'acsp'
                valid = valid and data[8] in (2, 4) and profile.get('/N') == 4
                count = int.from_bytes(data[128:132], 'big')
                valid = valid and 132 + count*12 <= len(data)
                for index in range(count):
                    entry = data[132+index*12:144+index*12]
                    offset, size = int.from_bytes(entry[4:8], 'big'), int.from_bytes(entry[8:12], 'big')
                    valid = valid and offset % 4 == 0 and size >= 8 and offset >= 128 and offset+size <= len(data)
                parsed = ImageCms.ImageCmsProfile(io.BytesIO(data))
                ImageCms.buildTransformFromOpenProfiles(parsed, ImageCms.createProfile('sRGB'), 'CMYK', 'RGB')
                self.require(valid and parsed.profile.xcolor_space.strip() == 'CMYK', 'profile-integrity', '/Root/OutputIntents', 'Invalid printer profile structure/channels')
                tag_types = []
                for index in range(count):
                    entry=data[132+index*12:144+index*12]; offset=int.from_bytes(entry[4:8],'big');size=int.from_bytes(entry[8:12],'big')
                    tag_types.append({'tag':entry[:4].decode('latin1'),'type':data[offset:offset+4].decode('latin1'),'bytes':size})
                self.observations['profiles'].append({'sha256': sha(data), 'bytes': len(data), 'version': version,
                                                       'channels': int(profile.get('/N', 0)), 'tags': count,'tag_types':tag_types,
                                                       'header':{'platform':data[40:44].hex(),'flags':int.from_bytes(data[44:48],'big'),
                                                                 'manufacturer_model':data[48:56].hex(),'attributes':int.from_bytes(data[56:64],'big'),
                                                                 'intent':int.from_bytes(data[64:68],'big'),'creator':data[80:84].hex()}})
            except Exception as error:
                self.require(False, 'profile-integrity', '/Root/OutputIntents', type(error).__name__ + ': ' + str(error))

    def pages(self):
        for index, page in enumerate(self.reader.pages):
            path = f'/Pages/{index+1}'
            boxes = {}
            for key in ('/MediaBox', '/CropBox', '/BleedBox', '/TrimBox', '/ArtBox'):
                if key in page:
                    box = list(page[key])
                    valid = len(box) == 4 and all(isinstance(n, (NumberObject, FloatObject)) for n in box) and box[0] < box[2] and box[1] < box[3]
                    if self.require(valid, 'page-boxes', path+key, 'Invalid rectangle'):
                        boxes[key] = box
            if self.mode == 'pdfx4':
                self.require('/MediaBox' in boxes and (('/TrimBox' in boxes) != ('/ArtBox' in boxes)), 'page-boxes', path, 'Missing media or unique trim/art box')
            def contains(outer, inner):
                return outer[0] <= inner[0] and outer[1] <= inner[1] and outer[2] >= inner[2] and outer[3] >= inner[3]
            media = boxes.get('/MediaBox')
            if media:
                self.require(all(contains(media, box) for box in boxes.values()), 'page-boxes', path, 'Box extends outside media')
            chosen = boxes.get('/TrimBox', boxes.get('/ArtBox'))
            if chosen:
                self.require(contains(boxes.get('/BleedBox', boxes.get('/CropBox', media)), chosen), 'page-boxes', path, 'Trim/art extends outside bleed')
            if '/CropBox' in boxes:
                self.require(all(contains(boxes['/CropBox'], boxes[key]) for key in ('/BleedBox', '/TrimBox', '/ArtBox') if key in boxes),
                             'page-boxes', path, 'Box extends outside crop')
            if '/Contents' in page:
                self.content(page['/Contents'], page.get('/Resources'), path + '/Contents')


def inspect_pdf(path, mode='pdfx4', qpdf=None):
    path = Path(path)
    data = path.read_bytes()
    reader = PdfReader(io.BytesIO(data), strict=True)
    inspection = Inspection(reader, mode)
    serialized = inspect_serialized_file(data,reader)
    inspection.require(not serialized, 'base-format', '/File', '; '.join(serialized))
    try:
        longest = inspect_nonstream_lines(data, reader)
        inspection.observations['maximum_nonstream_line_bytes'] = longest
        inspection.require(longest <= 255, 'nonstream-line-length', '/File', 'Physical nonstream line exceeds 255 bytes')
    except (ValueError, KeyError, TypeError) as error:
        inspection.require(False, 'nonstream-line-length', '/File', str(error))
    inspection.require(reader.pdf_header in ('%PDF-1.3', '%PDF-1.4', '%PDF-1.5', '%PDF-1.6'),
                       'base-format', '/Header', 'Unsupported PDF version')
    for generation, entries in reader.xref.items():
        for number in entries:
            if number:
                inspection.walk(IndirectObject(number, generation, reader), f'/Objects/{number}/{generation}')
    for number in reader.xref_objStm:
        inspection.walk(IndirectObject(number, 0, reader), f'/Objects/{number}/0')
    inspection.envelope()
    if inspection.page_tree():
        inspection.pages()
    qpdf_result = None
    if qpdf:
        result = subprocess.run([str(qpdf), '--check', str(path)], capture_output=True, timeout=120,
                                creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0), check=False)
        qpdf_result = {'exit_code': result.returncode, 'stdout_sha256': sha(result.stdout), 'stderr_sha256': sha(result.stderr)}
        inspection.require(result.returncode == 0, 'base-format', '/File', 'qpdf rejected the exact file')
    if path.read_bytes() != data:
        raise ValueError('Inspection changed its input')
    return {'schema': 'pdf-library-supported-output-verification/v1', 'mode': mode,
            'input_sha256': sha(data), 'input_bytes': len(data), 'unchanged': True,
            'passed': not inspection.findings, 'rules_evaluated': list(RULES),
            'findings': inspection.findings, 'observations': inspection.observations,
            'qpdf': qpdf_result,
            'tools': {name: importlib.metadata.version(name) for name in ('pypdf', 'fonttools', 'Pillow')},
            'littlecms': str(ImageCms.core.littlecms_version), 'evaluator_sha256': sha(Path(__file__).read_bytes())}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('pdf', type=Path)
    parser.add_argument('--mode', choices=('ordinary', 'pdfx4'), default='pdfx4')
    parser.add_argument('--qpdf', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    report = inspect_pdf(args.pdf, args.mode, args.qpdf)
    with args.output.open('x', encoding='utf-8', newline='\n') as output:
        json.dump(report, output, indent=2); output.write('\n')
    print(json.dumps({'passed': report['passed'], 'input_sha256': report['input_sha256'], 'findings': report['findings']}))
    raise SystemExit(0 if report['passed'] else 1)
