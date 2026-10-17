"""Optional controlled CMYK-to-display PDF preview companion.

The caller supplies a CMS-enabled Poppler pdftoppm executable and both ICC
profiles. PDF rendering intents, embedded color spaces and blending remain the
renderer's responsibility. This does not simulate arbitrary RGB artwork on a
press. Pillow only tags the resulting RGB pixels; it does not reconvert them.
"""
import hashlib
import io
import json
import math
import os
from pathlib import Path
import platform
import re
import subprocess
import sys

try:
    import PIL
    from PIL import Image, ImageCms
    import pypdf
    from pypdf import PdfReader
    DEPENDENCY_ERROR = None
except ImportError as error:
    DEPENDENCY_ERROR = str(error)


class ProofingError(ValueError):
    """The requested preview could not be completed."""


def _require(condition, message):
    if not condition:
        raise ProofingError(message)


def _digest(data):
    return hashlib.sha256(data).hexdigest()


def _identity(path):
    return {'file': path.name, 'sha256': _digest(path.read_bytes())}


def _profile(data, role):
    expected_class, expected_space = ((b'prtr', b'CMYK') if role == 'print'
                                      else (b'mntr', b'RGB '))
    _require(len(data) >= 132 and int.from_bytes(data[:4], 'big') == len(data)
             and data[36:40] == b'acsp', f'{role} ICC profile has an invalid header or size')
    _require(data[12:16] == expected_class and data[16:20] == expected_space,
             f'{role} profile must be {expected_class.decode()}-class {expected_space.decode().strip()} ICC')
    try:
        return ImageCms.ImageCmsProfile(io.BytesIO(data))
    except Exception as error:
        raise ProofingError(f'{role} ICC profile cannot be opened by LittleCMS: {error}') from error


def _print_condition(reader, selected):
    """Inspect every catalog/page OutputIntent, including unselected pages."""
    embedded = []
    owners = [('catalog', reader.trailer['/Root'])]
    owners.extend((f'page {index}', page) for index, page in enumerate(reader.pages, 1))
    for location, owner in owners:
        intents = owner.get('/OutputIntents')
        for reference in ([] if intents is None else intents.get_object()):
            intent = reference.get_object()
            profile = intent.get('/DestOutputProfile')
            _require(profile is not None,
                     f'{location} OutputIntent has no embedded print profile; its condition cannot be matched')
            data = profile.get_object().get_data()
            _require(len(data) >= 132 and data[16:20] == b'CMYK',
                     f'{location} OutputIntent is not an embedded CMYK print condition')
            embedded.append({'location': location, 'sha256': _digest(data)})
    hashes = {entry['sha256'] for entry in embedded}
    _require(len(hashes) <= 1, 'conflicting embedded OutputIntent print profiles')
    _require(not hashes or hashes == {_digest(selected)},
             'selected print profile does not match the embedded OutputIntent profile')
    return {'kind': 'embedded-output-intent-match' if hashes else 'selected-default-cmyk',
            'embedded_profiles': embedded}


def _run(arguments):
    try:
        return subprocess.run([str(value) for value in arguments], shell=False,
            capture_output=True, text=True, errors='replace',
            creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0), timeout=300)
    except subprocess.TimeoutExpired as error:
        raise ProofingError('pdftoppm did not finish within 300 seconds; partial output is incomplete; '
                            'inspect the input and rerun into a fresh directory') from error
    except OSError as error:
        raise ProofingError(f'cannot launch supplied pdftoppm executable: {error}') from error


def render_preview(pdf, *, print_profile, display_profile, renderer, output,
                   pages=None, dpi=144):
    """Render all pages or a sequence of unique 1-based page numbers.

    ``output`` must not exist. A completed ``manifest.json`` is written last;
    failures raise ProofingError and may leave partial PNGs for diagnosis. DPI
    defaults to 144. All paths are passed literally, without a shell.
    """
    pdf, print_profile, display_profile, renderer, output = map(
        lambda value: Path(value).resolve(), (pdf, print_profile, display_profile, renderer, output))
    _require(not output.exists(), f'output directory already exists: {output}; choose a fresh directory')
    _require(DEPENDENCY_ERROR is None,
             f'optional preview dependencies are missing: {DEPENDENCY_ERROR}; install Pillow and pypdf')
    _require(isinstance(dpi, (int, float)) and not isinstance(dpi, bool)
             and math.isfinite(dpi) and dpi > 0, 'DPI must be a positive finite number')
    for name, path in [('PDF', pdf), ('print profile', print_profile),
                       ('display profile', display_profile), ('renderer', renderer)]:
        _require(path.is_file(), f'{name} file does not exist: {path}')
    try:
        source_bytes = pdf.read_bytes()
        print_bytes, display_bytes = print_profile.read_bytes(), display_profile.read_bytes()
        renderer_identity = _identity(renderer)
        print_icc, display_icc = _profile(print_bytes, 'print'), _profile(display_bytes, 'display')
        try:
            transform = ImageCms.buildTransform(print_icc, display_icc, 'CMYK', 'RGB')
            ImageCms.applyTransform(Image.new('CMYK', (1, 1), (0, 0, 0, 0)), transform)
        except Exception as error:
            raise ProofingError(f'ICC profiles cannot form a usable CMYK-to-RGB LittleCMS transform: {error}') from error
        try:
            reader = PdfReader(io.BytesIO(source_bytes), strict=True)
            _require(not reader.is_encrypted, 'encrypted PDF input is unsupported; supply an unencrypted PDF')
            page_count = len(reader.pages)
            _require(page_count > 0, 'PDF has no pages')
            selected_pages = list(range(1, page_count+1)) if pages is None else list(pages)
            _require(bool(selected_pages) and all(type(page) is int and 1 <= page <= page_count
                     for page in selected_pages) and len(set(selected_pages)) == len(selected_pages),
                     f'page selection must contain unique integers from 1 through {page_count}')
            condition = _print_condition(reader, print_bytes)
        except ProofingError:
            raise
        except Exception as error:
            raise ProofingError(f'cannot read PDF input or its OutputIntents: {error}') from error
        help_result = _run([renderer, '-h'])
        help_text = help_result.stdout + help_result.stderr
        _require('-displayprofile' in help_text and '-defaultcmykprofile' in help_text,
                 'supplied pdftoppm lacks required CMS options -displayprofile and -defaultcmykprofile')
        version_result = _run([renderer, '-v'])
        _require(version_result.returncode == 0, 'cannot obtain supplied pdftoppm version')
        version = (version_result.stdout + version_result.stderr).strip()
        output.mkdir()
        records = []
        for number in selected_pages:
            prefix = output/f'page-{number}'
            command = [str(renderer), '-f', str(number), '-l', str(number), '-singlefile',
                       '-r', str(dpi), '-png', '-aa', 'yes', '-aaVector', 'yes',
                       '-displayprofile', str(display_profile), '-defaultcmykprofile', str(print_profile),
                       str(pdf), str(prefix)]
            result = _run(command)
            diagnostic = (result.stdout + result.stderr).strip()
            _require(result.returncode == 0,
                     f'pdftoppm failed for page {number} (exit {result.returncode}): {diagnostic}')
            _require(not re.search(r'profile|\bICC\b|\bCMS\b|LittleCMS|\berror\b', diagnostic, re.I),
                     f'pdftoppm reported a profile/rendering problem on page {number}: {diagnostic}')
            png = prefix.with_suffix('.png')
            _require(png.is_file(), f'pdftoppm did not produce PNG for page {number}: {png}')
            try:
                with Image.open(png) as image:
                    image.load()
                    # RGB PNGs can contain tRNS transparency or later animation frames.
                    _require(image.format == 'PNG' and image.mode == 'RGB'
                             and image.n_frames == 1 and 'transparency' not in image.info,
                             f'renderer page {number} must be an opaque RGB single-frame PNG')
                    before = image.tobytes()
                    dimensions = list(image.size)
                    image.save(png, format='PNG', icc_profile=display_bytes)
                with Image.open(png) as tagged:
                    tagged.load()
                    _require(tagged.mode == 'RGB' and tagged.n_frames == 1
                             and 'transparency' not in tagged.info
                             and tagged.tobytes() == before
                             and tagged.info.get('icc_profile') == display_bytes,
                             f'ICC tagging changed pixels, opacity, frame count or profile on page {number}')
            except ProofingError:
                raise
            except Exception as error:
                raise ProofingError(f'invalid PNG for page {number}: {error}') from error
            page = reader.pages[number-1]
            records.append({'page': number, **_identity(png),
                'pixel_sha256': _digest(before), 'pixel_dimensions': dimensions,
                'media_box_points': list(map(float, page.mediabox)),
                'crop_box_points': list(map(float, page.cropbox)),
                'rotation_degrees': int(page.get('/Rotate', 0)),
                'user_unit': float(page.get('/UserUnit', 1)),
                'renderer_arguments': command[1:], 'diagnostics': diagnostic})
        # Bind completion to unchanged input files, since the external process
        # reads their paths independently of the metadata checks above.
        for path, data in [(pdf, source_bytes), (print_profile, print_bytes),
                           (display_profile, display_bytes)]:
            _require(path.read_bytes() == data, f'input changed during preview: {path}')
        _require(_identity(renderer) == renderer_identity, 'renderer executable changed during preview')
        manifest = {'schema_version': 1, 'status': 'complete',
            'source_pdf': {'file': pdf.name, 'sha256': _digest(source_bytes)},
            'print_profile': {'file': print_profile.name, 'sha256': _digest(print_bytes)},
            'display_profile': {'file': display_profile.name, 'sha256': _digest(display_bytes)},
            'companion': _identity(Path(__file__).resolve()),
            'renderer': {**renderer_identity, 'version': version},
            'runtime': {'python': sys.version, 'executable': _identity(Path(sys.executable)),
                        'pillow': PIL.__version__, 'pypdf': pypdf.__version__,
                        'pillow_littlecms': ImageCms.core.littlecms_version,
                        # platform.platform()/machine() can spawn cmd.exe on
                        # Windows. Read the OS version without opening a window.
                        'platform': (f'Windows {sys.getwindowsversion()}' if sys.platform == 'win32'
                                     else platform.platform()),
                        'machine': (os.environ.get('PROCESSOR_ARCHITECTURE', 'unknown')
                                    if sys.platform == 'win32' else platform.machine())},
            'print_condition': condition,
            'conditions': {'dpi': dpi, 'pages': selected_pages, 'page_box': 'MediaBox',
                'output_mode': 'RGB', 'background': 'opaque white paper',
                'text_antialiasing': True, 'vector_antialiasing': True,
                'rendering_intent': 'PDF rendering intents honored by Poppler; no CLI override',
                'black_point_compensation': 'renderer build controlled; no CLI override',
                'transform_optimization': 'renderer build controlled; no CLI override',
                'embedded_color_spaces': 'preserved through renderer interpretation',
                'scope': 'controlled CMYK-to-display preview; no universal RGB-to-press simulation'},
            'outputs': records}
        temporary_manifest = output/'manifest.json.tmp'
        temporary_manifest.write_text(json.dumps(manifest, indent=2)+'\n', encoding='utf-8')
        temporary_manifest.replace(output/'manifest.json')
        return manifest
    except ProofingError:
        raise
    except (OSError, ValueError, TypeError) as error:
        raise ProofingError(f'preview could not complete: {error}') from error
