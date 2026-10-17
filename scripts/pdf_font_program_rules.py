"""Independent checks for the embedded TrueType and CID-CFF programs we emit.

This is a development evaluator for a bounded generated subset, not a general
font validator. It does not change input bytes. Binary predicates supplement
fontTools' outline decoding; successful parsing/drawing alone is insufficient.
Unsupported execution constructs produce findings rather than implied coverage.
"""
import io
import math
import struct

from fontTools.ttLib import TTFont


class _Invalid(ValueError):
    def __init__(self, rule, detail):
        self.rule = rule
        super().__init__(detail)


def _require(condition, rule, detail):
    if not condition:
        raise _Invalid(rule, detail)


def _integer(value, rule, label):
    _require(isinstance(value, (int, float)) and math.isfinite(value) and int(value) == value,
             rule, label + ' must be an integer')
    return int(value)


def _sum32(data):
    padded = data + b'\0' * (-len(data) % 4)
    return sum(struct.unpack('>' + 'I' * (len(padded) // 4), padded)) & 0xffffffff


def _instruction_tokens(data):
    """Check byte extents, not execution semantics or reachable stack state."""
    pos = 0; operators = []; largest_push = 0
    while pos < len(data):
        op = data[pos]; pos += 1; operators.append(op)
        if op in (0x40, 0x41):
            _require(pos < len(data), 'truetype-instructions', 'Missing variable PUSH count')
            count = data[pos]; pos += 1
            size = count * (2 if op == 0x41 else 1)
        elif 0xb0 <= op <= 0xbf:
            count = (op & 7) + 1
            size = count * (2 if op >= 0xb8 else 1)
        else:
            continue
        _require(pos + size <= len(data), 'truetype-instructions', 'PUSH data exceeds instruction extent')
        largest_push = max(largest_push, count); pos += size
    return operators, largest_push


def _raw_glyph(raw, n):
    if not raw:
        return [], b''
    _require(len(raw) >= 10, 'truetype-glyf', 'Truncated glyph header')
    contours, x0, y0, x1, y1 = struct.unpack_from('>5h', raw)
    _require(x0 <= x1 and y0 <= y1, 'truetype-glyf', 'Reversed glyph bounds')
    at = 10; components = []; instructions = b''
    if contours >= 0:
        # An explicitly encoded zero-contour glyph can omit its remaining data.
        if contours == 0 and len(raw) == 10:
            return [], b''
        _require(at + contours * 2 + 2 <= len(raw), 'truetype-glyf', 'Truncated contour endpoints')
        ends = struct.unpack_from('>' + 'H' * contours, raw, at)
        _require(all(a < b for a, b in zip(ends, ends[1:])), 'truetype-glyf', 'Contour endpoints do not increase')
        at += contours * 2
        length = struct.unpack_from('>H', raw, at)[0]; at += 2
        _require(at + length <= len(raw), 'truetype-glyf', 'Glyph instructions exceed glyph extent')
        instructions = raw[at:at + length]; at += length
        count = ends[-1] + 1 if ends else 0
        flags = []
        while len(flags) < count:
            _require(at < len(raw), 'truetype-glyf', 'Truncated point flags')
            flag = raw[at]; at += 1
            _require(flag & 0xc0 == 0, 'truetype-glyf', 'Reserved point flag bits')
            repeat = 0
            if flag & 8:
                _require(at < len(raw), 'truetype-glyf', 'Missing flag repeat count')
                repeat = raw[at]; at += 1
            _require(len(flags) + 1 + repeat <= count, 'truetype-glyf', 'Flag repeat exceeds point count')
            flags.extend([flag] * (repeat + 1))
        for short, same in ((2, 16), (4, 32)):
            at += sum(1 if flag & short else 0 if flag & same else 2 for flag in flags)
            _require(at <= len(raw), 'truetype-glyf', 'Coordinate deltas exceed glyph extent')
    else:
        have_instructions = False
        while True:
            _require(at + 4 <= len(raw), 'truetype-glyf', 'Truncated composite component')
            flags, gid = struct.unpack_from('>HH', raw, at); at += 4
            _require(gid < n and flags & 0x10 == 0, 'truetype-glyf', 'Invalid component glyph or obsolete flag')
            _require(sum(bool(flags & bit) for bit in (8, 64, 128)) <= 1,
                     'truetype-glyf', 'Conflicting component transforms')
            _require(flags & 0x1800 != 0x1800, 'truetype-glyf', 'Conflicting component-offset scaling flags')
            at += (4 if flags & 1 else 2) + (2 if flags & 8 else 4 if flags & 64 else 8 if flags & 128 else 0)
            _require(at <= len(raw), 'truetype-glyf', 'Composite arguments/transform exceed glyph extent')
            components.append(gid); have_instructions |= bool(flags & 256)
            if not flags & 32:
                break
        if have_instructions:
            _require(at + 2 <= len(raw), 'truetype-glyf', 'Missing composite instruction length')
            length = struct.unpack_from('>H', raw, at)[0]; at += 2
            _require(at + length <= len(raw), 'truetype-glyf', 'Composite instructions exceed glyph extent')
            instructions = raw[at:at + length]; at += length
    _require(not any(raw[at:]), 'truetype-glyf-padding-unresolved',
             'Noncanonical internal glyph padding requires separate analysis; no mandatory zero rule is inferred')
    return components, instructions


def _name_post(data, tables, n, advances, observations):
    def table(tag):
        offset, length = tables[tag]
        return data[offset:offset + length]
    if 'name' in tables:
        raw = table('name')
        _require(len(raw) >= 6, 'truetype-name', 'Truncated name header')
        version, count, base = struct.unpack_from('>3H', raw)
        _require(version == 0, 'truetype-name-unresolved', 'Selected name version needs separate analysis')
        _require(6 + 12 * count <= base <= len(raw), 'truetype-name', 'Invalid name record/storage extent')
        keys = []; encodings = set(); postscript_names = []
        from fontTools.misc.encodingTools import getEncoding
        for i in range(count):
            platform, encoding, language, name, length, offset = struct.unpack_from('>6H', raw, 6 + 12 * i)
            keys.append((platform, encoding, language, name)); encodings.add((platform, encoding))
            _require(base + offset + length <= len(raw), 'truetype-name', 'Name string extends outside table')
            codec = getEncoding(platform, encoding, language)
            _require(codec is not None, 'truetype-name-unresolved', 'Selected name encoding needs separate analysis')
            try:
                text = raw[base + offset:base + offset + length].decode(codec, errors='strict')
            except UnicodeError as error:
                raise _Invalid('truetype-name', 'Malformed encoded name string') from error
            if name == 6:
                _require(0 < len(text) <= 63 and all(33 <= ord(char) <= 126 and char not in '[](){}<>/%' for char in text),
                         'truetype-name', 'Invalid original PostScript name')
                postscript_names.append(text)
        _require(keys == sorted(keys), 'truetype-name', 'Name records are not sorted')
        _require(len(set(postscript_names)) <= 1, 'truetype-name', 'Original PostScript names disagree across records')
        observations.update(name_format=version, name_record_count=count, name_encodings=sorted(encodings))
    if 'post' in tables:
        raw = table('post')
        _require(len(raw) >= 32, 'truetype-post', 'Truncated post header')
        version = int.from_bytes(raw[:4], 'big')
        _require(version in (0x10000, 0x20000, 0x30000, 0x25000, 0x28000, 0x40000),
                 'truetype-post', 'Undefined post format')
        fixed = int.from_bytes(raw[12:16], 'big')
        _require(not fixed or len(set(advances) - {0}) <= 1, 'truetype-post', 'Fixed-pitch claim disagrees with advances')
        if version == 0x10000:
            _require(n == 258 and len(raw) == 32, 'truetype-post', 'Format1 requires the standard258 glyphs')
        elif version == 0x30000:
            _require(len(raw) == 32, 'truetype-post', 'Format3 has no glyph-name subtable')
        elif version == 0x20000:
            _require(len(raw) >= 34 and int.from_bytes(raw[32:34], 'big') == n
                     and 34 + n * 2 <= len(raw), 'truetype-post', 'post glyph count/extent disagrees with maxp')
            indices = struct.unpack_from('>' + 'H' * n, raw, 34)
            _require(all(value <= 32767 for value in indices), 'truetype-post', 'Reserved glyph-name index')
            at = 34 + n * 2; strings = 0
            while at < len(raw):
                length = raw[at]; at += 1
                _require(at + length <= len(raw), 'truetype-post', 'Truncated Pascal glyph name')
                at += length; strings += 1
            _require(max(indices, default=0) < 258 + strings, 'truetype-post', 'Glyph references an absent custom name')
        else:
            raise _Invalid('truetype-post-unresolved', 'Selected post format needs separate glyph association analysis')
        observations.update(post_format=version, post_fixed_pitch=bool(fixed))


def _truetype(data, report):
    def finding(rule, detail):
        report['findings'].append({'rule': rule, 'detail': detail})
    _require(len(data) >= 12 and data[:4] in (b'\0\1\0\0', b'true'),
             'truetype-structure', 'Expected one TrueType sfnt program')
    _require(data[:4] == b'\0\1\0\0', 'truetype-scaler-unresolved',
             'Apple true scaler needs separate Adobe embedding-context analysis')
    count, search, selector, shift = struct.unpack_from('>4H', data, 4)
    _require(count > 0 and len(data) >= 12 + 16 * count, 'truetype-structure', 'Incomplete table directory')
    power = 1 << (count.bit_length() - 1)
    _require((search, selector, shift) == (16 * power, power.bit_length() - 1, 16 * (count - power)),
             'truetype-structure', 'Inconsistent table-directory search fields')
    tables = {}
    regions = []
    for n in range(count):
        tag, checksum, offset, length = struct.unpack_from('>4sIII', data, 12 + 16 * n)
        tag = tag.decode('latin1')
        _require(tag not in tables and offset % 4 == 0 and offset >= 12 + 16 * count
                 and offset + length <= len(data), 'truetype-structure', 'Invalid table extent: ' + tag)
        tables[tag] = (offset, length)
        if length:
            padded_end = offset + ((length + 3) // 4) * 4
            _require(padded_end <= len(data) and not any(data[offset + length:padded_end]),
                     'truetype-padding', 'Missing or nonzero table padding: ' + tag)
            regions.append((offset, padded_end, tag))
        raw = data[offset:offset + length]
        if tag == 'head' and len(raw) >= 12:
            raw = raw[:8] + b'\0\0\0\0' + raw[12:]
        if _sum32(raw) != checksum:
            finding('truetype-checksum', 'Incorrect table checksum: ' + tag)
    _require(list(tables) == sorted(tables), 'truetype-directory', 'Table tags are not sorted')
    ordered = sorted(regions)
    _require(all(a[1] <= b[0] for a, b in zip(ordered, ordered[1:])), 'truetype-structure', 'Overlapping tables')
    if _sum32(data) != 0xb1b0afba:
        finding('truetype-checksum', 'Whole-program checksum adjustment is incorrect')
    # An embedded CID TrueType program may omit cmap; glyphs are addressed by GID.
    required = {'head', 'hhea', 'maxp', 'hmtx', 'loca', 'glyf', 'cvt ', 'prep', 'fpgm'}
    _require(required <= tables.keys(), 'truetype-structure', 'Missing required tables: ' + ', '.join(sorted(required - tables.keys())))
    _require(tables['head'][1] == 54 and tables['hhea'][1] == 36 and tables['maxp'][1] >= 32,
             'truetype-structure', 'Incorrect required table lengths')
    font = TTFont(io.BytesIO(data), recalcBBoxes=False, recalcTimestamp=False)
    head, hhea, maxp = font['head'], font['hhea'], font['maxp']
    _require(head.tableVersion == 1.0 and hhea.tableVersion == 0x10000
             and head.magicNumber == 0x5f0f3cf5 and head.indexToLocFormat in (0, 1)
             and head.glyphDataFormat == 0 and 64 <= head.unitsPerEm <= 16384,
             'truetype-structure', 'Invalid head fields')
    _require(head.flags & 64 == 0 and -2 <= head.fontDirectionHint <= 2,
             'truetype-head-fields', 'Invalid head flag6 or direction hint')
    _require(hhea.metricDataFormat == 0 and all(getattr(hhea, 'reserved' + str(i)) == 0 for i in range(4)),
             'truetype-structure', 'Invalid hhea reserved or metric format fields')
    _require(hhea.caretSlopeRise != 0 or hhea.caretSlopeRun != 0,
             'truetype-hhea-format', 'Caret rise/run cannot both be zero')
    _require(tables['cvt '][1] % 2 == 0, 'truetype-cvt', 'CVT has a partial signed16 value')
    n = maxp.numGlyphs
    _require(maxp.tableVersion == 0x10000 and 0 < n <= 65535 and 1 <= hhea.numberOfHMetrics <= n,
             'truetype-structure', 'Invalid glyph or horizontal-metric count')
    _require(maxp.maxZones == 2, 'truetype-maxp', 'Referenced maxp format requires two zones')
    _require(tables['hmtx'][1] == 4 * hhea.numberOfHMetrics + 2 * (n - hhea.numberOfHMetrics),
             'truetype-structure', 'hmtx length disagrees with glyph/metric counts')
    _require(tables['loca'][1] == (n + 1) * (2 if head.indexToLocFormat == 0 else 4),
             'truetype-structure', 'loca length disagrees with glyph count')
    locations = list(font['loca'].locations)
    _require(locations[0] == 0 and all(a <= b for a, b in zip(locations, locations[1:]))
             and all(at % 2 == 0 for at in locations) and locations[-1] <= tables['glyf'][1],
             'truetype-structure', 'Invalid glyph-location extents')
    glyf_offset, glyf_length = tables['glyf']
    _require(not any(data[glyf_offset + locations[-1]:glyf_offset + glyf_length]),
             'truetype-glyf-padding-unresolved', 'Noncanonical bytes after final glyph need separate analysis')
    components = []; programs = []
    for first, last in zip(locations, locations[1:]):
        parts, program = _raw_glyph(data[glyf_offset + first:glyf_offset + last], n)
        components.append(parts); programs.append(program)
    depths = {}
    def component_depth(gid, active):
        _require(gid not in active, 'truetype-glyf', 'Composite cycle')
        if gid not in depths:
            _require(not components[gid] or len(active) < 16, 'truetype-maxp', 'Composite recursion exceeds16 levels')
            depths[gid] = max((1 + component_depth(part, active | {gid}) for part in components[gid]), default=0)
        return depths[gid]
    maximum_depth = max(component_depth(gid, set()) for gid in range(n))
    _require(maximum_depth <= maxp.maxComponentDepth <= 16
             and max(map(len, components), default=0) <= maxp.maxComponentElements
             and max(map(len, programs), default=0) <= maxp.maxSizeOfInstructions,
             'truetype-maxp', 'Declared component/instruction capacities are insufficient')
    opcodes = set(); largest_push = 0
    for program in [*programs, *(data[offset:offset + length] for offset, length in (tables['fpgm'], tables['prep']))]:
        operators, push = _instruction_tokens(program)
        opcodes.update(operators); largest_push = max(largest_push, push)
    metric_offset = tables['hmtx'][0]
    raw_advances = [struct.unpack_from('>H', data, metric_offset + i * 4)[0] for i in range(hhea.numberOfHMetrics)]
    raw_advances.extend([raw_advances[-1]] * (n - len(raw_advances)))
    # Check raw names before fontTools uses post to invent the glyph-order view.
    _name_post(data, tables, n, raw_advances, report['observations'])
    if 'post' in tables and data[tables['post'][0] + 4:tables['post'][0] + 8] == b'\0' * 4:
        _require(hhea.caretOffset == 0, 'truetype-hhea-format', 'Non-slanted font has a nonzero caret offset')
    glyf, metrics = font['glyf'], font['hmtx'].metrics
    actual_bounds, sidebearings, rights, extents = [], [], [], []
    advances = []
    for name in font.getGlyphOrder():
        glyph = glyf[name]
        advance, lsb = metrics[name]
        advances.append(advance)
        # getCoordinates recursively applies composite transforms and point
        # attachment. Raw component glyph boxes are not aggregate bounds.
        coordinates, ends, flags = glyph.getCoordinates(glyf)
        if not len(coordinates):
            continue
        raw_bounds = (min(p[0] for p in coordinates), min(p[1] for p in coordinates),
                      max(p[0] for p in coordinates), max(p[1] for p in coordinates))
        declared = tuple(getattr(glyph, key) for key in ('xMin', 'yMin', 'xMax', 'yMax'))
        # F2Dot14 transforms can produce fractional extrema, while the stored
        # box is integral. Do not invent a required rounding direction. Integral
        # extrema must agree exactly; a fractional endpoint admits either
        # adjacent integer. Aggregate fields refer to the stored glyph boxes.
        if any(not math.floor(actual) <= encoded <= math.ceil(actual)
               for actual, encoded in zip(raw_bounds, declared)):
            finding('truetype-glyph-bounds', name + ': declared ' + str(declared) + ', outline ' + str(raw_bounds))
        bbox = declared
        x0, y0, x1, y1 = bbox
        actual_bounds.append(bbox)
        sidebearings.append(lsb); rights.append(advance - lsb - (x1 - x0)); extents.append(lsb + x1 - x0)
        if glyph.numberOfContours >= 0:
            _require(len(coordinates) <= maxp.maxPoints and len(ends) <= maxp.maxContours,
                     'truetype-structure', 'maxp understates simple glyph requirements')
        else:
            _require(len(coordinates) <= maxp.maxCompositePoints and len(ends) <= maxp.maxCompositeContours,
                     'truetype-structure', 'maxp understates composite glyph requirements')
    expected = (max(advances), min(sidebearings, default=0), min(rights, default=0), max(extents, default=0))
    declared = (hhea.advanceWidthMax, hhea.minLeftSideBearing, hhea.minRightSideBearing, hhea.xMaxExtent)
    if declared != expected:
        finding('truetype-hhea-extrema', 'Declared ' + str(declared) + ', retained outlines/metrics ' + str(expected))
    bbox = (min((b[0] for b in actual_bounds), default=0), min((b[1] for b in actual_bounds), default=0),
            max((b[2] for b in actual_bounds), default=0), max((b[3] for b in actual_bounds), default=0))
    declared_bbox = (head.xMin, head.yMin, head.xMax, head.yMax)
    if declared_bbox != bbox:
        finding('truetype-head-bounds', 'Declared ' + str(declared_bbox) + ', retained outlines ' + str(bbox))
    report['observations'].update(glyph_count=n, table_count=count, hhea_expected=list(expected), head_expected=list(bbox))
    report['observations'].update(max_component_depth=maximum_depth, max_component_elements=max(map(len, components), default=0),
                                  max_glyph_instruction_bytes=max(map(len, programs), default=0),
                                  instruction_opcodes=sorted(opcodes), largest_single_push=largest_push,
                                  maxp_capacities={field: getattr(maxp, field) for field in (
                                      'maxPoints', 'maxContours', 'maxCompositePoints', 'maxCompositeContours',
                                      'maxZones', 'maxTwilightPoints', 'maxStorage', 'maxFunctionDefs',
                                      'maxInstructionDefs', 'maxStackElements', 'maxSizeOfInstructions',
                                      'maxComponentElements', 'maxComponentDepth')},
                                  instruction_analysis='byte extents and syntactic PUSH counts only; reachability and execution capacities are a separate check')


class _CFF:
    def __init__(self, data):
        self.data = data

    def take(self, offset, count, rule='cff-structure'):
        _require(isinstance(offset, int) and isinstance(count, int) and 0 <= offset <= len(self.data)
                 and 0 <= count <= len(self.data) - offset, rule, 'Data extent outside program')
        return self.data[offset:offset + count]

    def index(self, offset):
        count = int.from_bytes(self.take(offset, 2, 'cff-index'), 'big')
        if count == 0:
            return [], offset + 2
        size = self.take(offset + 2, 1, 'cff-index')[0]
        _require(1 <= size <= 4, 'cff-index', 'Invalid INDEX offset size')
        raw = self.take(offset + 3, size * (count + 1), 'cff-index')
        offsets = [int.from_bytes(raw[n:n + size], 'big') for n in range(0, len(raw), size)]
        _require(offsets[0] == 1 and all(a <= b for a, b in zip(offsets, offsets[1:])),
                 'cff-index', 'INDEX offsets must start at one and be monotonic')
        base = offset + 3 + len(raw) - 1
        self.take(base + 1, offsets[-1] - 1, 'cff-index')
        return [self.data[base + a:base + b] for a, b in zip(offsets, offsets[1:])], base + offsets[-1]


def _number(data, pos, dictionary=False):
    byte = data[pos]; pos += 1
    need = {28: 2, 29: 4, 255: 4}.get(byte)
    rule = 'cff-dict' if dictionary else 'type2-number'
    if 32 <= byte <= 246:
        return byte - 139, pos
    if 247 <= byte <= 254:
        _require(pos < len(data), rule, 'Truncated two-byte number')
        tail = data[pos]; pos += 1
        return ((byte - 247) * 256 + tail + 108 if byte <= 250 else -(byte - 251) * 256 - tail - 108), pos
    if need:
        _require(pos + need <= len(data), rule, 'Truncated numeric operand')
        value = int.from_bytes(data[pos:pos + need], 'big', signed=True)
        return (value / 65536 if byte == 255 else value), pos + need
    if dictionary and byte == 30:
        text = ''; terminated = False
        chars = {10: '.', 11: 'E', 12: 'E-', 14: '-'}
        while pos < len(data) and not terminated:
            value = data[pos]; pos += 1
            for i, nibble in enumerate((value >> 4, value & 15)):
                if nibble == 15:
                    _require(i == 1 or value & 15 == 15, rule, 'Invalid real-number terminator padding')
                    terminated = True; break
                _require(nibble != 13, rule, 'Reserved real-number nibble')
                text += str(nibble) if nibble < 10 else chars[nibble]
        _require(terminated, rule, 'Unterminated real number')
        try:
            value = float(text)
        except ValueError as error:
            raise _Invalid(rule, 'Malformed real number') from error
        _require(math.isfinite(value), rule, 'Non-finite real number')
        return value, pos
    raise _Invalid(rule, 'Invalid numeric encoding')


_TOP = {**{k: 1 for k in (0, 1, 2, 3, 4, 13, 15, 16, 17)}, 5: 4, 14: None, 18: 2,
        **{(12, k): 1 for k in (0, 1, 2, 3, 4, 5, 6, 8, 20, 21, 22, 31, 32, 33, 34, 35, 36, 37, 38)},
        (12, 7): 6, (12, 23): None, (12, 30): 3}
_PRIVATE = {**{k: None for k in (6, 7, 8, 9)}, **{k: 1 for k in (10, 11, 19, 20, 21)},
            **{(12, k): 1 for k in (9, 10, 11, 14, 17, 18, 19)}, (12, 12): None, (12, 13): None}


def _dictionary(data, private=False, repeats=None):
    result = {}; stack = []; pos = 0
    allowed = _PRIVATE if private else _TOP
    while pos < len(data):
        byte = data[pos]
        if byte in (28, 29, 30) or 32 <= byte <= 254:
            value, pos = _number(data, pos, True)
            stack.append(value)
            _require(len(stack) <= 48, 'cff-dict-stack', 'DICT exceeds 48 operands')
            continue
        pos += 1
        key = byte
        if byte == 12:
            _require(pos < len(data), 'cff-dict', 'Missing escaped DICT operator')
            key = (12, data[pos]); pos += 1
        _require(key in allowed, 'cff-dict', 'Reserved or inapplicable DICT operator ' + str(key))
        arity = allowed[key]
        _require(arity is None or len(stack) == arity, 'cff-dict', 'Invalid DICT operand count for ' + str(key))
        # The original format does not explicitly prohibit duplicate keys.
        # Their effective semantics need separate analysis rather than a claim
        # that silently keeping the final value establishes complete coverage.
        if key in result:
            _require(repeats is not None, 'cff-dict-unresolved', 'Repeated DICT key requires separate effective-value analysis')
            repeats.append(str(key))
        sid_keys = (0, 1, 2, 3, 4, (12, 0), (12, 21), (12, 22), (12, 38))
        if not private and key in sid_keys:
            _require(all(type(value) is int for value in stack), 'cff-dict-type', 'SID operand is not an integer type')
        if not private and key == (12, 30):
            _require(all(type(value) is int for value in stack[:2]), 'cff-dict-type', 'ROS SIDs are not integer types')
        if key == ((12, 14) if private else (12, 1)):
            _require(type(stack[0]) is int and stack[0] in (0, 1), 'cff-dict-type', 'Boolean operand must be integer0 or1')
        if private and key == (12, 17):
            _require(type(stack[0]) is int and stack[0] in (0, 1), 'cff-dict-type', 'LanguageGroup must be integer0 or1')
        result[key] = stack; stack = []
    _require(not stack, 'cff-dict', 'Trailing DICT operands without operator')
    return result


def _delta(values):
    result = []; total = 0
    for value in values:
        total += value; result.append(total)
    return result


def _private_hints(private, finding):
    fuzz = private.get((12, 11), [1])[0]
    scale = private.get((12, 9), [0.039625])[0]
    if not (fuzz >= 0 and int(fuzz) == fuzz):
        finding('cff-blue-zones', 'BlueFuzz must be a nonnegative integer')
    all_pairs = []
    for group in ((6, 7), (8, 9)):
        pairs = []
        for key in group:
            values = _delta(private.get(key, []))
            limit = 14 if key in (6, 8) else 10
            if len(values) % 2 or len(values) > limit or any(int(v) != v for v in values):
                finding('cff-blue-zones', 'Invalid alignment-zone count or noninteger position')
                continue
            for a, b in zip(values[::2], values[1::2]):
                if a > b:
                    finding('cff-blue-zones', 'Alignment-zone pair is reversed')
                pairs.append((a, b)); all_pairs.append((a, b))
        ordered = sorted(pairs)
        if any(b[0] - a[1] < 2 * fuzz + 1 for a, b in zip(ordered, ordered[1:])):
            finding('cff-blue-zones', 'Alignment zones are too close for BlueFuzz')
    if scale <= 0 or any(scale * (b - a) >= 1 for a, b in all_pairs):
        finding('cff-blue-scale', 'BlueScale times the largest zone height must be less than one')
    for snap, standard in (((12, 12), 10), ((12, 13), 11)):
        if snap not in private:
            continue
        values = _delta(private[snap])
        if len(values) > 12 or any(a >= b for a, b in zip(values, values[1:])):
            finding('cff-stem-snap', 'StemSnap widths must increase and contain at most twelve entries')
        if standard in private and private[standard][0] not in values:
            finding('cff-stem-snap', 'StemSnap omits its declared dominant width')


class _Type2:
    """Execute reachable operand/control flow while checking path signatures."""
    def __init__(self, local, global_subrs, cid=True):
        self.local = local; self.global_subrs = global_subrs; self.cid = cid
        self.stack = []; self.transient = [None] * 32; self.width_seen = False
        self.path = False; self.ended = False; self.hints = []; self.mask_seen = False
        self.vertical_seen = False; self.normal_stems = False; self.steps = 0
        self.max_stack = 0; self.max_depth = 0; self.active = None
        self.last_stem_position = {}
        self.escaped_operators = set(); self.zero_rolls = 0

    def pop(self):
        _require(self.stack, 'type2-arity', 'Operand stack underflow')
        return self.stack.pop()

    def width(self, operator):
        if self.width_seen:
            return
        count = len(self.stack)
        if operator in (1, 3, 18, 23, 19, 20):
            extra = count % 2
        elif operator == 14:
            extra = count in (1, 5)
        else:
            extra = count > (2 if operator == 21 else 1)
        if extra:
            self.stack.pop(0)
        self.width_seen = True

    def no_overlap(self, selected=None):
        for axis in ('h', 'v'):
            intervals = [(a, b) for n, (direction, a, b) in enumerate(self.hints)
                         if direction == axis and (selected is None or n in selected)]
            intervals.sort()
            _require(all(a[1] <= b[0] for a, b in zip(intervals, intervals[1:])),
                     'type2-hints', 'Simultaneously active stem hints overlap')

    def stems(self, operator):
        _require(not self.path and not self.mask_seen, 'type2-hints', 'Stem declaration occurs after path/mask')
        _require(len(self.stack) % 2 == 0 and (self.stack or operator in (19, 20)),
                 'type2-arity', 'Stem hints require complete operand pairs')
        axis = 'h' if operator in (1, 18) else 'v'
        if axis == 'h':
            _require(not self.vertical_seen, 'type2-hints', 'Horizontal hints follow vertical hints')
        elif self.stack:
            self.vertical_seen = True
        if operator in (1, 3):
            self.normal_stems = True
        previous = self.last_stem_position.get(axis); position = 0
        for delta, width in zip(self.stack[::2], self.stack[1::2]):
            position += delta
            _require(previous is None or position >= previous, 'type2-hints', 'Stem hint positions decrease')
            _require(width >= 0 or width in (-20, -21), 'type2-hints', 'Undefined negative stem width')
            if width < 0:
                edge = position + width if width == -20 else position
                self.hints.append((axis, edge, edge))
            else:
                self.hints.append((axis, position, position + width))
            previous = position; position += width
        if previous is not None:
            self.last_stem_position[axis] = previous
        self.stack.clear()
        _require(len(self.hints) <= 96, 'type2-hint-limit', 'More than 96 stem hints')

    def execute(self, data, depth=0, subroutine=False):
        _require(len(data) <= 65535, 'type2-length', 'Charstring exceeds 65535 bytes')
        _require(depth <= 10, 'type2-subroutine-depth', 'More than ten nested subroutine calls')
        self.max_depth = max(self.max_depth, depth)
        pos = 0
        while pos < len(data):
            self.steps += 1
            _require(self.steps <= 1000000, 'type2-unresolved', 'Execution budget exceeded')
            byte = data[pos]
            if byte == 28 or byte >= 32:
                value, pos = _number(data, pos)
                self.stack.append(value)
            else:
                pos += 1
                if byte in (10, 29):
                    target = _integer(self.pop(), 'type2-subroutine', 'Subroutine index')
                    subrs = self.local if byte == 10 else self.global_subrs
                    bias = 107 if len(subrs) < 1240 else 1131 if len(subrs) < 33900 else 32768
                    target += bias
                    _require(0 <= target < len(subrs), 'type2-subroutine', 'Call references an absent subroutine')
                    self.execute(subrs[target], depth + 1, True)
                    if self.ended:
                        _require(pos == len(data), 'type2-termination', 'Bytes follow terminating subroutine call')
                        return
                elif byte == 11:
                    _require(subroutine and pos == len(data), 'type2-termination', 'return outside subroutine or followed by bytes')
                    return
                elif byte in (1, 3, 18, 23, 19, 20):
                    self.width(byte)
                    if byte not in (19, 20) or self.stack:
                        self.stems(byte)
                    if byte in (19, 20):
                        self.mask_seen = True
                        if byte == 19:
                            _require(not self.normal_stems, 'type2-hints', 'hintmask requires hstemhm/vstemhm declarations')
                        count = (len(self.hints) + 7) // 8
                        _require(count > 0 and pos + count <= len(data), 'type2-mask', 'Missing exact stem-mask bytes')
                        mask = data[pos:pos + count]; pos += count
                        remainder = len(self.hints) % 8
                        _require(not remainder or mask[-1] & ((1 << (8 - remainder)) - 1) == 0,
                                 'type2-mask', 'Unused stem-mask bits are nonzero')
                        selected = {n for n in range(len(self.hints)) if mask[n // 8] & (128 >> (n % 8))}
                        self.no_overlap(selected)
                        if byte == 19:
                            self.active = selected
                elif byte in (4, 21, 22):
                    self.width(byte)
                    _require(len(self.stack) == (2 if byte == 21 else 1), 'type2-arity', 'Incorrect moveto operand count')
                    if self.active is None:
                        self.no_overlap()
                    self.path = True; self.stack.clear()
                elif byte == 14:
                    self.width(byte)
                    if len(self.stack) == 4 and self.cid:
                        raise _Invalid('type2-composite', 'Deprecated composite endchar is not valid for CID glyphs')
                    _require(not self.stack, 'type2-arity', 'Unexpected endchar operands')
                    _require(pos == len(data), 'type2-termination', 'Bytes follow endchar')
                    if self.active is None:
                        self.no_overlap()
                    self.ended = True
                    return
                elif byte in (5, 6, 7, 8, 24, 25, 26, 27, 30, 31):
                    _require(self.path, 'type2-path', 'Drawing operator precedes moveto')
                    n = len(self.stack)
                    valid = {5: n >= 2 and n % 2 == 0, 6: n >= 1, 7: n >= 1,
                             8: n >= 6 and n % 6 == 0,
                             24: n >= 8 and (n - 2) % 6 == 0,
                             25: n >= 8 and (n - 6) % 2 == 0,
                             26: n >= 4 and n % 4 in (0, 1),
                             27: n >= 4 and n % 4 in (0, 1),
                             30: n >= 4 and n % 4 in (0, 1),
                             31: n >= 4 and n % 4 in (0, 1)}[byte]
                    _require(valid, 'type2-arity', 'Incomplete line/curve argument group')
                    self.stack.clear()
                elif byte == 12:
                    _require(pos < len(data), 'type2-operator', 'Truncated escape operator')
                    op = data[pos]; pos += 1
                    self.escaped(op)
                else:
                    raise _Invalid('type2-operator', 'Reserved Type2 operator ' + str(byte))
            _require(len(self.stack) <= 48, 'type2-stack', 'More than 48 operands')
            self.max_stack = max(self.max_stack, len(self.stack))
        raise _Invalid('type2-termination', 'Missing return' if subroutine else 'Missing endchar')

    def escaped(self, op):
        self.escaped_operators.add(op)
        if op == 0:  # Deprecated PDF-compatible dotsection is a no-op.
            return
        if op in (34, 35, 36, 37):
            _require(self.path, 'type2-path', 'Flex precedes moveto')
            _require(len(self.stack) == {34: 7, 35: 13, 36: 9, 37: 11}[op], 'type2-arity', 'Incorrect flex operand count')
            raise _Invalid('type2-flex-unresolved', 'Present flex requires independent joined-curve geometry analysis')
        if op in (3, 4, 10, 11, 12, 15, 24):
            b = self.pop(); a = self.pop()
            _require(op != 12 or b != 0, 'type2-arithmetic', 'Division by zero')
            value = {3: lambda: int(bool(a) and bool(b)), 4: lambda: int(bool(a) or bool(b)),
                     10: lambda: a + b, 11: lambda: a - b, 12: lambda: a / b,
                     15: lambda: int(a == b), 24: lambda: a * b}[op]()
            _require(math.isfinite(value), 'type2-arithmetic', 'Non-finite arithmetic result')
            self.stack.append(value)
        elif op in (5, 9, 14, 26):
            value = self.pop()
            _require(op != 26 or value >= 0, 'type2-arithmetic', 'Square root of negative operand')
            self.stack.append({5: lambda: int(not value), 9: lambda: abs(value),
                               14: lambda: -value, 26: lambda: math.sqrt(value)}[op]())
        elif op == 18:
            self.pop()
        elif op in (20, 21):
            n = _integer(self.pop(), 'type2-transient', 'Transient-array index')
            _require(0 <= n < 32, 'type2-transient', 'Transient-array index outside 0..31')
            if op == 20:
                self.transient[n] = self.pop()
            else:
                _require(self.transient[n] is not None, 'type2-transient', 'Transient-array read precedes its initialization')
                self.stack.append(self.transient[n])
        elif op == 22:
            v2 = self.pop(); v1 = self.pop(); s2 = self.pop(); s1 = self.pop()
            self.stack.append(s1 if v1 <= v2 else s2)
        elif op == 23:
            raise _Invalid('type2-unresolved', 'Random-dependent execution requires separate analysis')
        elif op == 27:
            value = self.pop(); self.stack.extend((value, value))
        elif op == 28:
            b = self.pop(); a = self.pop(); self.stack.extend((b, a))
        elif op == 29:
            n = _integer(self.pop(), 'type2-arity', 'index operand')
            _require(self.stack and n < len(self.stack), 'type2-arity', 'index outside operand stack')
            self.stack.append(self.stack[-1 - max(0, n)])
        elif op == 30:
            shift = _integer(self.pop(), 'type2-arity', 'roll shift')
            n = _integer(self.pop(), 'type2-arity', 'roll count')
            _require(0 <= n <= len(self.stack), 'type2-arity', 'Invalid roll count')
            if n == 0:
                # The source permits nonnegative N, although its printed
                # modulo formula is undefined at zero. Rotate no elements.
                self.zero_rolls += 1
                return
            shift %= n
            if shift:
                self.stack[-n:] = self.stack[-shift:] + self.stack[-n:-shift]
        else:
            raise _Invalid('type2-operator', 'Reserved escaped Type2 operator ' + str(op))


def _cff(data, report):
    def finding(rule, detail):
        report['findings'].append({'rule': rule, 'detail': detail})
    reader = _CFF(data)
    header = reader.take(0, 4)
    _require(header[0] == 1 and header[2] >= 4 and 1 <= header[3] <= 4,
             'cff-structure', 'Unsupported CFF header')
    names, pos = reader.index(header[2]); tops, pos = reader.index(pos)
    strings, pos = reader.index(pos); global_subrs, pos = reader.index(pos)
    _require(len(names) == len(tops) == 1, 'cff-structure', 'Expected one embedded CFF font')
    _require(names[0] and len(names[0]) <= 127 and all(33 <= b <= 126 and b not in b'[](){}<>/%' for b in names[0]),
             'cff-structure', 'Invalid embedded CFF font name')
    _require(len(strings) + 390 <= 64999 and len(strings) == len(set(strings)),
             'cff-strings', 'String INDEX has duplicate strings or exceeds SID range')
    from fontTools.cffLib import cffStandardStrings
    standard = {s.encode('ascii') for s in cffStandardStrings}
    _require(not any(s in standard for s in strings), 'cff-strings', 'String INDEX duplicates a standard string')
    repeats = []
    top = _dictionary(tops[0], repeats=repeats)
    _require(top and next(iter(top)) == (12, 30), 'cff-cid', 'Expected CID CFF beginning with ROS')
    _require(16 not in top and (12, 20) not in top and top.get((12, 6), [2]) == [2]
             and top.get((12, 33), [0]) == [0], 'cff-cid', 'Unsupported CID CFF organization')
    def check_sids(dictionary):
        for key in (0, 1, 2, 3, 4, (12, 0), (12, 21), (12, 22), (12, 38), (12, 30)):
            for sid in dictionary.get(key, [])[:2 if key == (12, 30) else 1]:
                sid = _integer(sid, 'cff-sid', 'SID')
                _require(0 <= sid <= 64999 and sid < 391 + len(strings), 'cff-sid', 'Undefined or reserved SID')
    check_sids(top)
    def offset(dictionary, key, rule='cff-structure'):
        _require(key in dictionary, rule, 'Missing offset operator ' + str(key))
        return _integer(dictionary[key][0], rule, 'Offset')
    charstrings, end = reader.index(offset(top, 17))
    _require(charstrings, 'cff-structure', 'CharStrings lacks .notdef')
    n = len(charstrings)
    charset_at = _integer(top.get(15, [0])[0], 'cff-charset', 'Charset offset')
    _require(charset_at > 2, 'cff-charset', 'CID fonts cannot use predefined charsets')
    form = reader.take(charset_at, 1, 'cff-charset')[0]; at = charset_at + 1; cids = [0]
    _require(form in (0, 1, 2), 'cff-charset', 'Unsupported charset format')
    while len(cids) < n:
        first = int.from_bytes(reader.take(at, 2, 'cff-charset'), 'big'); at += 2
        extra = 0
        if form:
            size = 1 if form == 1 else 2
            extra = int.from_bytes(reader.take(at, size, 'cff-charset'), 'big'); at += size
        _require(first + extra <= 65535 and len(cids) + extra + 1 <= n,
                 'cff-charset', 'Charset range exceeds glyph/CID limits')
        cids.extend(range(first, first + extra + 1))
    _require(len(cids) == len(set(cids)), 'cff-charset', 'CID charset entries must be unique')
    _require(max(cids) < top.get((12, 34), [8720])[0], 'cff-charset', 'CIDCount does not cover charset')
    fds, end = reader.index(offset(top, (12, 36)))
    _require(0 < len(fds) <= 256, 'cff-cid', 'FDArray must contain 1..256 entries')
    local_sets = []; matrices = []; fd_operators = []; private_operators = []
    top_matrix = top.get((12, 7), [0.001, 0, 0, 0.001, 0, 0])
    if top_matrix != [0.001, 0, 0, 0.001, 0, 0]:
        finding('cff-font-matrix-unresolved', 'Nondefault Top FontMatrix requires separate transformed-metric analysis')
    for fd_data in fds:
        fd = _dictionary(fd_data, repeats=repeats); check_sids(fd)
        fd_operators.append(list(map(str, fd)))
        matrix = fd.get((12, 7), [1, 0, 0, 1, 0, 0]); matrices.append(matrix)
        if matrix != [1, 0, 0, 1, 0, 0]:
            finding('cff-font-matrix-unresolved', 'Nonidentity FD FontMatrix requires separate composition/metric analysis')
        _require(18 in fd, 'cff-private', 'Every Font DICT requires a Private DICT')
        size, start = (_integer(x, 'cff-private', 'Private size/offset') for x in fd[18])
        private = _dictionary(reader.take(start, size, 'cff-private'), True, repeats)
        private_operators.append(list(map(str, private)))
        _require(not (7 in private and (6 not in private or list(private).index(7) < list(private).index(6)))
                 and not (9 in private and (8 not in private or list(private).index(9) < list(private).index(8))),
                 'cff-private', 'OtherBlues must follow its corresponding Blues operator')
        _private_hints(private, finding)
        if 19 in private:
            relative = _integer(private[19][0], 'cff-private-subrs', 'Local Subrs relative offset')
            _require(relative >= size, 'cff-private-subrs', 'Local Subrs overlaps or precedes Private DICT')
            try:
                local, end = reader.index(start + relative)
            except _Invalid as error:
                raise _Invalid('cff-private-subrs', str(error)) from error
        else:
            local = []
        local_sets.append(local)
    select_at = offset(top, (12, 37)); form = reader.take(select_at, 1, 'cff-fdselect')[0]; at = select_at + 1
    if form == 0:
        selection = list(reader.take(at, n, 'cff-fdselect'))
    elif form == 3:
        count = int.from_bytes(reader.take(at, 2, 'cff-fdselect'), 'big'); at += 2
        _require(count > 0, 'cff-fdselect', 'Empty FDSelect ranges')
        ranges = []
        for _ in range(count):
            raw = reader.take(at, 3, 'cff-fdselect'); at += 3
            ranges.append((int.from_bytes(raw[:2], 'big'), raw[2]))
        sentinel = int.from_bytes(reader.take(at, 2, 'cff-fdselect'), 'big')
        _require(ranges[0][0] == 0 and sentinel == n
                 and all(a[0] < b[0] for a, b in zip(ranges, ranges[1:])),
                 'cff-fdselect', 'Invalid FDSelect range boundaries')
        selection = []
        for i, (first, fd) in enumerate(ranges):
            last = ranges[i + 1][0] if i + 1 < count else sentinel
            _require(first < last <= n, 'cff-fdselect', 'FDSelect range outside glyph set')
            selection.extend([fd] * (last - first))
    else:
        raise _Invalid('cff-fdselect', 'Unsupported FDSelect format')
    _require(all(fd < len(fds) for fd in selection), 'cff-fdselect', 'Glyph selects an absent Font DICT')
    maximum_stack = 0; maximum_depth = 0; maximum_hints = 0; escaped_operators = set(); zero_rolls = 0
    for gid, program in enumerate(charstrings):
        interpreter = _Type2(local_sets[selection[gid]], global_subrs)
        try:
            interpreter.execute(program)
        except _Invalid as error:
            finding(error.rule, 'GID ' + str(gid) + ': ' + str(error))
        maximum_stack = max(maximum_stack, interpreter.max_stack)
        maximum_depth = max(maximum_depth, interpreter.max_depth)
        maximum_hints = max(maximum_hints, len(interpreter.hints))
        escaped_operators.update(interpreter.escaped_operators); zero_rolls += interpreter.zero_rolls
    report['observations'].update(glyph_count=n, cid_charset=cids, font_dict_count=len(fds), global_subr_count=len(global_subrs),
                                  local_subr_counts=list(map(len, local_sets)), max_type2_stack=maximum_stack,
                                  max_type2_subr_depth=maximum_depth, max_type2_hints=maximum_hints)
    report['observations'].update(top_font_matrix=top_matrix, fd_font_matrices=matrices,
                                  top_dict_operators=list(map(str, top)), fd_dict_operators=fd_operators,
                                  private_dict_operators=private_operators,
                                  matrix_analysis='default Top matrix and identity FDs only; other shapes are unresolved',
                                  reachable_type2_escaped_operators=sorted(escaped_operators),
                                  reachable_flex_absent=not escaped_operators.intersection((34, 35, 36, 37)),
                                  zero_count_rolls_treated_as_no_op=zero_rolls)
    if repeats:
        finding('cff-dict-unresolved', 'Repeated DICT keys require separate effective-value analysis: ' + ', '.join(repeats))


def inspect_font_program(data, kind):
    """Return neutral findings and decoded observations for exact program bytes.

    ``kind`` is ``truetype`` or ``cff`` (a CFF1 CIDFont program). A finding ending
    in ``unresolved`` represents missing analysis and must prevent a pass.
    """
    report = {'findings': [], 'observations': {'kind': kind, 'byte_length': len(data)}}
    try:
        if kind == 'truetype':
            _truetype(data, report)
        elif kind == 'cff':
            _cff(data, report)
        else:
            raise _Invalid('font-program-kind', 'Unsupported embedded program kind')
    except _Invalid as error:
        report['findings'].append({'rule': error.rule, 'detail': str(error)})
    except Exception as error:
        report['findings'].append({'rule': 'font-program-structure', 'detail': type(error).__name__ + ': ' + str(error)})
    return report
