"""Audit the neutral supported-output catalog and its exact code/evidence bindings.

This checks declared evidence, hashes and verdict consistency. It does not recreate
the evaluations or certify arbitrary PDFs. No private requirements inventory is read.
"""
import argparse
import ast
import hashlib
import json
from pathlib import Path, PurePosixPath
import re

ROOT = Path(__file__).resolve().parents[1]
EVALUATOR = 'scripts/verify-pdfx-supported-output.py'
RECEIPTS = {kind: 'docs/verification/current/three-area-20261016/'+kind+'.json'
            for kind in ('supported-output', 'color', 'linux', 'windows')}
RECEIPT_SETS = {'accepted-inputs-20261016': {
    kind: 'docs/verification/current/accepted-inputs-20261016/'+name+'.json'
    for kind, name in (('supported-output', 'supported-inputs'), ('color', 'color'),
                       ('linux', 'linux'), ('windows', 'windows'))}}
SHA = re.compile(r'[0-9a-f]{64}')


def require(value, message):
    if not value:
        raise ValueError(message)


def digest(data):
    return hashlib.sha256(data).hexdigest()


def file_bytes(root, name):
    require(isinstance(name, str) and name and '\\' not in name and ':' not in name
            and not PurePosixPath(name).is_absolute() and all(part not in ('', '.', '..') for part in name.split('/')),
            'unsafe public binding path')
    path = root/name
    require(path.is_file() and path.resolve().is_relative_to(root)
            and not any(parent.is_symlink() for parent in (path, *path.parents) if parent.is_relative_to(root)),
            'missing, linked or escaping public binding: '+name)
    return path.read_bytes()


def binding(root, row):
    require(isinstance(row, dict) and set(row) == {'path', 'sha256'}
            and isinstance(row['sha256'], str) and SHA.fullmatch(row['sha256']), 'invalid code/evidence identity')
    data = file_bytes(root, row['path'])
    require(digest(data) == row['sha256'], 'stale code/evidence identity: '+row['path'])
    return data


def unique(rows, key, label):
    require(isinstance(rows, list) and rows, 'empty '+label)
    result = {row[key]: row for row in rows}
    require(len(result) == len(rows), 'duplicate '+label)
    return result


def evaluator_contract(data):
    tree = ast.parse(data.decode('utf-8'))
    values = [ast.literal_eval(node.value) for node in tree.body if isinstance(node, ast.Assign)
              and any(isinstance(target, ast.Name) and target.id == 'RULES' for target in node.targets)]
    require(len(values) == 1 and isinstance(values[0], dict) and values[0]
            and all(isinstance(key, str) and isinstance(value, str) and value.strip() for key, value in values[0].items()),
            'evaluator must declare one nonempty neutral rule dictionary')
    helpers = {'scripts/'+node.args[0].value+'.py' for node in ast.walk(tree)
               if isinstance(node, ast.Call) and isinstance(node.func, ast.Name) and node.func.id == 'font_evaluator'
               and node.args and isinstance(node.args[0], ast.Constant) and isinstance(node.args[0].value, str)}
    return values[0], helpers


def inspect_report(report, rules, evaluator_sha, positive):
    require(report.get('schema') == 'pdf-library-supported-output-verification/v1'
            and report.get('evaluator_sha256') == evaluator_sha and report.get('unchanged') is True
            and isinstance(report.get('input_sha256'), str) and SHA.fullmatch(report['input_sha256'])
            and type(report.get('input_bytes')) is int and report['input_bytes'] > 0,
            'output report identity or unchanged-input evidence disagrees')
    evaluated = report.get('rules_evaluated', [])
    require(len(evaluated) == len(rules) and set(evaluated) == set(rules), 'output report omitted a rule')
    findings = report.get('findings')
    require(isinstance(findings, list) and report.get('passed') is positive
            and (not findings if positive else bool(findings)), 'output report has a false verdict')
    require(all(row.get('rule') in rules for row in findings), 'output report has an unknown finding rule')
    if positive:
        require((report.get('qpdf') or {}).get('exit_code') == 0, 'positive output lacks a successful exact qpdf check')


def audit_font_observations(documents, rules):
    if 'font-program-layout' not in rules:
        return
    kinds = set()
    for report in documents.values():
        for font in report.get('observations', {}).get('fonts', []):
            kind = font.get('program_layout', {}).get('kind')
            require(kind in ('truetype', 'cff'), 'binary font observations are missing')
            kinds.add(kind)
            if kind == 'truetype':
                hint = font.get('hint_execution') or {}
                executed=(hint.get('glyph_loads_attempted', 0) > 0
                          and hint.get('glyph_loads_attempted') == hint.get('glyph_loads_succeeded'))
                programs=hint.get('programs') or {}
                absent=(hint.get('hint_programs_absent') is True and hint.get('glyph_count',0)>0
                        and all(programs.get(key) == 0 for key in
                                ('fpgm_bytes','prep_bytes','glyph_instruction_bytes','glyphs_with_instructions')))
                require((executed or absent)
                        and hint.get('input_sha256') == font.get('program_sha256') == hint.get('input_sha256_after'),
                        'TrueType instruction execution or exact absence observations are incomplete')
    require(kinds == {'truetype', 'cff'}, 'both supported font program families need current positive evidence')


def audit_color(report, bindings):
    require(report.get('schema') == 'patterboxpdf-cmyk-diagnostic/v1'
            and report.get('controlled_checks_passed') is True and report.get('native_cmyk_components_checked') is True,
            'color controls are incomplete')
    require(report.get('verifier', {}).get('sha256') == bindings.get('scripts/verify-cmyk-gradients.py'),
            'color verifier identity disagrees')
    require(report.get('negative_controls', {}).get('wrong_gradient_rejected') is True
            and report['negative_controls'].get('mislabeled_profile_rejected') is True, 'color negative controls are incomplete')
    baseline = report['baseline']
    require(baseline.get('fixed_bounds') == {'mean_channel_difference_max': 5,
            'p95_max_channel_difference_max': 20, 'pixels_above_10_percent_max': 8}
            and baseline.get('default_comparison_passed') is False, 'default color bounds or failed verdict changed')
    failed = set()
    for document in baseline['default_metrics']:
        for page in document['pages']:
            metrics = page['metrics']
            values = [metrics['mean_channel_difference'], metrics['p95_max_channel_difference'], metrics['pixels_above_10_percent']]
            require(all(type(value) in (float, int) and float('-inf') < value < float('inf') and value >= 0 for value in values),
                    'nonfinite color comparison metric')
            passed = values[0] <= 5 and values[1] <= 20 and values[2] <= 8
            require(page['passed'] is passed, 'default color page verdict disagrees with metrics')
            if not passed:
                failed.add((document['mode'], page['page']))
    require(failed == {('ordinary', 2), ('pdfx4', 1), ('pdfx4', 2)}, 'original failed color cases were removed')


def audit_runtime(report, target):
    require(report.get('schema') == 'patterboxpdf-'+target+'-runtime/v1' and report.get('status') == 'passed'
            and type(report.get('native_tests_passed')) is int and report['native_tests_passed'] > 0
            and report.get('ignored') == 0, target+' runtime evidence is incomplete')
    suites = report.get('test_suites', [])
    require(suites and all(type(row.get('passed')) is int and row['passed'] > 0 and row.get('ignored') == 0
                          for row in suites) and sum(row['passed'] for row in suites) == report['native_tests_passed'],
            target+' runtime tests are empty, skipped or inconsistent')
    if target == 'linux':
        require(report.get('target') == 'x86_64-unknown-linux-gnu' and report.get('kernel', {}).get('landlock_abi', 0) >= 3
                and report.get('validator_runtime') == 'explicitly unsupported; tested refusal', 'Linux qualification boundary disagrees')
    else:
        require(type(report.get('confinement_tests_passed')) is int and report['confinement_tests_passed'] >= 6,
                'Windows native confinement proof is incomplete')


def audit(root, catalog_path='docs/requirements/public-output-rules.json'):
    root = Path(root).resolve(strict=True)
    catalog_data = file_bytes(root, catalog_path)
    catalog = json.loads(catalog_data)
    keys = {'schema', 'scope', 'evaluator', 'helpers', 'code_bindings', 'rules', 'receipts'}
    if catalog.get('schema') == 'patterboxpdf-output-rules/v2':
        require(set(catalog) == keys | {'receipt_set'} and catalog['receipt_set'] in RECEIPT_SETS,
                'unsupported neutral catalog receipt set')
        receipt_paths = RECEIPT_SETS[catalog['receipt_set']]
    else:
        require(set(catalog) == keys and catalog.get('schema') == 'patterboxpdf-output-rules/v1',
                'unsupported neutral catalog shape')
        receipt_paths = RECEIPTS
    require(isinstance(catalog['scope'], str) and bool(catalog['scope'].strip()), 'empty neutral catalog scope')
    require(catalog['evaluator']['path'] == EVALUATOR, 'unexpected output evaluator')
    rules, used_helpers = evaluator_contract(binding(root, catalog['evaluator']))
    code = unique(catalog['code_bindings'], 'path', 'code bindings')
    for path, row in code.items():
        require((path in ('Cargo.toml', 'Cargo.lock', 'rust-toolchain.toml')
                 or path.startswith(('scripts/', 'crates/', 'apps/', 'examples/standalone-client/')))
                and PurePosixPath(path).suffix in ('.py', '.rs', '.toml', '.lock', '.json'),
                'unsupported code binding path')
        binding(root, row)
    hashes = {path: row['sha256'] for path, row in code.items()}
    require(hashes.get(EVALUATOR) == catalog['evaluator']['sha256'], 'evaluator code binding is missing')
    helpers = unique(catalog['helpers'], 'path', 'helper bindings')
    require(used_helpers.issubset(helpers), 'an evaluator helper binding is missing')
    for path, row in helpers.items():
        require(path.startswith('scripts/') and path.endswith('.py') and hashes.get(path) == row['sha256'],
                'helper code binding disagrees')
        binding(root, row)
    receipts = unique(catalog['receipts'], 'kind', 'public receipts')
    require(set(receipts) == set(receipt_paths), 'the four public receipts are required')
    evidence = {}
    for kind, row in receipts.items():
        require(set(row) == {'kind', 'path', 'sha256'} and row['path'] == receipt_paths[kind], 'unexpected public receipt path')
        evidence[kind] = json.loads(binding(root, {'path': row['path'], 'sha256': row['sha256']}))
    supported = evidence['supported-output']
    require(supported.get('schema') == 'patterboxpdf-supported-output/v1' and supported.get('status') == 'passed',
            'supported output evidence is pending')
    documents = unique(supported.get('documents'), 'id', 'positive output reports')
    require({row.get('mode') for row in documents.values()} >= {'ordinary', 'pdfx4'}, 'ordinary and print reports are required')
    for report in documents.values():
        inspect_report(report, rules, catalog['evaluator']['sha256'], True)
    audit_font_observations(documents, rules)
    controls = unique(supported.get('negative_controls'), 'id', 'negative output controls')
    for row in controls.values():
        require(row.get('rule') in rules, 'negative control rule is unknown')
        inspect_report(row['report'], rules, catalog['evaluator']['sha256'], False)
        require(any(finding['rule'] == row['rule'] for finding in row['report']['findings']), 'negative control did not catch its claimed rule')
    checks = {}
    if supported.get('additional_checks'):
        checks = unique(supported['additional_checks'], 'id', 'additional measurements')
        for check in checks.values():
            evaluator = check['evaluator']
            require(evaluator['path'] in helpers and hashes.get(evaluator['path']) == evaluator['sha256'], 'measurement evaluator is unbound')
            binding(root, evaluator)
            require(check['report'].get('passed') is True and check['report'].get('findings') == []
                    and check['report'].get('evaluator_sha256') == evaluator['sha256'], 'measurement is pending or has findings')
    declared = unique(catalog['rules'], 'id', 'output rules')
    require(set(declared) == set(rules), 'catalog added or removed an evaluator rule')
    for key, rule in declared.items():
        require(rule.get('summary') == rules[key] and rule.get('status') == 'verified', 'rule summary or verified status disagrees: '+key)
        implementation = rule.get('implementation_paths', [])
        require(implementation and all(path in code and path.startswith(('apps/', 'crates/')) and path.endswith('.rs') for path in implementation),
                'rule implementation identity is missing: '+key)
        require(rule.get('documents') and all(value in documents for value in rule['documents']), 'positive rule evidence is missing: '+key)
        require(rule.get('negative_controls') and all(value in controls and controls[value]['rule'] == key for value in rule['negative_controls']),
                'negative rule evidence is missing: '+key)
        require(all(value in checks for value in rule.get('additional_checks', [])), 'unknown additional measurement')
        if key == 'font-descriptors':
            require(any(checks[value]['evaluator']['path'] == 'scripts/verify-font-metrics.py' for value in rule.get('additional_checks', [])),
                    'descriptor semantics require current source-font measurements')
    inventory = json.loads(file_bytes(root, 'docs/requirements/pdfx4-inventory.json'))
    assurance = inventory.get('coverage_assurance', {})
    require(inventory.get('schema') == 'pdf-library-requirements/v1' and inventory.get('normative_requirements') == []
            and assurance.get('status') == 'complete'
            and assurance.get('catalog') == {'path': catalog_path, 'sha256': digest(catalog_data)}, 'public coverage assurance is stale or incomplete')
    if catalog['schema'] == 'patterboxpdf-output-rules/v2':
        generation = assurance.get('generation_contract', {})
        require(generation.get('path') == 'docs/requirements/generation-contract.json',
                'generation coverage assurance is missing')
        contract = json.loads(binding(root, generation))
        require(contract.get('schema') == 'patterboxpdf-generation-contract/v1' and contract.get('status') == 'complete',
                'generation contract is pending or unsupported')
    audit_color(evidence['color'], hashes)
    audit_runtime(evidence['linux'], 'linux')
    audit_runtime(evidence['windows'], 'windows')
    return {'schema': 'patterboxpdf-output-rule-audit/v1', 'status': 'passed', 'rules': len(rules),
            'documents': len(documents), 'negative_controls': len(controls), 'public_receipts': 4,
            'catalog_sha256': digest(catalog_data), 'scope': 'catalog, exact code/evidence identities and verdict consistency only'}


if __name__ == '__main__':
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument('--root', type=Path, default=ROOT)
    args = parser.parse_args()
    print(json.dumps(audit(args.root), indent=2))
