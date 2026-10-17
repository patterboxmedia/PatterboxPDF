"""Audit the neutral generation contract and exact declared evidence identities.

This mechanical audit does not run evaluations or certify arbitrary PDFs. Native
generation/resource workers remain separate from development-only checkers.
"""
import argparse
import ast
import hashlib
import json
import math
from pathlib import Path, PurePosixPath
import re

ROOT = Path(__file__).resolve().parents[1]
CONTRACT = 'docs/requirements/generation-contract.json'
CATALOG = 'docs/requirements/public-output-rules.json'
FAMILIES = ('geometry', 'paths-paints', 'images', 'truetype', 'cff', 'font-preparation',
            'text', 'metadata-names', 'icc', 'gradients', 'composition', 'envelope',
            'adapters', 'closed-feature-boundary')
RECEIPTS = {kind: 'docs/verification/current/accepted-inputs-20261016/'+name+'.json'
            for kind, name in (('supported-output', 'supported-inputs'), ('color', 'color'),
                               ('linux', 'linux'), ('windows', 'windows'))}
SURFACE_FILES = (
    'crates/print-core/src/lib.rs', 'crates/print-core/src/finished.rs',
    'crates/print-core/src/finished_writer.rs', 'crates/print-core/src/candidate_metadata.rs',
    'crates/font-boundary/src/lib.rs', 'crates/color-boundary/src/lib.rs',
    'crates/raster-boundary/src/lib.rs', 'crates/print-core/src/profile.rs',
    'crates/print-core/src/scene.rs', 'crates/print-core/src/production.rs',
    'crates/print-core/src/candidate.rs', 'crates/print-core/src/publication_font.rs',
    'crates/print-core/src/raster.rs', 'crates/print-core/src/color.rs',
    'crates/print-core/src/publisher_adapter.rs', 'crates/print-core/src/font.rs',
    'crates/print-core/src/fixture.rs',
)
ROLE_GROUPS = {
    'construction': {
        'finished.rs': ['Point::new', 'PdfBox::new', 'Paint::gray', 'Paint::cmyk', 'Paint::rgb', 'FinishedItem::new', 'FontResource::from_prepared', 'ImageResource::from_decoded_rgb', 'ImageResource::from_decoded_gray', 'ImageResource::from_decoded_cmyk'],
        'candidate_metadata.rs': ['PdfCandidateMetadata::for_export', 'PdfCandidateMetadata::at_utc'],
        'profile.rs': ['ProfileHash::from_bytes', 'ProfileHash::of'],
        'production.rs': ['PublicationWarningAcknowledgement::for_report'],
        'font.rs': ['FontBoundary::from_prepared'],
        'fixture.rs': ['EvaluationKey::from_bytes', 'RasterResolutionPreset::from_static_thresholds', 'FixtureContract::new', 'FixtureContract::with_trusted_color', 'FixtureContract::with_raster_resolution_preset'],
    },
    'preparation': {
        'publication_font.rs': ['prepare_publication_fonts', 'prepare_publication_fonts_with_font_facts'], 'raster.rs': ['prepare_publication_rasters'],
        'color.rs': ['prepare_publication_colors'],
    },
    'inspection': {
        'profile.rs': ['inspect_output_intent'],
        'production.rs': ['PublicationCandidatePreflight::for_candidate', 'PublicationCandidatePreflight::for_candidate_with_font_resources'],
    },
    'conversion': {
        'publication_font.rs': ['ProductionFont::into_finished_resource'],
        'raster.rs': ['ProductionRaster::into_finished_resource'],
        'publisher_adapter.rs': ['PublisherSceneAdapter::lower'],
    },
    'generation': {'finished_writer.rs': ['FinishedPdfWriter::write']},
    'ordinary-generation': {'scene.rs': ['ScenePdfWriter::write', 'ScenePdfWriter::write_with_rasters', 'ScenePdfWriter::write_with_rasters_and_fonts', 'ScenePdfWriter::write_with_rasters_fonts_and_colors']},
    'adapter-generation': {'production.rs': ['PublicationCandidateWriter::write_with_rasters_and_fonts', 'PublicationCandidateWriter::write_with_resources', 'PublicationCandidateWriter::write_with_resources_and_metadata']},
    'diagnostic-generation': {
        'production.rs': ['PublicationCandidateWriter::write', 'PublicationCandidateWriter::write_with_rasters'],
        'candidate.rs': ['CandidatePdfWriter::write_structural_probe', 'CandidatePdfWriter::write_pdfx4_candidate_for_validation', 'CandidatePdfWriter::write_pdfx4_candidate_with_font_for_validation', 'CandidatePdfWriter::write_pdfx4_candidate_with_font_and_trusted_color_for_validation'],
    },
}
BOUNDARY_ROLES = {
    'font': {'construction': ['resolve_packaged'], 'preparation': ['prepare', 'prepare_publication', 'prepare_publication_with_ascent_facts', 'prepare_publication_with_font_facts'], 'inspection': ['inspect']},
    'color': {'construction': ['resolve_packaged'], 'preparation': ['convert', 'convert_srgb'], 'inspection': ['inspect_output_profile']},
    'raster': {'construction': ['resolve_packaged'], 'preparation': ['normalize_clipboard_dib', 'decode']},
}
CLAIMS = {
    'generation': 'positive-pdfx-only-through-write-with-PdfX4-mode',
    'ordinary-generation': 'ordinary-output-only-outside-positive-pdfx',
    'adapter-generation': 'conditional-on-fonts-some-and-managed-route-diagnostic-branches-excluded',
    'diagnostic-generation': 'diagnostic-only-outside-positive-pdfx',
}
POLICIES = {
    'adapter_branches': {'scene_public_overloads': 'strict-ordinary-generation',
        'fonts_none': 'diagnostic-authoring-outside-positive-pdfx',
        'fonts_some_including_empty': 'strict-font-contract', 'unmanaged_rgb': 'separate-diagnostic-exclusion'},
    'registered_resources': 'all-registered-including-unused',
    'runtime': 'free-native-confined-no-python',
    'resource_identity': 'exact-prepared-and-untouched-full-source-bytes',
    'diagnostics': 'outside-positive-pdfx-claim',
    'ordinary': 'preserve-documented-success-and-structured-errors',
    'font_ascent': {'positive_pdfx': 'all-registered-fonts-require-authoritative-ascent',
        'ordinary_unresolved': 'compatible-heuristic-outside-correct-ascent-guarantee',
        'authored_facts': 'complete-retained-source-role-set-truth-is-an-authoring-premise'},
    'font_capital_height': {'positive_pdfx': 'all-registered-fonts-require-authoritative-capital-height',
        'ordinary_unresolved': 'compatible-output-outside-complete-descriptor-guarantee',
        'authored_facts': 'source-face-bound-design-metric-in-final-pdf-glyph-units-only-fills-unresolved'},
    'font_component_context': {
        'vertical_anchors':'unsupported-only-when-actual-geometry-needs-unestablished-coordinates',
        'selected_metric_axis':'unsupported-if-b-nonzero-or-a-negative-project-boundary',
        'unused_source_context':'defer-unneeded-subset-source-context-and-unresolved-optional-reference',
        'ordinary_and_pdfx':'same-distinct-unsupported-preparation-errors'},
}
SHA = re.compile(r'[0-9a-f]{64}')


def require(value, message):
    if not value:
        raise ValueError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def words(value):
    return isinstance(value, str) and bool(value.strip())


def unique(rows, key, label):
    require(isinstance(rows, list) and rows and all(isinstance(row, dict) and words(row.get(key)) for row in rows), 'empty or invalid '+label)
    result = {row[key]: row for row in rows}
    require(len(result) == len(rows), 'duplicate '+label)
    return result


def file_bytes(root, name):
    require(isinstance(name, str) and name and '\\' not in name and ':' not in name
            and not PurePosixPath(name).is_absolute() and all(part not in ('', '.', '..') for part in name.split('/')), 'unsafe public binding path')
    path = root/name
    require(path.is_file() and path.resolve().is_relative_to(root)
            and not any(parent.is_symlink() for parent in (path, *path.parents) if parent.is_relative_to(root)), 'missing, linked or escaping binding: '+name)
    return path.read_bytes()


def binding(root, row):
    require(isinstance(row, dict) and set(row) == {'path', 'sha256'} and isinstance(row['sha256'], str)
            and SHA.fullmatch(row['sha256']), 'invalid exact binding')
    data = file_bytes(root, row['path'])
    require(digest(data) == row['sha256'], 'stale exact binding: '+row['path'])
    return data


def route_role(identity):
    for boundary, roles in BOUNDARY_ROLES.items():
        for role, names in roles.items():
            if identity in {'crates/'+boundary+'-boundary/src/lib.rs::Trusted'+boundary.title()+'Boundary::'+name for name in names}:
                return role
    for role, files in ROLE_GROUPS.items():
        for filename, names in files.items():
            if identity in {'crates/print-core/src/'+filename+'::'+name for name in names}:
                return role
    return 'observation'


def mask_rust(text):
    pattern = re.compile(r'//[^\n]*|/\*|r(\#*)"|"|\'(?:\\.|[^\'\\\n])\'')
    chars = list(text)
    offset = 0
    while match := pattern.search(text, offset):
        start, end = match.span()
        token = match.group()
        if token == '/*':
            depth = 1
            while depth:
                nested = re.search(r'/\*|\*/', text[end:])
                require(nested, 'unterminated Rust comment')
                end += nested.end()
                depth += 1 if nested.group() == '/*' else -1
        elif token.startswith('r'):
            closing = '"'+match.group(1)
            finish = text.find(closing, end)
            require(finish >= 0, 'unterminated Rust raw string')
            end = finish+len(closing)
        elif token == '"':
            while end < len(text):
                if text[end] == '\\':
                    end += 2
                elif text[end] == '"':
                    end += 1
                    break
                else:
                    end += 1
        for index in range(start, end):
            if chars[index] != '\n':
                chars[index] = ' '
        offset = end
    return ''.join(chars)


def block_end(text, start):
    depth = 0
    for index in range(start, len(text)):
        depth += (text[index] == '{') - (text[index] == '}')
        if depth == 0:
            return index
    raise ValueError('unclosed Rust declaration')


def normalize(text):
    return re.sub(r'\s+', ' ', text).strip()


def declaration_end(text, start):
    nesting = []
    pairs = {']': '[', ')': '(', '>': '<'}
    for end in range(start, len(text)):
        token = text[end]
        if token in '([<':
            nesting.append(token)
        elif token in pairs and nesting and nesting[-1] == pairs[token]:
            nesting.pop()
        elif token in '{;' and not nesting:
            return end
    raise ValueError('unterminated Rust declaration')


def public_fields(text):
    fields = []
    for match in re.finditer(r'\bpub\s+(?!\()\w+\s*:', text):
        depth = 0
        end = match.end()
        while end < len(text):
            token = text[end]
            if token in '(<[{':
                depth += 1
            elif token in ')>]}':
                depth -= 1
            elif token == ',' and depth == 0:
                end += 1
                break
            end += 1
        fields.append(normalize(text[match.start():end]))
    return ' '.join(fields)


def discover_surface(root):
    routes, forms = {}, {}
    for relative in SURFACE_FILES:
        text = mask_rust(file_bytes(root, relative).decode('utf-8'))
        impls = [(match.start(), block_end(text, match.end()-1), match.group(1))
                 for match in re.finditer(r'\bimpl(?:<[^{}]*>)?\s+(\w+)(?:<[^{}]*>)?\s*\{', text)]
        for match in re.finditer(r'\bpub\s+(?:const\s+)?fn\s+(\w+)\s*(?:<[^{}]*?>)?\s*\(', text):
            owner = next((name for start, end, name in impls if start < match.start() < end), None)
            identity = relative+'::'+(owner+'::' if owner else '')+match.group(1)
            end, depth = match.end(), 1
            while depth and end < len(text):
                depth += (text[end] == '(') - (text[end] == ')')
                end += 1
            end = declaration_end(text, end)
            require(identity not in routes, 'duplicate public callable: '+identity)
            routes[identity] = {'id': identity, 'source': relative, 'signature': normalize(text[match.start():end])}
        for match in re.finditer(r'\bpub\s+(struct|enum|trait|type)\s+(\w+)', text):
            kind, name = match.groups()
            end = declaration_end(text, match.end())
            if text[end] == '{':
                body = text[end+1:block_end(text, end)]
                shape = normalize(re.sub(r'#\[[^\]]*\]', '', body)) if kind in ('enum', 'trait') else public_fields(body)
            else:
                shape = normalize(text[match.start():end+1])
            identity = relative+'::'+name
            require(identity not in forms, 'duplicate public form: '+identity)
            forms[identity] = {'id': identity, 'source': relative, 'kind': kind, 'shape': shape}
        if relative.endswith('/lib.rs'):
            exports = re.findall(r'\bpub\s+(?:use\s+[^;]+;|mod\s+\w+\s*;)', text)
            identity = relative+'::public-exports'
            forms[identity] = {'id': identity, 'source': relative, 'kind': 'exports', 'shape': normalize(' '.join(exports))}
    return routes, forms


def native_cases(report, target):
    require(report.get('schema') == 'patterboxpdf-'+target+'-runtime/v1' and report.get('status') == 'passed'
            and report.get('target') == {'linux': 'x86_64-unknown-linux-gnu', 'windows': 'x86_64-pc-windows-msvc'}[target]
            and report.get('ignored') == 0, 'native receipt scope or verdict is incomplete')
    suites = unique(report.get('test_suites'), 'name', 'native test suites')
    result = set()
    for name, suite in suites.items():
        tests = suite.get('tests')
        require(isinstance(tests, list) and tests and all(words(test) for test in tests)
                and len(set(tests)) == len(tests) and suite.get('passed') == len(tests)
                and type(suite.get('passed')) is int and suite.get('ignored') == 0, 'native tests must list actual executed unique cases')
        result.update((target, name, test) for test in tests)
    require(type(report.get('native_tests_passed')) is int and report['native_tests_passed'] == len(result), 'native test count disagrees with executed lists')
    return result


def test_reference(row, executed):
    require(isinstance(row, dict) and set(row) == {'receipt', 'suite', 'test'}
            and (row['receipt'], row['suite'], row['test']) in executed, 'native case was not actually executed')


def symbol_reference(row, field, code, contents, native=False):
    keys = {'path', 'symbol', field} | ({'kind'} if native else set())
    require(isinstance(row, dict) and set(row) == keys and all(words(row[key]) for key in keys), 'incomplete concrete predicate or emitter')
    path = row['path']
    require(path in code and (path.endswith('.rs') if native or field == 'invariant' else path.startswith('scripts/') and path.endswith('.py')), 'predicate/emitter has no current code binding')
    require(re.fullmatch(r'[A-Za-z_][A-Za-z_0-9]*(?:::[A-Za-z_][A-Za-z_0-9]*)*', row['symbol']), 'invalid predicate/emitter symbol')
    name = row['symbol'].split('::')[-1]
    if path.endswith('.py'):
        tree = ast.parse(contents[path])
        parts = row['symbol'].split('::')
        nodes = list(ast.walk(tree)) if len(parts) == 1 else tree.body
        for part in parts:
            matches = [node for node in nodes if isinstance(node, (ast.FunctionDef, ast.AsyncFunctionDef, ast.ClassDef)) and node.name == part]
            require(matches, 'independent predicate symbol is absent')
            nodes = [child for node in matches for child in node.body]
    else:
        text = mask_rust(contents[path].decode('utf-8'))
        declarations = list(re.finditer(r'\b(?:fn|struct|enum|trait|type|const|static)\s+'+re.escape(name)+r'\b', text))
        if '::' in row['symbol']:
            owner, name = row['symbol'].rsplit('::', 1)
            impls = [(match.start(), block_end(text, match.end()-1))
                     for match in re.finditer(r'\bimpl(?:<[^{}]*>)?\s+'+re.escape(owner)+r'(?:<[^{}]*>)?\s*\{', text)]
            declarations = [match for match in declarations if any(start < match.start() < end for start, end in impls)]
        require(declarations, 'native predicate/emitter symbol is absent')
    if native:
        require(row['kind'] in ('admission', 'closed-construction'), 'unknown native predicate kind')


def identity(row):
    require(isinstance(row, dict) and set(row) == {'sha256', 'bytes'} and isinstance(row['sha256'], str)
            and SHA.fullmatch(row['sha256']) and type(row['bytes']) is int and row['bytes'] > 0, 'case input/output identity is missing')


def compare(row):
    require(isinstance(row, dict) and set(row) == {'id', 'expected', 'actual', 'comparison'}
            and words(row['id']) and row['expected'] in ('pass', 'finding') and row['actual'] in ('pass', 'finding'), 'invalid measured predicate verdict')
    comparison = row['comparison']
    require(isinstance(comparison, dict) and set(comparison) == {'operator', 'actual', 'expected'}, 'missing actual predicate comparison')
    actual, expected, operator = comparison['actual'], comparison['expected'], comparison['operator']
    require(actual is not None and expected is not None, 'null is not a measured predicate outcome')
    if operator in ('eq', 'ne'):
        # JSON encodings keep booleans distinct from numeric values.
        passed = json.dumps(actual, sort_keys=True, allow_nan=False) == json.dumps(expected, sort_keys=True, allow_nan=False)
        if operator == 'ne':
            passed = not passed
    else:
        require(operator in ('le', 'ge') and type(actual) in (int, float) and type(expected) in (int, float)
                and math.isfinite(actual) and math.isfinite(expected), 'invalid numeric predicate comparison')
        passed = actual <= expected if operator == 'le' else actual >= expected
    require(row['actual'] == ('pass' if passed else 'finding') == row['expected'], 'false green or unexpected predicate outcome')


def measured_cases(supported, code, executed):
    documents = unique(supported.get('documents'), 'id', 'positive documents')
    negatives = unique(supported.get('negative_controls'), 'id', 'negative controls')
    require(not documents.keys() & negatives.keys(), 'ambiguous document/control identity')
    require({row.get('mode') for row in documents.values()} >= {'ordinary', 'pdfx4'}, 'ordinary and print evidence is required')
    checks = unique(supported.get('additional_checks'), 'id', 'independent measurements')
    cases = {}
    for check in checks.values():
        evaluator = check.get('evaluator', {})
        require(set(evaluator) == {'path', 'sha256'} and evaluator.get('path') in code
                and code[evaluator['path']]['sha256'] == evaluator['sha256'], 'measurement evaluator is not currently bound')
        report = check.get('report', {})
        require(report.get('passed') is True and report.get('findings') == []
                and report.get('evaluator_sha256') == evaluator['sha256'], 'independent measurement is pending or stale')
        for case in report.get('cases', []):
            require(isinstance(case, dict) and set(case) in ({'id', 'kind', 'mode', 'inputs', 'output', 'document', 'evaluator_sha256', 'workers', 'predicates', 'environment', 'limits'},
                    {'id', 'kind', 'mode', 'inputs', 'output', 'document', 'evaluator_sha256', 'workers', 'predicates', 'environment', 'limits', 'native_outcome'}), 'unsupported measured case shape')
            require(words(case['id']) and case['id'] not in cases and case['kind'] in ('positive', 'negative', 'boundary', 'metamorphic')
                    and case['mode'] in ('ordinary', 'pdfx4') and case['evaluator_sha256'] == evaluator['sha256'], 'duplicate, unknown or stale measured case')
            inputs = unique(case['inputs'], 'id', 'measured case inputs')
            for row in inputs.values():
                require(set(row) == {'id', 'sha256', 'bytes'}, 'unsupported measured input shape')
                identity({key: row[key] for key in ('sha256', 'bytes')})
            environment = case['environment']
            require(isinstance(environment, dict) and set(environment) == {'platform', 'target', 'tools'}
                    and words(environment['platform']) and words(environment['target']), 'case execution environment is missing')
            tools = unique(environment['tools'], 'name', 'case execution tools')
            require(all(set(tool) == {'name', 'version'} and words(tool['version']) for tool in tools.values()), 'case tool version is missing')
            require(isinstance(case['limits'], list) and case['limits'] and all(words(limit) for limit in case['limits']), 'case execution limits are missing')
            require(isinstance(case['workers'], list), 'worker identities must be explicit')
            for worker in case['workers']:
                require(isinstance(worker, dict) and set(worker) == {'name', 'sha256'} and words(worker['name'])
                        and isinstance(worker['sha256'], str) and SHA.fullmatch(worker['sha256']), 'invalid exact worker identity')
            predicates = unique(case['predicates'], 'id', 'measured case predicates')
            for predicate in predicates.values():
                compare(predicate)
            finding = any(row['actual'] == 'finding' for row in predicates.values())
            if case['output'] is None:
                outcome = case.get('native_outcome', {})
                require(case['kind'] == 'negative' and case['document'] is None and not finding
                        and set(outcome) == {'status', 'error_code', 'driver', 'native_test'}
                        and outcome['status'] == 'refused' and words(outcome['error_code']), 'missing measured native refusal')
                driver = outcome['driver']
                require(isinstance(driver, dict) and driver.get('path') in code and driver == code[driver['path']], 'native refusal driver identity is stale')
                test_reference(outcome['native_test'], executed)
            else:
                identity(case['output'])
                require('native_outcome' not in case and finding == (case['kind'] == 'negative'), 'emitted control has a false verdict')
                document = case['document']
                # Semantic measurements can find a defect outside the finite
                # outer rules. Every supplied document still binds exact bytes.
                if case['kind'] == 'negative' and document is not None:
                    require(document in negatives, 'emitted negative needs an actual negative report')
                if document is not None:
                    require(document in (negatives if case['kind'] == 'negative' else documents), 'case names an unknown or wrong-verdict document')
                    report = documents[document] if document in documents else negatives[document]['report']
                    require(case['mode'] == report.get('mode') and case['output'] == {'sha256': report.get('input_sha256'), 'bytes': report.get('input_bytes')}, 'case output or mode differs from exact evaluated document')
            cases[case['id']] = (case, evaluator['path'])
    require(cases, 'no measured family controls')
    return cases


def audit(root):
    root = Path(root).resolve(strict=True)
    contract_data = file_bytes(root, CONTRACT)
    contract = json.loads(contract_data)
    require(set(contract) == {'schema', 'status', 'scope', 'policies', 'surface_sources', 'routes', 'api_forms', 'code_bindings', 'receipts', 'families'}
            and contract['schema'] == 'patterboxpdf-generation-contract/v1' and contract['status'] == 'complete'
            and words(contract['scope']) and contract['policies'] == POLICIES, 'unsupported, pending or changed generation contract')
    code = unique(contract['code_bindings'], 'path', 'code bindings')
    contents = {}
    for path, row in code.items():
        require((path in ('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml') or path.startswith(('scripts/', 'crates/', 'apps/', 'examples/standalone-client/')))
                and PurePosixPath(path).suffix in ('.py', '.rs', '.toml', '.lock', '.json'), 'unsupported public code binding')
        contents[path] = binding(root, row)
    require({'scripts/verify-accepted-inputs.py', 'scripts/verify-output-rules.py'}.issubset(code), 'audit implementation identities are missing')
    sources = contract['surface_sources']
    require(isinstance(sources, list) and [row.get('path') for row in sources] == list(SURFACE_FILES)
            and all(row == code.get(row['path']) for row in sources), 'public source inventory is incomplete or stale')
    families = unique(contract['families'], 'id', 'generation families')
    require(set(families) == set(FAMILIES), 'generation families were removed or renamed')
    actual_routes, actual_forms = discover_surface(root)
    for rows, actual, label in ((contract['routes'], actual_routes, 'route'), (contract['api_forms'], actual_forms, 'form')):
        declared = unique(rows, 'id', 'public '+label+' inventory')
        require(declared.keys() == actual.keys(), 'public '+label+' inventory differs from current source')
        for key, row in declared.items():
            expected = actual[key]
            require(all(row.get(field) == value for field, value in expected.items()), 'public '+label+' shape/signature is stale')
            require(isinstance(row.get('families'), list) and row['families'] and len(set(row['families'])) == len(row['families'])
                    and set(row['families']).issubset(families), 'missing or unknown public family edge')
            if label == 'route':
                role = route_role(key)
                require(set(row) == set(expected) | {'families', 'role', 'positive_claim'} and row['role'] == role
                        and row['positive_claim'] == CLAIMS.get(role, 'does-not-generate-pdf-bytes'), 'public route role or generation claim changed')
            else:
                require(set(row) == set(expected) | {'families', 'role'} and row['role'] == ('public-exports' if row['kind'] == 'exports' else 'public-form'), 'public form role changed')
    require({family for row in contract['routes'] for family in row['families']} == set(families), 'generation family has no public route')
    receipts = unique(contract['receipts'], 'kind', 'generation receipts')
    require(set(receipts) == set(RECEIPTS), 'the four exact generation receipts are required')
    evidence = {}
    for kind, row in receipts.items():
        require(set(row) == {'kind', 'path', 'sha256'} and row['path'] == RECEIPTS[kind], 'unexpected generation receipt path')
        evidence[kind] = json.loads(binding(root, {key: row[key] for key in ('path', 'sha256')}))
    require(evidence['color'].get('schema') == 'patterboxpdf-cmyk-diagnostic/v1', 'color receipt schema disagrees')
    supported = evidence['supported-output']
    require(supported.get('schema') == 'patterboxpdf-supported-output/v1' and supported.get('status') == 'passed', 'supported-input evidence is pending')
    executed = native_cases(evidence['linux'], 'linux') | native_cases(evidence['windows'], 'windows')
    cases = measured_cases(supported, code, executed)
    family_evidence = unique(supported.get('family_evidence'), 'family_id', 'family evidence')
    require(family_evidence.keys() == families.keys(), 'family evidence is incomplete or renamed')
    all_predicates = set()
    all_controls = set()
    for family_id, family in families.items():
        require(set(family) == {'id', 'status', 'scope', 'valid_supported', 'malformed', 'existing_unsupported', 'preconditions', 'invariants'}
                and family['status'] == 'complete' and isinstance(family['scope'], list) and family['scope']
                and len(set(family['scope'])) == len(family['scope'])
                and set(family['scope']).issubset({'ordinary', 'pdfx4'}), 'family scope or completion is incomplete')
        for field in ('valid_supported', 'malformed', 'existing_unsupported'):
            require(isinstance(family[field], list) and family[field] and all(words(row) for row in family[field]), 'missing family semantics: '+family_id)
        require(isinstance(family['preconditions'], list), 'family preconditions must be explicit')
        for row in family['preconditions']:
            require(isinstance(row, dict) and set(row) == {'id', 'statement'} and words(row['id']) and words(row['statement']), 'invalid external authoring precondition')
        invariants = unique(family['invariants'], 'id', 'family invariants')
        require(not all_predicates.intersection(invariants), 'duplicate invariant identity')
        all_predicates.update(invariants)
        used = set()
        for predicate_id, invariant in invariants.items():
            require(set(invariant) == {'id', 'native', 'emitter', 'independent', 'controls', 'native_tests', 'required_control_kinds', 'modes'}, 'incomplete family invariant chain')
            symbol_reference(invariant['native'], 'predicate', code, contents, True)
            symbol_reference(invariant['emitter'], 'invariant', code, contents)
            symbol_reference(invariant['independent'], 'predicate', code, contents)
            tests = invariant['native_tests']
            require(isinstance(tests, list) and tests, 'invariant lacks actual native cases')
            for test in tests:
                test_reference(test, executed)
            controls = invariant['controls']
            require(isinstance(controls, list) and controls and len(set(controls)) == len(controls), 'invariant has no discriminating controls')
            kinds, modes = set(), set()
            for control_id in controls:
                require(control_id in cases, 'invariant names an unmeasured control')
                case, evaluator = cases[control_id]
                require(evaluator == invariant['independent']['path'] and predicate_id in {row['id'] for row in case['predicates']}, 'control does not evaluate the named independent predicate')
                kinds.add(case['kind']); modes.add(case['mode']); used.add(control_id)
            required = invariant['required_control_kinds']
            scope = invariant['modes']
            require(isinstance(required, list) and required and len(set(required)) == len(required)
                    and set(required).issubset({'positive', 'negative', 'boundary', 'metamorphic'})
                    and set(required).issubset(kinds) and isinstance(scope, list) and scope
                    and len(set(scope)) == len(scope) and set(scope).issubset(family['scope'])
                    and modes == set(scope), 'invariant declared control partitions or modes are incomplete')
        require({'positive', 'negative'}.issubset({cases[key][0]['kind'] for key in used}), 'family needs positive and negative evidence')
        all_controls.update(used)
        report = family_evidence[family_id]
        require(set(report) == {'family_id', 'status', 'controls'} and report['status'] == 'passed'
                and isinstance(report['controls'], list) and len(set(report['controls'])) == len(report['controls'])
                and set(report['controls']) == used, 'family evidence has missing, pending or unowned controls')
    require(all(row['id'] in all_predicates for case, _ in cases.values() for row in case['predicates']), 'measured case names an unknown invariant')
    require(all_controls == cases.keys(), 'measured control has no family invariant edge')
    catalog_data = file_bytes(root, CATALOG)
    catalog = json.loads(catalog_data)
    require(catalog.get('schema') == 'patterboxpdf-output-rules/v2' and catalog.get('receipt_set') == 'accepted-inputs-20261016'
            and catalog.get('receipts') == contract['receipts'] and catalog.get('code_bindings') == contract['code_bindings'], 'outer catalog and generation evidence bindings disagree')
    inventory = json.loads(file_bytes(root, 'docs/requirements/pdfx4-inventory.json'))
    assurance = inventory.get('coverage_assurance', {})
    require(inventory.get('schema') == 'pdf-library-requirements/v1' and inventory.get('normative_requirements') == []
            and assurance.get('status') == 'complete'
            and assurance.get('catalog') == {'path': CATALOG, 'sha256': digest(catalog_data)}
            and assurance.get('generation_contract') == {'path': CONTRACT, 'sha256': digest(contract_data)}, 'generation coverage assurance is missing, stale or incomplete')
    return {'schema': 'patterboxpdf-accepted-input-audit/v1', 'status': 'passed', 'families': len(families),
            'routes': len(actual_routes), 'forms': len(actual_forms), 'invariants': len(all_predicates), 'controls': len(cases),
            'generation_contract_sha256': digest(contract_data),
            'scope': 'Mechanical consistency and exact declared identities; does not run evaluations or certify arbitrary PDFs.'}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=ROOT)
    parser.add_argument('--output', type=Path)
    args = parser.parse_args()
    try:
        result = audit(args.root)
        rendered = json.dumps(result, indent=2)+'\n'
        if args.output:
            output = args.output.resolve()
            require(output.is_relative_to(args.root.resolve()) and not output.is_symlink(), 'audit output escapes root')
            output.parent.mkdir(parents=True, exist_ok=True)
            output.write_text(rendered, encoding='utf-8', newline='\n')
    except (ValueError, KeyError, TypeError, OSError, SyntaxError) as error:
        parser.exit(1, 'accepted-input audit failed: '+str(error)+'\n')
    print(rendered, end='')


if __name__ == '__main__':
    main()
