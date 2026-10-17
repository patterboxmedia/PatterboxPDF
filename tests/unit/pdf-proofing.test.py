"""Caller contracts for the optional PDF preview companion (no Rust dependency)."""
import importlib.util
import contextlib
import hashlib
import io
import json
from pathlib import Path
import subprocess
import sys
import tempfile
from types import SimpleNamespace
import unittest
from unittest.mock import patch

from PIL import Image
from pypdf import PdfWriter
from pypdf.generic import (ArrayObject, DecodedStreamObject, DictionaryObject,
                          NameObject, NumberObject)

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('pdf_proofing', ROOT/'scripts/pdf_proofing.py')
proof = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(proof)
sys.modules['pdf_proofing'] = proof
CLI_SPEC = importlib.util.spec_from_file_location('proof_cli', ROOT/'scripts/proof-pdf.py')
cli = importlib.util.module_from_spec(CLI_SPEC)
CLI_SPEC.loader.exec_module(cli)
FIXTURES = ROOT/'crates/print-core/tests/fixtures'
PRINT = FIXTURES/'SWOP2006coated_300_mth.icc'
DISPLAY = FIXTURES/'generated-srgb.icc'


def authored_pdf(path, profiles=(), encrypted=False):
    writer = PdfWriter()
    for _ in range(2):
        page = writer.add_blank_page(width=72, height=36)
        stream = DecodedStreamObject()
        # Independently authored saturated magenta rectangle on a white page.
        stream.set_data(b'0 1 0 0 k 0 0 36 36 re f\n')
        page[NameObject('/Contents')] = writer._add_object(stream)
    if profiles:
        intents = ArrayObject()
        for data in profiles:
            profile = DecodedStreamObject()
            profile.set_data(data)
            profile[NameObject('/N')] = NumberObject(4)
            intents.append(DictionaryObject({NameObject('/S'): NameObject('/GTS_PDFX'),
                NameObject('/DestOutputProfile'): writer._add_object(profile)}))
        writer.root_object[NameObject('/OutputIntents')] = intents
    if encrypted:
        writer.encrypt('secret')
    writer.write(path)


class FakeRenderer:
    """Observe literal subprocess arguments; provide independent known pixels."""
    def __init__(self, behavior='success'):
        self.behavior = behavior
        self.renders = []

    def __call__(self, arguments, **kwargs):
        assert kwargs['shell'] is False
        assert kwargs['creationflags'] == getattr(subprocess, 'CREATE_NO_WINDOW', 0)
        assert kwargs['timeout'] == 300
        if arguments[-1] == '-h':
            help_text = '' if self.behavior == 'no-cms' else '-displayprofile -defaultcmykprofile'
            return subprocess.CompletedProcess(arguments, 0, '', help_text)
        if arguments[-1] == '-v':
            return subprocess.CompletedProcess(arguments, 0, '', 'pdftoppm version test-build')
        self.renders.append(arguments)
        if self.behavior == 'exit':
            return subprocess.CompletedProcess(arguments, 7, '', 'deliberate failure')
        if self.behavior != 'missing':
            target = Path(arguments[-1]).with_suffix('.png')
            if self.behavior == 'invalid':
                target.write_bytes(b'not a PNG')
            else:
                image = Image.new('RGBA' if self.behavior == 'alpha' else 'RGB', (144, 72), (17, 83, 149))
                image.save(target, format='PNG')
        diagnostic = 'Could not open display profile' if self.behavior == 'profile-warning' else ''
        return subprocess.CompletedProcess(arguments, 0, '', diagnostic)


class ProofingTests(unittest.TestCase):
    def setUp(self):
        # Mock only the companion's renderer calls; platform metadata may also
        # use subprocess.run internally on Linux.
        renderer_process = SimpleNamespace(run=subprocess.run,
            TimeoutExpired=subprocess.TimeoutExpired,
            CREATE_NO_WINDOW=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
        process_patch = patch.object(proof, 'subprocess', renderer_process)
        process_patch.start()
        self.addCleanup(process_patch.stop)
        self.temp = tempfile.TemporaryDirectory()
        self.addCleanup(self.temp.cleanup)
        self.work = Path(self.temp.name)/'literal space & semicolon; $ path'
        self.work.mkdir()
        self.pdf = self.work/'input with spaces.pdf'
        authored_pdf(self.pdf, [PRINT.read_bytes()])
        self.output = self.work/'new output'
        self.renderer = self.work/'pdftoppm supplied.exe'
        self.renderer.write_bytes(b'test executable identity')

    def render(self, **changes):
        arguments = dict(pdf=self.pdf, print_profile=PRINT, display_profile=DISPLAY,
                         renderer=self.renderer, output=self.output)
        arguments.update(changes)
        return proof.render_preview(**arguments)

    def test_existing_output_is_preserved(self):
        with tempfile.TemporaryDirectory() as directory:
            output = Path(directory)/'existing'
            output.mkdir()
            sentinel = output/'owner.txt'
            sentinel.write_text('preserve')
            with self.assertRaisesRegex(proof.ProofingError, 'already exists'):
                proof.render_preview('missing.pdf', print_profile='print.icc',
                    display_profile='display.icc', renderer='pdftoppm', output=output)
            self.assertEqual(sentinel.read_text(), 'preserve')

    def test_two_page_api_binds_profiles_pixels_inputs_and_environment(self):
        fake = FakeRenderer()
        original = self.pdf.read_bytes()
        with patch.object(proof.subprocess, 'run', side_effect=fake):
            manifest = self.render()
        self.assertEqual(manifest, json.loads((self.output/'manifest.json').read_text()))
        self.assertEqual(manifest['status'], 'complete')
        self.assertEqual(manifest['schema_version'], 1)
        self.assertEqual(manifest['print_condition']['kind'], 'embedded-output-intent-match')
        self.assertEqual(manifest['source_pdf']['sha256'], hashlib.sha256(original).hexdigest())
        self.assertEqual(self.pdf.read_bytes(), original)
        self.assertEqual(manifest['conditions']['dpi'], 144)
        self.assertEqual(manifest['conditions']['pages'], [1, 2])
        self.assertEqual(manifest['display_profile']['sha256'], hashlib.sha256(DISPLAY.read_bytes()).hexdigest())
        self.assertEqual(manifest['renderer']['sha256'], hashlib.sha256(self.renderer.read_bytes()).hexdigest())
        self.assertEqual(manifest['companion']['sha256'], hashlib.sha256((ROOT/'scripts/pdf_proofing.py').read_bytes()).hexdigest())
        self.assertTrue({'python', 'executable', 'pillow', 'pypdf', 'pillow_littlecms', 'platform'} <= manifest['runtime'].keys())
        for output, command in zip(manifest['outputs'], fake.renders):
            self.assertEqual(output['media_box_points'], [0, 0, 72, 36])
            self.assertEqual(output['pixel_dimensions'], [144, 72])
            path = self.output/output['file']
            with Image.open(path) as image:
                self.assertEqual(image.mode, 'RGB')
                self.assertEqual(image.info['icc_profile'], DISPLAY.read_bytes())
                self.assertEqual(image.getpixel((0, 0)), (17, 83, 149))
                self.assertEqual(output['pixel_sha256'], hashlib.sha256(bytes([17, 83, 149])*144*72).hexdigest())
            self.assertEqual(output['sha256'], hashlib.sha256(path.read_bytes()).hexdigest())
            self.assertEqual(command[-2], str(self.pdf.resolve()))
            self.assertEqual(command[command.index('-defaultcmykprofile')+1], str(PRINT.resolve()))
            self.assertEqual(command[command.index('-displayprofile')+1], str(DISPLAY.resolve()))

    def test_cli_shares_the_two_page_contract(self):
        with patch.object(proof.subprocess, 'run', side_effect=FakeRenderer()), contextlib.redirect_stdout(io.StringIO()):
            code = cli.main([str(self.pdf), '--print-profile', str(PRINT),
                '--display-profile', str(DISPLAY), '--renderer', str(self.renderer),
                '--output', str(self.output)])
        self.assertEqual(code, 0)
        self.assertEqual(len(json.loads((self.output/'manifest.json').read_text())['outputs']), 2)

    def test_untagged_and_page_selection(self):
        authored_pdf(self.pdf)
        fake = FakeRenderer()
        with patch.object(proof.subprocess, 'run', side_effect=fake):
            manifest = self.render(pages=[2], dpi=72)
        self.assertEqual(manifest['print_condition']['kind'], 'selected-default-cmyk')
        self.assertEqual(manifest['conditions']['pages'], [2])
        self.assertEqual(len(fake.renders), 1)
        self.assertEqual(fake.renders[0][fake.renders[0].index('-r')+1], '72')

    def test_matching_duplicate_output_intents_are_unambiguous(self):
        authored_pdf(self.pdf, [PRINT.read_bytes(), PRINT.read_bytes()])
        with patch.object(proof.subprocess, 'run', side_effect=FakeRenderer()):
            self.assertEqual(len(self.render()['print_condition']['embedded_profiles']), 2)

    def test_different_or_conflicting_embedded_print_profiles_fail_before_launch(self):
        different = bytearray(PRINT.read_bytes())
        # Valid alternative profile identity with the same usable transform.
        different[34:36] = ((int.from_bytes(different[34:36], 'big')+1) % 60).to_bytes(2, 'big')
        for profiles, message in [([bytes(different)], 'does not match'),
                                  ([PRINT.read_bytes(), bytes(different)], 'conflicting')]:
            with self.subTest(message=message):
                authored_pdf(self.pdf, profiles)
                with patch.object(proof.subprocess, 'run') as launch:
                    with self.assertRaisesRegex(proof.ProofingError, message):
                        self.render()
                launch.assert_not_called()
                self.assertFalse(self.output.exists())

    def test_invalid_profiles_fail_with_actionable_error(self):
        bad = self.work/'bad.icc'
        relabeled = bytearray(DISPLAY.read_bytes())
        relabeled[12:16], relabeled[16:20] = b'prtr', b'CMYK'
        for data, message in [(b'invalid', 'header'), (DISPLAY.read_bytes(), 'prtr-class CMYK'),
                              (bytes(relabeled), 'usable CMYK-to-RGB')]:
            with self.subTest(message=message):
                bad.write_bytes(data)
                with self.assertRaisesRegex(proof.ProofingError, message):
                    self.render(print_profile=bad)
                self.assertFalse(self.output.exists())
        bad.write_bytes(PRINT.read_bytes())
        with self.assertRaisesRegex(proof.ProofingError, 'mntr-class RGB'):
            self.render(display_profile=bad)

    def test_invalid_document_and_encryption(self):
        for encrypted in (False, True):
            if encrypted:
                authored_pdf(self.pdf, encrypted=True)
            else:
                self.pdf.write_bytes(b'not a PDF')
            with self.subTest(encrypted=encrypted), self.assertRaisesRegex(proof.ProofingError, 'PDF'):
                self.render()
            self.assertFalse(self.output.exists())

    def test_invalid_pages_and_dpi(self):
        for pages in ([], [0], [3], [1, 1], [True], ['1']):
            with self.subTest(pages=pages), self.assertRaisesRegex(proof.ProofingError, 'page selection'):
                self.render(pages=pages)
        for dpi in (0, -1, float('nan'), float('inf'), True, '144'):
            with self.subTest(dpi=dpi), self.assertRaisesRegex(proof.ProofingError, 'DPI'):
                self.render(dpi=dpi)
        self.assertFalse(self.output.exists())

    def test_renderer_failure_missing_invalid_alpha_and_profile_diagnostics(self):
        for behavior, message in [('exit', 'exit 7'), ('missing', 'did not produce PNG'),
                                  ('invalid', 'invalid PNG'), ('alpha', 'opaque RGB'),
                                  ('profile-warning', 'profile/rendering'), ('no-cms', 'CMS options')]:
            output = self.work/behavior
            with self.subTest(behavior=behavior), patch.object(proof.subprocess, 'run', side_effect=FakeRenderer(behavior)):
                with self.assertRaisesRegex(proof.ProofingError, message):
                    self.render(output=output)
            self.assertFalse((output/'manifest.json').exists())

    def test_missing_dependency_file_or_executable(self):
        with patch.object(proof, 'DEPENDENCY_ERROR', 'No module named PIL'):
            with self.assertRaisesRegex(proof.ProofingError, 'install Pillow and pypdf'):
                self.render()
        with self.assertRaisesRegex(proof.ProofingError, 'renderer file does not exist'):
            self.render(renderer=self.work/'missing.exe')
        with patch.object(proof.subprocess, 'run', side_effect=OSError('not executable')):
            with self.assertRaisesRegex(proof.ProofingError, 'cannot launch'):
                self.render()

    def test_cli_failure_is_nonzero_and_has_no_success_manifest(self):
        with contextlib.redirect_stderr(io.StringIO()) as errors:
            code = cli.main([str(self.pdf), '--print-profile', str(PRINT),
                '--display-profile', str(DISPLAY), '--renderer', str(self.renderer),
                '--output', str(self.output), '--pages', 'bad'])
        self.assertEqual(code, 1)
        self.assertIn('Preview failed', errors.getvalue())
        self.assertFalse((self.output/'manifest.json').exists())

    def test_timeout_and_input_change_cannot_report_completion(self):
        with patch.object(proof.subprocess, 'run', side_effect=subprocess.TimeoutExpired('pdftoppm', 300)):
            with self.assertRaisesRegex(proof.ProofingError, '300 seconds'):
                self.render()
        fake = FakeRenderer()
        def replace_input(arguments, **kwargs):
            result = fake(arguments, **kwargs)
            if fake.renders:
                self.pdf.write_bytes(b'replaced during external render')
            return result
        with patch.object(proof.subprocess, 'run', side_effect=replace_input):
            with self.assertRaisesRegex(proof.ProofingError, 'input changed during preview'):
                self.render()
        self.assertFalse((self.output/'manifest.json').exists())

    def test_unresolved_and_unselected_page_output_intents_cannot_be_ignored(self):
        writer = PdfWriter()
        writer.add_blank_page(width=72, height=36)
        page = writer.add_blank_page(width=72, height=36)
        page[NameObject('/OutputIntents')] = ArrayObject([
            DictionaryObject({NameObject('/S'): NameObject('/GTS_PDFX')})])
        writer.write(self.pdf)
        with self.assertRaisesRegex(proof.ProofingError, 'page 2 OutputIntent has no embedded'):
            self.render(pages=[1])
        self.assertFalse(self.output.exists())


if __name__ == '__main__':
    unittest.main()
