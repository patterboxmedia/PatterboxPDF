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

    def test_missing_runtime_library_is_a_failure(self):
        with patch.object(runtime, 'execute', return_value='libexample.so => not found\n'), self.assertRaises(ValueError):
            runtime.runtime_libraries(['worker'], 'ldd', Path('.'))
        with self.assertRaises(ValueError):
            runtime.runtime_libraries([], 'ldd', Path('.'))


if __name__ == '__main__':
    unittest.main()
