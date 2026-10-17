"""Check generated font metrics against retained outlines and explicit sources.

This development checker supports the generated Identity-H TrueType/CFF subset
profile. It reads frozen PDF/font bytes. It does not certify arbitrary PDFs or
infer the identity of an absent source face. Stem measurements cover source H
with native unhinted FreeType rasterization at the recorded conditions only.
"""
import argparse
import hashlib
from importlib.metadata import version
import io
import json
import math
from pathlib import Path
import re
import statistics
import struct
import sys
import unicodedata

from fontTools.cffLib import CFFFontSet
from fontTools.pens.boundsPen import BoundsPen, ControlBoundsPen
from fontTools.ttLib import TTFont
from pypdf import PdfReader
from pypdf.generic import ArrayObject, IndirectObject

sys.path.insert(0, str(Path(__file__).resolve().parent))
from pdf_text_metadata_rules import inspect_tounicode


def sha(data):
    return hashlib.sha256(data).hexdigest()


def f32(value):
    return struct.unpack('>f', struct.pack('>f', value))[0]


def scaled(value, scale):
    return f32(f32(value) * f32(scale))


def _bounds(glyph_set, name, pen_class):
    pen = pen_class(glyph_set)
    glyph_set[name].draw(pen)
    return list(pen.bounds) if pen.bounds is not None else None


def _union(boxes):
    boxes = [box for box in boxes if box is not None]
    return [choose(box[i] for box in boxes) for i, choose in enumerate((min, min, max, max))] if boxes else [0] * 4


def _source(data):
    font = TTFont(io.BytesIO(data), recalcBBoxes=False, recalcTimestamp=False)
    names = {item.toUnicode() for item in font['name'].names if item.nameID == 6}
    if len(names) != 1:
        raise ValueError('Source must have one unambiguous PostScript name')
    glyph_set = font.getGlyphSet()
    return {'font': font, 'data': data, 'sha256': sha(data), 'ps_name': names.pop(),
            'glyph_set': glyph_set, 'cmap': font.getBestCmap() or {},
            'scale': f32(1000 / font['head'].unitsPerEm),
            'pen': ControlBoundsPen if 'glyf' in font else BoundsPen}


def _program(data, kind):
    if kind == '/FontFile2':
        font = TTFont(io.BytesIO(data), recalcBBoxes=False, recalcTimestamp=False)
        names = font.getGlyphOrder()
        glyph_set = font.getGlyphSet()
        return {'kind': 'TrueType', 'bounds': [_bounds(glyph_set, name, ControlBoundsPen) for name in names],
                'names': names, 'advances': [font['hmtx'][name][0] for name in names],
                'scale': f32(1000 / font['head'].unitsPerEm),
                'cid_to_gid': dict(enumerate(range(len(names)))),
                'bounds_method': 'fontTools ControlBoundsPen over retained TrueType outlines'}
    cff = CFFFontSet(); cff.decompile(io.BytesIO(data), None)
    if len(cff.topDictIndex) != 1:
        raise ValueError('Expected one CFF face')
    top = cff.topDictIndex[0]
    matrix = getattr(top, 'FontMatrix', [0.001, 0, 0, 0.001, 0, 0])
    if len(matrix) != 6 or matrix[0] <= 0 or matrix[0] != matrix[3] or any(matrix[i] for i in (1, 2, 4, 5)):
        raise ValueError('Unsupported CFF coordinate matrix')
    names, boxes, advances = list(top.charset), [], []
    for name in names:
        pen = BoundsPen(top.CharStrings)
        top.CharStrings[name].draw(pen)
        boxes.append(list(pen.bounds) if pen.bounds is not None else None)
        advances.append(top.CharStrings[name].width)
    cid_to_gid = {}
    for gid, name in enumerate(names):
        if name == '.notdef':
            cid_to_gid[0] = gid
        elif name.startswith('cid') and name[3:].isdigit():
            cid_to_gid[int(name[3:])] = gid
        else:
            raise ValueError('Expected generated CID-keyed CFF charset')
    return {'kind': 'CFF', 'bounds': boxes, 'names': names, 'advances': advances,
            'scale': f32(matrix[0] * 1000), 'cid_to_gid': cid_to_gid,
            'bounds_method': 'fontTools BoundsPen exact retained CFF curve extrema'}


def _widths(child):
    result = {}
    items = list(child.get('/W', [])); at = 0
    while at < len(items):
        start, following = int(items[at]), items[at + 1].get_object(); at += 2
        if isinstance(following, ArrayObject):
            for offset, value in enumerate(following):
                result[start + offset] = float(value)
        else:
            value = float(items[at]); at += 1
            for cid in range(start, int(following) + 1):
                result[cid] = value
    return result


def _stem(source):
    # This oracle deliberately uses FreeType's native rasterizer rather than
    # reproducing the generator's outline intersection/stem estimator.
    import freetype as ft
    ppem = 4096
    face = ft.Face(io.BytesIO(source['data']))
    gid = face.get_char_index(ord('H'))
    if not gid or 72 not in source['cmap']:
        raise ValueError('Source has no H for the bounded flat-stem measurement')
    face.set_pixel_sizes(ppem, ppem)
    flags = ft.FT_LOAD_NO_HINTING | ft.FT_LOAD_NO_AUTOHINT | ft.FT_LOAD_NO_BITMAP
    face.load_glyph(gid, flags)
    face.glyph.render(ft.FT_RENDER_MODE_NORMAL)
    bitmap = face.glyph.bitmap
    if bitmap.pixel_mode != ft.FT_PIXEL_MODE_GRAY or bitmap.pitch <= 0:
        raise ValueError('Expected a positive-pitch grayscale FreeType bitmap')
    box = _bounds(source['glyph_set'], source['cmap'][72], source['pen'])
    pixels = bytes(bitmap.buffer)
    samples = []
    for fraction in (0.25, 0.75):
        y = box[1] + fraction * (box[3] - box[1])
        row = round(face.glyph.bitmap_top - y * ppem / face.units_per_EM - 0.5)
        if not 0 <= row < bitmap.rows:
            raise ValueError('Measurement row lies outside the H bitmap')
        values = pixels[row * bitmap.pitch:row * bitmap.pitch + bitmap.width]
        intervals, start = [], None
        for x, value in enumerate([*values, 0]):
            if value >= 128 and start is None:
                start = x
            elif value < 128 and start is not None:
                intervals.append([start, x]); start = None
        if len(intervals) != 2:
            raise ValueError('H quarter-height row does not expose two separate flat stems')
        samples.append({'height_fraction': fraction, 'bitmap_row': row, 'intervals_pixels': intervals,
                        'widths_pixels': [end - start for start, end in intervals]})
    uprights = []
    for index in (0, 1):
        widths = [sample['widths_pixels'][index] for sample in samples]
        if max(widths) - min(widths) > 2:
            raise ValueError('One H upright varies beyond the two-pixel raster uncertainty')
        uprights.append({'index_from_left': index, 'widths_pixels': widths,
                         'measured_pdf_units': statistics.median(widths) * 1000 / ppem})
    return {'method': 'FreeType native unhinted source H raster', 'ppem': ppem,
            'load_flags': ['FT_LOAD_NO_HINTING', 'FT_LOAD_NO_AUTOHINT', 'FT_LOAD_NO_BITMAP'],
            'load_flags_value': flags, 'render_mode': 'FT_RENDER_MODE_NORMAL', 'coverage_threshold': 128,
            'transform': 'identity', 'variation': 'default instance', 'samples': samples,
            'uprights': uprights, 'measured_pdf_units': uprights[0]['measured_pdf_units'],
            'comparison': 'Either separately measured H upright; no average between unequal uprights',
            'uncertainty_pdf_units': 2 * 1000 / ppem,
            'uncertainty_basis': 'At most two raster pixels across a measured filled interval',
            'freetype_version': '.'.join(map(str, ft.version())),
            'freetype_py_version': version('freetype-py'),
            'freetype_binary_sha256': sha(Path(ft.FT_Library_filename).read_bytes())}


def inspect_pdf_metrics(pdf, source_fonts):
    """Return findings/observations for one exact generated PDF and source faces.

    Unknown sources, unsupported maps and unavailable measurements are findings.
    Unmapped .notdef contributes its retained bounds and ascent; this necessary
    unclassified glyph is explicitly recorded. Other unclassified outlines do
    not silently receive an unaccented classification.
    """
    path = Path(pdf)
    data = path.read_bytes()
    observations = {'input_sha256': sha(data), 'input_bytes': len(data), 'fonts': [], 'sources': [],
                    'numeric_comparison': 'Exact binary32 values after decimal roundtrip for bounds, ascent, descent, capitals and advances; raster-pixel bound for stems',
                    'coverage_limit': 'Generated Identity-H TrueType/CFF subsets and explicit source faces only; '
                    'H stem thickness at the recorded raster conditions, not arbitrary fonts or PDF certification.'}
    report = {'schema': 'pdf-font-metrics-verification/v1', 'passed': False, 'findings': [],
              'input_sha256': sha(data), 'input_bytes': len(data),
              'observations': observations, 'evaluator_sha256': sha(Path(__file__).read_bytes()),
              'tools': {name: version(name) for name in ('fonttools', 'pypdf')}}

    def finding(rule, detail, **context):
        report['findings'].append({'rule': rule, 'detail': detail, **context})

    sources, source_paths = {}, []
    for item in source_fonts:
        source_path = Path(item)
        try:
            source_data = source_path.read_bytes(); source_paths.append((source_path, sha(source_data)))
            source = _source(source_data)
            observations['sources'].append({'file_name': source_path.name, 'sha256': source['sha256'],
                                            'postscript_name': source['ps_name']})
            sources.setdefault(source['ps_name'], []).append(source)
        except Exception as error:
            finding('font-metric-source-unresolved', type(error).__name__ + ': ' + str(error), source=source_path.name)
    try:
        reader = PdfReader(io.BytesIO(data), strict=True)
        refs = {(number, generation) for generation, entries in reader.xref.items() for number in entries if number}
        refs.update((number, 0) for number in reader.xref_objStm)
        for number, generation in sorted(refs):
            obj = reader.get_object(IndirectObject(number, generation, reader))
            if not hasattr(obj, 'get') or obj.get('/Subtype') != '/Type0':
                continue
            font_observation = {'object': f'{number} {generation}', 'unmapped_gids': [],
                                'accented_gids_excluded_from_ascent': [], 'expected': {}}
            observations['fonts'].append(font_observation)
            context = {'font_object': font_observation['object']}

            def check(key, expected, rule):
                font_observation['expected'][key] = expected
                declared = descriptor.get('/' + key)
                try:
                    equal = ([f32(float(value)) for value in declared] == expected
                             if isinstance(expected, list) else f32(float(declared)) == expected)
                except (TypeError, ValueError, OverflowError):
                    equal = False
                if not equal:
                    finding(rule, f'{key} differs from independently measured metric',
                            declared=list(declared) if isinstance(declared, ArrayObject) else declared,
                            expected=expected, **context)

            try:
                if obj.get('/Encoding') != '/Identity-H' or len(obj['/DescendantFonts']) != 1:
                    raise ValueError('Unsupported composite font encoding')
                child = obj['/DescendantFonts'][0].get_object()
                descriptor = child['/FontDescriptor'].get_object()
                ps_name = re.sub(r'^[A-Z]{6}\+', '', str(descriptor['/FontName']).lstrip('/'))
                font_observation['postscript_name'] = ps_name
                matches = sources.get(ps_name, [])
                if len(matches) != 1:
                    finding('font-metric-source-unresolved', 'Expected exactly one explicit matching source PostScript name', **context)
                    continue
                source = matches[0]
                font_observation['source_sha256'] = source['sha256']
                source_font = source['font']
                fixed = bool(source_font['post'].isFixedPitch)
                italic = bool(source_font['OS/2'].fsSelection & 1) if 'OS/2' in source_font else bool(source_font['head'].macStyle & 2)
                flags = int(descriptor.get('/Flags',-1))
                font_observation['source_style'] = {'fixed_pitch':fixed,'italic':italic,'descriptor_flags':flags}
                if bool(flags & 1) != fixed or bool(flags & 64) != italic:
                    finding('font-metric-flags','Descriptor style flags differ from the explicit original face',**context)
                kinds = [key for key in ('/FontFile2', '/FontFile3') if key in descriptor]
                if len(kinds) != 1:
                    raise ValueError('Expected one embedded font program')
                kind = kinds[0]
                if kind == '/FontFile2' and child.get('/CIDToGIDMap') != '/Identity':
                    raise ValueError('Expected Identity CID-to-GID mapping')
                if kind == '/FontFile3' and descriptor[kind].get('/Subtype') != '/CIDFontType0C':
                    raise ValueError('Expected raw CIDFontType0C')
                program_bytes = descriptor[kind].get_data()
                program = _program(program_bytes, kind)
                font_observation.update(program_sha256=sha(program_bytes), program_kind=program['kind'],
                                         glyph_count=len(program['names']), bounds_method=program['bounds_method'])
                boxes, scale = program['bounds'], program['scale']
                check('FontBBox', [scaled(value, scale) for value in _union(boxes)], 'font-metric-bbox')
                check('Descent', scaled(min(0, _union(boxes)[1]), scale), 'font-metric-descent')
                cmap_report = inspect_tounicode(obj['/ToUnicode'].get_data())
                if cmap_report['findings']:
                    raise ValueError('Unsupported or invalid ToUnicode: ' + json.dumps(cmap_report['findings']))
                mappings = cmap_report['observations']['mappings']
                gid_texts = {}
                for cid_hex, text in mappings.items():
                    gid = program['cid_to_gid'].get(int(cid_hex, 16))
                    if gid is None:
                        raise ValueError('ToUnicode maps a CID absent from the embedded charset')
                    gid_texts.setdefault(gid, []).append(text)
                reverse = {}
                for codepoint, name in source['cmap'].items():
                    reverse.setdefault(name, []).append(codepoint)
                ascent = 0
                source_bounds = {}
                source_names = set(source['font'].getGlyphOrder())
                for gid, name in enumerate(program['names']):
                    texts = gid_texts.get(gid, [])
                    if not texts:
                        font_observation['unmapped_gids'].append(gid)
                    candidates = {source['cmap'][ord(text)] for text in texts
                                  if len(text) == 1 and ord(text) in source['cmap']}
                    if not candidates and name in source_names:
                        candidates = {name}
                    if len(candidates) != 1:
                        if boxes[gid] is not None:
                            finding('font-metric-glyph-unresolved', 'Retained outline has no unique source glyph identity', gid=gid, **context)
                        continue
                    source_name = candidates.pop()
                    box = source_bounds.setdefault(source_name, _bounds(source['glyph_set'], source_name, source['pen']))
                    embedded_box = [scaled(v, scale) for v in boxes[gid]] if boxes[gid] is not None else None
                    source_box = [scaled(v, source['scale']) for v in box] if box is not None else None
                    if embedded_box != source_box:
                        finding('font-metric-source-outline', 'Retained glyph bounds differ from explicit source glyph', gid=gid, **context)
                    if scaled(program['advances'][gid], scale) != scaled(source['font']['hmtx'][source_name][0], source['scale']):
                        finding('font-metric-source-width', 'Retained advance differs from explicit source glyph', gid=gid, **context)
                    codes = reverse.get(source_name, [])
                    accented = bool(codes) and all(any(unicodedata.combining(ch) for ch in unicodedata.normalize('NFD', chr(code))) for code in codes)
                    if accented:
                        font_observation['accented_gids_excluded_from_ascent'].append(gid)
                    elif codes or gid == 0:
                        ascent = max(ascent, boxes[gid][3] if boxes[gid] is not None else 0)
                    elif boxes[gid] is not None:
                        finding('font-metric-ascent-unresolved', 'Source glyph has no Unicode accent classification', gid=gid, **context)
                check('Ascent', scaled(ascent, scale), 'font-metric-ascent')
                font_observation['notdef_classification_limit'] = 'Unmapped .notdef is included in retained bounds and unaccented ascent.'
                caps = {chr(code): _bounds(source['glyph_set'], source['cmap'][code], source['pen'])[3]
                        for code in map(ord, 'HITEF') if code in source['cmap']}
                cap = getattr(source['font']['OS/2'], 'sCapHeight', 0) if 'OS/2' in source['font'] else 0
                font_observation['source_flat_cap_tops'] = caps
                if not caps or max(caps.values()) - min(caps.values()) > 1 or cap > 0 and any(abs(cap - value) > 1 for value in caps.values()):
                    finding('font-metric-cap-unresolved', 'Source capital declaration cannot be confirmed against flat capital geometry', **context)
                else:
                    check('CapHeight', scaled(cap if cap > 0 else statistics.median(caps.values()), source['scale']), 'font-metric-cap-height')
                try:
                    measurement = _stem(source)
                    font_observation['stem_measurement'] = measurement
                    declared_stem = descriptor.get('/StemV')
                    value = f32(float(declared_stem)) if declared_stem is not None else math.nan
                    measurement['declared_binary32'] = value if math.isfinite(value) else None
                    measurement['matched_upright_indexes'] = [upright['index_from_left'] for upright in measurement['uprights']
                        if math.isfinite(value) and abs(value - upright['measured_pdf_units']) <= measurement['uncertainty_pdf_units']]
                    if not measurement['matched_upright_indexes']:
                        finding('font-metric-stem-v', 'StemV differs from both native unhinted filled-stroke measurements', declared=declared_stem, **context)
                except Exception as error:
                    finding('font-metric-stem-unresolved', type(error).__name__ + ': ' + str(error), **context)
                widths = _widths(child)
                font_observation['widths_checked'] = len(widths)
                for cid, width in widths.items():
                    gid = program['cid_to_gid'].get(cid)
                    if gid is None or f32(width) != scaled(program['advances'][gid], scale):
                        finding('font-metric-width', 'PDF width differs from embedded advance', cid=cid, **context)
            except Exception as error:
                finding('font-metric-unresolved', type(error).__name__ + ': ' + str(error), **context)
        if not observations['fonts']:
            finding('font-metric-unresolved', 'No supported composite font was measured')
    except Exception as error:
        finding('font-metric-unresolved', type(error).__name__ + ': ' + str(error))
    observations['input_sha256_after'] = sha(path.read_bytes())
    observations['sources_unchanged'] = all(sha(item.read_bytes()) == digest for item, digest in source_paths)
    report['unchanged'] = observations['input_sha256_after'] == observations['input_sha256'] and observations['sources_unchanged']
    if observations['input_sha256_after'] != observations['input_sha256'] or not observations['sources_unchanged']:
        finding('font-metric-input-changed', 'Input bytes changed during inspection')
    for entries in sources.values():
        for source in entries:
            source['font'].close()
    report['passed'] = not report['findings']
    return report


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--pdf', type=Path, required=True)
    parser.add_argument('--source-font', type=Path, action='append', required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    destination = args.output / 'font-metrics.json' if args.output.is_dir() else args.output
    if args.output.is_dir() and any(args.output.iterdir()):
        parser.error('Output directory must be empty')
    if destination.exists():
        parser.error('Output file already exists')
    report = inspect_pdf_metrics(args.pdf, args.source_font)
    with destination.open('x', encoding='utf-8', newline='\n') as output:
        json.dump(report, output, indent=2, allow_nan=False); output.write('\n')
    print(json.dumps({'passed': report['passed'], 'findings': report['findings']}))
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
