"""Build and execute the supported x86_64 Linux resource and isolation workflows.

Run natively on Linux with Python's public-API verification dependencies installed.
The output directory must be new. Cargo targets may be reused; Cargo rebuilds the
current locked source before any test or caller is selected. No kernel fallback,
ignored test, compile-only result or unavailable tool counts as qualification.
"""

import argparse
import ctypes
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import platform
import re
import shutil
import signal
import subprocess
import sys

ROOT = Path(__file__).resolve().parents[1]
TARGET = 'x86_64-unknown-linux-gnu'
WORKERS = ('font', 'color', 'raster')
# Package, Cargo target, target kind, and security/production cases that must run.
SUITES = (
    ('publisher-linux-worker-job', 'linux_isolation', 'test', (
        'actual_kernel_denies_files_symlinks_other_runs_network_processes_exec_and_inherited_fds',
        'immutable_executable_and_object_grants_survive_path_replacement',
        'kernel_memory_cpu_and_independent_wall_limits_are_enforced',
        'drop_and_run_cancellation_close_pipes_and_preserve_an_independent_run',
        'a_full_stdout_pipe_cannot_hold_a_worker_beyond_its_wall_limit',
        'killing_the_host_triggers_os_backed_worker_death',
        'missing_required_kernel_primitives_fail_before_the_parser_marker',
        'incompatible_syscall_architectures_cannot_bypass_the_filter',
        'unrelated_inheritable_pipe_writer_is_closed_while_worker_remains_alive',
        'timestamp_changes_are_denied_for_read_only_and_ungranted_objects',
        'a_1024_descriptor_soft_limit_still_launches_the_restricted_worker')),
    ('font-boundary', 'font_boundary', 'lib', (
        'tests::confined_worker_inspects_an_exact_face_into_an_opaque_receipt',
        'tests::packaged_font_worker_cannot_read_or_write_outside_its_private_run',
        'tests::linux_active_cancellation_reaps_and_removes_partial_outputs',
        'tests::linux_deadline_kills_sleeping_worker_and_cleans_partial_output',
        'tests::linux_deadline_remains_a_timeout_after_the_owner_has_already_reaped',
        'tests::linux_partial_grant_setup_never_runs_and_cleans_up',
        'tests::linux_real_elf_protocol_failures_clean_up_and_allow_fresh_retry')),
    ('font-boundary', 'linux_preparation', 'test', (
        'actual_truetype_and_cff_inspect_subset_and_publication_keep_exact_identities',
        'actual_worker_rejects_synthetic_restricted_rights_without_prepared_receipt')),
    ('color-boundary', 'color_boundary', 'lib', (
        'tests::confined_worker_converts_valid_profile_and_rejects_out_of_bounds_icc_tag',
        'tests::packaged_color_worker_cannot_read_or_write_outside_its_private_run')),
    ('color-boundary', 'linux_preparation', 'test', ('linux_prepares_actual_profile_through_public_boundary',)),
    ('raster-boundary', 'raster_boundary', 'lib', (
        'tests::packaged_raster_worker_decodes_exact_authored_pixel',
        'tests::packaged_raster_worker_cannot_read_or_write_outside_its_private_run')),
    ('raster-boundary', 'linux_preparation', 'test', (
        'oriented_tiff_keeps_straight_alpha_exact_profile_and_receipt_memory_lifetime',)),
    ('validator-boundary', 'validator_boundary', 'lib', (
        'tests::unsupported_validator_fails_before_a_script_can_create_a_marker',)),
    ('validator-boundary', 'unsupported_platform', 'test', (
        'public_validator_explicitly_refuses_the_unqualified_platform',)),
)


def require(condition, message):
    if not condition:
        raise ValueError(message)


def identity(path):
    path = Path(path).resolve(strict=True)
    return {'name': path.name, 'sha256': hashlib.sha256(path.read_bytes()).hexdigest(),
            'bytes': path.stat().st_size}


def elf_identity(path):
    path = Path(path).resolve(strict=True)
    with path.open('rb') as stream:
        header = stream.read(64)
    require(len(header) == 64 and header[:6] == b'\x7fELF\x02\x01'
            and int.from_bytes(header[16:18], 'little') in (2, 3)
            and int.from_bytes(header[18:20], 'little') == 62,
            'expected a native x86_64 little-endian ELF: '+path.name)
    require(os.access(path, os.X_OK), 'ELF is not executable: '+path.name)
    return identity(path)


def kernel_environment():
    require(sys.platform == 'linux' and platform.machine() == 'x86_64',
            'qualification requires native x86_64 Linux execution')
    libc = ctypes.CDLL(None, use_errno=True)
    libc.syscall.restype = ctypes.c_long
    # x86_64 landlock_create_ruleset(NULL, 0, LANDLOCK_CREATE_RULESET_VERSION).
    abi = libc.syscall(ctypes.c_long(444), ctypes.c_void_p(), ctypes.c_size_t(0), ctypes.c_uint(1))
    require(abi >= 3, 'Landlock ABI >=3 is required; query returned '+str(abi))
    return {'system': platform.system(), 'release': platform.release(),
            'version': platform.version(), 'machine': platform.machine(), 'landlock_abi': abi,
            'environment': 'WSL2' if 'microsoft' in platform.release().lower() else 'Linux'}


def execute(command, output, name, timeout=1800):
    """Keep every command's output and kill only its own process group on failure."""
    stdout = output/(name+'.stdout.log')
    stderr = output/(name+'.stderr.log')
    with stdout.open('x', encoding='utf-8') as out, stderr.open('x', encoding='utf-8') as err:
        child = subprocess.Popen([str(value) for value in command], cwd=ROOT,
                                 stdout=out, stderr=err, start_new_session=True)
        try:
            code = child.wait(timeout=timeout)
        except BaseException:
            try:
                os.killpg(child.pid, signal.SIGKILL)
            except ProcessLookupError:
                pass
            child.wait()
            raise
    require(code == 0, name+' failed; inspect '+stderr.name+' and '+stdout.name)
    return stdout.read_text(encoding='utf-8')


def audit_test_result(listing, result, required):
    names = [line.removesuffix(': test') for line in listing.splitlines() if line.endswith(': test')]
    require(names and len(names) == len(set(names)) and set(required).issubset(names),
            'required native tests are missing or duplicated')
    summaries = re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out;', result)
    require(len(summaries) == 1 and tuple(map(int, summaries[0])) == (len(names), 0, 0, 0, 0),
            'native suite must execute every listed test with zero failures, ignores or filters')
    passed = re.findall(r'^test (.+?) \.\.\. ok$', result, re.MULTILINE)
    require(len(passed) == len(names) and set(passed) == set(names), 'native test names and verdicts disagree')
    return {'passed': len(names), 'ignored': 0, 'tests': names}


def select_artifacts(messages, packages):
    artifacts = {}
    for line in messages.splitlines():
        row = json.loads(line)
        if row.get('reason') != 'compiler-artifact' or not row.get('executable') or not row.get('profile', {}).get('test'):
            continue
        package = packages[row['package_id']]
        for kind in row['target']['kind']:
            key = (package, row['target']['name'], kind)
            require(key not in artifacts, 'duplicate compiled test target')
            artifacts[key] = Path(row['executable']).resolve(strict=True)
    require(all(tuple(suite[:3]) in artifacts for suite in SUITES), 'Cargo did not produce every required native suite')
    return artifacts


def source_snapshot():
    paths = {ROOT/'Cargo.toml', ROOT/'Cargo.lock', ROOT/'rust-toolchain.toml'}
    for folder in ['crates', 'apps']+['examples/standalone-client']:
        base = ROOT/folder
        paths.update(path for path in base.rglob('*') if path.is_file()
                     and not {'target', '__pycache__'}.intersection(path.relative_to(base).parts)
                     and not {'evidence', 'artifacts'}.intersection(path.relative_to(base).parts)
                     and (path.suffix in ('.rs', '.toml', '.lock', '.json', '.ttf', '.otf', '.icc') or path.name == 'Cargo.lock'))
    paths.update(ROOT/'scripts'/name for name in ('verify-linux-runtime.py', 'stage-workers.py', 'verify-public-api.py'))
    paths.update(ROOT/name for name in ('crates/layout-core/tests/fixtures/DejaVuSans.ttf',
                 'crates/layout-core/tests/fixtures/NotoSansCJKsc-Regular.otf',
                 'crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc'))
    return [{'path': path.relative_to(ROOT).as_posix(), **identity(path)} for path in sorted(paths)]


def runtime_libraries(executables, ldd, output):
    libraries = {}
    for index, executable in enumerate(executables):
        text = execute([ldd, executable], output, 'runtime-libraries-'+str(index), 30)
        require('not found' not in text, 'a native shared library is unavailable')
        for filename in re.findall(r'(/[^\n]+?)\s+\(0x[0-9a-f]+\)', text):
            library = identity(Path(filename.strip()))
            old = libraries.setdefault(library['name'], library)
            require(old == library, 'runtime shared library identity disagrees')
    require(libraries, 'native runtime library identities were not discovered')
    return [libraries[name] for name in sorted(libraries)]


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--output', type=Path, required=True)
    parser.add_argument('--target-dir', type=Path, required=True)
    parser.add_argument('--cargo', default='cargo')
    parser.add_argument('--rustc', default='rustc')
    parser.add_argument('--qpdf', required=True, help='Native trusted development checker')
    parser.add_argument('--jobs', type=int, default=2)
    args = parser.parse_args()
    environment = kernel_environment()
    require(args.jobs > 0, 'jobs must be positive')
    tools = {}
    for name in ('cargo', 'rustc', 'qpdf'):
        found = shutil.which(getattr(args, name))
        require(found is not None, 'required tool is unavailable: '+name)
        # Rustup selects cargo/rustc by the invoked filename. Hashing may follow
        # its symlink, but execution must retain the proxy's original basename.
        tools[name] = Path(found).absolute()
    elf_identity(tools['qpdf'])
    ldd = shutil.which('ldd')
    require(ldd is not None, 'required runtime dependency inspector is unavailable: ldd')
    # Validate dependencies before exposing an evidence directory.
    import pypdf
    output = args.output.resolve()
    output.mkdir()
    target = args.target_dir.resolve()
    before = source_snapshot()
    versions = {name: execute([path, '--version'], output, name+'-version', 30).strip() for name, path in tools.items()}
    common = ['--release', '--locked', '--offline', '--target', TARGET, '--target-dir', target/'workspace', '-j', args.jobs]
    workers = [part for name in WORKERS for part in ('-p', 'publisher-'+name+'-worker')]
    execute([tools['cargo'], 'build', *common, *workers], output, 'workers-build')
    execute([tools['cargo'], 'build', *common, '-p', 'font-boundary', '--example', 'linux-boundary-probe'],
            output, 'boundary-probe-build')
    metadata = json.loads(execute([tools['cargo'], 'metadata', '--format-version', '1', '--no-deps', '--locked', '--offline'],
                                  output, 'workspace-metadata'))
    packages = {row['id']: row['name'] for row in metadata['packages']}
    selected = sorted({suite[0] for suite in SUITES})
    cargo_args = [part for name in selected for part in ('-p', name)]
    messages = execute([tools['cargo'], 'test', *common, *cargo_args, '--lib', '--tests', '--no-run', '--message-format=json'],
                       output, 'tests-build')
    artifacts = select_artifacts(messages, packages)
    spec = importlib.util.spec_from_file_location('stage_workers', ROOT/'scripts/stage-workers.py')
    staging = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(staging)
    runtime = target/'workspace'/TARGET/'release'
    binary_ids = {name: elf_identity(runtime/('publisher-'+name+'-worker')) for name in WORKERS}
    probes = [elf_identity(runtime/'examples/linux-boundary-probe'), elf_identity(runtime/'publisher-linux-isolation-probe')]
    tests = []
    for package, name, kind, required in SUITES:
        executable = artifacts[package, name, kind]
        binary = elf_identity(executable)
        stage = staging.stage(runtime, executable, WORKERS, test_layout=True, target=TARGET)
        label = package+'-'+name
        listing = execute([executable, '--list', '--format', 'terse'], output, label+'-list', 60)
        result = execute([executable, '--test-threads=1', '--color', 'never'], output, label+'-run', 300)
        tests.append({'package': package, 'target': name, 'kind': kind, 'binary': binary, 'staging': stage,
                      **audit_test_result(listing, result, required)})
        print(label+': '+str(tests[-1]['passed'])+' native tests passed', flush=True)
    client_common = [value for value in common]
    client_common[client_common.index(target/'workspace')] = target/'client'
    execute([tools['cargo'], 'build', '--manifest-path', ROOT/'examples/standalone-client/Cargo.toml', *client_common],
            output, 'client-build')
    client = target/'client'/TARGET/'release/pdf-library-standalone-client'
    client_id = elf_identity(client)
    client_stage = staging.stage(runtime, client, ('font', 'color'), target=TARGET)
    client_metadata = output/'client-metadata.stdout.log'
    execute([tools['cargo'], 'metadata', '--manifest-path', ROOT/'examples/standalone-client/Cargo.toml',
             '--format-version', '1', '--filter-platform', TARGET, '--locked', '--offline'], output, 'client-metadata')
    execute([sys.executable, ROOT/'scripts/verify-public-api.py', '--client', client, '--cargo-metadata', client_metadata,
             '--qpdf', tools['qpdf'], '--output', output/'public-api'], output, 'public-api', 600)
    public = json.loads((output/'public-api/results.json').read_text(encoding='utf-8'))
    require(public['repeatable_bytes'] is True and len(public['worker_failures']) == 3
            and all(row['no_output_exposed'] is True for row in public['worker_failures']), 'public runtime checks incomplete')
    require(before == source_snapshot(), 'source changed during native qualification; rerun against stable bytes')
    for name in WORKERS:
        require(binary_ids[name] == elf_identity(runtime/('publisher-'+name+'-worker')), 'worker changed during native qualification')
    require(client_id == elf_identity(client), 'client changed during native qualification')
    libraries = runtime_libraries([tools['qpdf'], client, *(runtime/('publisher-'+name+'-worker') for name in WORKERS)], ldd, output)
    receipt = {'schema': 'patterboxpdf-linux-runtime/v1', 'status': 'passed', 'target': TARGET,
               'kernel': environment, 'tools': {name: {**identity(path), 'version': versions[name]} for name, path in tools.items()},
               'python': {'version': platform.python_version(), 'pypdf': pypdf.__version__, **identity(sys.executable)},
               'runtime_inspector': identity(ldd), 'runtime_libraries': libraries,
               'sources': before, 'workers': binary_ids, 'test_probes': probes, 'test_suites': tests,
               'native_tests_passed': sum(row['passed'] for row in tests), 'ignored': 0,
               'client': client_id, 'client_staging': client_stage, 'public_api_receipt': identity(output/'public-api/results.json'),
               'pdfs': public['standalone'], 'validator_runtime': 'explicitly unsupported; tested refusal',
               'format_conformance': 'separate exact-output evaluation required'}
    (output/'results.json').write_text(json.dumps(receipt, indent=2)+'\n', encoding='utf-8')
    print(json.dumps({'status': 'passed', 'native_tests_passed': receipt['native_tests_passed'], 'ignored': 0}))


if __name__ == '__main__':
    main()
