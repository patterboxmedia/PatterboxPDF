"""Reject native-runtime false greens without requiring a Linux build in unit tests."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest.mock import patch

spec = importlib.util.spec_from_file_location('runtime', Path(__file__).resolve().parents[2]/'scripts/verify-linux-runtime.py')
runtime = importlib.util.module_from_spec(spec)
spec.loader.exec_module(runtime)


class RuntimeVerificationTests(unittest.TestCase):
    def test_suite_requires_actual_named_execution_without_skips(self):
        listing = 'files: test\nnetwork: test\n'
        valid = 'test files ... ok\ntest network ... ok\ntest result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;\n'
        self.assertEqual(runtime.audit_test_result(listing, valid, ('files', 'network'))['passed'], 2)
        for result in ('', valid.replace('2 passed; 0 failed', '1 passed; 1 failed'),
                       valid.replace('0 ignored', '1 ignored'), valid.replace('0 filtered out', '1 filtered out'),
                       valid.replace('test network ... ok', 'test unrelated ... ok')):
            with self.subTest(result=result), self.assertRaises(ValueError):
                runtime.audit_test_result(listing, result, ('files', 'network'))
        with self.assertRaises(ValueError):
            runtime.audit_test_result('', 'test result: ok. 0 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out;', ())
        for required in ((), ('files', 'files'), ('missing',)):
            with self.subTest(required=required), self.assertRaises(ValueError):
                runtime.audit_test_result(listing, valid, required)

    def test_unavailable_platform_or_landlock_is_a_failure(self):
        with patch.object(runtime.sys, 'platform', 'win32'), self.assertRaises(ValueError):
            runtime.kernel_environment()
        with patch.object(runtime.sys, 'platform', 'linux'), patch.object(runtime.platform, 'machine', return_value='x86_64'), patch.object(runtime.ctypes, 'CDLL') as libc:
            libc.return_value.syscall.return_value = 2
            with self.assertRaises(ValueError):
                runtime.kernel_environment()
            libc.return_value.syscall.return_value = 3
            self.assertEqual(runtime.kernel_environment()['landlock_abi'], 3)

    def test_non_native_or_nonexecutable_inputs_are_rejected(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary)/'candidate'
            header = bytearray(64)
            header[:6] = b'\x7fELF\x02\x01'
            header[16:18] = (3).to_bytes(2, 'little')
            header[18:20] = (62).to_bytes(2, 'little')
            path.write_bytes(header)
            path.chmod(0o700)
            with patch.object(runtime.os, 'access', return_value=True):
                self.assertEqual(runtime.elf_identity(path)['bytes'], 64)
                for invalid in (b'MZ'+bytes(62), header[:18]+b'\xb7\x00'+header[20:], header[:4]):
                    path.write_bytes(invalid)
                    with self.assertRaises(ValueError):
                        runtime.elf_identity(path)
            path.write_bytes(header)
            with patch.object(runtime.os, 'access', return_value=False), self.assertRaises(ValueError):
                runtime.elf_identity(path)

    def test_compile_messages_alone_or_missing_suite_do_not_qualify(self):
        with self.assertRaises(ValueError):
            runtime.select_artifacts(json.dumps({'reason': 'build-finished', 'success': True}), {})
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary)/'test'
            path.write_bytes(b'fixture')
            message = {'reason': 'compiler-artifact', 'package_id': 'pkg', 'executable': str(path),
                       'profile': {'test': True}, 'target': {'name': 'linux_isolation', 'kind': ['test']}}
            with self.assertRaises(ValueError):
                runtime.select_artifacts(json.dumps(message), {'pkg': 'publisher-linux-worker-job'})

    def test_worker_binary_and_resource_suites_cannot_disappear_from_build_evidence(self):
        with tempfile.TemporaryDirectory() as temporary:
            path = Path(temporary)/'compiled-test'
            path.write_bytes(b'compiled fixture')
            packages = {suite[0]: suite[0] for suite in runtime.SUITES}
            messages = [{'reason': 'compiler-artifact', 'package_id': package, 'executable': str(path),
                         'profile': {'test': True}, 'target': {'name': name, 'kind': [kind]}}
                        for package, name, kind, _ in runtime.SUITES]
            self.assertEqual(len(runtime.select_artifacts('\n'.join(map(json.dumps, messages)), packages)), len(runtime.SUITES))
            framed = '\n\n'+'\n\n'.join(map(json.dumps, messages))+'\n\n'
            self.assertEqual(len(runtime.select_artifacts(framed, packages)), len(runtime.SUITES))
            with self.assertRaises(json.JSONDecodeError):
                runtime.select_artifacts(framed+'not JSON\n', packages)
            for omitted in ('icc-profile', 'publisher-font-worker', 'publisher-color-worker', 'print-core'):
                partial = [row for row in messages if row['package_id'] != omitted]
                with self.subTest(omitted=omitted), self.assertRaises(ValueError):
                    runtime.select_artifacts('\n'.join(map(json.dumps, partial)), packages)
            wrong_kind = [dict(row, target={**row['target'], 'kind': ['test']})
                          if row['package_id'] == 'publisher-font-worker' else row for row in messages]
            with self.assertRaises(ValueError):
                runtime.select_artifacts('\n'.join(map(json.dumps, wrong_kind)), packages)

    def test_resource_programs_and_reproduction_sources_are_bound_by_snapshot(self):
        self.assertIn('apps/font-worker/src/tests/components/generate.py', runtime.SOURCE_SUPPORT_FILES)
        self.assertIn('apps/font-worker/src/tests/components/probe.py', runtime.SOURCE_SUPPORT_FILES)
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            names = {'Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml',
                     'crates/icc-profile/src/lib.rs', 'crates/icc-profile/src/tests.rs',
                     'apps/font-worker/src/tests/fixtures/provenance.json',
                     'apps/font-worker/src/tests/fixtures/LICENSE-OFL',
                     'apps/font-worker/src/tests/fixtures/control.otf',
                     'crates/layout-core/tests/fixtures/DejaVuSans.ttf',
                     'crates/layout-core/tests/fixtures/NotoSansCJKsc-Regular.otf',
                     'crates/layout-core/tests/fixtures/NotoEmoji-Regular.ttf',
                     'scripts/verify-font-metrics.py',
                     'crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc',
                     *runtime.SOURCE_SUPPORT_FILES}
            for name in names:
                path = root/name
                path.parent.mkdir(parents=True, exist_ok=True)
                path.write_bytes(b'original reproduction input')
            with patch.object(runtime, 'ROOT', root):
                before = runtime.source_snapshot()
                self.assertEqual({row['path'] for row in before}, names)
                for name in ('apps/font-worker/src/tests/fixtures/provenance.json',
                             'apps/font-worker/src/tests/fixtures/control.otf',
                             'apps/font-worker/src/tests/generate_controls.py',
                             'crates/icc-profile/src/lib.rs',
                             'scripts/verify-font-metrics.py'):
                    path = root/name
                    path.write_bytes(b'changed actual reproduction input')
                    self.assertNotEqual(runtime.source_snapshot(), before, name)
                    path.write_bytes(b'original reproduction input')
                self.assertEqual(runtime.source_snapshot(), before)

    def test_component_suite_requires_the_actual_geometry_and_context_cases(self):
        suites = {tuple(row[:3]): row[3] for row in runtime.SUITES}
        cases = suites.get(('publisher-font-worker', 'components', 'test'), ())
        self.assertTrue(cases)
        self.assertIn('child_horizontal_phantoms_are_valid_attachment_anchors', cases)
        self.assertIn('use_my_metrics_selects_component_advance_and_origin', cases)
        self.assertIn('unused_source_context_is_deferred_but_all_full_retained_glyphs_are_checked', cases)
        self.assertIn('unsupported_vertical_table_version_is_only_needed_by_an_actual_anchor', cases)

    def test_missing_runtime_library_is_a_failure(self):
        with patch.object(runtime, 'execute', return_value='libexample.so => not found\n'), self.assertRaises(ValueError):
            runtime.runtime_libraries(['worker'], 'ldd', Path('.'))
        with self.assertRaises(ValueError):
            runtime.runtime_libraries([], 'ldd', Path('.'))


if __name__ == '__main__':
    unittest.main()
