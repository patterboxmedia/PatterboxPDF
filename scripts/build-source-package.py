"""Prepare a licensed source ZIP from an explicit public file allowlist.

No dependency downloads, binary redistribution or publication occur here. A fresh output directory preserves every previous package attempt.
"""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import tomllib
import zipfile

ROOT_FILES = (".gitattributes", ".gitignore", "LICENSE-MIT", "LICENSE-APACHE", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "README.md", "ROADMAP.md")
PUBLIC_DOCS = ("architecture.md", "development.md", "api.md", "release.md",
               "supported-platforms.md", "verification-status.md", "color-behavior.md", "requirements/README.md",
               "requirements/public-technical-sources.md", "requirements/supported-output-review.md",
               "requirements/pdfx4-inventory.json", "requirements/public-output-rules.json")
PUBLIC_SCRIPTS = ("build-source-package.py", "verify-requirements.py", "stage-workers.py", "verify-public-api.py",
                  "verify-feature-gallery.py", "verify-feature-gallery-appearance.py", "cargo-xwin-wsl.cmd",
                  "verify-cmyk-gradients.py", "verify-pdfx-supported-output.py", "pdf_font_program_rules.py",
                  "pdf_truetype_hint_execution.py", "verify-linux-runtime.py",
                  "pdf_text_metadata_rules.py", "verify-font-metrics.py", "verify-output-rules.py",
                  "collect-pdf-candidate-licenses.py")
PUBLIC_TESTS = ("source-package.test.py", "font-map-metadata.test.py", "worker-staging.test.py",
                "pdfx-supported-output.test.py", "font-program-rules.test.py", "cmyk-gradient-verification.test.py",
                "truetype-hint-execution.test.py", "linux-runtime-verification.test.py",
                "text-metadata-rules.test.py", "font-metrics-verification.test.py", "output-rules.test.py")
PUBLIC_RECEIPTS = (
    "corpus-fidelity.json", "corpus-generation.json", "corpus-independent.json", "corpus-scope.json",
    "corpus-structure.json", "fixture-isolation.json", "standalone-api.json", "standalone-fidelity.json",
    "standalone-independent.json", "u1.json", "u2.json", "u3.json", "windows-confinement.json",
    "finalization-20261004/corpus-fidelity.json", "finalization-20261004/corpus-generation.json",
    "finalization-20261004/corpus-independent.json", "finalization-20261004/corpus-scope.json",
    "finalization-20261004/corpus-structure.json", "finalization-20261004/examples-independent.json",
    "finalization-20261004/feature-gallery.json", "finalization-20261004/gallery-appearance-probes-reproducible.json",
    "finalization-20261004/gallery-appearance-probes.json", "finalization-20261004/gallery-controls.json",
    "finalization-20261004/native-tests.json", "finalization-20261004/public-api.json",
    "finalization-20261004/standalone-fidelity.json", "finalization-20261004/unicode-fixture.json",
    "finalization-20261004/windows-confinement.json", "font-map-metadata-20261006/repair.json",
    "public-release-20261016/gallery-appearance.json", "public-release-20261016/gallery.json",
    "public-release-20261016/public-api.json", "public-release-20261016/source-isolation.json",
    "three-area-20261016/supported-output.json", "three-area-20261016/color.json",
    "three-area-20261016/linux.json", "three-area-20261016/windows.json",
)
SUPPORTED_TARGETS = ("x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu")
FORBIDDEN_PARTS = {".git", ".codex", ".agents", ".tools", "target", "artifacts", "node_modules", "__pycache__"}
FORBIDDEN_SUFFIXES = {".exe", ".dll", ".pyd", ".so", ".dylib", ".kfpx", ".pyc"}
TEXT_SUFFIXES = {".rs", ".toml", ".json", ".md", ".txt", ".py", ".mjs", ".cmd", ".lock", ".html"}
HOME_PATH = re.compile(r"(?i)(?:[a-z]:[\\/]+Users[\\/]+|/mnt/[a-z]/Users/|/home/)([a-z0-9_.-]+)")


LICENSE_EXPRESSION = "MIT OR Apache-2.0"
LICENSE_HASHES = {
    "LICENSE-MIT": "a0ad8d45b8e8f78a5d0e750b7cc07f15aca55c987ed0a56892f8bdb98d85fa22",
    "LICENSE-APACHE": "1f83bfbb6ab612d244b9619bcf615e54c2e107fc78a05eb6e16bc1ce41fd2f8f",
}
POLICIES = {
    "pdf-library-source-package/v1": {
        "public_license_status": "undecided", "publication_authorized": False,
        "status": "license_pending"},
    "patterboxpdf-source-package/v2": {
        "public_license_status": "selected", "publication_authorized": True,
        "registry_publication_authorized": False,
        "license_expression": LICENSE_EXPRESSION, "status": "licensed_source"},
}


def audit_policy(manifest, files):
    schema = manifest.get('schema')
    if schema not in POLICIES:
        raise ValueError('unsupported source package schema')
    for key, expected in POLICIES[schema].items():
        if type(manifest.get(key)) is not type(expected) or manifest[key] != expected:
            raise ValueError('conflicting source package policy: ' + key)
    for key in {'license_expression', 'registry_publication_authorized'} - POLICIES[schema].keys():
        if key in manifest:
            raise ValueError('conflicting legacy policy field: ' + key)
    licensed = schema == 'patterboxpdf-source-package/v2'
    if licensed:
        for name, digest in LICENSE_HASHES.items():
            if name not in files or sha(files[name]) != digest:
                raise ValueError('missing or changed root license: ' + name)
    expected_license = LICENSE_EXPRESSION if licensed else 'UNLICENSED'
    workspace = tomllib.loads(files['Cargo.toml'].decode('utf-8'))['workspace']
    if workspace['package']['license'] != expected_license:
        raise ValueError('workspace license disagrees with package policy')
    members = [member + '/Cargo.toml' for member in workspace['members']]
    members.append('examples/standalone-client/Cargo.toml')
    notices = json.loads(files['third_party/cargo-notices/dependencies.json'])
    if licensed and any(not asset.get('provenance_sha256') for asset in notices['example_assets']):
        raise ValueError('licensed asset provenance identity is missing')
    owned = {(row['name'], row['version']): row for row in notices['dependencies'] if row['source'] is None}
    identities = set()
    for name in members:
        package = tomllib.loads(files[name].decode('utf-8'))['package']
        effective = package.get('license')
        if effective == {'workspace': True}:
            effective = workspace['package']['license']
        if effective != expected_license or package.get('publish') is not False:
            raise ValueError('first-party license/publication policy disagreement: ' + name)
        version = package.get('version')
        if version == {'workspace': True}:
            version = workspace['package']['version']
        identity = (package['name'], version)
        identities.add(identity)
        row = owned.get(identity)
        # v1 retained collection predates later manifest maintenance; preserve
        # its original notice policy while v2 binds current effective metadata.
        if licensed and (row is None or row['license'] != effective or row['cargo_manifest_sha256'] != sha(files[name])):
            raise ValueError('first-party notice identity disagreement: ' + name)
    if licensed and set(owned) != identities:
        raise ValueError('first-party notice coverage disagreement')


def sha(data):
    return hashlib.sha256(data).hexdigest()


def audit_entry(name, data):
    path = PurePosixPath(name)
    if not name or "\\" in name or ":" in name or path.is_absolute() or any(p in {"", ".", ".."} for p in name.split("/")):
        raise ValueError(f"unsafe package path: {name}")
    if any(p in FORBIDDEN_PARTS or p.startswith((".env", "callas-")) for p in path.parts):
        raise ValueError(f"private package path: {name}")
    if path.suffix.lower() in FORBIDDEN_SUFFIXES or path.name == "AGENTS.md":
        raise ValueError(f"non-source payload: {name}")
    if path.suffix.lower() in TEXT_SUFFIXES:
        content = data.decode("utf-8")
        if any(m.group(1).lower() not in {"testuser", "user", "username", "public"}
               for m in HOME_PATH.finditer(content)):
            raise ValueError(f"machine home path in public content: {name}")


def audit_notices(files, *, require_union=True):
    notice_root = 'third_party/cargo-notices/'
    notices = json.loads(files[notice_root + 'dependencies.json'])
    if notices['cargo_lock_sha256'] != sha(files['Cargo.lock']):
        raise ValueError('dependency notices refer to a different Cargo lockfile')
    locked = set()
    for lockfile in ('Cargo.lock', 'examples/standalone-client/Cargo.lock'):
        lock = tomllib.loads(files[lockfile].decode())
        locked.update((row['name'], row['version']) for row in lock['package'])
    if require_union:
        if notices.get('includes_dev_dependencies') is not True or notices.get('all_workspace_members') is not True:
            raise ValueError('source package requires workspace and development notice coverage')
        audit_notice_coverage(notices, locked)
        if notices['client_cargo_lock_sha256'] != sha(files['examples/standalone-client/Cargo.lock']):
            raise ValueError('dependency notices refer to a different client lockfile')
        if files[notice_root+'Cargo.lock'] != files['Cargo.lock'] or files[notice_root+'client-Cargo.lock'] != files['examples/standalone-client/Cargo.lock']:
            raise ValueError('retained notice lockfile identity disagreement')
    elif not {(row['name'], row['version']) for row in notices['dependencies']}.issubset(locked):
        raise ValueError('dependency notices include an unpinned package')
    def check(name, expected):
        if name not in files or sha(files[name]) != expected:
            raise ValueError(f'missing or changed notice/asset: {name}')
    for row in notices['dependencies']:
        if row['source'] is not None and (row.get('notice_gap') or not row['license_texts']):
            raise ValueError('third-party notice gap')
        for notice in row['license_texts']:
            check(notice_root + notice['path'], notice['sha256'])
    for asset in notices['example_assets']:
        check(asset['path'], asset['sha256'])
        if 'license_notice' in asset:
            check(notice_root + asset['license_notice'], asset['license_notice_sha256'])
        if notice_root + asset['provenance'] not in files:
            raise ValueError('asset provenance absent from package')
        if 'provenance_sha256' in asset:
            check(notice_root + asset['provenance'], asset['provenance_sha256'])
    for notice in notices['canonical_terms']:
        check(notice_root + notice['path'], notice['sha256'])
    standard = notices['rust_standard_library']
    check(notice_root + standard['notice'], standard['sha256'])
    for notice in standard.get('license_texts', []):
        check(notice_root + notice['path'], notice['sha256'])
    for notice in json.loads(files['third_party/upstream/receipt.json'])['notices']:
        check(notice['path'], notice['sha256'])


def audit_notice_coverage(notices, locked):
    if notices.get('schema') != 'patterboxpdf-licenses/v2':
        raise ValueError('dependency notice target coverage schema is missing')
    targets = notices.get('supported_targets', [])
    closures = notices.get('target_closures', [])
    if len(targets) != len(SUPPORTED_TARGETS) or set(targets) != set(SUPPORTED_TARGETS):
        raise ValueError('dependency notice target coverage disagreement')
    if len(closures) != len(targets) or {row['target'] for row in closures} != set(targets):
        raise ValueError('dependency notice target closure disagreement')
    inputs = notices.get('metadata_inputs')
    if (not isinstance(inputs, list) or len(inputs) != 2*len(targets)
            or any(not isinstance(row, dict) or type(row.get('additional_workspace')) is not bool
                   or not re.fullmatch(r'[0-9a-f]{64}', row.get('sha256', '')) or not row.get('roots') for row in inputs)
            or {(row['target'], row['additional_workspace']) for row in inputs} != {(target, client) for target in targets for client in (False, True)}):
        raise ValueError('dependency metadata input identity disagreement')
    expected = {}
    for closure in closures:
        identities = [tuple(row) for row in closure['packages']]
        if not identities or len(identities) != len(set(identities)):
            raise ValueError('dependency notice closure coverage disagreement')
        for identity in identities:
            expected.setdefault(identity, set()).add(closure['target'])
    recorded = {(row['name'], row['version']): row for row in notices['dependencies']}
    if len(recorded) != len(notices['dependencies']) or set(recorded) != set(expected):
        raise ValueError('dependency notice coverage disagreement')
    if not set(recorded).issubset(locked):
        raise ValueError('dependency notices include an unpinned package')
    for identity, row in recorded.items():
        if len(row.get('targets', [])) != len(expected[identity]) or set(row['targets']) != expected[identity]:
            raise ValueError('dependency notice package target disagreement')


def audit_public_inventory(inventory):
    allowed = {'schema', 'scope', 'retained_topic_authority', 'normative_pdfx4_coverage_complete',
               'normative_requirements', 'topics', 'writer_contracts', 'library_contracts', 'latest_focused_repair',
               'coverage_assurance'}
    if (inventory.get('schema') != 'pdf-library-requirements/v1' or set(inventory) - allowed
            or inventory.get('normative_requirements') != [] or 'coverage_assurance' not in inventory):
        raise ValueError('source packaging requires the neutral public inventory')


def declared_paths(workspace, notices, upstream):
    """Fixed public documents and the exact notice files named by their inventory."""
    paths = set(ROOT_FILES)
    paths.update("docs/" + p for p in PUBLIC_DOCS)
    paths.update("scripts/" + p for p in PUBLIC_SCRIPTS)
    paths.update("tests/unit/" + p for p in PUBLIC_TESTS)
    paths.update("docs/verification/current/" + p for p in PUBLIC_RECEIPTS)
    paths.add('crates/linux-worker-job/README.md')
    for member in workspace["workspace"]["members"]:
        paths.add(member + "/Cargo.toml")
    paths.update('examples/standalone-client/'+name for name in ('Cargo.toml', 'Cargo.lock', 'README.md'))
    prefix = 'third_party/cargo-notices/'
    paths.update((prefix+'dependencies.json', prefix+'Cargo.lock', prefix+'client-Cargo.lock',
                  'third_party/upstream/receipt.json', 'third_party/qpdf/NOTICE.md'))
    for row in notices['dependencies']:
        paths.update(prefix+notice['path'] for notice in row['license_texts'])
    for row in notices['example_assets']:
        paths.add(row['path'])
        paths.add(prefix+row['provenance'])
        if 'license_notice' in row:
            paths.add(prefix+row['license_notice'])
    paths.update(prefix+row['path'] for row in notices['canonical_terms'])
    standard = notices['rust_standard_library']
    paths.add(prefix+standard['notice'])
    paths.update(prefix+row['path'] for row in standard.get('license_texts', []))
    paths.update(row['path'] for row in upstream['notices'])
    return paths


def source_roots(workspace):
    members = workspace['workspace']['members']
    for member in members:
        audit_entry(member+'/Cargo.toml', b'')
        if not member.startswith(('crates/', 'apps/')) or len(member.split('/')) != 2:
            raise ValueError('unsupported source workspace member')
    return [member+'/'+subtree+'/' for member in members for subtree in ('src', 'tests', 'rules')]+[
        'examples/standalone-client/src/', 'examples/standalone-client/examples/']


def audit_payload_paths(files):
    workspace = tomllib.loads(files['Cargo.toml'].decode())
    required = declared_paths(workspace, json.loads(files['third_party/cargo-notices/dependencies.json']),
                              json.loads(files['third_party/upstream/receipt.json']))
    roots = source_roots(workspace)
    build_scripts = {member+'/build.rs' for member in workspace['workspace']['members']}
    if not required.issubset(files):
        raise ValueError('required public package file is missing')
    for name in files:
        if name not in required and name not in build_scripts and not name.startswith(tuple(roots)):
            raise ValueError('file is outside the public package allowlist: '+name)


def public_paths(root):
    """Select declared source trees; receipts never expand by traversal."""
    workspace = tomllib.loads((root/'Cargo.toml').read_text(encoding='utf-8'))
    paths = declared_paths(workspace, json.loads((root/'third_party/cargo-notices/dependencies.json').read_bytes()),
                           json.loads((root/'third_party/upstream/receipt.json').read_bytes()))
    for member in workspace['workspace']['members']:
        if (root/member/'build.rs').is_file():
            paths.add(member+'/build.rs')
    for subtree in source_roots(workspace):
        directory = root/subtree
        if directory.is_dir():
            paths.update(path.relative_to(root).as_posix() for path in directory.rglob('*')
                         if path.is_file() and not any(part in FORBIDDEN_PARTS for part in path.relative_to(directory).parts))
    return paths


def collect(root):
    paths = public_paths(root)
    audit_public_inventory(json.loads((root/'docs/requirements/pdfx4-inventory.json').read_bytes()))
    files = {}
    for name in sorted(paths):
        source = root / name
        if not source.is_file() or source.is_symlink() or not source.resolve().is_relative_to(root):
            raise ValueError(f"missing, linked or escaping source: {name}")
        if any(parent.is_symlink() for parent in source.parents if parent != root and parent.is_relative_to(root)):
            raise ValueError(f"linked package directory: {name}")
        data = source.read_bytes()
        audit_entry(name, data)
        files[name] = data
    audit_policy({'schema': 'patterboxpdf-source-package/v2', **POLICIES['patterboxpdf-source-package/v2']}, files)
    audit_payload_paths(files)
    audit_notices(files)
    return tomllib.loads(files['Cargo.toml'].decode()), files


def verify_archive(archive):
    with zipfile.ZipFile(archive) as package:
        names = package.namelist()
        if len(names) != len(set(names)):
            raise ValueError("duplicate package entries")
        manifest = json.loads(package.read("source-manifest.json"))
        expected = {row["path"]: row for row in manifest["files"]}
        if len(expected) != len(manifest["files"]) or set(names) != {*expected, "source-manifest.json"}:
            raise ValueError("package and inventory disagree")
        for info in package.infolist():
            if (info.external_attr >> 16) & 0o170000 == 0o120000:
                raise ValueError("linked ZIP entry")
            data = package.read(info)
            audit_entry(info.filename, data)
            if info.filename != "source-manifest.json":
                row = expected[info.filename]
                if len(data) != row["bytes"] or sha(data) != row["sha256"]:
                    raise ValueError("package file identity disagreement")
        files = {name: package.read(name) for name in expected}
        audit_policy(manifest, files)
        if manifest['schema'] == 'patterboxpdf-source-package/v2':
            audit_public_inventory(json.loads(files['docs/requirements/pdfx4-inventory.json']))
            audit_payload_paths(files)
        audit_notices(files, require_union=manifest['schema'] == 'patterboxpdf-source-package/v2')
        return manifest


def build(root, output):
    root = root.resolve(strict=True)
    workspace, files = collect(root)
    manifest = {"schema": "patterboxpdf-source-package/v2", "version": workspace["workspace"]["package"]["version"],
                **POLICIES["patterboxpdf-source-package/v2"], "distribution": "source only; dependencies acquired separately",
                "files": [{"path": name, "bytes": len(data), "sha256": sha(data)} for name, data in files.items()]}
    payload = json.dumps(manifest, indent=2, sort_keys=True).encode() + b"\n"
    output.mkdir()  # Refuse replacement of previous attempts.
    archive = output / f"patterboxpdf-{manifest['version']}-source.zip"
    with zipfile.ZipFile(archive, "x", compression=zipfile.ZIP_DEFLATED) as package:
        for name, data in [*files.items(), ("source-manifest.json", payload)]:
            info = zipfile.ZipInfo(name, date_time=(1980, 1, 1, 0, 0, 0))
            info.compress_type = zipfile.ZIP_DEFLATED
            info.external_attr = 0o100644 << 16
            package.writestr(info, data)
    verify_archive(archive)
    # Also catch source changes that raced package collection.
    if any((root / name).read_bytes() != data for name, data in files.items()):
        raise ValueError("source changed during package preparation")
    receipt = {"schema": "patterboxpdf-source-package-receipt/v2", "archive": archive.name,
               "sha256": sha(archive.read_bytes()), "files": len(files),
               "source_manifest_sha256": sha(payload), **POLICIES["patterboxpdf-source-package/v2"], "isolated_build_verified": False}
    (output / "receipt.json").write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    return receipt


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("output", type=Path, nargs="?")
    parser.add_argument("--verify", type=Path, help="Audit an existing archive without extracting it")
    arguments = parser.parse_args()
    if arguments.verify:
        result = verify_archive(arguments.verify)
        print(json.dumps({"verified_files": len(result["files"]), "status": result["status"]}))
    elif arguments.output:
        print(json.dumps(build(Path(__file__).resolve().parents[1], arguments.output), indent=2))
    else:
        parser.error("provide a fresh output directory or --verify archive")
