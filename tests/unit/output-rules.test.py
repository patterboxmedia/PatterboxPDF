"""Synthetic neutral catalog controls; no private requirements data is imported."""
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest

spec = importlib.util.spec_from_file_location('output_audit', Path(__file__).resolve().parents[2]/'scripts/verify-output-rules.py')
audit = importlib.util.module_from_spec(spec)
spec.loader.exec_module(audit)


class Fixture:
    def __init__(self, root):
        self.root = root
        self.rules = {'base-format': 'Readable original fixture.', 'font-descriptors': 'Measured original fixture.'}
        self.evaluator = self.write(audit.EVALUATOR, 'RULES = '+repr(self.rules)+"\nfont_evaluator('pdf_font_program_rules')\n")
        self.helpers = [self.write('scripts/'+name+'.py', '# Original fixture.\n') for name in ('pdf_font_program_rules', 'verify-font-metrics')]
        writer = self.write('crates/example/src/lib.rs', '// Original fixture.\n')
        color = self.write('scripts/verify-cmyk-gradients.py', '# Original color fixture.\n')
        checks = [{'id': 'metrics', 'evaluator': self.helpers[1], 'report': {'passed': True, 'findings': [], 'evaluator_sha256': self.helpers[1]['sha256']}}]
        supported = {'schema': 'patterboxpdf-supported-output/v1', 'status': 'passed',
                     'documents': [self.report('ordinary', 'ordinary', True), self.report('print', 'pdfx4', True)],
                     'negative_controls': [{'id': key, 'rule': key, 'report': self.report(key, 'pdfx4', False, key)} for key in self.rules],
                     'additional_checks': checks}
        metrics = {'mean_channel_difference': 1, 'p95_max_channel_difference': 24, 'pixels_above_10_percent': 10}
        baseline = [{'mode': mode, 'pages': [{'page': page, 'metrics': metrics, 'passed': False} for page in pages]}
                    for mode, pages in (('ordinary', (2,)), ('pdfx4', (1, 2)))]
        color_report = {'schema': 'patterboxpdf-cmyk-diagnostic/v1', 'controlled_checks_passed': True,
                        'native_cmyk_components_checked': True, 'verifier': {'sha256': color['sha256']},
                        'negative_controls': {'wrong_gradient_rejected': True, 'mislabeled_profile_rejected': True},
                        'baseline': {'default_comparison_passed': False, 'fixed_bounds': {'mean_channel_difference_max': 5,
                            'p95_max_channel_difference_max': 20, 'pixels_above_10_percent_max': 8}, 'default_metrics': baseline}}
        linux = {'schema': 'patterboxpdf-linux-runtime/v1', 'status': 'passed', 'native_tests_passed': 9, 'ignored': 0,
                 'test_suites': [{'passed': 9, 'ignored': 0}], 'target': 'x86_64-unknown-linux-gnu',
                 'kernel': {'landlock_abi': 3}, 'validator_runtime': 'explicitly unsupported; tested refusal'}
        windows = {'schema': 'patterboxpdf-windows-runtime/v1', 'status': 'passed', 'native_tests_passed': 6,
                   'ignored': 0, 'test_suites': [{'passed': 6, 'ignored': 0}], 'confinement_tests_passed': 6}
        receipts = [dict(kind=kind, **self.write(audit.RECEIPTS[kind], report))
                    for kind, report in zip(audit.RECEIPTS, (supported, color_report, linux, windows))]
        self.catalog = {'schema': 'patterboxpdf-output-rules/v1', 'scope': 'Original neutral audit fixture.',
                        'evaluator': self.evaluator, 'helpers': self.helpers, 'code_bindings': [self.evaluator, *self.helpers, writer, color],
                        'receipts': receipts, 'rules': [{'id': key, 'summary': summary, 'status': 'verified',
                            'implementation_paths': [writer['path']], 'documents': ['ordinary', 'print'], 'negative_controls': [key],
                            **({'additional_checks': ['metrics']} if key == 'font-descriptors' else {})} for key, summary in self.rules.items()]}
        self.save()

    def write(self, name, data):
        payload = (json.dumps(data, indent=2)+'\n').encode() if isinstance(data, dict) else data.encode()
        path = self.root/name
        path.parent.mkdir(parents=True, exist_ok=True)
        path.write_bytes(payload)
        return {'path': name, 'sha256': audit.digest(payload)}

    def report(self, name, mode, passed, rule=None):
        return {'id': name, 'schema': 'pdf-library-supported-output-verification/v1', 'mode': mode,
                'input_sha256': '1'*64, 'input_bytes': 10, 'unchanged': True, 'passed': passed,
                'rules_evaluated': list(self.rules), 'findings': [] if passed else [{'rule': rule}],
                'qpdf': {'exit_code': 0}, 'evaluator_sha256': self.evaluator['sha256']}

    def save(self):
        row = self.write('docs/requirements/public-output-rules.json', self.catalog)
        self.write('docs/requirements/pdfx4-inventory.json', {'schema': 'pdf-library-requirements/v1',
                   'normative_requirements': [], 'coverage_assurance': {'status': 'complete', 'catalog': row}})

    def mutate_receipt(self, kind, change):
        row = next(row for row in self.catalog['receipts'] if row['kind'] == kind)
        data = json.loads((self.root/row['path']).read_text())
        change(data)
        row.update(self.write(row['path'], data))
        self.save()


class OutputRuleTests(unittest.TestCase):
    def test_current_neutral_catalog_passes_and_source_drift_fails(self):
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary); fixture = Fixture(root)
            self.assertEqual(audit.audit(root)['rules'], 2)
            (root/'crates/example/src/lib.rs').write_text('// Changed original fixture.\n')
            with self.assertRaisesRegex(ValueError, 'stale code'):
                audit.audit(root)

    def test_missing_rule_pending_rule_or_missing_control_fails(self):
        for mutation in ('removed', 'pending', 'negative'):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary); fixture = Fixture(root)
                if mutation == 'removed': fixture.catalog['rules'].pop()
                elif mutation == 'pending': fixture.catalog['rules'][0]['status'] = 'pending'
                else: fixture.catalog['rules'][0]['negative_controls'] = []
                fixture.save()
                with self.assertRaises(ValueError): audit.audit(root)

    def test_false_positive_negative_or_omitted_report_rule_fails(self):
        def false_green(data): data['documents'][0]['findings'] = [{'rule': 'base-format'}]
        def false_negative(data): data['negative_controls'][0]['report']['passed'] = True
        def removed(data): data['documents'][0]['rules_evaluated'].pop()
        def missing_metrics(data): data['additional_checks'][0]['report']['passed'] = False
        def missing_qpdf(data): data['documents'][0]['qpdf'] = None
        for change in (false_green, false_negative, removed, missing_metrics, missing_qpdf):
            with self.subTest(change=change.__name__), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary); fixture = Fixture(root)
                fixture.mutate_receipt('supported-output', change)
                with self.assertRaises(ValueError): audit.audit(root)

    def test_stale_receipt_or_assurance_or_missing_fourth_receipt_fails(self):
        for mutation in ('receipt', 'assurance', 'missing'):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary); fixture = Fixture(root)
                if mutation == 'receipt': (root/audit.RECEIPTS['windows']).write_text('{}')
                elif mutation == 'assurance': fixture.write('docs/requirements/pdfx4-inventory.json', {'coverage_assurance': {'status': 'incomplete'}})
                else: fixture.catalog['receipts'].pop(); fixture.save()
                with self.assertRaises(ValueError): audit.audit(root)

    def test_relaxed_color_failure_or_skipped_runtime_cannot_turn_green(self):
        for kind, change in (('color', lambda row: row['baseline'].update(default_comparison_passed=True)),
                             ('linux', lambda row: row['test_suites'][0].update(ignored=1)),
                             ('windows', lambda row: row.update(confinement_tests_passed=0))):
            with self.subTest(kind=kind), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary); fixture = Fixture(root)
                fixture.mutate_receipt(kind, change)
                with self.assertRaises(ValueError): audit.audit(root)

    def test_private_shape_or_escaping_binding_is_refused(self):
        for mutation in ('shape', 'escape'):
            with self.subTest(mutation=mutation), tempfile.TemporaryDirectory() as temporary:
                root = Path(temporary); fixture = Fixture(root)
                if mutation == 'shape': fixture.catalog['unlisted_source_map'] = []
                else: fixture.catalog['code_bindings'][0]['path'] = '../elsewhere.py'
                fixture.save()
                with self.assertRaises(ValueError): audit.audit(root)


if __name__ == '__main__':
    unittest.main()
