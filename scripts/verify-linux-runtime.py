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
    ('icc-profile', 'icc_profile', 'lib', (
        'tests::original_profile_and_generated_edition_headers_have_exact_byte_checks',
        'tests::profile_id_uses_the_edition_recipe_and_zero_retains_its_meaning',
        'tests::dormant_lut8_and_lut16_require_complete_data_and_correct_direction',
        'tests::parametric_power_is_defined_throughout_its_active_interval',
        'tests::v4_viewing_illuminance_matches_luminance_only_when_both_are_present',
        'tests::opaque_private_data_has_generic_framing_and_standard_types_keep_their_schema',
        'tests::adaptation_role_is_checked_when_a_private_tag_visits_the_shared_body_first')),
    ('publisher-font-worker', 'publisher-font-worker', 'bin', (
        'tests::accepted_cff_affine_flex_and_empty_controls_complete',
        'tests::cumulative_fixed_point_deltas_survive_native_outline_measurement',
        'tests::incomplete_and_partial_cff_controls_never_produce_a_prepared_resource',
        'tests::full_truetype_keeps_exact_bytes_and_point_attached_composites',
        'tests::malformed_unused_full_glyph_is_rejected_inside_preparation',
        'tests::retained_full_font_metadata_and_capacity_fail_before_success',
        'tests::authored_high_unencoded_ascent_needs_explicit_classification',
        'tests::facts_none_empty_presence_and_malformed_identity_are_distinct',
        'tests::conflicting_and_unclassified_unicode_aliases_remain_unknown',
        'tests::facts_scope_includes_full_unused_glyphs_and_component_tails',
        'tests::unencoded_only_requests_prepare_without_a_cmap_anchor',
        'tests::diagnostic_exercise_preserves_mapped_preference_and_notdef_only_policy',
        'tests::unencoded_exercise_fallback_preserves_full_bytes_and_retained_authority',
        'tests::cap_height_empty_known_h_is_explicit_zero',
        'tests::cap_height_old_os2_l_reference_is_measured',
        'tests::cap_height_usable_declaration_does_not_require_a_reference',
        'tests::cap_height_declaration_uses_global_upem_units_across_cff_matrices',
        'tests::cap_height_unknown_authority_and_explicit_facts_preserve_program',
        'tests::cap_height_conflicts_and_malformed_facts_refuse_before_output',
        'tests::cap_height_fact_hash_binds_source_face_presence_and_value',
        'tests::cap_height_empty_reference_does_not_classify_unknown_aliases',
        'tests::cap_height_facts_keep_rights_required_full_font_bytes_and_independent_ascent',
        'outline::tests::touching_same_winding_contours_measure_one_filled_union_in_either_order',
        'outline::tests::opposite_winding_cancellation_and_one_ulp_gap_keep_distinct_fill',
        'pdf_metrics::tests::certified_attained_dominance_allows_equality_but_not_uncertain_overlap',
        'pdf_metrics::tests::a_control_point_is_not_an_attained_known_maximum',
        'pdf_metrics::tests::bounded_unicode_roles_do_not_generalize_non_nfd_or_combining_labels')),
    ('publisher-font-worker', 'worker_process', 'test', (
        'unsupported_protocol_fails_before_attempting_to_open_the_source',
        'inspect_process_returns_bounded_path_free_face_facts_without_an_output_file',
        'inspect_process_rejects_malformed_font_with_path_free_structured_failure',
        'worker::truetype_program::tests::name_v1_language_tag_valid_neighbors',
        'worker::truetype_program::tests::windows_name_strings_follow_naming_utf16_instead_of_cmap_codecs',
        'worker::truetype_program::tests::name_v1_language_indices_and_record_extents_are_checked',
        'worker::truetype_program::tests::name_v1_language_tags_require_bcp47_lexical_syntax',
        'worker::truetype_program::tests::empty_composite_cached_rectangle_does_not_enter_aggregate_bounds',
        'worker::tests::valid_version_one_naming_prepares_without_changing_full_source_bytes',
        'worker::tests::known_raw_naming_encodings_prepare_and_inspect_without_a_unicode_alias',
        'worker::tests::retained_postscript_records_cannot_hide_invalid_or_disagreeing_names',
        'worker::tests::unknown_postscript_encoding_preserves_bytes_without_guessing_identity')),
    ('publisher-font-worker', 'components', 'test', (
        'unhinted_component_round_flag_preserves_fractional_design_offset',
        'point_matching_ignores_round_to_grid_flag',
        'child_horizontal_phantoms_are_valid_attachment_anchors',
        'use_my_metrics_selects_component_advance_and_origin',
        'inherited_metrics_remain_original_under_scale_translation_and_recursion',
        'parent_horizontal_phantoms_use_final_composite_point_numbers',
        'first_component_point_matching_remains_malformed_with_phantoms',
        'component_metric_context_preserves_shear_and_positive_scales',
        'vertical_anchor_unknown_coordinates_are_required_only_when_geometry_depends_on_them',
        'unused_source_context_is_deferred_but_all_full_retained_glyphs_are_checked',
        'unresolved_optional_cap_reference_is_never_an_empty_cap_proof',
        'unsupported_vertical_table_version_is_only_needed_by_an_actual_anchor',
        'subset_rights_preserve_all_supported_full_component_counterparts')),
    ('publisher-color-worker', 'publisher-color-worker', 'bin', (
        'tests::inspection_accepts_exact_bidirectional_printer_profile_without_conversion_output',
        'tests::inspection_rejects_tolerated_raw_header_errors_before_attestation',
        'tests::genuine_serialized_editions_pass_exact_checks_and_every_intent_probe')),
    ('print-core', 'print_core', 'lib', (
        'candidate_metadata::tests::leap_and_fractional_seconds_have_one_consistent_pdf_date',
        'profile::tests::shared_bodies_still_obey_each_signature_role',
        'profile::tests::shared_tag_body_work_is_bounded_by_unique_ranges',
        'finished_writer::tests::authored_ascent_print_checks_used_and_registered_unused_fonts',
        'finished_writer::tests::authored_cap_height_print_checks_used_and_registered_unused_fonts')),
    ('print-core', 'publication_fonts', 'test', (
        'automatic_adapter_font_preserves_missing_ascent_authority_before_print',
        'adapter_font_facts_preserve_authority_through_finished_print',
        'adapter_font_facts_validate_resources_before_callback',
        'adapter_font_facts_propagate_native_conflicts')),
    ('print-core', 'finished_numeric', 'test', (
        'accepted_binary32_matrix_keeps_its_nonzero_determinant_in_encoded_decimals',
        'ordinary_neighbors_and_zero_sign_keep_existing_geometry_contract')),
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
        'tests::linux_real_elf_protocol_failures_clean_up_and_allow_fresh_retry',
        'tests::authored_ascent_public_api_preserves_fact_presence_and_authority',
        'tests::authored_ascent_report_hash_and_authority_tampering_is_rejected',
        'tests::authored_ascent_fact_capacity_uses_source_face_and_large_request_transport',
        'tests::authored_cap_height_public_api_preserves_legacy_and_canonical_presence',
        'tests::authored_cap_height_report_binding_rejects_hash_presence_value_and_authority_tampering',
        'tests::worker_failures_preserve_authoring_denials_without_hiding_integrity_codes')),
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
SOURCE_SUPPORT_FILES = (
    'scripts/verify-linux-runtime.py', 'scripts/stage-workers.py', 'scripts/verify-public-api.py',
    'apps/font-worker/src/tests/generate_controls.py', 'apps/font-worker/src/tests/probe_controls.py',
    'apps/font-worker/src/tests/components/generate.py', 'apps/font-worker/src/tests/components/probe.py',
    'scripts/verify-font-metrics.py', 'scripts/verify-pdfx-supported-output.py',
    'scripts/pdf_font_program_rules.py', 'scripts/pdf_text_metadata_rules.py',
    'scripts/pdf_truetype_hint_execution.py', 'scripts/pdf_numeric_rules.py', 'scripts/verify-icc-profile.py',
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
    require(required and len(required) == len(set(required)), 'required native test identities must be nonempty and unique')
    names = [line.removesuffix(': test') for line in listing.splitlines() if line.endswith(': test')]
    require(names and len(names) == len(set(names)) and set(required).issubset(names),
            'required native tests are missing or duplicated')
    summaries = re.findall(r'test result: ok\. (\d+) passed; (\d+) failed; (\d+) ignored; (\d+) measured; (\d+) filtered out;', result)
    require(len(summaries) == 1 and tuple(map(int, summaries[0])) == (len(names), 0, 0, 0, 0),
            'native suite must execute every listed test with zero failures, ignores or filters')
    passed = re.findall(r'^test (.+?) \.\.\. ok$', result, re.MULTILINE)
    require(len(passed) == len(names) and set(passed) == set(names), 'native test names and verdicts disagree')
    return {'passed': len(names), 'failed': 0, 'ignored': 0, 'filtered': 0,
            'required_tests': list(required), 'tests': names}


def select_artifacts(messages, packages):
    artifacts = {}
    for line in messages.splitlines():
        if not line.strip():
            continue
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
    paths.update(ROOT/name for name in SOURCE_SUPPORT_FILES)
    controls = ROOT/'apps/font-worker/src/tests/fixtures'
    paths.update(path for path in controls.rglob('*') if path.is_file()
                 and '__pycache__' not in path.relative_to(controls).parts)
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
    selected = sorted({suite[0] for suite in SUITES if suite[2] != 'bin'})
    cargo_args = [part for name in selected for part in ('-p', name)]
    messages = execute([tools['cargo'], 'test', *common, *cargo_args, '--lib', '--tests', '--features', 'print-core/publisher-adapter', '--no-run', '--message-format=json'],
                       output, 'tests-build')
    # The font-worker manifest disables its automatic binary unit target.
    # Explicit target selection builds those test harnesses as well as the
    # separately built production sidecars; compile success alone proves none.
    binary_packages = sorted({suite[0] for suite in SUITES if suite[2] == 'bin'})
    binary_args = [part for name in binary_packages for part in ('-p', name)]
    binary_targets = [part for name in sorted({suite[1] for suite in SUITES if suite[2] == 'bin'})
                      for part in ('--bin', name)]
    messages += '\n'+execute([tools['cargo'], 'test', *common, *binary_args, *binary_targets,
                             '--no-run', '--message-format=json'], output, 'worker-tests-build')
    artifacts = select_artifacts(messages, packages)
    spec = importlib.util.spec_from_file_location('stage_workers', ROOT/'scripts/stage-workers.py')
    staging = importlib.util.module_from_spec(spec)
    spec.loader.exec_module(staging)
    runtime = target/'workspace'/TARGET/'release'
    binary_ids = {name: elf_identity(runtime/('publisher-'+name+'-worker')) for name in WORKERS}
    probes = [elf_identity(runtime/'examples/linux-boundary-probe'), elf_identity(runtime/'publisher-linux-isolation-probe')]
    tests = []
    os.environ['PATTERBOX_ADAPTER_FACTS_OUTPUT'] = str(output/'adapter-positive')
    for package, name, kind, required in SUITES:
        executable = artifacts[package, name, kind]
        binary = elf_identity(executable)
        stage = staging.stage(runtime, executable, WORKERS, test_layout=True, target=TARGET)
        label = package+'-'+name
        listing = execute([executable, '--list', '--format', 'terse'], output, label+'-list', 60)
        inclusion = ['--include-ignored'] if (package, name, kind) == ('print-core', 'print_core', 'lib') else []
        result = execute([executable, *inclusion, '--test-threads=1', '--color', 'never'], output, label+'-run', 300)
        tests.append({'package': package, 'target': name, 'kind': kind, 'binary': binary, 'staging': stage,
                      **audit_test_result(listing, result, required)})
        print(label+': '+str(tests[-1]['passed'])+' native tests passed', flush=True)
    adapter = output/'adapter-positive'
    source_font = ROOT/'crates/layout-core/tests/fixtures/NotoEmoji-Regular.ttf'
    facts = output/'adapter-authored-font-facts.json'
    facts.write_text(json.dumps({'source_sha256': identity(source_font)['sha256'], 'face_index': 0,
                                 'accented_source_glyph_ids': []}, indent=2)+'\n', encoding='utf-8')
    adapter_checks = []
    for name, mode in (('ordinary', 'ordinary'), ('print', 'pdfx4')):
        pdf = adapter/(name+'.pdf')
        outer_report = output/('adapter-'+name+'-outer.json')
        metric_report = output/('adapter-'+name+'-metrics.json')
        execute([sys.executable, ROOT/'scripts/verify-pdfx-supported-output.py', pdf,
                 '--mode', mode, '--qpdf', tools['qpdf'], '--output', outer_report], output, 'adapter-'+name+'-outer', 180)
        execute([sys.executable, ROOT/'scripts/verify-font-metrics.py', '--pdf', pdf,
                 '--source-font', source_font, '--preparation-report', adapter/'prepared.json',
                 '--font-facts', facts, '--output', metric_report], output, 'adapter-'+name+'-metrics', 180)
        adapter_checks.append({'mode': mode, 'pdf': identity(pdf), 'outer': identity(outer_report),
                               'metrics': identity(metric_report)})
    adapter_evidence = {'source_font': identity(source_font), 'authored_facts': identity(facts),
                        'preparation_identity': identity(adapter/'prepared.json'), 'outputs': adapter_checks,
                        'authoring_premise': 'The retained Mahjong tile and .notdef have no accented role; facts are authored independently of producer metrics.'}
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
               'native_tests_passed': sum(row['passed'] for row in tests), 'failed': 0, 'ignored': 0, 'filtered': 0,
               'client': client_id, 'client_staging': client_stage, 'public_api_receipt': identity(output/'public-api/results.json'),
               'pdfs': public['standalone'], 'adapter_exact_output': adapter_evidence, 'validator_runtime': 'explicitly unsupported; tested refusal',
               'format_conformance': 'separate exact-output evaluation required'}
    (output/'results.json').write_text(json.dumps(receipt, indent=2)+'\n', encoding='utf-8')
    print(json.dumps({'status': 'passed', 'native_tests_passed': receipt['native_tests_passed'], 'ignored': 0}))


if __name__ == '__main__':
    main()
