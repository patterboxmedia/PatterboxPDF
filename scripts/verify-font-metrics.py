"""Check generated font metrics against retained outlines and explicit sources.

This development checker supports generated Identity-H TrueType/CFF programs.
Optional preparation witnesses bind source faces and CID anchors without using
logical Unicode text as a source-glyph identifier. Separate authored Ascent
facts identify the source roles excluded from that metric. Their semantic truth
is an explicit authoring premise. It reads frozen input bytes. It does not certify arbitrary PDFs or
infer the identity of an absent source face. Stem measurements use native
unhinted FreeType rasterization of a selected capital or retained fallback at
the recorded conditions only; the observations identify tricky fonts.
"""
import argparse
import hashlib
from importlib.metadata import version
import io
import itertools
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
from fontTools.pens.transformPen import TransformPen
from fontTools.misc.transform import Transform
from fontTools.ttLib import TTFont
from pypdf import PdfReader
from pypdf.generic import ArrayObject, IndirectObject

sys.path.insert(0, str(Path(__file__).resolve().parent))
from pdf_text_metadata_rules import inspect_tounicode
from pdf_font_program_rules import inspect_font_bbox_enclosure, truetype_geometry
from fractions import Fraction


def sha(data):
    return hashlib.sha256(data).hexdigest()


def f32(value):
    return struct.unpack('>f', struct.pack('>f', value))[0]


def scaled(value, scale):
    return f32(value * scale)


def _draw(glyph_set,name,pen,tt_geometry=None):
    if hasattr(glyph_set, 'glyfTable'):
        # Flatten the independently resolved points before drawing quadratic
        # contours; the generic fontTools component shortcut omits phantoms.
        from fontTools.ttLib.tables._g_l_y_f import Glyph, GlyphCoordinates
        glyf = glyph_set.glyfTable
        if tt_geometry is None:tt_geometry=truetype_geometry(glyph_set.font)
        decoded = tt_geometry[name]
        coordinates, ends, flags = decoded['points'],decoded['ends'],decoded['flags']
        flat = Glyph(); flat.numberOfContours = len(ends)
        flat.coordinates=GlyphCoordinates((float(x),float(y)) for x,y in coordinates)
        flat.endPtsOfContours, flat.flags = ends, flags
        offset = -float(decoded['phantoms'][0][0])
        flat.draw(pen, glyf, offset)
    else:
        glyph_set[name].draw(pen)


def _bounds(glyph_set, name, pen_class, tt_geometry=None):
    pen = pen_class(glyph_set)
    _draw(glyph_set,name,pen,tt_geometry)
    return list(pen.bounds) if pen.bounds is not None else None


def _union(boxes):
    boxes = [box for box in boxes if box is not None]
    return [choose(box[i] for box in boxes) for i, choose in enumerate((min, min, max, max))] if boxes else [0] * 4


def _cff_geometry(top):
    """Raw CFF defaults once; FD glyph space -> CID space -> PDF glyph units."""
    top_present='FontMatrix' in top.rawDict or 'FontMatrix' in top.__dict__
    top_matrix = Transform(*getattr(top, 'FontMatrix', [0.001, 0, 0, 0.001, 0, 0]))
    names, boxes, advances, matrices = list(top.charset), [], [], []
    for gid, name in enumerate(names):
        fd = top.FDArray[top.FDSelect[gid]] if hasattr(top, 'FDArray') else None
        relative = Transform(*getattr(fd, 'FontMatrix', [1, 0, 0, 1, 0, 0]))
        matrix = Transform(1000, 0, 0, 1000, 0, 0).transform((Transform() if not top_present and fd is not None and ('FontMatrix' in fd.rawDict or 'FontMatrix' in fd.__dict__) else top_matrix).transform(relative))
        pen = BoundsPen(top.CharStrings)
        char = top.CharStrings[name]
        char.draw(TransformPen(pen, matrix))
        boxes.append(list(pen.bounds) if pen.bounds is not None else None)
        advances.append(char.width * matrix.xx)
        matrices.append(matrix)
    return names, boxes, advances, matrices


def _postscript_name(font):
    names = set()
    for item in font['name'].names:
        if item.nameID != 6:
            continue
        if item.platformID == 0 or (item.platformID == 3 and item.platEncID in (0, 1, 10)):
            codec = 'utf-16-be'
        elif item.platformID == 1 and item.platEncID == 0:
            codec = 'mac_roman'
        else:
            # Other permitted ID6 encodings do not select the source identity.
            continue
        name = bytes(item.string).decode(codec, errors='strict')
        if not (0 < len(name) <= 63 and all(33 <= ord(char) <= 126 and char not in '[](){}<>/%' for char in name)):
            raise ValueError('Invalid source PostScript name')
        names.add(name)
    if len(names) != 1:
        raise ValueError('Source must have one unambiguous PostScript name')
    return names.pop()


def _source(data, face_index=0):
    collection = data[:4] == b'ttcf'
    if not collection and face_index != 0:
        raise ValueError('Standalone source requires face index zero')
    font = TTFont(io.BytesIO(data), fontNumber=face_index if collection else -1,
                  recalcBBoxes=False, recalcTimestamp=False)
    try:
        ps_name = _postscript_name(font)
        glyph_set = font.getGlyphSet()
        cff = _cff_geometry(font['CFF '].cff.topDictIndex[0]) if 'CFF ' in font else None
        aliases,unresolved_context = {},False
        for table in font['cmap'].tables if 'cmap' in font else []:
            if not table.isUnicode(): continue
            if table.format in (8,14) or not isinstance(getattr(table,'cmap',None),dict):
                unresolved_context = True
                continue
            for code,name in table.cmap.items():
                aliases.setdefault(name,set()).add(code)
        return {'font': font, 'data': data, 'sha256': sha(data), 'ps_name': ps_name,
                'face_index': face_index,
                'glyph_set': glyph_set, 'tt_geometry':truetype_geometry(font,allow_unresolved=True) if cff is None else None,
                'cmap': font.getBestCmap() or {},
                'scale': 1000 / font['head'].unitsPerEm,
                'pen': BoundsPen, 'cff_geometry': cff,'unicode_aliases':aliases,
                'unicode_context_unresolved':unresolved_context}
    except Exception:
        font.close()
        raise


def _source_box(source, name):
    cff = source['cff_geometry']
    if cff:
        return cff[1][cff[0].index(name)]
    if 'unresolved' in source['tt_geometry'][name]:
        raise ValueError('Unresolved source component context: '+source['tt_geometry'][name]['unresolved'][1])
    box = _bounds(source['glyph_set'], name, BoundsPen,source['tt_geometry'])
    return [v * source['scale'] for v in box] if box is not None else None


def _source_advance(source, name):
    cff = source['cff_geometry']
    if cff:return cff[2][cff[0].index(name)]
    first,last=source['tt_geometry'][name]['phantoms'][:2]
    return float(last[0]-first[0])*source['scale']


def _capital_height(source):
    candidates = {letter:source['cmap'][ord(letter)] for letter in 'HITEFL' if ord(letter) in source['cmap']}
    flat_codes = set(map(ord,'HITEFL'))
    references = {letter:name for letter,name in candidates.items()
                  if not source['unicode_context_unresolved'] and source['unicode_aliases'].get(name)
                  and source['unicode_aliases'][name] <= flat_codes}
    boxes = {letter:_source_box(source,name) for letter,name in references.items() if _known_geometry(source,name)}
    chosen = next((letter for letter,box in boxes.items() if box is not None),None)
    reference = chosen or next(iter(references),None)
    name = references[reference] if reference else source['font'].getGlyphOrder()[0]
    cff = source['cff_geometry']
    matrix = cff[3][cff[0].index(name)] if cff else Transform(source['scale'],0,0,source['scale'],0,0)
    declaration = getattr(source['font']['OS/2'],'sCapHeight',0) if 'OS/2' in source['font'] else 0
    if declaration > 0:
        expected,method = f32(declaration*source['scale']),'source designer capital-height declaration in fontwide units per em'
    elif chosen:
        expected,method = f32(boxes[chosen][3]-matrix.dy),'established flat-capital outline above its transformed baseline'
    else:
        names = source['font'].getGlyphOrder()
        covered = {source['font'].getGlyphID(references[letter]) for letter in boxes}|{0}
        if set(range(len(names))) <= covered or all(_known_geometry(source,name) and _source_box(source,name) is None for name in names):
            expected,method = 0.0,'established empty capital-reference population or entirely empty source geometry'
        else:
            expected,method = None,'capital design unresolved without a usable declaration/reference or separate author fact'
    return expected,{'source_flat_cap_tops':{letter:box[3] if box is not None else None for letter,box in boxes.items()},
        'source_flat_cap_geometry_unresolved':[letter for letter in references if letter not in boxes],
        'capital_reference':chosen,'capital_height_method':method,'source_capital_declaration':declaration,
        'capital_reference_context_unresolved':source['unicode_context_unresolved'],
        'capital_declaration_minus_reference':(declaration*source['scale']-(boxes[chosen][3]-matrix.dy)) if chosen and declaration>0 else None},matrix


def _known_geometry(source,name):
    return source['cff_geometry'] is not None or 'unresolved' not in source['tt_geometry'][name]


def _program(data, kind):
    if kind == '/FontFile2':
        font = TTFont(io.BytesIO(data), recalcBBoxes=False, recalcTimestamp=False)
        try:
            names = font.getGlyphOrder()
            glyph_set = font.getGlyphSet()
            geometry=truetype_geometry(font)
            scale = 1000 / font['head'].unitsPerEm
            return {'kind': 'TrueType', 'font': font, 'bounds': [[v * scale for v in box] if box is not None else None
                        for box in (_bounds(glyph_set, name, BoundsPen,geometry) for name in names)],
                    'names': names, 'advances': [float(geometry[name]['phantoms'][1][0]-geometry[name]['phantoms'][0][0])*scale for name in names],
                    'scale': 1,
                    'cid_to_gid': dict(enumerate(range(len(names)))),
                    'numeric_uncertainty': ['nonbinary em scale or nonlinear/composite glyph arithmetic'] if Fraction(1000, font['head'].unitsPerEm).denominator & (Fraction(1000, font['head'].unitsPerEm).denominator-1) or any(font['glyf'][n].isComposite() or any(not (flag & 1) for flag in getattr(font['glyf'][n], 'flags', [])) for n in names) else [],
                    'bounds_method': 'fontTools BoundsPen true quadratic extrema over retained composite outlines'}
        except Exception:
            font.close()
            raise
    cff = CFFFontSet(); cff.decompile(io.BytesIO(data), None)
    if len(cff.topDictIndex) != 1:
        raise ValueError('Expected one CFF face')
    top = cff.topDictIndex[0]
    names, boxes, advances, matrices = _cff_geometry(top)
    cid_to_gid = {}
    for gid, name in enumerate(names):
        if name == '.notdef':
            cid_to_gid[0] = gid
        elif name.startswith('cid') and name[3:].isdigit():
            cid_to_gid[int(name[3:])] = gid
        else:
            raise ValueError('Expected generated CID-keyed CFF charset')
    return {'kind': 'CFF', 'bounds': boxes, 'names': names, 'advances': advances,
            'scale': 1, 'cid_to_gid': cid_to_gid, 'effective_matrices': list(map(list, matrices)),
            'numeric_uncertainty': ['nonbinary effective decimal affine coefficient or retained cubic arithmetic'] if any(Fraction(str(v)).denominator & (Fraction(str(v)).denominator-1) for m in matrices for v in m) or any(any(op in str(char.program) for op in ('curveto','flex')) for char in top.CharStrings.values()) else [],
            'bounds_method': 'fontTools transformed cubics and BoundsPen extrema over all retained CFF glyphs'}


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


def _linear_stem(source, chosen, reason):
    from fontTools.pens.recordingPen import RecordingPen
    cff = source['cff_geometry']
    pen = RecordingPen()
    if cff is None:
        _draw(source['glyph_set'],chosen,TransformPen(pen,Transform(source['scale'],0,0,source['scale'],0,0)),source['tt_geometry'])
    else:
        top = source['font']['CFF '].cff.topDictIndex[0]
        top.CharStrings[chosen].draw(TransformPen(pen,cff[3][cff[0].index(chosen)]))
    edges = []; current = first = None
    for operation, points in pen.value:
        if operation == 'moveTo':
            first = current = tuple(Fraction.from_float(float(value)) for value in points[0])
        elif operation == 'lineTo' and current is not None:
            point = tuple(Fraction.from_float(float(value)) for value in points[0])
            edges.append((current,point)); current = point
        elif operation in ('closePath','endPath') and current is not None and first is not None:
            edges.append((current,first)); current = first = None
        else:
            raise ValueError(reason+'; independent linear measurement cannot evaluate '+operation)
    heights = [point[1] for edge in edges for point in edge]
    if not heights or max(heights) <= min(heights):
        raise ValueError(reason+'; no positive-height decoded line outline')
    samples, widths = [], []
    for fraction in (Fraction(1,4),Fraction(1,2),Fraction(3,4)):
        y = min(heights)+fraction*(max(heights)-min(heights))
        directions = {}
        for a,b in edges:
            if a[1] <= y < b[1] or b[1] <= y < a[1]:
                x=a[0]+(b[0]-a[0])*(y-a[1])/(b[1]-a[1])
                directions[x]=directions.get(x,0)+(1 if b[1]>a[1] else -1)
        winding = 0; start = None; intervals = []
        for x,direction in sorted(directions.items()):
            was_filled = winding != 0
            winding += direction
            if not was_filled and winding != 0: start=x
            elif was_filled and winding == 0 and x > start:
                intervals.append([str(start),str(x)]); widths.append(x-start)
        if winding != 0: raise ValueError('Unbalanced independent line crossings')
        samples.append({'height_fraction':str(fraction),'height_pdf_units':str(y),'filled_intervals_pdf_units':intervals})
    if not widths: raise ValueError(reason+'; no independently filled line intervals')
    measured = sorted(widths)[len(widths)//2]
    return {'method':'FontTools decoded linear filled intervals after native raster detail loss',
        'source_glyph':chosen, 'measured_exact_fraction':str(measured), 'measured_pdf_units':f32(float(measured)),
        'uprights':[{'index_from_left':0,'measured_pdf_units':f32(float(measured))}],
        'uncertainty_pdf_units':0, 'uncertainty_basis':'Exact Fraction intersections of independently decoded binary64 line coordinates; final binary32 representation',
        'samples':samples, 'raster_limitation':reason,
        'limits':['Line-only transformed font geometry; curved or unresolved geometry is not inferred from an unavailable raster.']}


def _stem(source):
    """Independent raster reasonableness measurement of horizontal stem spans.

    The producer documents a dominant-stem estimate. Here actual unhinted
    filled raster intervals are measured at recorded interior heights. The
    two-pixel interval uncertainty is a measurement bound, not a normative PDF
    tolerance. Tricky fonts remain explicitly identified in the evidence.
    """
    import freetype as ft
    ppem = 4096
    face = ft.Face(io.BytesIO(source['data']), index=source['face_index'])
    names = source['font'].getGlyphOrder()
    chosen = next((source['cmap'][ord(c)] for c in 'HITEFL' if ord(c) in source['cmap']
                   and _known_geometry(source,source['cmap'][ord(c)])
                   and _source_box(source, source['cmap'][ord(c)]) is not None), None)
    if chosen is None:
        chosen = next((name for name in names if _known_geometry(source,name) and _source_box(source, name) is not None), None)
    if chosen is None:
        return {'method': 'Every source outline empty', 'uprights': [{'index_from_left': 0, 'measured_pdf_units': 0}],
                'uncertainty_pdf_units': 0, 'samples': []}
    gid = names.index(chosen)
    face.set_pixel_sizes(ppem, ppem)
    flags = ft.FT_LOAD_NO_HINTING | ft.FT_LOAD_NO_AUTOHINT | ft.FT_LOAD_NO_BITMAP
    def resolution_failure(reason):
        measurement = _linear_stem(source,chosen,reason)
        measurement.update(source_gid=gid,ppem=ppem,freetype_version='.'.join(map(str,ft.version())),
            freetype_py_version=version('freetype-py'),freetype_binary_sha256=sha(Path(ft.FT_Library_filename).read_bytes()),
            load_flags_value=flags,variation='default instance')
        return measurement
    try:
        face.load_glyph(gid, flags)
        tight = face.glyph.outline.get_bbox()
        face.glyph.render(ft.FT_RENDER_MODE_NORMAL)
    except ft.FT_Exception as error:
        return resolution_failure('Native outline/raster unavailable: '+str(error))
    bitmap = face.glyph.bitmap
    if bitmap.pixel_mode != ft.FT_PIXEL_MODE_GRAY or bitmap.pitch <= 0:
        return resolution_failure('Expected positive-pitch grayscale FreeType bitmap')
    pixels = bytes(bitmap.buffer)
    samples, widths, bracket_widths = [], [], []
    for fraction in (.25, .5, .75):
        height = (tight.yMin + fraction * (tight.yMax - tight.yMin)) / 64
        row_coordinate = face.glyph.bitmap_top - height - .5
        rows = sorted({min(bitmap.rows-1,max(0,choose(row_coordinate))) for choose in (math.floor,math.ceil)})
        brackets = []
        for row in rows:
            values = pixels[row * bitmap.pitch:row * bitmap.pitch + bitmap.width]
            intervals, start = [], None
            for x, value in enumerate([*values, 0]):
                if value >= 128 and start is None:
                    start = x
                elif value < 128 and start is not None:
                    intervals.append([start, x]); start = None
            brackets.append({'bitmap_row':row,'row_center_height_pixels':face.glyph.bitmap_top-row-.5,
                             'intervals_pixels':intervals})
        if len({len(item['intervals_pixels']) for item in brackets}) != 1:
            raise ValueError('Raster interval topology changes across the bracketing sample rows')
        selected = min(brackets,key=lambda item:abs(item['row_center_height_pixels']-height))
        widths.extend(end-start for start,end in selected['intervals_pixels'])
        bracket_widths.append([[end-start for start,end in item['intervals_pixels']] for item in brackets])
        samples.append({'height_fraction_of_tight_outline': fraction, 'height_pixels': height,
                        **selected,'bracketing_rows':brackets})
    if not widths:
        return resolution_failure('No filled horizontal intervals at the declared raster sample heights')
    measured = sorted(widths)[len(widths) // 2] * 1000 / ppem
    medians = []
    for combination in itertools.product(*bracket_widths):
        spans = sorted(value for group in combination for value in group)
        if spans: medians.append(spans[len(spans)//2]*1000/ppem)
    return {'method': 'FreeType native unhinted filled horizontal intervals', 'source_glyph': chosen,
            'source_gid': gid, 'ppem': ppem, 'load_flags': ['FT_LOAD_NO_HINTING', 'FT_LOAD_NO_AUTOHINT', 'FT_LOAD_NO_BITMAP'],
            'load_flags_value': flags, 'render_mode': 'FT_RENDER_MODE_NORMAL', 'coverage_threshold': 128,
            'transform': 'source font effective matrix', 'variation': 'default instance',
            'tricky_font': bool(face.face_flags & ft.FT_FACE_FLAG_TRICKY), 'samples': samples,
            'tight_outline_bbox_26_6': [tight.xMin, tight.yMin, tight.xMax, tight.yMax],
            'uprights': [{'index_from_left': 0, 'measured_pdf_units': measured}],
            'measured_pdf_units': measured,
            'observed_median_interval_pdf_units': [min(medians),max(medians)],
            'sample_row_combinations': len(medians),
            'sampling_limit': 'Observed bracketing-row median interval, not a certified continuous-curve enclosure; changing raster topology is unresolved.',
            'uncertainty_pdf_units': 2 * 1000 / ppem,
            'uncertainty_basis': 'Two raster pixels across a filled interval; estimate reasonableness only',
            'freetype_version': '.'.join(map(str, ft.version())), 'freetype_py_version': version('freetype-py'),
            'freetype_binary_sha256': sha(Path(ft.FT_Library_filename).read_bytes())}


def _preparation_identity(source, program, program_bytes, witness):
    """Resolve an exact preparation claim independently of logical Unicode text.

    Anchors are caller provenance, not metric verdicts. TrueType subset tails
    follow paired fontTools-decoded component edges; no tail ordering is assumed.
    FontTools retains semantic flags, XY/point arguments and affine coefficients
    while decoding the component record's integer-width/scale representation.
    """
    source_font = source['font']
    source_names = source_font.getGlyphOrder()
    source_kind = 'Cff' if source['cff_geometry'] else 'TrueType'
    if witness['outline_kind'] != source_kind or program['kind'] != ('CFF' if source_kind == 'Cff' else 'TrueType'):
        raise ValueError('Witness outline kind does not match source and embedded program')
    mapping, reverse, cids = {}, {}, set()

    def bind(gid, source_gid):
        if not 0 <= gid < len(program['names']) or not 0 <= source_gid < len(source_names):
            raise ValueError('Preparation anchor or component GID is out of range')
        if gid in mapping and mapping[gid] != source_gid or source_gid in reverse and reverse[source_gid] != gid:
            raise ValueError('Preparation glyph correspondence conflicts')
        mapping[gid], reverse[source_gid] = source_gid, gid

    anchors = witness['remapping']
    seen_sources = set()
    for anchor in anchors:
        if not isinstance(anchor, dict) or any(type(anchor.get(key)) is not int for key in ('source_gid', 'cid')):
            raise ValueError('Preparation anchors require integer source GIDs and CIDs')
        cid, source_gid = anchor['cid'], anchor['source_gid']
        if cid in cids or source_gid in seen_sources:
            raise ValueError('Duplicate preparation CID or source GID anchor')
        cids.add(cid); seen_sources.add(source_gid)
        if cid not in program['cid_to_gid']:
            raise ValueError('Preparation CID is absent from embedded charset')
        bind(program['cid_to_gid'][cid], source_gid)
    if witness['preparation_kind'] == 'Full':
        original_program = source_font.reader['CFF '] if source_kind == 'Cff' else source['data']
        if program_bytes != original_program or len(program['names']) != len(source_names):
            raise ValueError('Full preparation requires exact original program bytes')
        for gid in range(len(program['names'])): bind(gid, gid)
    else:
        if mapping.get(0) != 0 or 0 not in cids:
            raise ValueError('Subset preparation requires the explicit notdef anchor')
        if cids != set(range(len(anchors))):
            raise ValueError('Subset preparation anchors must describe the complete requested CID prefix')
        if program['kind'] == 'TrueType':
            source_glyf, prepared_glyf = source_font['glyf'], program['font']['glyf']
            source_ids = {name: gid for gid, name in enumerate(source_names)}
            prepared_ids = {name: gid for gid, name in enumerate(program['names'])}
            pending, visited, edges = list(mapping), set(), {}

            def record(component):
                arguments = ('points', component.firstPt, component.secondPt) if hasattr(component, 'firstPt') else ('xy', component.x, component.y)
                transform = tuple(tuple(row) for row in getattr(component, 'transform', ((1, 0), (0, 1))))
                return component.flags, arguments, transform

            while pending:
                gid = pending.pop()
                if gid in visited: continue
                visited.add(gid)
                original = source_glyf[source_names[mapping[gid]]]
                prepared = prepared_glyf[program['names'][gid]]
                if original.isComposite() != prepared.isComposite():
                    raise ValueError('Source/prepared simple or composite structure differs')
                original_components = getattr(original, 'components', [])
                prepared_components = getattr(prepared, 'components', [])
                if len(original_components) != len(prepared_components):
                    raise ValueError('Source/prepared component counts differ')
                edges[gid] = []
                for left, right in zip(original_components, prepared_components):
                    if record(left) != record(right):
                        raise ValueError('Source/prepared ordered component flags, arguments or transforms differ')
                    child, source_child = prepared_ids[right.glyphName], source_ids[left.glyphName]
                    bind(child, source_child); edges[gid].append(child); pending.append(child)
            active, complete = set(), set()
            for start in edges:
                stack = [(start, False)]
                while stack:
                    gid, leaving = stack.pop()
                    if leaving:
                        active.remove(gid); complete.add(gid)
                    elif gid not in complete:
                        if gid in active: raise ValueError('Cyclic preparation component correspondence')
                        active.add(gid); stack.append((gid, True))
                        stack.extend((child, False) for child in reversed(edges[gid]))
    if set(mapping) != set(range(len(program['names']))):
        raise ValueError('Preparation identity is incomplete, including empty or unrequested glyphs')
    return mapping


def _unique_json(data):
    def unique_object(pairs):
        result = {}
        for key, value in pairs:
            if key in result: raise ValueError('Duplicate JSON field: ' + key)
            result[key] = value
        return result
    return json.loads(data, object_pairs_hook=unique_object)


def _read_preparation_report(data):
    witness = _unique_json(data)
    if not isinstance(witness, dict): raise ValueError('Preparation report must be an object')
    for key in ('source_sha256', 'prepared_sha256'):
        if not isinstance(witness.get(key), str) or not re.fullmatch('[0-9a-f]{64}', witness[key]):
            raise ValueError('Preparation report requires an exact SHA-256: ' + key)
    if type(witness.get('face_index')) is not int or witness['face_index'] < 0:
        raise ValueError('Preparation report requires an explicit nonnegative source face index')
    if witness.get('preparation_kind') not in ('Subset', 'Full') or witness.get('outline_kind') not in ('Cff', 'TrueType'):
        raise ValueError('Unknown preparation or outline kind')
    if not isinstance(witness.get('remapping'), list): raise ValueError('Preparation remapping must be an array')
    return witness


def _read_ascent_facts(data):
    facts = _unique_json(data)
    if not isinstance(facts, dict) or set(facts) != {'source_sha256', 'face_index', 'accented_source_glyph_ids'}:
        raise ValueError('Ascent facts require exactly source_sha256, face_index and accented_source_glyph_ids')
    if not isinstance(facts['source_sha256'], str) or not re.fullmatch('[0-9a-f]{64}', facts['source_sha256']):
        raise ValueError('Ascent facts require an exact source SHA-256')
    if type(facts['face_index']) is not int or not 0 <= facts['face_index'] <= 0xffffffff:
        raise ValueError('Ascent facts require a nonnegative 32-bit source face index')
    gids = facts['accented_source_glyph_ids']
    if not isinstance(gids, list) or any(type(gid) is not int or not 0 < gid <= 0xffff for gid in gids):
        raise ValueError('Ascent facts require nonzero 16-bit source GIDs')
    if gids != sorted(set(gids)):
        raise ValueError('Ascent fact GIDs must be strictly increasing and unique')
    return facts


def _ascent_facts_digest(facts):
    return sha(b'publisher-v2/font-ascent-facts/v1\0' + bytes.fromhex(facts['source_sha256']) +
               struct.pack('>II', facts['face_index'], len(facts['accented_source_glyph_ids'])) +
               b''.join(struct.pack('>H', gid) for gid in facts['accented_source_glyph_ids']))


def _read_font_facts(data):
    facts = _unique_json(data)
    required = {'source_sha256','face_index'}
    optional = {'accented_source_glyph_ids','cap_height_pdf_units'}
    if not isinstance(facts,dict) or not required <= set(facts) <= required|optional:
        raise ValueError('Font facts require source/face and only the two supported optional design facts')
    accented = facts.get('accented_source_glyph_ids')
    _read_ascent_facts(json.dumps({key:facts[key] for key in required} |
        {'accented_source_glyph_ids':[] if accented is None else accented}).encode())
    cap = facts.get('cap_height_pdf_units')
    if accented is None and cap is None:
        raise ValueError('Font facts must supply at least one design fact')
    if cap is not None:
        if type(cap) not in (int,float) or not math.isfinite(cap) or abs(cap)>10000000:
            raise ValueError('Capital-height fact must be a finite bounded number')
        facts['cap_height_pdf_units'] = 0.0 if cap == 0 else f32(cap)
    return facts


def _font_facts_digest(facts):
    encoded = (b'publisher-v2/font-design-facts/v1\0'+bytes.fromhex(facts['source_sha256'])+
               struct.pack('>I',facts['face_index']))
    accented = facts.get('accented_source_glyph_ids')
    encoded += bytes([accented is not None])
    if accented is not None:
        encoded += struct.pack('>I',len(accented))+b''.join(struct.pack('>H',gid) for gid in accented)
    cap = facts.get('cap_height_pdf_units')
    encoded += bytes([cap is not None])
    if cap is not None: encoded += struct.pack('>f',0.0 if cap == 0 else cap)
    return sha(encoded)


def inspect_pdf_metrics(pdf, source_fonts, preparation_reports=None, ascent_facts=None, font_facts=None):
    """Return findings/observations for one exact generated PDF and source faces.

    Exact witnesses, when supplied, are required for every embedded program;
    producer metric values and verdict fields are ignored. Without witnesses,
    the earlier finite source-name/text matching remains available.
    Unknown sources, unsupported maps and unavailable measurements are findings.
    Unmapped .notdef contributes its retained bounds and ascent; this necessary
    unclassified glyph is explicitly recorded. Other unclassified outlines do
    not silently receive an unaccented classification.
    """
    path = Path(pdf)
    data = path.read_bytes()
    observations = {'input_sha256': sha(data), 'input_bytes': len(data), 'fonts': [], 'sources': [],
                    'numeric_comparison': 'Outward integer-unit curve enclosure; binary32 roundtrip for ascent, descent, capitals, angle and advances; measured raster uncertainty for stem reasonableness',
                    'coverage_limit': 'Generated Identity-H TrueType/CFF programs and explicit source faces only; '
                    'Selected-capital or fallback stem estimate reasonableness at recorded raster conditions; no all-font VM or PDF certification claim.'}
    report = {'schema': 'pdf-font-metrics-verification/v1', 'passed': False, 'findings': [],
              'input_sha256': sha(data), 'input_bytes': len(data),
              'observations': observations, 'evaluator_sha256': sha(Path(__file__).read_bytes()),
              'tools': {name: version(name) for name in ('fonttools', 'pypdf')}}

    def finding(rule, detail, **context):
        report['findings'].append({'rule': rule, 'detail': detail, **context})

    witness_mode = preparation_reports is not None
    fact_records, fact_paths = {}, []
    observations['ascent_fact_inputs'] = []
    observations['font_fact_inputs'] = []
    fact_inputs = [(item,True) for item in ascent_facts or []]+[(item,False) for item in font_facts or []]
    for item,legacy_ascent in fact_inputs:
        fact_path = Path(item)
        try:
            fact_data = fact_path.read_bytes(); fact_paths.append((fact_path, sha(fact_data)))
            facts = (_read_ascent_facts if legacy_ascent else _read_font_facts)(fact_data)
            key = (facts['source_sha256'], facts['face_index'])
            if key in fact_records:
                raise ValueError('Duplicate or conflicting Ascent facts for the same source face')
            if not witness_mode:
                raise ValueError('Authored Ascent facts require an exact preparation identity witness')
            fact_records[key] = {'data': facts, 'used': False}
            observation = {'file_name': fact_path.name, 'sha256': sha(fact_data),
                           'source_sha256': key[0], 'face_index': key[1]}
            observations['font_fact_inputs'].append(observation)
            if legacy_ascent: observations['ascent_fact_inputs'].append(observation)
        except Exception as error:
            finding('font-metric-ascent-facts' if legacy_ascent else 'font-metric-font-facts', type(error).__name__ + ': ' + str(error))
    witnesses, report_paths = [], []
    observations['preparation_reports'] = []
    for item in preparation_reports or []:
        report_path = Path(item)
        try:
            report_data = report_path.read_bytes(); digest = sha(report_data)
            report_paths.append((report_path, digest))
            decoded = _read_preparation_report(report_data)
            observations['preparation_reports'].append({'file_name': report_path.name, 'sha256': digest,
                **{key:decoded[key] for key in ('source_sha256','prepared_sha256','face_index')}})
            witnesses.append({'data': decoded, 'sha256': digest, 'used': False})
        except Exception as error:
            finding('font-metric-preparation-identity', type(error).__name__ + ': ' + str(error))
    sources, source_paths = {}, []
    for item in source_fonts:
        source_path = Path(item)
        try:
            source_data = source_path.read_bytes(); source_digest = sha(source_data)
            source_paths.append((source_path, source_digest))
            faces = sorted({entry['data']['face_index'] for entry in witnesses
                            if entry['data']['source_sha256'] == source_digest}) if witness_mode else [0]
            if not witness_mode and source_data[:4] == b'ttcf':
                raise ValueError('Collection sources require an explicit preparation face witness')
            for face_index in faces:
                source = _source(source_data, face_index)
                observations['sources'].append({'file_name': source_path.name, 'sha256': source['sha256'],
                                                'face_index': face_index, 'postscript_name': source['ps_name']})
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
            font_observation = {'object': f'{number} {generation}', 'unmapped_gids': [], 'emitted':{},
                                'accented_gids_excluded_from_ascent': [], 'expected': {}}
            observations['fonts'].append(font_observation)
            context = {'font_object': font_observation['object']}

            def check(key, expected, rule):
                font_observation['expected'][key] = expected
                declared = descriptor.get('/' + key)
                font_observation['emitted'][key]=(list(map(float,declared)) if isinstance(declared,ArrayObject)
                                                  else float(declared) if declared is not None else None)
                try:
                    equal = ([f32(float(value)) for value in declared] == expected
                             if isinstance(expected, list) else f32(float(declared)) == expected)
                except (TypeError, ValueError, OverflowError):
                    equal = False
                if not equal:
                    finding(rule, f'{key} differs from independently measured metric',
                            declared=list(declared) if isinstance(declared, ArrayObject) else declared,
                            expected=expected, **context)

            program = None
            try:
                if obj.get('/Encoding') != '/Identity-H' or len(obj['/DescendantFonts']) != 1:
                    raise ValueError('Unsupported composite font encoding')
                child = obj['/DescendantFonts'][0].get_object()
                descriptor = child['/FontDescriptor'].get_object()
                ps_name = re.sub(r'^[A-Z]{6}\+', '', str(descriptor['/FontName']).lstrip('/'))
                font_observation['postscript_name'] = ps_name
                kinds = [key for key in ('/FontFile2', '/FontFile3') if key in descriptor]
                if len(kinds) != 1:
                    raise ValueError('Expected one embedded font program')
                kind = kinds[0]
                if kind == '/FontFile2' and child.get('/CIDToGIDMap') != '/Identity':
                    raise ValueError('Expected Identity CID-to-GID mapping')
                if kind == '/FontFile3' and descriptor[kind].get('/Subtype') != '/CIDFontType0C':
                    raise ValueError('Expected raw CIDFontType0C')
                program_bytes = descriptor[kind].get_data()
                program_digest = sha(program_bytes)
                program = _program(program_bytes, kind)
                font_observation.update(program_sha256=program_digest, program_kind=program['kind'],
                                         glyph_count=len(program['names']), bounds_method=program['bounds_method'])
                source_gid_map = None
                if witness_mode:
                    matches = [entry for entry in witnesses if entry['data']['prepared_sha256'] == program_digest]
                    if len(matches) != 1:
                        finding('font-metric-preparation-identity', 'Expected exactly one report for the exact embedded program hash', **context)
                        continue
                    entry = matches[0]; entry['used'] = True; witness = entry['data']
                    matches = [source for source in sources.get(ps_name, [])
                               if (source['sha256'], source['face_index']) == (witness['source_sha256'], witness['face_index'])]
                    if len(matches) != 1:
                        finding('font-metric-preparation-identity', 'Expected one source with the witnessed hash, face and PostScript name', **context)
                        continue
                    source = matches[0]
                    try:
                        source_gid_map = _preparation_identity(source, program, program_bytes, witness)
                    except Exception as error:
                        finding('font-metric-preparation-identity', type(error).__name__ + ': ' + str(error), **context)
                        continue
                    font_observation['source_gid_map'] = {str(gid): source_gid_map[gid] for gid in sorted(source_gid_map)}
                    font_observation['preparation_report_sha256'] = entry['sha256']
                    font_observation['identity_method'] = 'Exact hash/face and requested anchors; independent ordered component closure for TrueType subsets, CID charset for CFF'
                else:
                    matches = sources.get(ps_name, [])
                    if len(matches) != 1:
                        finding('font-metric-source-unresolved', 'Expected exactly one explicit matching source PostScript name', **context)
                        continue
                    source = matches[0]
                font_observation['source_sha256'] = source['sha256']
                font_observation['source_face_index'] = source['face_index']
                source_font = source['font']
                fact_record = fact_records.get((source['sha256'], source['face_index']))
                excluded_sources = cap_fact = None
                if fact_record is not None:
                    fact_record['used'] = True
                    facts = fact_record['data']
                    accented_ids = facts.get('accented_source_glyph_ids')
                    cap_fact = facts.get('cap_height_pdf_units')
                    font_observation['font_facts_sha256'] = _font_facts_digest(facts)
                    if witness_mode and witness.get('font_facts_sha256') not in (None,font_observation['font_facts_sha256']):
                        finding('font-metric-font-facts','Prepared combined fact identity differs from the separately authored input',**context)
                    if accented_ids is not None and any(gid >= len(source_font.getGlyphOrder()) for gid in accented_ids):
                        finding('font-metric-ascent-facts', 'Authored source GID is outside the selected face', **context)
                        continue
                    if accented_ids is not None:
                        excluded_sources = set(accented_ids)
                        font_observation['ascent_classification'] = 'separate-authored-source-gid-facts'
                        font_observation['ascent_facts_sha256'] = _ascent_facts_digest(facts)
                        if witness_mode and witness.get('ascent_facts_sha256') not in (None, font_observation['ascent_facts_sha256']):
                            finding('font-metric-ascent-facts', 'Prepared fact identity differs from the separately authored facts', **context)
                elif witness_mode and witness.get('ascent_facts_sha256') is not None:
                    finding('font-metric-ascent-facts', 'Prepared font declares authored facts but no matching independent input was supplied', **context)
                else:
                    font_observation['ascent_classification'] = 'finite-source-unicode-classification'
                font_observation.setdefault('ascent_classification','finite-source-unicode-classification')
                if fact_record is None and witness_mode and witness.get('font_facts_sha256') is not None:
                    finding('font-metric-font-facts','Prepared font declares design facts but no separate matching input was supplied',**context)
                fixed = bool(source_font['post'].isFixedPitch)
                italic = bool(source_font['OS/2'].fsSelection & 1) if 'OS/2' in source_font else bool(source_font['head'].macStyle & 2)
                flags = int(descriptor.get('/Flags',-1))
                font_observation['source_style'] = {'fixed_pitch':fixed,'italic':italic,'descriptor_flags':flags}
                if bool(flags & 1) != fixed or bool(flags & 64) != italic:
                    finding('font-metric-flags','Descriptor style flags differ from the explicit original face',**context)
                boxes, scale = program['bounds'], program['scale']
                raw_bounds = _union(boxes)
                font_observation['complete_transformed_geometry'] = boxes
                bbox_check=inspect_font_bbox_enclosure(list(map(float,descriptor.get('/FontBBox',[]))),raw_bounds,program['numeric_uncertainty'])
                font_observation['emitted']['FontBBox']=list(map(float,descriptor.get('/FontBBox',[])))
                font_observation['bbox_enclosure']=bbox_check
                font_observation['expected']['FontBBox']=bbox_check['ideal_integer_encoding']
                if not bbox_check['passed']:
                    finding('font-metric-bbox','FontBBox fails independent enclosure/conditional representation bound',**context)
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
                    candidates = ({source_font.getGlyphOrder()[source_gid_map[gid]]} if source_gid_map is not None else
                                  {source['cmap'][ord(text)] for text in texts
                                   if len(text) == 1 and ord(text) in source['cmap']})
                    if source_gid_map is None and not candidates and name in source_names:
                        candidates = {name}
                    if len(candidates) != 1:
                        if boxes[gid] is not None:
                            finding('font-metric-glyph-unresolved', 'Retained outline has no unique source glyph identity', gid=gid, **context)
                        continue
                    source_name = candidates.pop()
                    box = source_bounds.setdefault(source_name, _source_box(source, source_name))
                    embedded_box = [scaled(v, scale) for v in boxes[gid]] if boxes[gid] is not None else None
                    source_box = [f32(v) for v in box] if box is not None else None
                    if embedded_box != source_box:
                        finding('font-metric-source-outline', 'Retained glyph bounds differ from explicit source glyph', gid=gid, **context)
                    if scaled(program['advances'][gid], scale) != f32(_source_advance(source, source_name)):
                        finding('font-metric-source-width', 'Retained advance differs from explicit source glyph', gid=gid, **context)
                    codes = reverse.get(source_name, [])
                    source_gid = source_font.getGlyphID(source_name)
                    accented = (source_gid in excluded_sources if excluded_sources is not None else
                                bool(codes) and all(any(unicodedata.combining(ch) for ch in unicodedata.normalize('NFD', chr(code))) for code in codes))
                    if accented:
                        font_observation['accented_gids_excluded_from_ascent'].append(gid)
                    elif excluded_sources is not None or codes or gid == 0:
                        ascent = max(ascent, boxes[gid][3] if boxes[gid] is not None else 0)
                    elif boxes[gid] is not None:
                        finding('font-metric-ascent-unresolved', 'Source glyph has no Unicode accent classification', gid=gid, **context)
                check('Ascent', scaled(ascent, scale), 'font-metric-ascent')
                font_observation['notdef_classification_limit'] = 'Unmapped .notdef is included in retained bounds and unaccented ascent.'
                expected_cap,cap_observation,matrix = _capital_height(source)
                font_observation.update(cap_observation)
                if cap_fact is not None:
                    if expected_cap is not None and f32(cap_fact) != expected_cap:
                        finding('font-metric-cap-height-facts','Authored capital height conflicts with an independently established source metric',**context)
                    if expected_cap is None:
                        expected_cap = cap_fact
                        font_observation['capital_height_method'] = 'separately authored source/face-bound design fact in final PDF glyph units'
                if expected_cap is None:
                    finding('font-metric-cap-unresolved','Capital design requires a separately authored fact',**context)
                else:
                    check('CapHeight',f32(expected_cap),'font-metric-cap-height')
                chosen = cap_observation['capital_reference']
                matrix = (source['cff_geometry'][3][source['cff_geometry'][0].index(source['cmap'][ord(chosen)])]
                          if chosen and source['cff_geometry'] else
                          source['cff_geometry'][3][0] if source['cff_geometry'] else Transform())
                original_angle = getattr(source_font['CFF '].cff.topDictIndex[0], 'ItalicAngle', 0) if source['cff_geometry'] else source_font['post'].italicAngle
                slant = math.tan(math.radians(-original_angle))
                expected_angle = -math.degrees(math.atan2(matrix.xx * slant + matrix.yx,
                                                        matrix.xy * slant + matrix.yy))
                check('ItalicAngle', f32(expected_angle), 'font-metric-italic-angle')
                try:
                    measurement = _stem(source)
                    font_observation['stem_measurement'] = measurement
                    declared_stem = descriptor.get('/StemV')
                    value = f32(float(declared_stem)) if declared_stem is not None else math.nan
                    measurement['declared_binary32'] = value if math.isfinite(value) else None
                    lower,upper = measurement.get('observed_median_interval_pdf_units',
                                                 [measurement['uprights'][0]['measured_pdf_units']]*2)
                    measurement['matched_upright_indexes'] = [0] if (math.isfinite(value) and
                        lower-measurement['uncertainty_pdf_units'] <= value <= upper+measurement['uncertainty_pdf_units']) else []
                    if not measurement['matched_upright_indexes']:
                        finding('font-metric-stem-v', 'StemV is outside the independently rasterized dominant filled-interval measurement', declared=declared_stem, **context)
                except Exception as error:
                    finding('font-metric-stem-unresolved', type(error).__name__ + ': ' + str(error), **context)
                widths = _widths(child)
                font_observation['widths_checked'] = len(widths)
                font_observation['width_comparisons']=[]
                for cid, width in widths.items():
                    gid = program['cid_to_gid'].get(cid)
                    expected_width=scaled(program['advances'][gid], scale) if gid is not None else None
                    font_observation['width_comparisons'].append({'cid':cid,'embedded_gid':gid,
                        'emitted':float(width),'expected':expected_width})
                    if gid is None or f32(width) != expected_width:
                        finding('font-metric-width', 'PDF width differs from embedded advance', cid=cid, **context)
            except Exception as error:
                finding('font-metric-preparation-identity' if witness_mode and program is None else 'font-metric-unresolved',
                        type(error).__name__ + ': ' + str(error), **context)
            finally:
                if program is not None and 'font' in program: program['font'].close()
        if not observations['fonts']:
            finding('font-metric-unresolved', 'No supported composite font was measured')
    except Exception as error:
        finding('font-metric-unresolved', type(error).__name__ + ': ' + str(error))
    for entry in witnesses:
        if not entry['used']:
            finding('font-metric-preparation-identity', 'Preparation report has no unique matching embedded program', report_sha256=entry['sha256'])
    for entry in fact_records.values():
        if not entry['used']:
            finding('font-metric-ascent-facts', 'Authored facts have no matching embedded source face',
                    source_sha256=entry['data']['source_sha256'], face_index=entry['data']['face_index'])
    observations['ascent_facts_unchanged'] = all(sha(item.read_bytes()) == digest for item, digest in fact_paths)
    observations['font_facts_unchanged'] = observations['ascent_facts_unchanged']
    observations['preparation_reports_unchanged'] = all(sha(item.read_bytes()) == digest for item, digest in report_paths)
    observations['input_sha256_after'] = sha(path.read_bytes())
    observations['sources_unchanged'] = all(sha(item.read_bytes()) == digest for item, digest in source_paths)
    report['unchanged'] = observations['input_sha256_after'] == observations['input_sha256'] and observations['sources_unchanged'] and observations['preparation_reports_unchanged'] and observations['ascent_facts_unchanged']
    if not report['unchanged']:
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
    parser.add_argument('--preparation-report', type=Path, action='append',
                        help='Exact source/face/program and requested CID witness; repeat for each prepared font')
    parser.add_argument('--ascent-facts', type=Path, action='append',
                        help='Separate authored source/face and complete accented-role GIDs; repeat for each source face')
    parser.add_argument('--font-facts', type=Path, action='append',
                        help='Separate source/face-bound optional accented-role set and final PDF capital-height design fact')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    destination = args.output / 'font-metrics.json' if args.output.is_dir() else args.output
    if args.output.is_dir() and any(args.output.iterdir()):
        parser.error('Output directory must be empty')
    if destination.exists():
        parser.error('Output file already exists')
    report = inspect_pdf_metrics(args.pdf, args.source_font, preparation_reports=args.preparation_report,
                                 ascent_facts=args.ascent_facts,font_facts=args.font_facts)
    with destination.open('x', encoding='utf-8', newline='\n') as output:
        json.dump(report, output, indent=2, allow_nan=False); output.write('\n')
    print(json.dumps({'passed': report['passed'], 'findings': report['findings']}))
    return 0 if report['passed'] else 1


if __name__ == '__main__':
    raise SystemExit(main())
