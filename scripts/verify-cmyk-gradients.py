"""Verify this project's CMYK diagnostic and retained feature-gallery fixtures.

Checks decoded component/opacity functions independently of the writer, compares
gradients with flat component controls in each reader, and tests explicit ICC
conversion against LittleCMS. Default cross-reader verdicts remain separate.
This development tool does not validate arbitrary PDFs or alter library output.
"""
import argparse
import bisect
import ctypes
import hashlib
import json
import math
from pathlib import Path
import subprocess

import numpy as np
from PIL import Image
import pypdf
import pypdfium2 as pdfium
from pypdf import PdfReader, PdfWriter
from pypdf.generic import (ArrayObject, BooleanObject, ContentStream, DecodedStreamObject,
                          DictionaryObject, FloatObject, NameObject, NumberObject)

C0 = np.array([0., 1., 0., 0.])
C1 = np.array([1., 0., 1., .25])
SAMPLES = [.125, .25, .5, .75, .875]
FUNCTION_SAMPLES = [0., .125, .25, .499, .5, .501, .75, .875, 1.]
CASES = ['axial', 'radial', 'hard-stop', 'constant-alpha', 'varying-alpha',
         'radial-varying-alpha', 'group-opacity', 'varying-alpha-group-opacity']
BASELINE_HASHES = {'ordinary': 'ac8a687a3f91cde2cf768d0099ff37e1a8ce722448a6b4062c51ef2aada10fb8',
                   'pdfx4': '5b6a94bf69381718c1eff572305a13ce71b78f608657ab34246d96750206ecab'}
BOUNDS = {'mean_channel_difference_max': 5, 'p95_max_channel_difference_max': 20,
          'pixels_above_10_percent_max': 8}
FLAGS = getattr(subprocess, 'CREATE_NO_WINDOW', 0)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha(path):
    return hashlib.sha256(Path(path).read_bytes()).hexdigest()


def identity(path):
    return {'file': Path(path).name, 'sha256': sha(path)}


def evaluate_function(reference, t, depth=0):
    """Evaluate the emitted Type 2/3 subset from PDF bytes, with bounded recursion."""
    require(depth < 12 and math.isfinite(t) and 0 <= t <= 1, 'invalid function recursion/input')
    value = reference.get_object()
    require(list(value['/Domain']) == [0, 1], 'unexpected function domain')
    kind = int(value['/FunctionType'])
    if kind == 2:
        require(float(value['/N']) == 1., 'unexpected interpolation exponent')
        a, b = np.array(value['/C0'], dtype=float), np.array(value['/C1'], dtype=float)
        require(a.shape == b.shape and a.ndim == 1 and len(a) in (1, 3, 4), 'invalid function components')
        require(np.isfinite(a).all() and np.isfinite(b).all() and
                min(a.min(), b.min()) >= 0 and max(a.max(), b.max()) <= 1,
                'invalid component endpoints')
        return a + (b-a) * t
    require(kind == 3, 'unexpected function type')
    bounds = [0., *map(float, value['/Bounds']), 1.]
    functions, encoded = value['/Functions'], list(map(float, value['/Encode']))
    require(len(functions) == len(bounds)-1 and len(encoded) == 2*len(functions), 'invalid stitching arity')
    require(all(a < b for a, b in zip(bounds, bounds[1:])) and
            all(math.isfinite(v) for v in bounds + encoded), 'invalid stitching bounds')
    index = min(bisect.bisect_right(bounds, t)-1, len(functions)-1)
    require(index >= 0, 'function input outside domain')
    low, high = encoded[2*index:2*index+2]
    mapped = low + (high-low) * (t-bounds[index]) / (bounds[index+1]-bounds[index])
    require(0 <= mapped <= 1, 'stitching maps outside child domain')
    return evaluate_function(functions[index], mapped, depth+1)


def expected_sample(case, t):
    fraction = (0. if t < .5 else 1.) if case == 2 else t
    alpha = .5 if case == 3 else (t if case in (4, 5, 7) else 1.)
    return C0+(C1-C0)*fraction, alpha*(.5 if case >= 6 else 1.)


def painted_shadings(reader, page):
    """Walk executed Forms and graphics states; do not count unused resources."""
    records = []
    def visit(stream, resources, placement=1., initial=None, depth=0):
        require(depth < 12, 'cyclic/deep Form graph')
        state = dict(initial or {'ca': 1., 'mask': None})
        stack = []
        for args, op in ContentStream(stream, reader).operations:
            if op == b'q':
                stack.append(dict(state))
            elif op == b'Q':
                require(bool(stack), 'unbalanced graphics restore')
                state = stack.pop()
            elif op == b'gs':
                graphics = resources['/ExtGState'][args[0]].get_object()
                if '/ca' in graphics:
                    state['ca'] = float(graphics['/ca'])
                if '/SMask' in graphics:
                    state['mask'] = graphics['/SMask'].get_object()
            elif op == b'sh':
                shading = resources['/Shading'][args[0]].get_object()
                if shading['/ColorSpace'] == '/DeviceCMYK':
                    records.append((shading, state.copy(), placement))
            elif op == b'Do':
                form = resources['/XObject'][args[0]].get_object()
                if form.get('/Subtype') == '/Form':
                    group = form.get('/Group')
                    if group:
                        require(group['/S'] == '/Transparency', 'unexpected Form group')
                        visit(form, form.get('/Resources', resources), placement*state['ca'], depth=depth+1)
                    else:
                        visit(form, form.get('/Resources', resources), placement, state, depth+1)
        require(not stack, 'unbalanced graphics save')
    visit(page.get_contents(), page['/Resources'])
    return records


def opacity(state, placement, t):
    alpha = state['ca']*placement
    mask = state['mask']
    if mask is None or mask == '/None':
        return alpha
    require(mask['/S'] == '/Luminosity' and list(mask['/BC']) == [0], 'unexpected opacity mask')
    group = mask['/G'].get_object()
    require(group['/Group']['/CS'] == '/DeviceGray', 'opacity mask must use gray')
    shadings = group['/Resources']['/Shading']
    require(len(shadings) == 1, 'unexpected opacity shading count')
    shading = next(iter(shadings.values())).get_object()
    require(shading['/ColorSpace'] == '/DeviceGray', 'opacity function must use gray')
    result = evaluate_function(shading['/Function'], t)
    require(len(result) == 1, 'opacity function arity')
    return alpha*float(result[0])


def audit_diagnostic(path):
    reader = PdfReader(path, strict=True)
    require(len(reader.pages) == 2, 'expected two diagnostic pages')
    checks = 0
    for index, page in enumerate(reader.pages):
        require(list(page.mediabox) == [0, 0, 640, 960], 'unexpected diagnostic dimensions')
        expected_space = '/DeviceRGB' if index else '/DeviceCMYK'
        require(page['/Group']['/CS'] == expected_space, 'page blending control changed')
        records = painted_shadings(reader, page)
        require(len(records) == len(CASES), 'missing diagnostic gradient')
        for case, (shading, state, placement) in enumerate(records):
            expected_kind = 3 if case in (1, 5) else 2
            require(shading['/ShadingType'] == expected_kind, 'gradient geometry family changed')
            coords = [0, 24, 0, 0, 24, 256] if expected_kind == 3 else [0, 0, 256, 0]
            require(list(shading['/Coords']) == coords, 'gradient geometry changed')
            require(all(isinstance(x, BooleanObject) and x.value for x in shading['/Extend']), 'gradient padding changed')
            for t in FUNCTION_SAMPLES:
                expected, alpha = expected_sample(case, t)
                actual = evaluate_function(shading['/Function'], t)
                require(actual.shape == (4,) and np.allclose(actual, expected, rtol=0, atol=1e-6),
                        f'wrong color function: {case}/{t}')
                require(abs(opacity(state, placement, t)-alpha) < 1e-6, f'wrong alpha function: {case}/{t}')
                checks += 2
    return {'decoded_component_and_opacity_checks': checks, 'pages': 2, 'cases_per_page': CASES}


def render(path, poppler, output):
    output.mkdir()
    records = []
    with pdfium.PdfDocument(path) as document:
        for index in range(len(document)):
            page = document[index]
            bitmap = page.render(scale=2)
            im = bitmap.to_pil().convert('RGB')
            first = output/f'pdfium-{index+1}.png'
            im.save(first)
            bitmap.close()
            page.close()
            prefix = output/f'poppler-{index+1}'
            subprocess.run([str(poppler), '-f', str(index+1), '-l', str(index+1), '-singlefile',
                            '-scale-to-x', str(im.width), '-scale-to-y', str(im.height),
                            '-png', str(path), str(prefix)], capture_output=True, check=True,
                           timeout=90, creationflags=FLAGS)
            records.append([identity(first), identity(prefix.with_suffix('.png'))])
    return records


def pixels(path):
    with Image.open(path) as im:
        return np.asarray(im.convert('RGB')).astype(int)


def pixel(image, x, y, height):
    return image[round((height-y)*2), round(x*2)]


def compare_flat_controls(output):
    records = []
    for engine in ('pdfium', 'poppler'):
        for page in (1, 2):
            image = pixels(output/f'{engine}-{page}.png')
            for case, label in enumerate(CASES):
                y = 864-case*112+24
                for j, t in enumerate(SAMPLES):
                    a = pixel(image, 32+256*t, y, 960)
                    b = pixel(image, 352+j*52+24, y, 960)
                    error = int(np.abs(a-b).max())
                    # The hard transition itself is compared on both sides by
                    # decoded function checks; avoid a raster pixel straddling it.
                    if case == 2 and t == .5:
                        continue
                    require(error <= 4, f'{engine} {page} {label}/{t}: gradient differs from flat control by {error}')
                    records.append({'engine': engine, 'page': page, 'case': label, 't': t,
                                    'gradient_rgb': a.tolist(), 'flat_rgb': b.tolist(), 'maximum_error': error})
    return records


def n(value):
    return NameObject('/'+value)


def array(values):
    return ArrayObject([FloatObject(float(x)) for x in values])


def dictionary(**values):
    return DictionaryObject({n(k): v for k, v in values.items()})


def profile_channels(data):
    require(len(data) >= 132 and data[36:40] == b'acsp' and data[16:20] == b'CMYK',
            'reference profile must declare four CMYK channels')
    require(int.from_bytes(data[:4], 'big') == len(data), 'reference profile length mismatch')
    return 4


def lcms_reference(library, profile, components, intent=1, flags=0x100, encoding='double'):
    data = profile.read_bytes()
    profile_channels(data)
    dll = ctypes.CDLL(str(library))
    ptr = ctypes.c_void_p
    bindings = [('cmsOpenProfileFromMem', [ptr, ctypes.c_uint32], ptr),
                ('cmsCreate_sRGBProfile', [], ptr), ('cmsCloseProfile', [ptr], ctypes.c_int),
                ('cmsCreateTransform', [ptr, ctypes.c_uint32, ptr, ctypes.c_uint32, ctypes.c_uint32, ctypes.c_uint32], ptr),
                ('cmsDeleteTransform', [ptr], None),
                ('cmsDoTransform', [ptr, ptr, ptr, ctypes.c_uint32], None),
                ('cmsGetEncodedCMMversion', [], ctypes.c_uint32)]
    for name, argtypes, restype in bindings:
        fn = getattr(dll, name)
        fn.argtypes, fn.restype = argtypes, restype
    buffer = ctypes.create_string_buffer(data)
    source, target, transform = None, None, None
    try:
        source = dll.cmsOpenProfileFromMem(buffer, len(data))
        target = dll.cmsCreate_sRGBProfile()
        require(source and target, 'LittleCMS could not open reference profiles')
        # Public LittleCMS lcms2.h format macros. The readers use different
        # quantization, intent and compensation policies; bind each explicitly.
        source_format = ((1 << 22) | (6 << 16) | (4 << 3)) if encoding == 'double' else ((4 << 3) | 1)
        if encoding == 'round-u8':
            source_format |= 6 << 16
        transform = dll.cmsCreateTransform(source, source_format, target, (4 << 16) | (3 << 3) | 1, intent, flags)
        require(transform, 'LittleCMS could not create reference transform')
        if encoding == 'double':
            incoming = (ctypes.c_double*(4*len(components)))(*(100*x for row in components for x in row))
        else:
            require(encoding in ('truncate-u8', 'round-u8'), 'unknown reference encoding')
            offset = .5 if encoding == 'round-u8' else 0.
            incoming = (ctypes.c_ubyte*(4*len(components)))(*(int(x*255+offset) for row in components for x in row))
        outgoing = (ctypes.c_ubyte*(3*len(components)))()
        dll.cmsDoTransform(transform, incoming, outgoing, len(components))
        return np.asarray(outgoing).reshape(-1, 3).astype(int), int(dll.cmsGetEncodedCMMversion())
    finally:
        if transform:
            dll.cmsDeleteTransform(transform)
        if target:
            dll.cmsCloseProfile(target)
        if source:
            dll.cmsCloseProfile(source)


def reference_pdf(path, profile, rows, rgb_rows=None):
    """Author flat reference colors without the library's writer/paint code."""
    writer = PdfWriter()
    writer.pdf_header = '%PDF-1.6'
    icc = DecodedStreamObject()
    icc.update(dictionary(N=NumberObject(4)))
    icc.set_data(profile.read_bytes())
    icc_ref = writer._add_object(icc)
    for page_index, row in enumerate(rows):
        page = writer.add_blank_page(width=640, height=160)
        page[n('Resources')] = dictionary(ColorSpace=dictionary(ICC=ArrayObject([n('ICCBased'), icc_ref])))
        commands = ['/RelativeColorimetric ri']
        for j, color in enumerate(row):
            components = ' '.join(f'{float(x):.9f}' for x in color)
            commands.extend([f'{components} k {j*60+20} 100 40 40 re f',
                             f'/ICC cs {components} scn {j*60+20} 55 40 40 re f'])
            if rgb_rows is not None:
                rgb = ' '.join(str(int(x)/255) for x in rgb_rows[page_index][j])
                commands.append(f'{rgb} rg {j*60+20} 10 40 40 re f')
        stream = DecodedStreamObject()
        stream.set_data(('\n'.join(commands)+'\n').encode())
        page[n('Contents')] = writer._add_object(stream)
    with path.open('xb') as f:
        writer.write(f)


def profile_controls(profile, library, poppler, output):
    components = [expected_sample(0, t)[0].tolist() for t in SAMPLES]
    expected, version = lcms_reference(library, profile, components)
    path = output/'profile-controls.pdf'
    reference_pdf(path, profile, [components], [expected])
    renders = output/'profile-renders'
    render_ids = render(path, poppler, renders)
    require(PdfReader(path).pages[0]['/Resources']['/ColorSpace']['/ICC'][1].get_object().get_data() == profile.read_bytes(),
            'controlled embedded profile identity mismatch')
    records = []
    conditions = {'pdfium': {'intent': 0, 'flags': 0, 'encoding': 'truncate-u8'},
                  'poppler': {'intent': 1, 'flags': 0x100 | 0x2000, 'encoding': 'round-u8'}}
    for engine in ('pdfium', 'poppler'):
        im = pixels(renders/f'{engine}-1.png')
        matched, _ = lcms_reference(library, profile, components, **conditions[engine])
        for j, t in enumerate(SAMPLES):
            rgb = pixel(im, j*60+40, 75, 160)
            reference = pixel(im, j*60+40, 30, 160)
            require(np.abs(reference-expected[j]).max() <= 1, 'RGB reference rendering changed')
            error = int(np.abs(rgb-matched[j]).max())
            require(error <= 4, f'{engine}: explicit ICC conversion differs from LittleCMS by {error}')
            records.append({'engine': engine, 't': t, 'components': components[j], 'icc_rgb': rgb.tolist(),
                            'matched_littlecms_rgb': matched[j].tolist(), 'maximum_error': error,
                            'full_precision_relative_rgb': expected[j].tolist(),
                            'default_device_rgb': pixel(im, j*60+40, 120, 160).tolist()})
    return {'reference_pdf': identity(path), 'renders': render_ids, 'littlecms_version_encoded': version,
            'pdf_rendering_intent': 'relative-colorimetric', 'matched_reader_conditions': conditions,
            'full_precision_reference': {'intent': 1, 'flags': 0x100, 'encoding': 'double'},
            'source_profile': identity(profile), 'littlecms_library': identity(library), 'samples': records}


def mutation_controls(path, output):
    writer = PdfWriter(clone_from=path)
    first = painted_shadings(writer, writer.pages[0])[0][0]['/Function'].get_object()
    while first['/FunctionType'] == 3:
        first = first['/Functions'][0].get_object()
    first[n('C0')] = array([.2, .2, .2, .2])
    changed = output/'wrong-gradient.pdf'
    with changed.open('xb') as f:
        writer.write(f)
    try:
        audit_diagnostic(changed)
    except ValueError as error:
        require('wrong color function' in str(error), 'negative control failed for an unrelated reason')
    else:
        raise ValueError('wrong gradient escaped the diagnostic')
    return {'wrong_gradient_rejected': True, 'mutated_pdf': identity(changed)}


def gallery_components(t, radial):
    cyan, magenta, yellow = np.eye(4)[:3]
    colors, split = ([yellow, magenta, cyan], .45) if radial else ([cyan, magenta, yellow], .35)
    if t < split:
        return colors[0]+(colors[1]-colors[0])*t/split
    return colors[1]+(colors[2]-colors[1])*(t-split)/(1-split)


def audit_default_comparison(results):
    """A controlled experiment cannot overwrite or relax the default verdict."""
    require(results['fixed_bounds'] == BOUNDS, 'default comparison bounds changed')
    files = results['files']
    require(len(files) == 2 and {r['mode'] for r in files} == set(BASELINE_HASHES), 'default file set changed')
    observed = set()
    for row in files:
        require(row['sha256'] == BASELINE_HASHES[row['mode']], 'default comparison input changed')
        require([p['page'] for p in row['pages']] == [1, 2, 3], 'default page set changed')
        for page in row['pages']:
            metrics = page['metrics']
            values = [metrics['mean_channel_difference'], metrics['p95_max_channel_difference'],
                      metrics['pixels_above_10_percent']]
            require(all(math.isfinite(v) and v >= 0 for v in values), 'invalid default metrics')
            passed = all(v <= maximum for v, maximum in zip(values, (5, 20, 8)))
            require(type(page['passed']) is bool and page['passed'] == passed, 'default page verdict disagrees with metrics')
            if not passed:
                observed.add((row['mode'], page['page']))
    require(observed == {('ordinary', 2), ('pdfx4', 1), ('pdfx4', 2)}, 'original failing pages changed')
    require(results['passed'] is False and results['all_inputs_unchanged'] is True, 'default comparison verdict changed')


def baseline_controls(args, output):
    files = {'ordinary': args.baseline_ordinary, 'pdfx4': args.baseline_print}
    for mode, path in files.items():
        require(sha(path) == BASELINE_HASHES[mode], 'retained gallery bytes changed: '+mode)
    gallery = output/'gallery-default'
    helper = Path(__file__).with_name('verify-feature-gallery.py')
    import sys
    run = subprocess.run([sys.executable, str(helper), '--ordinary', str(files['ordinary']),
                          '--print', str(files['pdfx4']), '--qpdf', str(args.qpdf),
                          '--poppler', str(args.poppler), '--profile', str(args.profile),
                          '--output', str(gallery)], capture_output=True, timeout=600, creationflags=FLAGS)
    require(run.returncode == 1 and b'Renderer comparison exceeded the declared bounds' in run.stderr,
            'original comparison did not reproduce its expected documented failure')
    results = json.loads((gallery/'results.json').read_text(encoding='utf-8'))
    audit_default_comparison(results)
    appearance = output/'gallery-appearance.json'
    subprocess.run([sys.executable, str(helper.with_name('verify-feature-gallery-appearance.py')),
                    '--renders', str(gallery), '--output', str(appearance)],
                   capture_output=True, check=True, timeout=90, creationflags=FLAGS)
    authored = json.loads(appearance.read_text(encoding='utf-8'))
    require(authored['all_scenario_probes_passed'] and
            sum(len(r['probes']) for r in authored['scenario_probes']) == 50,
            'existing authored appearance controls changed')
    cases = [('ordinary', 2), ('pdfx4', 1), ('pdfx4', 2)]
    rows, functions = [], []
    for mode, page_index in cases:
        reader = PdfReader(files[mode], strict=True)
        page = reader.pages[page_index-1]
        require(page['/Group']['/CS'] == '/DeviceCMYK', 'retained page blending space changed')
        shadings = painted_shadings(reader, page)[:2]
        require(len(shadings) == 2, 'missing baseline CMYK gradients')
        samples = []
        for kind, (shading, state, placement) in enumerate(shadings):
            require(shading['/ShadingType'] == (3 if kind else 2), 'baseline gradient family changed')
            for t in FUNCTION_SAMPLES:
                expected = gallery_components(t, bool(kind))
                require(np.allclose(evaluate_function(shading['/Function'], t), expected, rtol=0, atol=1e-6),
                        'baseline component function differs from authored stops')
                require(abs(opacity(state, placement, t)-1.) < 1e-6, 'baseline opaque control changed')
            samples.extend(gallery_components(t, bool(kind)).tolist() for t in SAMPLES)
        rows.append(samples)
        # The pinned PDFium shading implementation uses a 256-entry color
        # table: the selected bucket is floor(t*255), evaluated at bucket/256.
        # Record this reader sampling policy separately from the PDF function.
        rows.append([gallery_components(math.floor(t*255)/256, bool(kind)).tolist()
                     for kind in range(2) for t in SAMPLES])
        functions.append({'mode': mode, 'page': page_index, 'correct_component_and_alpha_samples': 36})
    reference = output/'baseline-flat-controls.pdf'
    reference_pdf(reference, args.profile, rows)
    flat_renders = output/'baseline-flat-renders'
    render_ids = render(reference, args.poppler, flat_renders)
    samples = []
    for case, (mode, page_index) in enumerate(cases):
        folder = gallery/('ordinary' if mode == 'ordinary' else 'print')
        by_engine = {}
        for engine in ('pdfium', 'poppler'):
            actual = pixels(folder/f'{engine}-{page_index}.png')
            control_page = case*2+(2 if engine == 'pdfium' else 1)
            controls = pixels(flat_renders/f'{engine}-{control_page}.png')
            values = []
            for kind in range(2):
                for j, t in enumerate(SAMPLES):
                    y = (350 if mode == 'pdfx4' and page_index == 1 else 315)+55
                    x = 455+80*t if kind else 55+200*t
                    a = pixel(actual, x, y, 650)
                    b = pixel(controls, (kind*5+j)*60+40, 120, 160)
                    error = int(np.abs(a-b).max())
                    require(error <= 4, f'original {mode}/{page_index} {engine} gradient/flat difference {error}')
                    samples.append({'mode': mode, 'page': page_index, 'engine': engine,
                                    'geometry': 'radial' if kind else 'axial', 't': t,
                                    'reader_sample_parameter': math.floor(t*255)/256 if engine == 'pdfium' else t,
                                    'gradient_rgb': a.tolist(), 'independent_flat_rgb': b.tolist(), 'maximum_error': error})
                    values.append(a)
            by_engine[engine] = np.array(values)
        require(np.abs(by_engine['pdfium']-by_engine['poppler']).max() > 10,
                'expected reader conversion difference was not observed on original bytes')
    return {'default_comparison_passed': False, 'fixed_bounds': BOUNDS,
            'default_receipt': identity(gallery/'results.json'), 'original_comparison_exit': run.returncode,
            'appearance_receipt': identity(appearance), 'authored_appearance_probes_passed': 50,
            'failed_pages': [{'mode': m, 'page': p} for m, p in cases],
            'default_metrics': [{'mode': r['mode'], 'sha256': r['sha256'],
                                'pages': [{'page': p['page'], 'metrics': p['metrics'], 'passed': p['passed']} for p in r['pages']]}
                               for r in results['files']],
            'component_checks': functions, 'independent_flat_pdf': identity(reference),
            'flat_renders': render_ids, 'samples': samples,
            'explanation': 'On each original failed page, decoded native gradient components match the authored stops. '
                           'Each reader matches separately authored flat colors at those components, while their RGB previews differ. '
                           'Explicit profile controls are checked separately. The default whole-page failures remain failures.'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--diagnostic', type=Path, required=True)
    parser.add_argument('--profile', type=Path, required=True)
    parser.add_argument('--lcms', type=Path, required=True)
    parser.add_argument('--poppler', type=Path, required=True)
    parser.add_argument('--qpdf', type=Path, required=True)
    parser.add_argument('--baseline-ordinary', type=Path, required=True)
    parser.add_argument('--baseline-print', type=Path, required=True)
    parser.add_argument('--output', type=Path, required=True)
    args = parser.parse_args()
    # All inputs resolve before a fresh evidence directory is exposed.
    for field in ('diagnostic', 'profile', 'lcms', 'poppler', 'qpdf', 'baseline_ordinary', 'baseline_print'):
        setattr(args, field, getattr(args, field).resolve(strict=True))
    args.output.mkdir()
    subprocess.run([str(args.qpdf), '--check', str(args.diagnostic)], capture_output=True, check=True, timeout=90, creationflags=FLAGS)
    decoded = audit_diagnostic(args.diagnostic)
    renders = args.output/'diagnostic-renders'
    render_ids = render(args.diagnostic, args.poppler, renders)
    flat = compare_flat_controls(renders)
    profile = profile_controls(args.profile, args.lcms, args.poppler, args.output)
    negative = mutation_controls(args.diagnostic, args.output)
    wrong = bytearray(args.profile.read_bytes())
    wrong[16:20] = b'RGB '
    try:
        profile_channels(wrong)
    except ValueError:
        negative['mislabeled_profile_rejected'] = True
    else:
        raise ValueError('mislabeled profile escaped the diagnostic')
    receipt = {'schema': 'patterboxpdf-cmyk-diagnostic/v1', 'diagnostic': identity(args.diagnostic),
               'verifier': identity(__file__), 'pdfium_version': str(pdfium.PDFIUM_INFO),
               'pypdf_version': pypdf.__version__, 'poppler': identity(args.poppler), 'qpdf': identity(args.qpdf),
               'decoded_checks': decoded, 'diagnostic_renders': render_ids, 'flat_controls': flat,
               'profile_controls': profile, 'negative_controls': negative,
               'native_cmyk_components_checked': True, 'controlled_checks_passed': True}
    # The retained gallery and its original default-render bounds are replayed
    # separately below; a controlled proof cannot replace that verdict.
    receipt['baseline'] = baseline_controls(args, args.output)
    with (args.output/'results.json').open('x', encoding='utf-8', newline='\n') as f:
        json.dump(receipt, f, indent=2)
        f.write('\n')
    print(json.dumps({'controlled_checks_passed': True, 'flat_samples': len(flat),
                      'baseline_default_comparison_passed': receipt['baseline']['default_comparison_passed']}))


if __name__ == '__main__':
    main()
