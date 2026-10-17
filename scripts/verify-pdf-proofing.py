"""Measure actual controlled PDF previews, independently of their completion claims.

Development verifier for the pinned CMYK diagnostic and retained galleries.
Requires Pillow, pypdf and an explicitly supplied native LittleCMS library and
CMS-enabled pdftoppm. It does not import the historical PDFium/numpy verifier.
"""
import argparse
import ctypes
import hashlib
import json
import math
from pathlib import Path
import subprocess
import sys

import PIL
from PIL import Image, ImageCms
import pypdf
from pypdf import PdfReader, PdfWriter
from pypdf.generic import (ArrayObject, DecodedStreamObject, DictionaryObject,
                          NameObject, NumberObject)

import pdf_proofing

HASHES = {
    'diagnostic': '1124ab1ab80aa179cc1c1b222c46a37afa542ccd41a6180c9a22b11852c8d342',
    'ordinary': 'ac8a687a3f91cde2cf768d0099ff37e1a8ce722448a6b4062c51ef2aada10fb8',
    'print': '5b6a94bf69381718c1eff572305a13ce71b78f608657ab34246d96750206ecab',
    'historical_color': '9130c052af53b9161020945470bfeb3a43a2b1fdbc0620c13461c6c0cb71b2ef'}
BOUNDS = {'mean_channel_difference_max': 5, 'p95_max_channel_difference_max': 20,
          'pixels_above_10_percent_max': 8}
SAMPLES = (.125, .25, .5, .75, .875)
CASES = ('axial', 'radial', 'hard-stop', 'constant-alpha', 'varying-alpha',
         'radial-varying-alpha', 'group-opacity', 'varying-alpha-group-opacity')
FLAGS = 0x2100  # cmsFLAGS_NOOPTIMIZE | cmsFLAGS_BLACKPOINTCOMPENSATION
LIMIT = 4


def require(condition, message):
    if not condition:
        raise ValueError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def identity(path):
    path = Path(path)
    return {'file': path.name, 'sha256': digest(path.read_bytes())}


def run(arguments):
    return subprocess.run(list(map(str, arguments)), capture_output=True, text=True,
        errors='replace', timeout=300, creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))


def renderer_version(renderer):
    result = run([renderer, '-v'])
    require(result.returncode == 0, 'cannot independently obtain pdftoppm version')
    return (result.stdout+result.stderr).strip()


def print_condition(reader, selected):
    embedded = []
    owners = [('catalog', reader.trailer['/Root'])]
    owners.extend((f'page {i}', page) for i, page in enumerate(reader.pages, 1))
    for location, owner in owners:
        intents = owner.get('/OutputIntents')
        for reference in ([] if intents is None else intents.get_object()):
            data = reference.get_object()['/DestOutputProfile'].get_object().get_data()
            require(data[16:20] == b'CMYK', 'non-CMYK OutputIntent in qualification input')
            require(digest(data) == digest(selected), 'OutputIntent does not match selected print bytes')
            embedded.append({'location': location, 'sha256': digest(data)})
    return {'kind': 'embedded-output-intent-match' if embedded else 'selected-default-cmyk',
            'embedded_profiles': embedded}


def audit_preview(bundle, *, pdf, print_profile, display_profile, renderer,
                  renderer_version, pages=None, dpi=144):
    """Reopen source/profile/PNG bytes and check the producer's claims against them.

    The expected pages, DPI and renderer identity come from the verifier's request,
    never from the manifest. Color correctness is measured separately below.
    """
    bundle, pdf, print_profile, display_profile, renderer = map(
        lambda p: Path(p).resolve(), (bundle, pdf, print_profile, display_profile, renderer))
    manifest = json.loads((bundle/'manifest.json').read_text(encoding='utf-8'))
    require(manifest.get('schema_version') == 1 and manifest.get('status') == 'complete',
            'preview has no valid complete status')
    for key, path in [('source_pdf', pdf), ('print_profile', print_profile),
                      ('display_profile', display_profile),
                      ('companion', Path(pdf_proofing.__file__)), ('renderer', renderer)]:
        expected = identity(path)
        require(all(manifest.get(key, {}).get(k) == v for k, v in expected.items()),
                f'{key} identity mismatch')
    require(manifest['renderer'].get('version') == renderer_version, 'renderer version mismatch')
    runtime = manifest.get('runtime', {})
    for key, value in {'python': sys.version, 'executable': identity(Path(sys.executable)),
                       'pillow': PIL.__version__, 'pypdf': pypdf.__version__,
                       'pillow_littlecms': ImageCms.core.littlecms_version}.items():
        require(runtime.get(key) == value, f'runtime {key} identity mismatch')
    require(runtime.get('platform') and runtime.get('machine'), 'missing actual platform identity')
    reader = PdfReader(pdf, strict=True)
    selected = list(range(1, len(reader.pages)+1)) if pages is None else list(pages)
    require(manifest.get('print_condition') == print_condition(reader, print_profile.read_bytes()),
            'print condition claim differs from source PDF')
    conditions = manifest.get('conditions', {})
    for key, expected in {'dpi': dpi, 'pages': selected, 'page_box': 'MediaBox',
                          'output_mode': 'RGB', 'background': 'opaque white paper',
                          'text_antialiasing': True, 'vector_antialiasing': True}.items():
        require(conditions.get(key) == expected, f'wrong preview condition: {key}')
    records = manifest.get('outputs', [])
    require(len(records) == len(selected) and [r.get('page') for r in records] == selected,
            'missing or fabricated page records')
    checked = []
    for number, record in zip(selected, records):
        page = reader.pages[number-1]
        media = list(map(float, page.mediabox))
        crop = list(map(float, page.cropbox))
        rotation = int(page.get('/Rotate', 0))
        unit = float(page.get('/UserUnit', 1))
        for key, expected in [('media_box_points', media), ('crop_box_points', crop),
                              ('rotation_degrees', rotation), ('user_unit', unit)]:
            require(record.get(key) == expected, f'page {number} wrong {key}')
        width, height = media[2]-media[0], media[3]-media[1]
        if rotation % 180:
            width, height = height, width
        dimensions = [math.ceil(width*dpi/72), math.ceil(height*dpi/72)]
        png = bundle/f'page-{number}.png'
        require(record.get('file') == png.name and record.get('sha256') == digest(png.read_bytes()),
                f'page {number} PNG identity mismatch')
        with Image.open(png) as image:
            image.load()
            require(image.format == 'PNG' and image.mode == 'RGB', f'page {number} must be opaque RGB PNG')
            require(image.info.get('icc_profile') == display_profile.read_bytes(),
                    f'page {number} ICC bytes differ from selected display profile')
            require(list(image.size) == dimensions == record.get('pixel_dimensions'),
                    f'page {number} dimensions disagree with independent MediaBox/DPI calculation')
            pixels = digest(image.tobytes())
            require(pixels == record.get('pixel_sha256'), f'page {number} decoded pixel hash mismatch')
        arguments = ['-f', str(number), '-l', str(number), '-singlefile', '-r', str(dpi),
                     '-png', '-aa', 'yes', '-aaVector', 'yes', '-displayprofile', str(display_profile),
                     '-defaultcmykprofile', str(print_profile), str(pdf), str(png.with_suffix(''))]
        require(record.get('renderer_arguments') == arguments, f'page {number} wrong renderer arguments')
        require(record.get('diagnostics') == '', f'page {number} unexpected renderer diagnostics')
        checked.append({'page': number, **identity(png), 'pixel_sha256': pixels,
                        'icc_sha256': digest(display_profile.read_bytes()), 'pixel_dimensions': dimensions})
    return {'manifest': identity(bundle/'manifest.json'), 'pages': selected,
            'source_pdf': identity(pdf), 'print_profile': identity(print_profile),
            'display_profile': identity(display_profile), 'outputs': checked,
            'print_condition': manifest['print_condition'], 'runtime': runtime}


def compare_rgb(actual, expected, label):
    error = max(abs(int(a)-int(b)) for a, b in zip(actual, expected))
    require(error <= LIMIT, f'{label}: maximum channel error {error} exceeds {LIMIT}')
    return {'actual_rgb': list(actual), 'reference_rgb': list(expected), 'maximum_error': error}


def sample(image, x, y, height):
    return image.getpixel((round(x*2), round((height-y)*2)))


def open_rgb(path):
    with Image.open(path) as source:
        source.load()
        return source.copy()


class LittleCMS:
    """Direct native reference using the public lcms2.h ABI and format constants."""
    def __init__(self, library):
        self.dll = ctypes.CDLL(str(library))
        ptr, u32 = ctypes.c_void_p, ctypes.c_uint32
        for name, args, result in [
            ('cmsOpenProfileFromMem', [ptr, u32], ptr), ('cmsCloseProfile', [ptr], ctypes.c_int),
            ('cmsCreateTransform', [ptr, u32, ptr, u32, u32, u32], ptr),
            ('cmsDeleteTransform', [ptr], None), ('cmsDoTransform', [ptr, ptr, ptr, u32], None),
            ('cmsGetEncodedCMMversion', [], u32), ('cmsBuildGamma', [ptr, ctypes.c_double], ptr),
            ('cmsFreeToneCurve', [ptr], None), ('cmsCreateRGBProfile', [ptr, ptr, ptr], ptr),
            ('cmsSaveProfileToMem', [ptr, ptr, ctypes.POINTER(u32)], ctypes.c_int)]:
            fn = getattr(self.dll, name)
            fn.argtypes, fn.restype = args, result

    def reference(self, print_profile, display_profile, components):
        buffers = [ctypes.create_string_buffer(Path(p).read_bytes()) for p in (print_profile, display_profile)]
        handles, transform = [], None
        try:
            for buffer in buffers:
                handle = self.dll.cmsOpenProfileFromMem(buffer, len(buffer)-1)
                require(handle, 'LittleCMS cannot open independent reference profile')
                handles.append(handle)
            # TYPE_CMYK_8, TYPE_RGB_8; Poppler quantizes DeviceCMYK by round(value*255).
            transform = self.dll.cmsCreateTransform(handles[0], (6 << 16)|(4 << 3)|1,
                handles[1], (4 << 16)|(3 << 3)|1, 1, FLAGS)
            require(transform, 'LittleCMS cannot create independent reference transform')
            incoming = (ctypes.c_ubyte*(4*len(components)))(
                *(int(c*255+.5) for row in components for c in row))
            outgoing = (ctypes.c_ubyte*(3*len(components)))()
            self.dll.cmsDoTransform(transform, incoming, outgoing, len(components))
            return [list(outgoing[i:i+3]) for i in range(0, len(outgoing), 3)]
        finally:
            if transform: self.dll.cmsDeleteTransform(transform)
            for handle in handles: self.dll.cmsCloseProfile(handle)

    def alternate_display(self, path):
        class xyY(ctypes.Structure):
            _fields_ = [('x', ctypes.c_double), ('y', ctypes.c_double), ('Y', ctypes.c_double)]
        class Triple(ctypes.Structure):
            _fields_ = [('red', xyY), ('green', xyY), ('blue', xyY)]
        white = xyY(.3127, .3290, 1.)
        primaries = Triple(xyY(.64, .33, 1.), xyY(.30, .60, 1.), xyY(.15, .06, 1.))
        curve = self.dll.cmsBuildGamma(None, 1.8)
        require(curve, 'LittleCMS cannot construct gamma 1.8 display control')
        curves = (ctypes.c_void_p*3)(curve, curve, curve)
        profile = None
        try:
            profile = self.dll.cmsCreateRGBProfile(ctypes.byref(white), ctypes.byref(primaries), curves)
            require(profile, 'LittleCMS cannot construct independent display profile')
            size = ctypes.c_uint32()
            require(self.dll.cmsSaveProfileToMem(profile, None, ctypes.byref(size)), 'profile size query failed')
            buffer = ctypes.create_string_buffer(size.value)
            require(self.dll.cmsSaveProfileToMem(profile, buffer, ctypes.byref(size)), 'profile serialization failed')
            data = bytearray(buffer.raw[:size.value])
            # Stabilize the generated profile's creation date and clear its optional ID.
            data[24:36] = b'\x07\xea\x00\x0a\x00\x11\x00\x00\x00\x00\x00\x00'
            data[84:100] = bytes(16)
            require(data[12:20] == b'mntrRGB ', 'generated control must be display-class RGB')
            path.write_bytes(data)
        finally:
            if profile: self.dll.cmsCloseProfile(profile)
            self.dll.cmsFreeToneCurve(curve)


def reference_pdf(path, profile, rows):
    """Author DeviceCMYK and explicitly ICCBased flat patches without Rust writer code."""
    writer = PdfWriter()
    writer.pdf_header = '%PDF-1.6'
    icc = DecodedStreamObject()
    icc[NameObject('/N')] = NumberObject(4)
    icc.set_data(profile.read_bytes())
    reference = writer._add_object(icc)
    for row in rows:
        page = writer.add_blank_page(width=640, height=160)
        page[NameObject('/Resources')] = DictionaryObject({NameObject('/ColorSpace'): DictionaryObject({
            NameObject('/ICC'): ArrayObject([NameObject('/ICCBased'), reference])})})
        commands = ['/RelativeColorimetric ri']
        for j, color in enumerate(row):
            numbers = ' '.join(f'{c:.9f}' for c in color)
            commands += [f'{numbers} k {j*60+20} 100 40 40 re f',
                         f'/ICC cs {numbers} scn {j*60+20} 55 40 40 re f']
        stream = DecodedStreamObject()
        stream.set_data(('\n'.join(commands)+'\n').encode('ascii'))
        page[NameObject('/Contents')] = writer._add_object(stream)
    with path.open('xb') as handle:
        writer.write(handle)


def gallery_components(t, radial):
    cyan, magenta, yellow = (1., 0., 0., 0.), (0., 1., 0., 0.), (0., 0., 1., 0.)
    colors, split = ([yellow, magenta, cyan], .45) if radial else ([cyan, magenta, yellow], .35)
    a, b, fraction = (colors[0], colors[1], t/split) if t < split else (
        colors[1], colors[2], (t-split)/(1-split))
    return [x+(y-x)*fraction for x, y in zip(a, b)]


def diagnostic_controls(bundle, pdf):
    reader = PdfReader(pdf, strict=True)
    require(len(reader.pages) == 2, 'diagnostic must contain two pages')
    records = []
    for number in (1, 2):
        space = '/DeviceCMYK' if number == 1 else '/DeviceRGB'
        require(reader.pages[number-1]['/Group']['/CS'] == space, 'diagnostic page blending space changed')
        image = open_rgb(bundle/f'page-{number}.png')
        for case, label in enumerate(CASES):
            y = 864-case*112+24
            for j, t in enumerate(SAMPLES):
                if case == 2 and t == .5:
                    continue  # Avoid the single raster pixel straddling a hard transition.
                result = compare_rgb(sample(image, 32+256*t, y, 960),
                    sample(image, 352+j*52+24, y, 960), f'diagnostic {number}/{label}/{t}')
                records.append({'page': number, 'page_blending_space': space, 'case': label, 't': t, **result})
    return records


def historical_control(path):
    require(identity(path)['sha256'] == HASHES['historical_color'], 'historical color receipt bytes changed')
    old = json.loads(path.read_text(encoding='utf-8'))['baseline']
    require(old['default_comparison_passed'] is False and old['fixed_bounds'] == BOUNDS,
            'historical failed verdict or 5/20/8 bounds changed')
    failed = {(r['mode'], r['page']) for r in old['failed_pages']}
    require(failed == {('ordinary', 2), ('pdfx4', 1), ('pdfx4', 2)}, 'historical failing page set changed')
    return {'receipt': identity(path), 'default_comparison_passed': False, 'fixed_bounds': BOUNDS,
            'failed_pages': old['failed_pages'], 'default_metrics': old['default_metrics'],
            'preservation': 'exact original receipt bytes retained; default comparison was not rerun'}


def qualify(args):
    for key in ('renderer', 'lcms', 'print_profile', 'display_profile', 'diagnostic',
                'ordinary', 'print', 'historical_color', 'output'):
        setattr(args, key, getattr(args, key).resolve())
    require(not args.output.exists(), 'qualification output must be a fresh directory')
    if args.receipt:
        args.receipt = args.receipt.resolve()
        require(not args.receipt.exists(), 'receipt path must be fresh')
    for mode in ('diagnostic', 'ordinary', 'print'):
        require(identity(getattr(args, mode))['sha256'] == HASHES[mode], f'{mode} source PDF identity changed')
    historical = historical_control(args.historical_color)
    args.output.mkdir()
    version = renderer_version(args.renderer)
    cms = LittleCMS(args.lcms)
    previews = {}

    def render(name, pdf, display=None, pages=None):
        display = display or args.display_profile
        bundle = args.output/name
        pdf_proofing.render_preview(pdf, print_profile=args.print_profile,
            display_profile=display, renderer=args.renderer, output=bundle, pages=pages)
        previews[name] = audit_preview(bundle, pdf=pdf, print_profile=args.print_profile,
            display_profile=display, renderer=args.renderer, renderer_version=version, pages=pages)
        return bundle

    diagnostic = render('diagnostic', args.diagnostic)
    ordinary = render('ordinary', args.ordinary)
    printed = render('print', args.print)
    require(previews['ordinary']['pages'] == [1, 2, 3] and previews['print']['pages'] == [1, 2, 3],
            'retained galleries must each have all three pages')
    diagnostics = diagnostic_controls(diagnostic, args.diagnostic)
    colors = [[t, 1-t, t, t*.25] for t in SAMPLES]
    colors += [[1., 0., 0., 0.], [0., 1., 0., 0.], [0., 0., 1., 0.],
               [0., 0., 0., .5], [.2, .4, .6, .1]]
    gallery_row = [gallery_components(t, radial) for radial in (False, True) for t in SAMPLES]
    reference = args.output/'independent-flat-controls.pdf'
    reference_pdf(reference, args.print_profile, [colors, gallery_row])
    reference_bundle = render('flat-controls', reference)
    image = open_rgb(reference_bundle/'page-1.png')
    expected = cms.reference(args.print_profile, args.display_profile, colors)
    flat_controls = []
    for j, (components, rgb) in enumerate(zip(colors, expected)):
        for kind, y in [('default-cmyk', 120), ('icc-based-cmyk', 75)]:
            flat_controls.append({'sample': j, 'color_space': kind, 'components': components,
                **compare_rgb(sample(image, j*60+40, y, 160), rgb, f'{kind} sample {j}')})
    gallery_controls = []
    flat_image = open_rgb(reference_bundle/'page-2.png')
    for mode, page, bundle in [('ordinary', 2, ordinary), ('print', 1, printed), ('print', 2, printed)]:
        actual = open_rgb(bundle/f'page-{page}.png')
        source = PdfReader(getattr(args, mode), strict=True).pages[page-1]
        require(source['/Group']['/CS'] == '/DeviceCMYK', 'gallery page blending space changed')
        for kind in (0, 1):
            for j, t in enumerate(SAMPLES):
                y = (350 if mode == 'print' and page == 1 else 315)+55
                x = 455+80*t if kind else 55+200*t
                gallery_controls.append({'mode': mode, 'page': page,
                    'geometry': 'radial' if kind else 'axial', 't': t,
                    **compare_rgb(sample(actual, x, y, 650), sample(flat_image, (kind*5+j)*60+40, 120, 160),
                                  f'gallery {mode}/{page}/{kind}/{t}')})
    alternate = args.output/'display-gamma18.icc'
    cms.alternate_display(alternate)
    alternate_bundle = render('alternate-display', reference, alternate, pages=[1])
    alternate_image = open_rgb(alternate_bundle/'page-1.png')
    alternate_expected = cms.reference(args.print_profile, alternate, colors)
    display_controls = []
    for j, rgb in enumerate(alternate_expected):
        actual = sample(alternate_image, j*60+40, 120, 160)
        original = sample(image, j*60+40, 120, 160)
        predicted = max(abs(a-b) for a, b in zip(expected[j], rgb))
        display_controls.append({'sample': j, 'original_rgb': list(original),
            'predicted_profile_difference': predicted,
            'observed_profile_difference': max(abs(a-b) for a, b in zip(original, actual)),
            **compare_rgb(actual, rgb, f'alternate display {j}')})
    require(any(r['predicted_profile_difference'] > 8 and r['observed_profile_difference'] > 4
                for r in display_controls), 'valid display profile did not affect rendered pixels as predicted')
    # Omit DefaultCMYK only in this independent contrast: it is not a companion output.
    unmanaged_prefix = args.output/'without-default-cmyk'
    unmanaged = run([args.renderer, '-f', '1', '-l', '1', '-singlefile', '-r', '144', '-png',
                     '-displayprofile', args.display_profile, reference, unmanaged_prefix])
    require(unmanaged.returncode == 0 and not (unmanaged.stdout+unmanaged.stderr).strip(),
            'independent unmanaged contrast render failed')
    unmanaged_image = open_rgb(unmanaged_prefix.with_suffix('.png'))
    default_controls = []
    for j, rgb in enumerate(expected):
        actual = sample(image, j*60+40, 120, 160)
        uncontrolled = sample(unmanaged_image, j*60+40, 120, 160)
        default_controls.append({'sample': j, 'managed_rgb': list(actual),
            'without_default_cmyk_rgb': list(uncontrolled), 'littlecms_rgb': rgb,
            'maximum_contrast': max(abs(a-b) for a, b in zip(actual, uncontrolled))})
    require(any(r['maximum_contrast'] > 4 for r in default_controls),
            'explicit DefaultCMYK profile has no measurable effect versus the unmanaged contrast')
    wrong = args.output/'different-valid-print.icc'
    data = bytearray(args.print_profile.read_bytes())
    data[24:26] = (2001 if int.from_bytes(data[24:26], 'big') != 2001 else 2002).to_bytes(2, 'big')
    data[84:100] = bytes(16)
    wrong.write_bytes(data)
    refusal = None
    try:
        pdf_proofing.render_preview(args.print, print_profile=wrong,
            display_profile=args.display_profile, renderer=args.renderer, output=args.output/'wrong-print')
    except pdf_proofing.ProofingError as error:
        refusal = str(error)
    require(refusal and 'does not match the embedded OutputIntent' in refusal,
            'different valid print profile did not trigger actual OutputIntent mismatch refusal')
    require(not (args.output/'wrong-print/manifest.json').exists(), 'wrong print profile produced a success manifest')
    repeats = []
    for name, pdf in [('diagnostic', args.diagnostic), ('ordinary', args.ordinary),
                      ('print', args.print), ('flat-controls', reference)]:
        repeated = render(name+'-repeat', pdf)
        for first, second in zip(previews[name]['outputs'], previews[name+'-repeat']['outputs']):
            a = open_rgb(args.output/name/first['file'])
            b = open_rgb(repeated/second['file'])
            require(a.tobytes() == b.tobytes() and a.info.get('icc_profile') == b.info.get('icc_profile'),
                    f'{name} page {first["page"]} repeat pixels or ICC bytes differ')
            repeats.append({'preview': name, 'page': first['page'], 'pixel_sha256': first['pixel_sha256'],
                            'icc_sha256': first['icc_sha256'], 'decoded_pixels_equal': True, 'exact_icc_equal': True,
                            'encoded_sha256_first': first['sha256'], 'encoded_sha256_repeat': second['sha256']})
    require(identity(args.historical_color)['sha256'] == historical['receipt']['sha256'], 'historical receipt changed during run')
    for mode in ('diagnostic', 'ordinary', 'print'):
        require(identity(getattr(args, mode))['sha256'] == HASHES[mode], f'{mode} source changed during run')
    result = {'schema': 'patterboxpdf-controlled-proofing-verification/v1', 'passed': True,
        'verifier': identity(Path(__file__)), 'companion': identity(Path(pdf_proofing.__file__)),
        'renderer': {**identity(args.renderer), 'version': version},
        'littlecms': {**identity(args.lcms), 'encoded_version': int(cms.dll.cmsGetEncodedCMMversion())},
        'reference_conditions': {'intent': 1, 'flags': FLAGS, 'encoding': 'round-u8', 'maximum_channel_error': LIMIT},
        'previews': previews, 'diagnostic_gradient_flat_controls': diagnostics,
        'independent_littlecms_flat_controls': flat_controls, 'retained_gallery_gradient_flat_controls': gallery_controls,
        'alternate_display': {'profile': identity(alternate), 'samples': display_controls},
        'explicit_default_cmyk_contrast': {'unmanaged_png': identity(unmanaged_prefix.with_suffix('.png')), 'samples': default_controls},
        'wrong_print_output_intent': {'profile': identity(wrong), 'refusal': refusal, 'success_manifest_exists': False},
        'repeatability': repeats, 'historical_reader_comparison': historical,
        'scope': 'Exact pinned diagnostic and galleries under the recorded native CMS conditions; no unrestricted conformance or printer claim.'}
    (args.output/'results.json').write_text(json.dumps(result, indent=2)+'\n', encoding='utf-8')
    if args.receipt:
        args.receipt.write_text(json.dumps(result, indent=2)+'\n', encoding='utf-8')
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    for name in ('renderer', 'lcms', 'print-profile', 'display-profile', 'diagnostic',
                 'ordinary', 'print', 'historical-color', 'output'):
        parser.add_argument('--'+name, type=Path, required=True)
    parser.add_argument('--receipt', type=Path)
    args = parser.parse_args()
    try:
        result = qualify(args)
    except (ValueError, OSError, KeyError, subprocess.SubprocessError) as error:
        parser.exit(1, f'Proof qualification failed: {error}\n')
    print(json.dumps({'passed': result['passed'], 'output': str(args.output),
                      'preview_runs': len(result['previews']),
                      'diagnostic_samples': len(result['diagnostic_gradient_flat_controls']),
                      'littlecms_samples': len(result['independent_littlecms_flat_controls']),
                      'gallery_samples': len(result['retained_gallery_gradient_flat_controls'])}))


if __name__ == '__main__':
    main()
