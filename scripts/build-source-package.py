"""Prepare a licensed source ZIP from an explicit public file allowlist.

No dependency downloads, binary redistribution or publication occur here. A fresh output directory preserves every previous package attempt.
"""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import re
import struct
import tomllib
import zipfile

ROOT_FILES = (".gitattributes", ".gitignore", "LICENSE-MIT", "LICENSE-APACHE", "Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "README.md", "ROADMAP.md")
PUBLIC_DOCS = ("architecture.md", "development.md", "api.md", "release.md",
               "supported-platforms.md", "verification-status.md", "color-behavior.md", "proofing.md", "requirements/README.md",
               "requirements/public-technical-sources.md", "requirements/supported-output-review.md",
               "requirements/pdfx4-inventory.json", "requirements/public-output-rules.json",
               "requirements/generation-contract.json")
PUBLIC_SCRIPTS = ("build-source-package.py", "verify-requirements.py", "stage-workers.py", "verify-public-api.py",
                  "verify-feature-gallery.py", "verify-feature-gallery-appearance.py", "cargo-xwin-wsl.cmd",
                  "verify-cmyk-gradients.py", "verify-pdfx-supported-output.py", "pdf_font_program_rules.py",
                  "pdf_truetype_hint_execution.py", "verify-linux-runtime.py",
                  "pdf_text_metadata_rules.py", "verify-font-metrics.py", "verify-output-rules.py",
                  "collect-pdf-candidate-licenses.py", "pdf_numeric_rules.py",
                  "verify-icc-profile.py", "verify-accepted-inputs.py", "verify-composition-corpus.py",
                  "pdf_proofing.py", "proof-pdf.py", "verify-pdf-proofing.py")
PUBLIC_TESTS = ("source-package.test.py", "font-map-metadata.test.py", "worker-staging.test.py",
                "pdfx-supported-output.test.py", "font-program-rules.test.py", "cmyk-gradient-verification.test.py",
                "truetype-hint-execution.test.py", "linux-runtime-verification.test.py",
                "text-metadata-rules.test.py", "font-metrics-verification.test.py", "output-rules.test.py",
                "icc-profile.test.py", "accepted-inputs.test.py",
                "pdf-proofing.test.py", "pdf-proofing-verification.test.py")
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
    "accepted-inputs-20261016/supported-inputs.json", "accepted-inputs-20261016/color.json",
    "accepted-inputs-20261016/linux.json", "accepted-inputs-20261016/windows.json",
    "proofing-20261017/windows.json", "proofing-20261017/ubuntu.json", "proofing-20261017/source-package.json",
    "proofing-20261017-r2/windows.json", "proofing-20261017-r2/ubuntu.json", "proofing-20261017-r2/source-package.json",
)
SUPPORTED_TARGETS = ("x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu")
FORBIDDEN_PARTS = {".git", ".codex", ".agents", ".tools", "target", "artifacts", "node_modules", "__pycache__"}
FORBIDDEN_SUFFIXES = {".exe", ".dll", ".pyd", ".so", ".dylib", ".kfpx", ".pyc"}
TEXT_SUFFIXES = {".rs", ".toml", ".json", ".md", ".txt", ".py", ".mjs", ".cmd", ".lock", ".html"}
HOME_PATH = re.compile(r"(?i)(?:[a-z]:[\\/]+Users[\\/]+|/mnt/[a-z]/Users/|/home/)([a-z0-9_.-]+)")
FONT_CONTROL_ROOT = "apps/font-worker/src/tests/fixtures/"
COMPONENT_CONTROL_ROOT = "apps/font-worker/src/tests/components/"
CHARACTER_MAPPING_FILES = ('third_party/character-mappings/mac-roman-provenance.json',
                           'third_party/character-mappings/NOTICE.txt')


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
    audit_font_control_notices(files, notices)
    audit_component_control_notices(files, notices)
    audit_character_mapping_notices(files, notices)


def audit_character_mapping_notices(files, notices):
    provenance_path, notice_path = CHARACTER_MAPPING_FILES
    if provenance_path not in files or notice_path not in files:
        raise ValueError('character mapping provenance or notice missing')
    provenance = json.loads(files[provenance_path])
    implementation = provenance['implementation']
    path = 'apps/font-worker/src/truetype_program.rs'
    source = files.get(path, b'')
    block = re.search(rb'const MAC_ROMAN:\s*\[char;\s*128\]\s*=\s*\[(.*?)\];', source, re.S)
    values = [int(value, 16) for value in re.findall(rb"'\\u\{([0-9a-fA-F]+)\}'", block[1])] if block else []
    rows = [row for row in notices['example_assets'] if row['path'] == path]
    if (provenance.get('schema') != 'pdf-library-character-mapping-provenance/v1'
            or implementation['path'] != path or implementation['first_byte'] != 128 or implementation['entries'] != 128
            or len(values) != 128 or sha(struct.pack('>128I', *values)) != implementation['codepoints_sha256']
            or provenance.get('license_expression') != 'Unicode-3.0 AND PSF-2.0'
            or provenance['notice']['path'] != notice_path or provenance['notice']['sha256'] != sha(files[notice_path])
            or len(rows) != 1):
        raise ValueError('character mapping data or notice coverage disagreement')
    row = rows[0]
    prefix = 'third_party/cargo-notices/'
    if (row['sha256'] != sha(source) or row.get('license_expression') != provenance['license_expression']
            or files.get(prefix+row['provenance']) != files[provenance_path]
            or files.get(prefix+row.get('license_notice', '')) != files[notice_path]):
        raise ValueError('character mapping upstream notice disagreement')


def audit_font_control_notices(files, notices):
    programs = {name for name in files if name.startswith(FONT_CONTROL_ROOT)
                and PurePosixPath(name).suffix in {'.otf', '.ttf'}}
    if not programs:
        return
    provenance_path = FONT_CONTROL_ROOT+'provenance.json'
    provenance = json.loads(files[provenance_path])
    expected = {FONT_CONTROL_ROOT+name: digest for name, digest in provenance['controls'].items()}
    if set(expected) != programs:
        raise ValueError('font control provenance coverage disagreement')
    assets = {row['path']: row for row in notices['example_assets']}
    for name, digest in expected.items():
        if not re.fullmatch(r'[^/\\]+\.(?:otf|ttf)', name[len(FONT_CONTROL_ROOT):]):
            raise ValueError('unsafe font control provenance path')
        row = assets.get(name)
        if (sha(files[name]) != digest or row is None or row['sha256'] != digest
                or files['third_party/cargo-notices/'+row['provenance']] != files[provenance_path]):
            raise ValueError('font control asset notice disagreement: '+name)
        if name.endswith('.otf'):
            notice = row.get('license_notice')
            if (not notice or files['third_party/cargo-notices/'+notice] != files[FONT_CONTROL_ROOT+'LICENSE-OFL']
                    or files[FONT_CONTROL_ROOT+'LICENSE-OFL'] != files['crates/layout-core/tests/fixtures/LICENSE-NotoSansCJK']):
                raise ValueError('font control OFL notice disagreement: '+name)
        elif row.get('scope') != 'project-generated font control; MIT OR Apache-2.0':
            raise ValueError('original font control license disagreement: '+name)


def audit_component_control_notices(files, notices):
    programs = {name for name in files if name.startswith(COMPONENT_CONTROL_ROOT)
                and PurePosixPath(name).suffix in {'.otf', '.ttf'}}
    if not programs:
        return
    provenance_path = COMPONENT_CONTROL_ROOT+'provenance.json'
    if provenance_path not in files:
        raise ValueError('component control provenance missing')
    provenance = json.loads(files[provenance_path])
    controls = provenance.get('controls')
    if (provenance.get('schema') != 'pdf-library-original-component-controls/v1'
            or not isinstance(controls, dict) or not controls):
        raise ValueError('component control provenance schema disagreement')
    if any(not re.fullmatch(r'[^/\\]+\.ttf', name) for name in controls):
        raise ValueError('unsafe component control provenance path')
    if {COMPONENT_CONTROL_ROOT+name for name in controls} != programs:
        raise ValueError('component control provenance coverage disagreement')
    assets = {row['path']: row for row in notices['example_assets']}
    for name, control in controls.items():
        path = COMPONENT_CONTROL_ROOT+name
        row = assets.get(path)
        if (not isinstance(control, dict) or control.get('sha256') != sha(files[path])
                or row is None or row['sha256'] != control['sha256']
                or files.get('third_party/cargo-notices/'+row['provenance']) != files[provenance_path]):
            raise ValueError('component control asset notice disagreement: '+name)
        if (row.get('scope') != 'project-generated component font control; MIT OR Apache-2.0'
                or 'license_notice' in row):
            raise ValueError('original component control license disagreement: '+name)


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
    paths.update(CHARACTER_MAPPING_FILES)
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
    audit_proofing_evidence(files)
    return tomllib.loads(files['Cargo.toml'].decode()), files


def audit_proofing_evidence(files):
    """Reject stale or failed current proofing evidence, even with a fresh ZIP inventory.

    R1 receipts and the original failed reader comparisons remain historical.
    R2 qualifies the delivered companion and measurements; exact archive build
    isolation still needs its separate adjacent receipt (no self-hash claim).
    """
    prefix = 'docs/verification/current/proofing-20261017-r2/'
    programs = ('scripts/pdf_proofing.py', 'scripts/proof-pdf.py', 'scripts/verify-pdf-proofing.py')
    source_paths = (*programs, 'scripts/build-source-package.py', 'tests/unit/pdf-proofing.test.py',
                    'tests/unit/pdf-proofing-verification.test.py', 'tests/unit/source-package.test.py')
    native_paths = (prefix+'windows.json', prefix+'ubuntu.json')

    def require(condition, message):
        if not condition:
            raise ValueError('proofing evidence: '+message)

    def bindings(rows, paths, label, sizes=False):
        require(isinstance(rows, list) and len(rows) == len(paths)
                and {row['path'] for row in rows} == set(paths), label+' coverage disagreement')
        for row in rows:
            name = row['path']
            require(name in files and row['sha256'] == sha(files[name]), label+' stale identity: '+name)
            if sizes:
                require(type(row['bytes']) is int and row['bytes'] == len(files[name]), label+' size disagreement: '+name)

    def identity(row):
        require(isinstance(row, dict) and isinstance(row.get('file'), str) and row['file']
                and re.fullmatch(r'[0-9a-f]{64}', row.get('sha256', '')), 'missing recorded byte identity')
        return row['sha256']

    def rgb(value):
        require(isinstance(value, list) and len(value) == 3
                and all(type(c) is int and 0 <= c <= 255 for c in value), 'invalid RGB measurement')
        return value

    def measured(rows, expected, keys, label):
        require(isinstance(rows, list) and len(rows) == len(expected)
                and {tuple(row[k] for k in keys) for row in rows} == expected, label+' sample coverage disagreement')
        errors = []
        for row in rows:
            error = max(abs(a-b) for a, b in zip(rgb(row['actual_rgb']), rgb(row['reference_rgb'])))
            require(type(row['maximum_error']) is int and row['maximum_error'] == error <= 4,
                    label+' recorded channel error exceeds fixed bound or disagrees')
            errors.append(error)
        return max(errors)

    try:
        source = json.loads(files[prefix+'source-package.json'])
        require(source['schema'] == 'patterboxpdf-proofing-source-qualification/v1'
                and source['status'] == 'source-inputs-qualified', 'source qualification status disagreement')
        bindings(source['source_bindings'], source_paths, 'source bindings')
        bindings(source['preview_qualifications'], native_paths, 'native qualifications')
        generation = source['native_generation']
        inputs = {name for name in files if name.startswith(('apps/', 'crates/', 'examples/standalone-client/'))
                  or name in ('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml')}
        require(generation['all_inputs_unchanged'] is True, 'native generation inputs changed')
        bindings(generation['inputs'], inputs, 'native generation inputs', sizes=True)
        isolation = source['source_archive_isolation']
        require(isolation['required'] is True and isolation['asserted_by_this_record'] is False,
                'exact archive isolation boundary disagreement')
        history = source['historical_color']
        historical_path = 'docs/verification/current/accepted-inputs-20261016/color.json'
        require(history['receipt'] == historical_path and history['sha256'] == sha(files[historical_path])
                and history['original_three_default_comparison_failures_preserved'] is True,
                'historical color identity disagreement')
        times = (.125, .25, .5, .75, .875)
        cases = ('axial', 'radial', 'hard-stop', 'constant-alpha', 'varying-alpha',
                 'radial-varying-alpha', 'group-opacity', 'varying-alpha-group-opacity')
        diagnostic_keys = {(page, space, case, t)
                           for page, space in ((1, '/DeviceCMYK'), (2, '/DeviceRGB'))
                           for case in cases for t in times if case != 'hard-stop' or t != .5}
        pinned = {'diagnostic': '1124ab1ab80aa179cc1c1b222c46a37afa542ccd41a6180c9a22b11852c8d342',
                  'ordinary': 'ac8a687a3f91cde2cf768d0099ff37e1a8ce722448a6b4062c51ef2aada10fb8',
                  'print': '5b6a94bf69381718c1eff572305a13ce71b78f608657ab34246d96750206ecab'}
        pages = {'diagnostic': [1, 2], 'ordinary': [1, 2, 3], 'print': [1, 2, 3], 'flat-controls': [1, 2]}
        pages.update({name+'-repeat': selected for name, selected in list(pages.items())})
        pages['alternate-display'] = [1]
        display = sha(files['crates/print-core/tests/fixtures/generated-srgb.icc'])
        for path, target in zip(native_paths, SUPPORTED_TARGETS):
            native = json.loads(files[path])
            require(native['schema'] == 'patterboxpdf-controlled-proofing-verification/v1'
                    and native['passed'] is True and native['target'] == target, 'native qualification did not pass: '+path)
            bindings(native['source_bindings'], programs, 'native source bindings')
            for key, name in (('companion', programs[0]), ('verifier', programs[2])):
                require(identity(native[key]) == sha(files[name]), 'stale native '+key)
            identity(native['renderer'])
            require(native['renderer']['version'] and identity(native['littlecms'])
                    and type(native['littlecms']['encoded_version']) is int, 'missing renderer/CMM identity')
            require(native['reference_conditions'] == {'intent': 1, 'flags': 8448, 'encoding': 'round-u8',
                    'maximum_channel_error': 4, 'diagnostic_background_rgb': [204, 204, 204],
                    'diagnostic_compositing': 'encoded-RGB source-over on opaque Splash RGB8; paint alpha times isolated group opacity once'},
                    'fixed reference conditions changed')
            previews = native['previews']
            require(set(previews) == set(pages), 'preview coverage disagreement')
            print_profile = identity(previews['diagnostic']['print_profile'])
            alternate = identity(native['alternate_display']['profile'])
            for name, selected in pages.items():
                preview = previews[name]
                identity(preview['manifest'])
                pdf = identity(preview['source_pdf'])
                base = name.removesuffix('-repeat')
                if base in pinned:
                    require(pdf == pinned[base], 'pinned preview PDF changed')
                require(identity(preview['print_profile']) == print_profile
                        and identity(preview['display_profile']) == (alternate if name == 'alternate-display' else display),
                        'preview profile identity disagreement')
                require(preview['pages'] == selected and [row['page'] for row in preview['outputs']] == selected,
                        'preview page coverage disagreement')
                runtime = preview['runtime']
                require(runtime['platform'].startswith('Windows' if target == SUPPORTED_TARGETS[0] else 'Linux')
                        and runtime['machine'] and runtime['python'] and runtime['pillow'] and runtime['pypdf']
                        and runtime['pillow_littlecms'] and identity(runtime['executable']), 'missing native runtime identity')
                condition = preview['print_condition']
                require(condition['kind'] == ('embedded-output-intent-match' if base == 'print' else 'selected-default-cmyk'),
                        'preview print condition disagreement')
                if base == 'print':
                    require(condition['embedded_profiles'] and all(row['sha256'] == print_profile
                            for row in condition['embedded_profiles']), 'embedded print profile disagreement')
                for row in preview['outputs']:
                    identity(row)
                    require(re.fullmatch(r'[0-9a-f]{64}', row['pixel_sha256'])
                            and row['icc_sha256'] == preview['display_profile']['sha256']
                            and len(row['pixel_dimensions']) == 2
                            and all(type(n) is int and n > 0 for n in row['pixel_dimensions']), 'invalid preview output record')
            summary = native['summary']
            require(summary['preview_runs'] == 9 and summary['pages_checked'] == 21, 'preview summary coverage disagreement')
            groups = [('diagnostic_gradient_flat_controls', diagnostic_keys,
                       ('page', 'page_blending_space', 'case', 't'), 'diagnostic'),
                      ('independent_diagnostic_appearance_controls',
                       {(*key, region) for key in diagnostic_keys for region in ('gradient', 'flat')},
                       ('page', 'page_blending_space', 'case', 't', 'region'), 'independent_diagnostic'),
                      ('independent_littlecms_flat_controls', {(n, cs) for n in range(10)
                           for cs in ('default-cmyk', 'icc-based-cmyk')}, ('sample', 'color_space'), 'direct_littlecms'),
                      ('retained_gallery_gradient_flat_controls', {(mode, page, kind, t)
                           for mode, page in (('ordinary', 2), ('print', 1), ('print', 2))
                           for kind in ('axial', 'radial') for t in times}, ('mode', 'page', 'geometry', 't'), 'gallery')]
            for field, expected, keys, label in groups:
                error = measured(native[field], expected, keys, label)
                require(summary[label+'_samples'] == len(expected) and summary[label+'_maximum_error'] == error,
                        label+' summary disagrees with measured records')
            for row in native['independent_diagnostic_appearance_controls']:
                case, t = row['case'], row['t']
                fraction = (0. if t < .5 else 1.) if case == 'hard-stop' else t
                alpha = .5 if case == 'constant-alpha' else t if case in (
                    'varying-alpha', 'radial-varying-alpha', 'varying-alpha-group-opacity') else 1.
                group = .5 if case in ('group-opacity', 'varying-alpha-group-opacity') else 1.
                expected = [round(alpha*group*c+(1-alpha*group)*204) for c in rgb(row['foreground_rgb'])]
                require(row['foreground_components'] == [fraction, 1-fraction, fraction, .25*fraction]
                        and row['background_rgb'] == [204, 204, 204] and row['paint_alpha'] == alpha
                        and row['group_opacity'] == group and row['effective_alpha'] == alpha*group
                        and row['reference_rgb'] == expected, 'independent diagnostic reference disagreement')
            alternate_samples = native['alternate_display']['samples']
            measured(alternate_samples, {(n,) for n in range(10)}, ('sample',), 'alternate display')
            require(alternate != display and summary['alternate_display_samples'] == 10, 'alternate display identity disagreement')
            for row in alternate_samples:
                observed = max(abs(a-b) for a, b in zip(rgb(row['original_rgb']), rgb(row['actual_rgb'])))
                require(row['observed_profile_difference'] == observed, 'alternate display contrast disagreement')
            require(any(row['predicted_profile_difference'] > 8 and row['observed_profile_difference'] > 4
                        for row in alternate_samples), 'display profile sensitivity failed')
            contrast = native['explicit_default_cmyk_contrast']['samples']
            require(len(contrast) == 10 and {row['sample'] for row in contrast} == set(range(10)),
                    'DefaultCMYK contrast coverage disagreement')
            for row in contrast:
                difference = max(abs(a-b) for a, b in zip(rgb(row['managed_rgb']), rgb(row['without_default_cmyk_rgb'])))
                require(row['maximum_contrast'] == difference, 'DefaultCMYK contrast disagreement')
            require(any(row['maximum_contrast'] > 4 for row in contrast), 'DefaultCMYK profile sensitivity failed')
            wrong = native['wrong_print_output_intent']
            require(identity(wrong['profile']) != print_profile and wrong['success_manifest_exists'] is False
                    and 'does not match the embedded OutputIntent' in wrong['refusal'], 'wrong-profile refusal failed')
            repeats = native['repeatability']
            expected_repeats = {(name, page) for name in ('diagnostic', 'ordinary', 'print', 'flat-controls') for page in pages[name]}
            require(len(repeats) == 10 and {(row['preview'], row['page']) for row in repeats} == expected_repeats
                    and summary['repeat_pages'] == 10, 'repeatability coverage disagreement')
            for row in repeats:
                first = next(output for output in previews[row['preview']]['outputs'] if output['page'] == row['page'])
                second = next(output for output in previews[row['preview']+'-repeat']['outputs'] if output['page'] == row['page'])
                require(row['decoded_pixels_equal'] is True and row['exact_icc_equal'] is True
                        and row['pixel_sha256'] == first['pixel_sha256'] == second['pixel_sha256']
                        and row['icc_sha256'] == first['icc_sha256'] == second['icc_sha256']
                        and row['encoded_sha256_first'] == first['sha256']
                        and row['encoded_sha256_repeat'] == second['sha256'], 'repeatability record disagreement')
            retained = native['historical_reader_comparison']
            require(identity(retained['receipt']) == history['sha256'] and retained['default_comparison_passed'] is False
                    and retained['fixed_bounds'] == {'mean_channel_difference_max': 5, 'p95_max_channel_difference_max': 20,
                                                     'pixels_above_10_percent_max': 8}, 'historical comparison changed')
    except (KeyError, TypeError, AttributeError, json.JSONDecodeError) as error:
        raise ValueError('proofing evidence: missing or malformed current qualification record') from error


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
        if manifest['schema'] == 'patterboxpdf-source-package/v2':
            audit_proofing_evidence(files)
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
