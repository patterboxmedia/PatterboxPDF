"""Independent preview auditing rejects false completion and altered real PNGs."""
import copy
import hashlib
import importlib.util
import json
from pathlib import Path
import sys
import tempfile
import unittest

import PIL
from PIL import Image, ImageCms
import pypdf
from pypdf import PdfWriter

ROOT = Path(__file__).resolve().parents[2]
sys.path.insert(0, str(ROOT/'scripts'))
SPEC = importlib.util.spec_from_file_location('verification', ROOT/'scripts/verify-pdf-proofing.py')
verification = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(verification)


def ident(path):
    return {'file': path.name, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest()}


class PreviewAuditTests(unittest.TestCase):
    def setUp(self):
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.work = Path(self.temp.name)
        self.pdf = self.work/'authored.pdf'
        writer = PdfWriter()
        writer.add_blank_page(width=72, height=36)
        writer.add_blank_page(width=36, height=72)
        writer.write(self.pdf)
        fixtures = ROOT/'crates/print-core/tests/fixtures'
        self.print_profile = fixtures/'SWOP2006coated_300_mth.icc'
        self.display_profile = fixtures/'generated-srgb.icc'
        self.renderer = self.work/'renderer'
        self.renderer.write_bytes(b'fixture renderer identity, never executed')
        self.bundle = self.work/'preview'
        self.bundle.mkdir()
        self.png = self.bundle/'page-2.png'
        image = Image.new('RGB', (72, 144), (31, 89, 147))
        image.putpixel((4, 5), (217, 81, 19))
        image.save(self.png, icc_profile=self.display_profile.read_bytes())
        self.manifest = {
            'schema_version': 1, 'status': 'complete',
            'source_pdf': ident(self.pdf), 'print_profile': ident(self.print_profile),
            'display_profile': ident(self.display_profile),
            'companion': ident(ROOT/'scripts/pdf_proofing.py'),
            'renderer': {**ident(self.renderer), 'version': 'pdftoppm fixture'},
            'runtime': {'python': sys.version, 'executable': ident(Path(sys.executable)),
                        'pillow': PIL.__version__, 'pypdf': pypdf.__version__,
                        'pillow_littlecms': ImageCms.core.littlecms_version,
                        'platform': 'fixture platform', 'machine': 'fixture machine'},
            'conditions': {'dpi': 144, 'pages': [2], 'page_box': 'MediaBox',
                'output_mode': 'RGB', 'background': 'opaque white paper',
                'text_antialiasing': True, 'vector_antialiasing': True},
            'print_condition': {'kind': 'selected-default-cmyk', 'embedded_profiles': []},
            'outputs': [{'page': 2, **ident(self.png),
                'pixel_sha256': hashlib.sha256(image.tobytes()).hexdigest(),
                'pixel_dimensions': [72, 144], 'media_box_points': [0, 0, 36, 72],
                'crop_box_points': [0, 0, 36, 72], 'rotation_degrees': 0, 'user_unit': 1.,
                'renderer_arguments': [
                    '-f', '2', '-l', '2', '-singlefile', '-r', '144', '-png',
                    '-aa', 'yes', '-aaVector', 'yes', '-displayprofile', str(self.display_profile.resolve()),
                    '-defaultcmykprofile', str(self.print_profile.resolve()), str(self.pdf.resolve()),
                    str((self.bundle/'page-2').resolve())], 'diagnostics': ''}]
        }

    def audit(self, manifest=None):
        (self.bundle/'manifest.json').write_text(json.dumps(manifest or self.manifest), encoding='utf-8')
        return verification.audit_preview(self.bundle, pdf=self.pdf,
            print_profile=self.print_profile, display_profile=self.display_profile,
            renderer=self.renderer, renderer_version='pdftoppm fixture', pages=[2], dpi=144)

    def test_accepts_independently_authored_tagged_png_and_selection(self):
        result = self.audit()
        self.assertEqual(result['pages'], [2])
        self.assertEqual(result['outputs'][0]['pixel_dimensions'], [72, 144])

    def test_rejects_fabricated_missing_or_failed_page_records(self):
        for change in ('missing', 'extra', 'wrong-page', 'failed', 'false-status', 'wrong-file'):
            bad = copy.deepcopy(self.manifest)
            if change == 'missing': bad['outputs'] = []
            elif change == 'extra': bad['outputs'].append(copy.deepcopy(bad['outputs'][0]))
            elif change == 'wrong-page': bad['outputs'][0]['page'] = 1
            elif change == 'failed': bad['status'] = 'failed'
            elif change == 'false-status': bad['status'] = False
            elif change == 'wrong-file': bad['outputs'][0]['file'] = '../other.png'
            with self.subTest(change=change), self.assertRaises(ValueError): self.audit(bad)

    def test_rejects_wrong_source_profiles_companion_or_renderer(self):
        for key in ('source_pdf', 'print_profile', 'display_profile', 'companion', 'renderer'):
            bad = copy.deepcopy(self.manifest)
            bad[key]['sha256'] = '0'*64
            with self.subTest(key=key), self.assertRaises(ValueError): self.audit(bad)
        bad = copy.deepcopy(self.manifest)
        bad['renderer']['version'] = 'invented version'
        with self.assertRaises(ValueError): self.audit(bad)

    def test_rejects_altered_pixels_even_when_encoded_hash_is_updated(self):
        with Image.open(self.png) as original:
            image = original.copy()
        image.putpixel((4, 5), (0, 0, 0))
        image.save(self.png, icc_profile=self.display_profile.read_bytes())
        self.manifest['outputs'][0].update(ident(self.png))
        with self.assertRaisesRegex(ValueError, 'pixel'): self.audit()

    def test_rejects_wrong_icc_even_when_all_claimed_hashes_match(self):
        with Image.open(self.png) as original:
            image = original.copy()
        image.save(self.png, icc_profile=b'fabricated ICC bytes')
        self.manifest['outputs'][0].update(ident(self.png))
        with self.assertRaisesRegex(ValueError, 'ICC'): self.audit()

    def test_rejects_wrong_dimensions_and_fabricated_geometry(self):
        for field, value in [('pixel_dimensions', [1, 1]), ('media_box_points', [0, 0, 72, 36]),
                             ('rotation_degrees', 90), ('user_unit', 2)]:
            bad = copy.deepcopy(self.manifest)
            bad['outputs'][0][field] = value
            with self.subTest(field=field), self.assertRaises(ValueError): self.audit(bad)
        image = Image.new('RGB', (2, 2))
        image.save(self.png, icc_profile=self.display_profile.read_bytes())
        self.manifest['outputs'][0].update(ident(self.png))
        self.manifest['outputs'][0]['pixel_sha256'] = hashlib.sha256(image.tobytes()).hexdigest()
        self.manifest['outputs'][0]['pixel_dimensions'] = [2, 2]
        with self.assertRaisesRegex(ValueError, 'dimensions'): self.audit()

    def test_rejects_ignored_profile_command_or_false_condition(self):
        bad = copy.deepcopy(self.manifest)
        bad['outputs'][0]['renderer_arguments'][13] = 'wrong-profile.icc'
        with self.assertRaises(ValueError): self.audit(bad)
        bad = copy.deepcopy(self.manifest)
        bad['print_condition']['kind'] = 'embedded-output-intent-match'
        with self.assertRaises(ValueError): self.audit(bad)

    def test_fixed_sample_tolerance_rejects_wrong_color(self):
        record = verification.compare_rgb([16, 82, 145], [20, 82, 145], 'boundary')
        self.assertEqual(record['maximum_error'], 4)
        with self.assertRaisesRegex(ValueError, '5'):
            verification.compare_rgb([15, 82, 145], [20, 82, 145], 'changed color')


if __name__ == '__main__':
    unittest.main()
