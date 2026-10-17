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
collector_spec = importlib.util.spec_from_file_location('notice_collector', Path(__file__).resolve().parents[2] / 'scripts/collect-pdf-candidate-licenses.py')
collector = importlib.util.module_from_spec(collector_spec)
collector_spec.loader.exec_module(collector)


def metadata_inputs():
    return [{'target': target, 'additional_workspace': client, 'sha256': '0'*64,
             'roots': ['pdf-library-standalone-client' if client else 'print-core']}
            for target in package.SUPPORTED_TARGETS for client in (False, True)]


def public_fixture(root):
    """An isolated, neutral source tree; no developer history is copied into it."""
    original = Path(__file__).resolve().parents[2]
    def write(name, data):
        path = root/name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
    for name in package.ROOT_FILES:
        write(name, (original/name).read_bytes() if name in package.LICENSE_HASHES else b'fixture\n')
    members = {'crates/print-core': 'print-core', 'crates/linux-worker-job': 'publisher-linux-worker-job'}
    write('Cargo.toml', ('[workspace]\nmembers = '+json.dumps(list(members))+'\n'
                        '[workspace.package]\nversion = "0.0.0"\nlicense = "MIT OR Apache-2.0"\n').encode())
    owned = []
    for directory, name in {**members, 'examples/standalone-client': 'pdf-library-standalone-client'}.items():
        manifest = f'[package]\nname = "{name}"\nversion = "0.0.0"\nlicense = "MIT OR Apache-2.0"\npublish = false\n'.encode()
        write(directory+'/Cargo.toml', manifest)
        write(directory+'/src/lib.rs', b'// Original test fixture.\n')
        owned.append({'name': name, 'version': '0.0.0', 'license': 'MIT OR Apache-2.0', 'source': None,
                      'cargo_manifest_sha256': package.sha(manifest), 'license_texts': [], 'targets': list(package.SUPPORTED_TARGETS)})
    identities = [[row['name'], row['version']] for row in owned]+[['fixture-dependency', '1.0.0']]
    lock = ('version = 4\n'+''.join(f'[[package]]\nname = "{name}"\nversion = "{version}"\n' for name, version in identities)).encode()
    for name in ('Cargo.lock', 'examples/standalone-client/Cargo.lock', 'third_party/cargo-notices/Cargo.lock', 'third_party/cargo-notices/client-Cargo.lock'):
        write(name, lock)
    for name in package.PUBLIC_DOCS:
        write('docs/'+name, b'{}\n' if name.endswith('.json') else b'Public fixture.\n')
    write('docs/requirements/pdfx4-inventory.json', json.dumps({'schema': 'pdf-library-requirements/v1',
          'normative_requirements': [], 'coverage_assurance': {'status': 'incomplete'}}).encode())
    for name in package.PUBLIC_SCRIPTS:
        write('scripts/'+name, b'# Public fixture.\n')
    for name in package.PUBLIC_TESTS:
        write('tests/unit/'+name, b'# Public fixture.\n')
    for name in package.PUBLIC_RECEIPTS:
        write('docs/verification/current/'+name, b'{}\n')
    for name in ('crates/linux-worker-job/README.md', 'examples/standalone-client/README.md',
                 'third_party/qpdf/NOTICE.md', 'examples/standalone-client/examples/sample.rs'):
        write(name, b'Public fixture.\n')
    write('third_party/upstream/receipt.json', b'{"notices": []}\n')
    notice_files = ['canonical/MIT.txt', 'texts/fixture-dependency-1.0.0/LICENSE', 'assets/profile-provenance.md',
                    'rust-standard-library/COPYRIGHT-library.html', 'rust-standard-library/licenses/MIT.txt']
    for name in notice_files:
        write('third_party/cargo-notices/'+name, b'Original synthetic notice fixture.\n')
    notice_sha = package.sha(b'Original synthetic notice fixture.\n')
    asset = 'crates/print-core/tests/fixtures/generated-srgb.icc'
    write(asset, b'Original synthetic asset fixture.\n')
    notice = {'schema': 'patterboxpdf-licenses/v2', 'supported_targets': list(package.SUPPORTED_TARGETS),
              'metadata_inputs': metadata_inputs(),
              'includes_dev_dependencies': True, 'all_workspace_members': True,
              'cargo_lock_sha256': package.sha(lock), 'client_cargo_lock_sha256': package.sha(lock),
              'target_closures': [{'target': target, 'packages': identities} for target in package.SUPPORTED_TARGETS],
              'dependencies': [*owned, {'name': 'fixture-dependency', 'version': '1.0.0', 'source': 'registry',
                                       'targets': list(package.SUPPORTED_TARGETS),
                                       'license_texts': [{'path': notice_files[1], 'sha256': notice_sha}]}],
              'example_assets': [{'path': asset, 'sha256': package.sha(b'Original synthetic asset fixture.\n'),
                                  'provenance': notice_files[2], 'provenance_sha256': notice_sha}],
              'canonical_terms': [{'path': notice_files[0], 'sha256': notice_sha}],
              'rust_standard_library': {'notice': notice_files[3], 'sha256': notice_sha,
                                         'license_texts': [{'path': notice_files[4], 'sha256': notice_sha}]}}
    write('third_party/cargo-notices/dependencies.json', json.dumps(notice).encode())


class SourcePackageTests(unittest.TestCase):
    def test_target_graph_union_preserves_platform_only_dependencies(self):
        with tempfile.TemporaryDirectory() as temporary:
            inputs = []
            for target, leaf in zip(package.SUPPORTED_TARGETS, ('windows-only', 'linux-only')):
                filename = Path(temporary)/f'{leaf}.json'
                filename.write_text(json.dumps({'workspace_members': ['root'],
                    'packages': [{'id': key, 'name': key, 'version': '1.0', 'source': None} for key in ('root', leaf)],
                    'resolve': {'nodes': [{'id': 'root', 'features': [], 'deps': [{'pkg': leaf, 'dep_kinds': [{'kind': None}]}]},
                                          {'id': leaf, 'features': [], 'deps': []}]}}), encoding='utf-8')
                inputs.append((target, filename, False))
            packages, _, _, closures, _, targets = collector.metadata_union(inputs, all_workspace=True, include_dev=True)
            self.assertEqual(set(packages), {'root', 'windows-only', 'linux-only'})
            self.assertEqual(closures[package.SUPPORTED_TARGETS[0]], {'root', 'windows-only'})
            self.assertEqual(targets['linux-only'], {package.SUPPORTED_TARGETS[1]})
            with self.assertRaisesRegex(ValueError, 'Windows and Linux'):
                collector.metadata_union(inputs[:1], all_workspace=True, include_dev=True)

    def test_public_selection_is_explicit_and_inventory_is_neutral(self):
        names = package.public_paths(Path(__file__).resolve().parents[2])
        self.assertIn('docs/color-behavior.md', names)
        self.assertIn('scripts/pdf_font_program_rules.py', names)
        self.assertIn('tests/unit/font-program-rules.test.py', names)
        self.assertIn('crates/linux-worker-job/src/process.rs', names)
        self.assertNotIn('tests/unit/requirements-inventory.test.py', names)
        self.assertNotIn('crates/linux-worker-job/evidence/native-ubuntu-20261016.json', names)
        self.assertEqual({name for name in names if name.startswith('docs/requirements/')},
                         {'docs/'+name for name in package.PUBLIC_DOCS if name.startswith('requirements/')})
        with self.assertRaises(ValueError):
            package.audit_public_inventory({'schema': 'pdf-library-requirements/v1',
                                            'normative_requirements': [{'id': 'private-row'}]})
        package.audit_public_inventory({'schema': 'pdf-library-requirements/v1',
                                        'normative_requirements': [], 'writer_contracts': [],
                                        'coverage_assurance': {'status': 'incomplete'}})

    def test_notice_union_rejects_omitted_linux_dependency_or_target(self):
        notices = {'schema': 'patterboxpdf-licenses/v2', 'supported_targets': list(package.SUPPORTED_TARGETS),
                   'metadata_inputs': metadata_inputs(),
                   'target_closures': [{'target': target, 'packages': [['example', '1.0']]} for target in package.SUPPORTED_TARGETS],
                   'dependencies': [{'name': 'example', 'version': '1.0', 'targets': list(package.SUPPORTED_TARGETS)}]}
        package.audit_notice_coverage(notices, {('example', '1.0')})
        missing = copy.deepcopy(notices)
        missing['dependencies'] = []
        with self.assertRaisesRegex(ValueError, 'coverage'):
            package.audit_notice_coverage(missing, {('example', '1.0')})
        missing = copy.deepcopy(notices)
        missing['supported_targets'].pop()
        with self.assertRaisesRegex(ValueError, 'target'):
            package.audit_notice_coverage(missing, {('example', '1.0')})
        missing = copy.deepcopy(notices)
        missing['metadata_inputs'] = {'unrelated': 'receipt'}
        with self.assertRaisesRegex(ValueError, 'metadata input'):
            package.audit_notice_coverage(missing, {('example', '1.0')})
        missing = copy.deepcopy(notices)
        missing['dependencies'][0]['targets'].pop()
        with self.assertRaisesRegex(ValueError, 'target'):
            package.audit_notice_coverage(missing, {('example', '1.0')})

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
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)/'source'
            public_fixture(root)
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
                     ('fixture drift', {}, ('crates/print-core/tests/fixtures/generated-srgb.icc', b'changed asset')),
                     ('unlisted note', {}, ('docs/unlisted-note.json', b'{}')),
                     ('unlisted receipt', {}, ('docs/verification/current/unlisted.json', b'{}')),
                     ('unlisted unit test', {}, ('tests/unit/unlisted.test.py', b'# fixture'))]
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
