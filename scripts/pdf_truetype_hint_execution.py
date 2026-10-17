"""Development-only execution of exact embedded TrueType programs with FreeType.

Every GID is loaded with native hinting at the declared representative sizes.
This exercises reachable fpgm, prep and glyph instructions under those conditions;
it does not prove every branch, size, transform, variation or renderer. Binary
table/limit checks belong to the separate embedded-program structural evaluator.
The input sfnt is never rewritten, subsetted or repaired before execution.
"""
import ctypes
import hashlib
from importlib.metadata import version
import io
from pathlib import Path
import platform


PPEM = (12, 72, 300)
INTERPRETER_VERSION = 35


class _Unavailable(RuntimeError):
    pass


def inspect_truetype_hints(data):
    """Return neutral findings/observations for one raw TrueType sfnt.

    ``truetype-hint-unresolved`` means the required interpreter or dependency
    identity could not be established and must prevent a passing verdict.
    An execution error is reported with its GID, ppem and FreeType error code;
    FreeType can surface fpgm/prep errors during glyph loading, so that API
    stage does not identify which program caused the error.
    """
    data = bytes(data)
    observations = {
        'input_sha256': hashlib.sha256(data).hexdigest(),
        'byte_length': len(data),
        'glyph_loads_attempted': 0,
        'glyph_loads_succeeded': 0,
        'sizes': [],
        'conditions': {
            'ppem': list(PPEM),
            'interpreter_version': INTERPRETER_VERSION,
            'load_flags': ['FT_LOAD_PEDANTIC', 'FT_LOAD_NO_AUTOHINT', 'FT_LOAD_NO_BITMAP'],
            'target': 'normal',
            'fresh_face_per_size': True,
            'glyph_order': 'all GIDs in ascending order',
            'transform': 'identity',
            'variation': 'default instance',
        },
        'coverage_limit': (
            'FreeType native instruction execution at the declared conditions only; '
            'not all branches, ppem, transforms, variation instances or renderers. '
            'A font may itself disable glyph hinting with its prep program.'),
    }
    report = {'findings': [], 'observations': observations}

    def finding(rule, detail, **context):
        report['findings'].append({'rule': rule, 'detail': detail, **context})

    library = None
    try:
        try:
            import freetype as ft
            import fontTools
            from fontTools.ttLib import TTFont
            observations['dependencies'] = {
                'python_version': platform.python_version(),
                'freetype_py_version': version('freetype-py'),
                'fonttools_version': fontTools.__version__,
                'freetype_binary_sha256': hashlib.sha256(
                    Path(ft.FT_Library_filename).read_bytes()).hexdigest(),
            }
        except (ImportError, OSError, AttributeError) as error:
            raise _Unavailable(type(error).__name__ + ': ' + str(error)) from error

        if data[:4] not in (b'\0\1\0\0', b'true'):
            raise ValueError('Expected one raw TrueType sfnt program')
        with TTFont(io.BytesIO(data), recalcBBoxes=False, recalcTimestamp=False) as font:
            glyphs = font['glyf']
            lengths = [len(getattr(glyphs[name], 'program', b'').getBytecode())
                       if hasattr(glyphs[name], 'program') else 0
                       for name in font.getGlyphOrder()]
            observations['glyph_count'] = font['maxp'].numGlyphs
            observations['programs'] = {
                'fpgm_bytes': len(font['fpgm'].program.getBytecode()) if 'fpgm' in font else 0,
                'prep_bytes': len(font['prep'].program.getBytecode()) if 'prep' in font else 0,
                'glyphs_with_instructions': sum(length > 0 for length in lengths),
                'glyph_instruction_bytes': sum(lengths),
                'maximum_glyph_instruction_bytes': max(lengths, default=0),
            }

        library = ft.FT_Library()
        error = ft.FT_Init_FreeType(ctypes.byref(library))
        if error:
            library = None
            raise _Unavailable('FT_Init_FreeType failed: ' + str(error))
        major, minor, patch = ft.FT_Int(), ft.FT_Int(), ft.FT_Int()
        ft.FT_Library_Version(library, ctypes.byref(major), ctypes.byref(minor), ctypes.byref(patch))
        observations['dependencies']['freetype_version'] = '.'.join(
            str(value.value) for value in (major, minor, patch))
        engine_type = ft.FT_Get_TrueType_Engine_Type(library)
        observations['conditions']['engine_type'] = engine_type
        # Type 2 is the full bytecode engine. Type 0 is absent; type 1 was a
        # historical incomplete engine. Parser success cannot replace this.
        if engine_type != 2:
            raise _Unavailable('Full TrueType bytecode interpreter is unavailable')
        interpreter = ctypes.c_uint(INTERPRETER_VERSION)
        error = ft.FT_Property_Set(library, b'truetype', b'interpreter-version', ctypes.byref(interpreter))
        if error:
            raise _Unavailable('Cannot select interpreter version 35: ' + str(error))
        actual = ctypes.c_uint()
        error = ft.FT_Property_Get(library, b'truetype', b'interpreter-version', ctypes.byref(actual))
        if error or actual.value != INTERPRETER_VERSION:
            raise _Unavailable('Cannot verify selected TrueType interpreter version')

        flags = ft.FT_LOAD_PEDANTIC | ft.FT_LOAD_NO_AUTOHINT | ft.FT_LOAD_NO_BITMAP
        observations['conditions']['load_flags_value'] = flags
        # Keep the original bytes alive for every FT_New_Memory_Face. A fresh
        # face makes each size start with its own fpgm/prep state.
        memory = (ctypes.c_ubyte * len(data)).from_buffer_copy(data)
        for ppem in PPEM:
            face = ft.FT_Face()
            error = ft.FT_New_Memory_Face(library, memory, len(data), 0, ctypes.byref(face))
            if error:
                finding('truetype-hint-execution', 'FT_New_Memory_Face rejected the exact bytes',
                        stage='face-load', ppem=ppem, freetype_error=error)
                continue
            size = {'ppem': ppem, 'glyph_loads_attempted': 0, 'glyph_loads_succeeded': 0}
            observations['sizes'].append(size)
            try:
                if face.contents.num_glyphs != observations['glyph_count']:
                    raise ValueError('FreeType and fontTools disagree on the glyph count')
                error = ft.FT_Set_Pixel_Sizes(face, ppem, ppem)
                if error:
                    finding('truetype-hint-execution', 'FT_Set_Pixel_Sizes failed',
                            stage='size', ppem=ppem, freetype_error=error)
                    continue
                for gid in range(observations['glyph_count']):
                    size['glyph_loads_attempted'] += 1
                    observations['glyph_loads_attempted'] += 1
                    error = ft.FT_Load_Glyph(face, gid, flags)
                    if error:
                        finding('truetype-hint-execution', 'Native pedantic glyph loading/hinting failed',
                                stage='glyph-load', ppem=ppem, gid=gid, freetype_error=error)
                    else:
                        size['glyph_loads_succeeded'] += 1
                        observations['glyph_loads_succeeded'] += 1
            finally:
                ft.FT_Done_Face(face)
    except _Unavailable as error:
        finding('truetype-hint-unresolved', str(error))
    except Exception as error:
        finding('truetype-hint-structure', type(error).__name__ + ': ' + str(error))
    finally:
        if library is not None:
            ft.FT_Done_FreeType(library)
        observations['input_sha256_after'] = hashlib.sha256(data).hexdigest()
    return report
