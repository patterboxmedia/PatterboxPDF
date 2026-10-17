"""Synthetic audit controls; these fixtures are not project acceptance evidence."""
import copy
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('generation_audit', Path(__file__).resolve().parents[2]/'scripts/verify-accepted-inputs.py')
audit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(audit)


class Fixture:
    def __init__(self, root):
        self.root = root
        self.code = []
        for path in audit.SURFACE_FILES:
            self.code.append(self.write(path, '// Synthetic source.\n'))
        source = audit.SURFACE_FILES[0]
        text = ''.join('pub struct Form'+str(index)+';\npub fn route'+str(index)+'() {}\n'
                       for index in range(len(audit.FAMILIES)))
        self.code[0] = self.write(source, text+'fn admit() {}\nfn emit() {}\n')
        evaluator = self.write('scripts/independent.py', 'def inspect():\n    return True\n')
        self.code.append(evaluator)
        self.code.append(self.write('scripts/verify-accepted-inputs.py', '# Synthetic auditor identity.\n'))
        self.code.append(self.write('scripts/verify-output-rules.py', '# Synthetic outer auditor identity.\n'))
        routes, forms = audit.discover_surface(root)
        self.cases = []
        families = []
        for index, identity in enumerate(audit.FAMILIES):
            predicate = identity+'-invariant'
            control_ids = []
            for kind in ('positive', 'negative', 'boundary', 'metamorphic'):
                case_id = identity+'-'+kind
                control_ids.append(case_id)
                finding = kind == 'negative'
                self.cases.append({'id': case_id, 'kind': kind, 'mode': 'ordinary' if kind == 'positive' else 'pdfx4',
                    'inputs': [{'id': 'synthetic-input', 'sha256': '1'*64, 'bytes': 11}],
                    'output': None if finding else {'sha256': '2'*64, 'bytes': 12}, 'document': None,
                    'evaluator_sha256': evaluator['sha256'], 'workers': [],
                    **({'native_outcome': {'status': 'refused', 'error_code': 'synthetic-refusal',
                        'driver': self.code[0], 'native_test': {'receipt': 'windows', 'suite': 'native',
                        'test': 'tests::'+identity}}} if finding else {}),
                    'predicates': [{'id': predicate, 'expected': 'pass',
                        'actual': 'pass',
                        'comparison': {'operator': 'eq', 'actual': True, 'expected': True}}],
                    'environment': {'platform': 'synthetic', 'target': 'synthetic-test',
                                    'tools': [{'name': 'synthetic-oracle', 'version': '1'}]},
                    'limits': ['Synthetic fixture only; no project acceptance.']})
            families.append({'id': identity, 'status': 'complete', 'scope': ['ordinary', 'pdfx4'],
                'valid_supported': ['Synthetic supported form.'], 'malformed': ['Synthetic malformed neighbor.'],
                'existing_unsupported': ['Synthetic unsupported form.'], 'preconditions': [],
                'invariants': [{'id': predicate,
                    'native': {'kind': 'admission', 'path': source, 'symbol': 'admit', 'predicate': 'Synthetic admission.'},
                    'emitter': {'path': source, 'symbol': 'emit', 'invariant': 'Synthetic emitted relationship.'},
                    'independent': {'path': evaluator['path'], 'symbol': 'inspect', 'predicate': 'Synthetic independent comparison.'},
                    'controls': control_ids, 'required_control_kinds': ['positive', 'negative', 'boundary', 'metamorphic'],
                    'modes': ['ordinary', 'pdfx4'],
                    'native_tests': [{'receipt': 'windows', 'suite': 'native', 'test': 'tests::'+identity}]}]})
        for index, row in enumerate(routes.values()):
            row.update(families=[audit.FAMILIES[index]], role=audit.route_role(row['id']),
                       positive_claim=audit.CLAIMS.get(audit.route_role(row['id']), 'does-not-generate-pdf-bytes'))
        for index, row in enumerate(forms.values()):
            row.update(families=[audit.FAMILIES[min(index, len(audit.FAMILIES)-1)]],
                       role='public-exports' if row['kind'] == 'exports' else 'public-form')
        supported = {'schema': 'patterboxpdf-supported-output/v1', 'status': 'passed',
            'documents': [{'id': 'ordinary', 'mode': 'ordinary', 'input_sha256': '2'*64, 'input_bytes': 12},
                          {'id': 'print', 'mode': 'pdfx4', 'input_sha256': '2'*64, 'input_bytes': 12}],
            'negative_controls': [{'id': 'outer-negative', 'report': {'mode': 'pdfx4', 'input_sha256': '3'*64, 'input_bytes': 13}}],
            'additional_checks': [{'id': 'family-measurements', 'evaluator': evaluator,
                'report': {'passed': True, 'findings': [], 'evaluator_sha256': evaluator['sha256'], 'cases': self.cases}}],
            'family_evidence': [{'family_id': family['id'], 'status': 'passed',
                                'controls': family['invariants'][0]['controls']} for family in families]}
        self.reports = {'supported-output': supported, 'color': {'schema': 'patterboxpdf-cmyk-diagnostic/v1'},
            'linux': self.runtime('linux'), 'windows': self.runtime('windows')}
        self.contract = {'schema': 'patterboxpdf-generation-contract/v1', 'status': 'complete',
            'scope': 'Synthetic test contract only.', 'policies': copy.deepcopy(audit.POLICIES),
            'surface_sources': self.code[:len(audit.SURFACE_FILES)], 'routes': list(routes.values()),
            'api_forms': list(forms.values()), 'code_bindings': self.code, 'families': families, 'receipts': []}
        self.save()

    def runtime(self, target):
        tests = ['tests::'+identity for identity in audit.FAMILIES]
        return {'schema': 'patterboxpdf-'+target+'-runtime/v1', 'status': 'passed',
                'target': 'x86_64-unknown-linux-gnu' if target == 'linux' else 'x86_64-pc-windows-msvc',
                'native_tests_passed': len(tests), 'ignored': 0,
                'test_suites': [{'name': 'native', 'passed': len(tests), 'ignored': 0, 'tests': tests}]}

    def write(self, name, value):
        data = (json.dumps(value, indent=2)+'\n').encode() if isinstance(value, dict) else value.encode()
        path = self.root/name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(data)
        return {'path': name, 'sha256': audit.digest(data)}

    def save(self):
        self.contract['receipts'] = [dict(kind=kind, **self.write(path, self.reports[kind]))
                                     for kind, path in audit.RECEIPTS.items()]
        contract = self.write(audit.CONTRACT, self.contract)
        catalog = self.write('docs/requirements/public-output-rules.json', {'schema': 'patterboxpdf-output-rules/v2',
            'receipt_set': 'accepted-inputs-20261016', 'receipts': self.contract['receipts'],
            'code_bindings': self.code})
        self.write('docs/requirements/pdfx4-inventory.json', {'schema': 'pdf-library-requirements/v1',
            'normative_requirements': [], 'coverage_assurance': {'status': 'complete', 'catalog': catalog,
                                                               'generation_contract': contract}})


class AcceptedInputTests(unittest.TestCase):
    def test_authored_ascent_entrypoint_is_preparation_and_authority_is_explicit(self):
        identity = 'crates/font-boundary/src/lib.rs::TrustedFontBoundary::prepare_publication_with_ascent_facts'
        self.assertEqual(audit.route_role(identity), 'preparation')
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); fixture = Fixture(root)
            fixture.contract['policies'].pop('font_ascent')
            fixture.save()
            with self.assertRaises(ValueError): audit.audit(root)

    def test_combined_font_facts_entrypoint_and_capital_authority_are_explicit(self):
        identity = 'crates/font-boundary/src/lib.rs::TrustedFontBoundary::prepare_publication_with_font_facts'
        self.assertEqual(audit.route_role(identity),'preparation')
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); fixture = Fixture(root)
            self.assertEqual(audit.audit(root)['status'],'passed')
            fixture.contract['policies'].pop('font_capital_height'); fixture.save()
            with self.assertRaises(ValueError): audit.audit(root)

    def test_adapter_facts_callback_is_an_audited_preparation_route(self):
        identity='crates/print-core/src/publication_font.rs::prepare_publication_fonts_with_font_facts'
        self.assertEqual(audit.route_role(identity),'preparation')

    def test_component_interpretation_boundary_is_explicit(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);fixture=Fixture(root)
            self.assertEqual(audit.audit(root)['status'],'passed')
            fixture.contract['policies'].pop('font_component_context');fixture.save()
            with self.assertRaises(ValueError):audit.audit(root)

    def test_complete_synthetic_chain_and_derived_surface_pass(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); fixture = Fixture(root)
            result = audit.audit(root)
            self.assertEqual(result['families'], len(audit.FAMILIES))
            self.assertEqual(result['routes'], len(fixture.contract['routes']))
            self.assertIn('does not run evaluations', result['scope'])

    def test_missing_family_edge_predicate_emitter_control_or_native_case_fails(self):
        mutations = [lambda f: f.contract['families'].pop(),
            lambda f: f.contract['routes'][0].update(families=[]),
            lambda f: f.contract['api_forms'][0].update(families=['renamed']),
            lambda f: f.contract['families'][0]['invariants'][0]['native'].pop('predicate'),
            lambda f: f.contract['families'][0]['invariants'][0]['emitter'].update(symbol='invented'),
            lambda f: f.contract['families'][0]['invariants'][0]['independent'].update(symbol='invented'),
            lambda f: f.contract['families'][0]['invariants'][0].update(controls=[]),
            lambda f: f.contract['families'][0]['invariants'][0]['native_tests'][0].update(test='invented')]
        for index, change in enumerate(mutations):
            with self.subTest(index=index), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary); fixture = Fixture(root); change(fixture); fixture.save()
                with self.assertRaises(ValueError): audit.audit(root)

    def test_false_green_pending_missing_comparison_or_unmeasured_control_fails(self):
        mutations = [lambda f: f.cases[0]['predicates'][0]['comparison'].update(actual=False),
            lambda f: f.cases[1]['native_outcome'].update(status='pending'),
            lambda f: f.contract['families'][0].update(status='pending'),
            lambda f: f.cases[0]['predicates'][0].pop('comparison'),
            lambda f: f.cases[0].update(predicates=[]),
            lambda f: f.cases[0].update(evaluator_sha256='0'*64),
            lambda f: f.reports['supported-output']['family_evidence'][0].update(controls=['invented']),
            lambda f: f.cases[0].update(limits=[]),
            lambda f: f.cases[0].update(environment={'platform': 'synthetic'})]
        for index, change in enumerate(mutations):
            with self.subTest(index=index), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary); fixture = Fixture(root); change(fixture); fixture.save()
                with self.assertRaises(ValueError): audit.audit(root)

    def test_semantic_negative_can_have_its_own_measurement_without_an_outer_rule_failure(self):
        with tempfile.TemporaryDirectory() as temporary:
            root=Path(temporary);fixture=Fixture(root)
            case=fixture.cases[1]
            case.pop('native_outcome')
            case.update(output={'sha256':'3'*64,'bytes':13},document=None)
            predicate=case['predicates'][0]
            predicate.update(expected='finding',actual='finding',
                             comparison={'operator':'eq','actual':881,'expected':880})
            fixture.save()
            self.assertEqual(audit.audit(root)['status'],'passed')
            for change in ('false-finding','stale-evaluator','invented-document','wrong-document-identity'):
                with self.subTest(change=change):
                    saved=copy.deepcopy(case)
                    if change=='false-finding':predicate['comparison']['actual']=880
                    elif change=='stale-evaluator':case['evaluator_sha256']='0'*64
                    elif change=='invented-document':case['document']='invented'
                    else:
                        case['document']='outer-negative';case['output']['sha256']='4'*64
                    fixture.save()
                    with self.assertRaises(ValueError):audit.audit(root)
                    case.clear();case.update(saved);predicate=case['predicates'][0]

    def test_current_source_evaluator_report_and_assurance_hashes_are_required(self):
        for target in ('source', 'evaluator', 'receipt', 'assurance', 'catalog'):
            with self.subTest(target=target), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary); fixture = Fixture(root)
                paths = {'source': audit.SURFACE_FILES[0], 'evaluator': 'scripts/independent.py',
                    'receipt': audit.RECEIPTS['supported-output'], 'assurance': 'docs/requirements/pdfx4-inventory.json',
                    'catalog': 'docs/requirements/public-output-rules.json'}
                (root/paths[target]).write_text('{}')
                with self.assertRaises(ValueError): audit.audit(root)

    def test_surface_drift_or_generation_role_change_fails_with_fresh_hash(self):
        for target in ('route', 'form', 'role', 'policy'):
            with self.subTest(target=target), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary); fixture = Fixture(root)
                if target == 'route': fixture.contract['routes'][0]['signature'] = 'pub fn renamed()'
                elif target == 'form': fixture.contract['api_forms'][0]['shape'] = 'renamed'
                elif target == 'role': fixture.contract['routes'][0]['role'] = 'generation'
                else: fixture.contract['policies']['adapter_branches']['fonts_none'] = 'positive'
                fixture.save()
                with self.assertRaises(ValueError): audit.audit(root)

    def test_only_actual_executed_native_lists_can_establish_evidence(self):
        for mutation in ('omitted', 'duplicate', 'count', 'target'):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary); fixture = Fixture(root)
                report = fixture.reports['windows']; suite = report['test_suites'][0]
                if mutation == 'omitted': suite.pop('tests')
                elif mutation == 'duplicate': suite['tests'][0] = suite['tests'][1]
                elif mutation == 'count': suite['passed'] += 1
                else: report['target'] = 'unknown'
                fixture.save()
                with self.assertRaises(ValueError): audit.audit(root)

    def test_receipt_paths_output_identity_and_public_shape_are_closed(self):
        for mutation in ('path', 'document', 'private-shape', 'precondition', 'missing-generation'):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary); fixture = Fixture(root)
                if mutation == 'document':
                    fixture.cases[0]['document'] = 'ordinary'; fixture.cases[0]['output']['sha256'] = '4'*64
                elif mutation == 'private-shape': fixture.contract['unlisted_source_map'] = []
                elif mutation == 'precondition': fixture.contract['families'][0]['preconditions'] = ['avoid malformed resources']
                fixture.save()
                if mutation == 'path':
                    fixture.contract['receipts'][0]['path'] = 'docs/verification/other.json'
                    fixture.write(audit.CONTRACT, fixture.contract)
                elif mutation == 'missing-generation':
                    path = 'docs/requirements/pdfx4-inventory.json'; data = json.loads((root/path).read_text())
                    data['coverage_assurance'].pop('generation_contract'); fixture.write(path, data)
                with self.assertRaises(ValueError): audit.audit(root)

    def test_lexical_surface_ignores_comments_strings_and_tracks_fields(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            for source in audit.SURFACE_FILES:
                path = root/source; path.parent.mkdir(parents=True, exist_ok=True); path.write_text('')
            (root/audit.SURFACE_FILES[0]).write_text('''
                // pub fn fake() {}
                const TEXT: &str = r#"pub struct Fake;"#;
                pub struct Real { pub field: Vec<(u8, u8)>, hidden: u8 }
                impl Real { pub fn get(&self) -> &str { "}" } }
                pub enum Choice { A, B { count: u32 } }
            ''')
            routes, forms = audit.discover_surface(root)
            self.assertEqual(len(routes), 1)
            self.assertTrue(next(iter(routes)).endswith('Real::get'))
            real = next(row for row in forms.values() if row['id'].endswith('::Real'))
            self.assertIn('pub field: Vec<(u8, u8)>', real['shape'])
            self.assertNotIn('hidden', real['shape'])

    def test_relevant_control_kinds_and_print_only_invariant_are_supported(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); fixture = Fixture(root)
            family = fixture.contract['families'][0]; invariant = family['invariants'][0]
            family['scope'] = ['pdfx4']; invariant['modes'] = ['pdfx4']
            invariant['required_control_kinds'] = ['positive', 'negative']
            removed = set(invariant['controls'][2:]); del invariant['controls'][2:]
            fixture.cases[:] = [case for case in fixture.cases if case['id'] not in removed]
            fixture.cases[0]['mode'] = 'pdfx4'; fixture.save()
            self.assertEqual(audit.audit(root)['families'], len(audit.FAMILIES))
            invariant['required_control_kinds'].append('boundary'); fixture.save()
            with self.assertRaisesRegex(ValueError, 'partitions'): audit.audit(root)

    def test_emitted_finding_and_successful_native_refusal_are_distinct(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); fixture = Fixture(root)
            case = fixture.cases[1]; case.pop('native_outcome')
            case.update(output={'sha256': '3'*64, 'bytes': 13}, document='outer-negative')
            case['predicates'][0].update(expected='finding', actual='finding')
            case['predicates'][0]['comparison']['actual'] = False
            fixture.save(); self.assertEqual(audit.audit(root)['status'], 'passed')
            case['document'] = 'ordinary'; fixture.save()
            with self.assertRaisesRegex(ValueError, 'negative report'): audit.audit(root)

    def test_refusal_requires_driver_error_and_actual_native_execution(self):
        for change in (lambda row: row.pop('native_outcome'),
                       lambda row: row['native_outcome'].update(error_code=''),
                       lambda row: row['native_outcome']['driver'].update(sha256='0'*64),
                       lambda row: row['native_outcome']['native_test'].update(test='invented')):
            with self.subTest(change=change), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary); fixture = Fixture(root)
                # Isolate the record from the code binding object that was reused in the fixture.
                fixture.cases[1]['native_outcome'] = copy.deepcopy(fixture.cases[1]['native_outcome'])
                change(fixture.cases[1]); fixture.save()
                with self.assertRaises(ValueError): audit.audit(root)

    def test_concrete_owner_and_unowned_measured_cases_cannot_be_invented(self):
        for mutation in ('owner', 'unowned'):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary); fixture = Fixture(root)
                if mutation == 'owner':
                    fixture.contract['families'][0]['invariants'][0]['native']['symbol'] = 'Invented::admit'
                else:
                    row = copy.deepcopy(fixture.cases[0]); row['id'] = 'detached-control'; fixture.cases.append(row)
                fixture.save()
                with self.assertRaises(ValueError): audit.audit(root)

    def test_conditional_adapter_and_diagnostic_roles_are_explicit(self):
        expected = {
            'crates/print-core/src/finished_writer.rs::FinishedPdfWriter::write': 'generation',
            'crates/print-core/src/scene.rs::ScenePdfWriter::write_with_rasters_and_fonts': 'ordinary-generation',
            'crates/print-core/src/production.rs::PublicationCandidateWriter::write_with_rasters_and_fonts': 'adapter-generation',
            'crates/print-core/src/production.rs::PublicationCandidateWriter::write_with_rasters': 'diagnostic-generation',
            'crates/font-boundary/src/lib.rs::TrustedFontBoundary::prepare_publication': 'preparation',
            'crates/font-boundary/src/lib.rs::PdfFontMetrics::italic_angle': 'observation',
        }
        self.assertEqual({key: audit.route_role(key) for key in expected}, expected)
        self.assertEqual(audit.POLICIES['adapter_branches'], {
            'scene_public_overloads': 'strict-ordinary-generation',
            'fonts_none': 'diagnostic-authoring-outside-positive-pdfx',
            'fonts_some_including_empty': 'strict-font-contract',
            'unmanaged_rgb': 'separate-diagnostic-exclusion'})

    def test_emitted_positive_cannot_borrow_negative_or_wrong_mode_document(self):
        for mutation in ('negative', 'mode'):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary); fixture = Fixture(root)
                if mutation == 'negative':
                    fixture.cases[0].update(document='outer-negative', output={'sha256': '3'*64, 'bytes': 13})
                else: fixture.cases[0]['document'] = 'print'
                fixture.save()
                with self.assertRaises(ValueError): audit.audit(root)


if __name__ == '__main__':
    unittest.main()
