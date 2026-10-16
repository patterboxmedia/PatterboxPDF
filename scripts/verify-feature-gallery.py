"""Independently inspect and render the native feature gallery, without fixups.

This verifier checks this project's examples. It is not a general PDF validator.
Python needs pypdf, pypdfium2, Pillow and numpy; qpdf and Poppler are development
tools supplied explicitly by the caller. No checker is staged into the library.
"""
if not __debug__:
    raise SystemExit('Run this verifier without -O; its validation assertions must remain enabled.')

import argparse
from collections import Counter
import hashlib
import json
from pathlib import Path
import subprocess

import numpy as np
from PIL import Image
import pypdfium2 as pdfium
from pypdf import PdfReader
from pypdf.generic import BooleanObject, ContentStream

BLENDS = {'/Normal', '/Multiply', '/Screen', '/Overlay', '/Darken', '/Lighten',
          '/ColorDodge', '/ColorBurn', '/HardLight', '/SoftLight', '/Difference',
          '/Exclusion', '/Hue', '/Saturation', '/Color', '/Luminosity'}
BOUNDS = {'mean_channel_difference_max': 5, 'p95_max_channel_difference_max': 20,
          'pixels_above_10_percent_max': 8}
FLAGS = getattr(subprocess, 'CREATE_NO_WINDOW', 0)


def sha(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def inspect(path, candidate, expected_profile):
    reader = PdfReader(path, strict=True)
    root = reader.trailer['/Root']
    assert not reader.is_encrypted
    assert all(key not in root for key in ('/AcroForm', '/OpenAction', '/AA', '/OCProperties'))
    intents = root.get('/OutputIntents', [])
    assert len(intents) == int(candidate)
    if candidate:
        intent = intents[0].get_object()
        assert intent['/S'] == '/GTS_PDFX' and str(intent['/Info']).strip()
        profile = intent['/DestOutputProfile'].get_object()
        assert profile['/N'] == 4 and profile.get_data()[16:20] == b'CMYK'
        assert profile.get_data() == expected_profile
        assert str((reader.metadata or {})['/Title']).strip()
    else:
        assert '/GTS_PDFXVersion' not in (reader.metadata or {})
    counts = Counter()
    blends = set()
    visited = set()

    def identity(value):
        reference = getattr(value, 'indirect_reference', None)
        return (reference.idnum, reference.generation) if reference else id(value)

    def group(value):
        assert value['/S'] == '/Transparency'
        assert isinstance(value['/I'], BooleanObject) and value['/I'].value is True
        assert value['/CS'] in ('/DeviceRGB', '/DeviceCMYK', '/DeviceGray')
        if candidate:
            assert value['/CS'] != '/DeviceRGB'
        counts['group_' + str(value['/CS'])[1:]] += 1

    def function(reference, channels):
        value = reference.get_object()
        assert list(value['/Domain']) == [0, 1]
        kind = int(value['/FunctionType'])
        assert kind in (2, 3)
        if kind == 2:
            assert float(value['/N']) == 1
            assert len(value['/C0']) == len(value['/C1']) == channels
            assert all(0 <= float(c) <= 1 for c in [*value['/C0'], *value['/C1']])
        else:
            functions, bounds, encode = value['/Functions'], value['/Bounds'], value['/Encode']
            assert len(functions) == len(bounds) + 1 and len(encode) == 2 * len(functions)
            assert all(a < b for a, b in zip([0, *map(float, bounds)], [*map(float, bounds), 1]))
            assert list(encode) == [0, 1] * len(functions)
            for child in functions:
                function(child, channels)
        counts['function_type_' + str(kind)] += 1

    def shading(reference):
        value = reference.get_object()
        kind = int(value['/ShadingType'])
        assert kind in (2, 3)
        assert '/Domain' not in value or list(value['/Domain']) == [0, 1]
        space = value['/ColorSpace']
        channels = {'/DeviceGray': 1, '/DeviceRGB': 3, '/DeviceCMYK': 4}[space]
        if candidate:
            assert space != '/DeviceRGB'
        assert len(value['/Coords']) == (4 if kind == 2 else 6)
        assert len(value['/Extend']) == 2
        assert all(isinstance(v, BooleanObject) and v.value is True for v in value['/Extend'])
        function(value['/Function'], channels)
        counts['shading_type_' + str(kind)] += 1

    def object(reference):
        value = reference.get_object()
        key = identity(value)
        if key in visited:
            return
        visited.add(key)
        kind = value.get('/Subtype')
        if kind == '/Image':
            space = value['/ColorSpace']
            channels = {'/DeviceGray': 1, '/DeviceRGB': 3, '/DeviceCMYK': 4}[space]
            assert not candidate or space != '/DeviceRGB'
            assert value['/BitsPerComponent'] == 8
            assert len(value.get_data()) == int(value['/Width']) * int(value['/Height']) * channels
            counts['image_' + str(space)[1:]] += 1
            if '/SMask' in value:
                assert value['/SMask']['/ColorSpace'] == '/DeviceGray'
                object(value['/SMask'])
        elif kind == '/Form':
            group(value['/Group'])
            stream(value, value['/Resources'])
            counts['forms'] += 1
        else:
            raise AssertionError(f'unexpected XObject subtype {kind}')

    def stream(content, resources):
        use = {name: set() for name in ('/Font', '/XObject', '/ExtGState', '/Shading')}
        depth = 0
        for operands, operator in ContentStream(content, reader).operations:
            if operator == b'q':
                depth += 1
            elif operator == b'Q':
                depth -= 1
                assert depth >= 0
            category = {b'Tf': '/Font', b'Do': '/XObject', b'gs': '/ExtGState', b'sh': '/Shading'}.get(operator)
            if category:
                use[category].add(str(operands[0]))
        assert depth == 0
        for category, names in use.items():
            assert names == set(resources.get(category, {})), (category, names, set(resources.get(category, {})))
        counts['streams_with_exact_local_resources'] += 1
        for reference in resources.get('/Font', {}).values():
            font = reference.get_object()
            assert font['/Subtype'] == '/Type0' and '/ToUnicode' in font
            descriptor = font['/DescendantFonts'][0]['/FontDescriptor']
            assert sum(key in descriptor for key in ('/FontFile', '/FontFile2', '/FontFile3')) == 1
        for reference in resources.get('/XObject', {}).values():
            object(reference)
        for reference in resources.get('/Shading', {}).values():
            shading(reference)
        for reference in resources.get('/ExtGState', {}).values():
            value = reference.get_object()
            blend = str(value.get('/BM', '/Normal'))
            assert blend in BLENDS
            blends.add(blend)
            assert all(key not in value for key in ('/TR', '/TR2', '/HT'))
            mask = value.get('/SMask')
            if mask is not None and str(mask) != '/None':
                mask = mask.get_object()
                assert mask['/S'] == '/Luminosity'
                object(mask['/G'])
                counts['luminosity_masks'] += 1
        assert '/Pattern' not in resources

    for page in reader.pages:
        assert '/Annots' not in page
        group(page['/Group'])
        stream(page.get_contents(), page['/Resources'])
    assert blends == BLENDS, ('missing blend coverage', BLENDS - blends)
    assert counts['shading_type_2'] and counts['shading_type_3'] and counts['luminosity_masks']
    assert counts['image_DeviceGray'] and counts['image_DeviceCMYK']
    assert candidate or counts['image_DeviceRGB']
    return {'pages': len(reader.pages), 'counts': dict(counts), 'blend_modes': sorted(blends),
            'local_resource_dictionaries_exact': True, 'native_functions_images_and_masks_checked': True}


def render(path, poppler, output):
    pages = []
    with pdfium.PdfDocument(path) as document:
        for index in range(len(document)):
            page = document[index]
            width, height = page.get_size()
            scale = min(2, 1600 / max(width, height))
            bitmap = page.render(scale=scale)
            image = bitmap.to_pil().convert('RGB')
            first = np.asarray(image).copy()
            pdfium_path = output / f'pdfium-{index+1}.png'
            image.save(pdfium_path)
            bitmap.close()
            page.close()
            prefix = output / f'poppler-{index+1}'
            run = subprocess.run([str(poppler), '-f', str(index+1), '-l', str(index+1),
                '-singlefile', '-scale-to-x', str(first.shape[1]), '-scale-to-y', str(first.shape[0]),
                '-png', str(path), str(prefix)], capture_output=True, timeout=90, creationflags=FLAGS)
            assert run.returncode == 0, run.stderr.decode(errors='replace')
            poppler_path = prefix.with_suffix('.png')
            with Image.open(poppler_path) as second_image:
                second = np.asarray(second_image.convert('RGB')).copy()
            assert first.shape == second.shape
            delta = np.abs(first.astype(np.int16) - second.astype(np.int16))
            maximum = delta.max(axis=2)
            metrics = {'mean_channel_difference': float(delta.mean()),
                'p95_max_channel_difference': float(np.percentile(maximum, 95)),
                'pixels_above_10_percent': float((maximum > 10).mean() * 100)}
            passed = (metrics['mean_channel_difference'] <= BOUNDS['mean_channel_difference_max']
                and metrics['p95_max_channel_difference'] <= BOUNDS['p95_max_channel_difference_max']
                and metrics['pixels_above_10_percent'] <= BOUNDS['pixels_above_10_percent_max'])
            pages.append({'page':index+1, 'size_px':[first.shape[1], first.shape[0]],
                'metrics':metrics, 'passed':passed, 'renders':[{'name':p.name, 'sha256':sha(p)} for p in (pdfium_path, poppler_path)]})
    return pages


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--ordinary', type=Path, required=True)
    parser.add_argument('--print', dest='print_pdf', type=Path, required=True)
    parser.add_argument('--qpdf', type=Path, required=True)
    parser.add_argument('--poppler', type=Path, required=True)
    parser.add_argument('--profile', type=Path, default=Path(__file__).resolve().parents[1] /
                        'crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc')
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    args.output.mkdir()
    records = []
    for path, candidate in [(args.ordinary, False), (args.print_pdf, True)]:
        path = path.resolve(strict=True)
        before = sha(path)
        folder = args.output / ('print' if candidate else 'ordinary')
        folder.mkdir()
        run = subprocess.run([str(args.qpdf.resolve()), '--check', str(path)], capture_output=True, timeout=90, creationflags=FLAGS)
        assert run.returncode == 0, run.stderr.decode(errors='replace')
        result = inspect(path, candidate, args.profile.read_bytes())
        records.append({'file':path.name, 'sha256':before, 'mode':'pdfx4' if candidate else 'ordinary',
                        'structure':result, 'pages':render(path, args.poppler.resolve(), folder)})
        assert sha(path) == before
    passed = all(page['passed'] for record in records for page in record['pages'])
    receipt = {'schema':'pdf-library-feature-gallery-verification/v1', 'files':records,
        'qpdf_sha256':sha(args.qpdf), 'poppler_sha256':sha(args.poppler), 'fixed_bounds':BOUNDS,
        'all_inputs_unchanged':True, 'passed':passed, 'normative_pdfx4_coverage_complete':False}
    (args.output/'results.json').write_text(json.dumps(receipt, indent=2)+'\n', encoding='utf-8')
    print(json.dumps({'files':len(records), 'pages':sum(len(r['pages']) for r in records), 'passed':passed}))
    assert passed, 'Renderer comparison exceeded the declared bounds; inspect the retained results.json and PNGs'


if __name__ == '__main__':
    main()
