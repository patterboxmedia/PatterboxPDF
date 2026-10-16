"""Exercise package rejection at the archive and public-content boundaries."""
import copy
import importlib.util
import json
import os
from pathlib import Path
import subprocess
import sys
import tempfile
import unittest
import zipfile

spec = importlib.util.spec_from_file_location("source_package", Path(__file__).resolve().parents[2] / "scripts/build-source-package.py")
package = importlib.util.module_from_spec(spec)
spec.loader.exec_module(package)


class SourcePackageTests(unittest.TestCase):
    def test_public_verifiers_reject_optimization_before_creating_output(self):
        root = Path(__file__).resolve().parents[2]
        for verifier in ('verify-feature-gallery.py', 'verify-public-api.py'):
            for via_environment in (False, True):
                with self.subTest(verifier=verifier, via_environment=via_environment), tempfile.TemporaryDirectory() as temporary:
                    output = Path(temporary) / 'receipt'
                    environment = dict(os.environ)
                    environment.pop('PYTHONOPTIMIZE', None)
                    if via_environment:
                        environment['PYTHONOPTIMIZE'] = '1'
                    result = subprocess.run(
                        [sys.executable, *([] if via_environment else ['-O']),
                         str(root / 'scripts' / verifier), '--output', str(output)],
                        env=environment, capture_output=True, timeout=30,
                        creationflags=getattr(subprocess, 'CREATE_NO_WINDOW', 0))
                    self.assertNotEqual(result.returncode, 0)
                    self.assertIn(b'validation assertions must remain enabled', result.stderr)
                    self.assertFalse(output.exists())

    def test_rejects_escaping_private_and_executable_paths(self):
        for name in ("../README.md", "/README.md", "docs//README.md", ".env.local", "target/tool.exe", "tool.dll", "profile.kfpx", "AGENTS.md", "tests/fixtures/callas-17.1.692/pass.xml"):
            with self.subTest(name=name), self.assertRaises(ValueError):
                package.audit_entry(name, b"source")

    def test_rejects_real_home_paths_but_accepts_synthetic_test_paths(self):
        with self.assertRaises(ValueError):
            home_path = br'{"path":"C:\\' + br'Users\\' + br'OwnerName\\private"}'
            package.audit_entry("receipt.json", home_path)
        package.audit_entry("test.rs", br'let fixture = r"C:\Users\TestUser\fixture";')

    def test_licensed_package_and_policy_rejections(self):
        root = Path(__file__).resolve().parents[2]
        with tempfile.TemporaryDirectory() as temporary:
            output = Path(temporary) / 'package'
            receipt = package.build(root, output)
            archive = output / receipt['archive']
            manifest = package.verify_archive(archive)
            self.assertEqual(manifest['schema'], 'patterboxpdf-source-package/v2')
            self.assertEqual(manifest['license_expression'], 'MIT OR Apache-2.0')
            self.assertEqual(manifest['status'], 'licensed_source')
            with zipfile.ZipFile(archive) as source:
                files = {name: source.read(name) for name in source.namelist() if name != 'source-manifest.json'}
            cases = [('unknown schema', {'schema': 'unknown/v99'}, None),
                     ('conflicting policy', {'public_license_status': 'undecided'}, None),
                     ('registry authorized', {'registry_publication_authorized': True}, None),
                     ('missing license', {}, ('LICENSE-MIT', None)),
                     ('changed license', {}, ('LICENSE-APACHE', b'other grant')),
                     ('package mismatch', {}, ('Cargo.toml', files['Cargo.toml'].replace(b'MIT OR Apache-2.0', b'UNLICENSED'))),
                     ('registry enabled', {}, ('examples/standalone-client/Cargo.toml', files['examples/standalone-client/Cargo.toml'].replace(b'publish = false', b'publish = true'))),
                     ('notice drift', {}, ('third_party/cargo-notices/canonical/MIT.txt', b'changed notice')),
                     ('fixture drift', {}, ('crates/print-core/tests/fixtures/generated-srgb.icc', b'changed asset'))]
            for label, changes, mutation in cases:
                with self.subTest(label=label):
                    altered = dict(files)
                    if mutation:
                        name, data = mutation
                        if data is None:
                            del altered[name]
                        else:
                            altered[name] = data
                    policy = copy.deepcopy(manifest)
                    policy.update(changes)
                    policy['files'] = [{'path': name, 'bytes': len(data), 'sha256': package.sha(data)} for name, data in altered.items()]
                    candidate = Path(temporary) / 'changed.zip'
                    with zipfile.ZipFile(candidate, 'w', compression=zipfile.ZIP_DEFLATED) as zipped:
                        for name, data in altered.items():
                            zipped.writestr(name, data)
                        zipped.writestr('source-manifest.json', json.dumps(policy))
                    with self.assertRaises(ValueError):
                        package.verify_archive(candidate)

    def test_rejects_tampered_archive_file(self):
        with tempfile.TemporaryDirectory() as temporary:
            archive = Path(temporary) / "source.zip"
            original = b"documented source\n"
            manifest = {"schema": "pdf-library-source-package/v1", "status": "license_pending", "public_license_status": "undecided", "publication_authorized": False,
                        "files": [{"path": "README.md", "bytes": len(original), "sha256": package.sha(original)}]}
            with zipfile.ZipFile(archive, "w") as output:
                output.writestr("source-manifest.json", json.dumps(manifest))
                output.writestr("README.md", b"different source\n")
            with self.assertRaisesRegex(ValueError, "identity disagreement"):
                package.verify_archive(archive)


if __name__ == "__main__":
    unittest.main()
