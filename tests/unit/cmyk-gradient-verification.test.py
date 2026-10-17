"""Protect CMYK/alpha interpretation and the retained default-failure boundary."""
import copy
import importlib.util
from pathlib import Path
import unittest

import numpy as np
from pypdf.generic import ArrayObject, NumberObject

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('cmyk', ROOT/'scripts/verify-cmyk-gradients.py')
cmyk = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(cmyk)


def linear(a, b):
    return cmyk.dictionary(FunctionType=NumberObject(2), Domain=cmyk.array([0, 1]),
                           C0=cmyk.array(a), C1=cmyk.array(b), N=NumberObject(1))


class ColorDiagnosticTests(unittest.TestCase):
    def test_independent_components_and_opacity(self):
        color = linear([0, 1, 0, 0], [1, 0, 1, .25])
        alpha = linear([0], [1])
        np.testing.assert_allclose(cmyk.evaluate_function(color, .25), [.25, .75, .25, .0625])
        self.assertEqual(cmyk.evaluate_function(alpha, .25).tolist(), [.25])
        # Components stay straight: opacity does not premultiply their values.
        self.assertNotEqual(cmyk.evaluate_function(color, .25).tolist(), [.0625, .1875, .0625, .015625])

    def test_hard_stop_and_segment_encoding(self):
        jump = cmyk.dictionary(FunctionType=NumberObject(3), Domain=cmyk.array([0, 1]),
            Bounds=cmyk.array([.5]), Encode=cmyk.array([0, 1, 0, 1]),
            Functions=ArrayObject([linear([0], [0]), linear([1], [1])]))
        self.assertEqual([cmyk.evaluate_function(jump, t).tolist() for t in (.499, .5, .501)], [[0], [1], [1]])
        ramp = copy.deepcopy(jump)
        ramp[cmyk.n('Functions')] = ArrayObject([linear([0], [1]), linear([0], [1])])
        self.assertEqual(cmyk.evaluate_function(ramp, .75).tolist(), [.5])

    def test_corrupt_interpolation_cannot_be_accepted(self):
        function = linear([0, 1, 0, 0], [1, 0, 1, .25])
        function[cmyk.n('N')] = NumberObject(2)
        with self.assertRaisesRegex(ValueError, 'exponent'):
            cmyk.evaluate_function(function, .5)
        for t in (float('nan'), float('inf'), -.1, 1.1):
            with self.subTest(t=t), self.assertRaises(ValueError):
                cmyk.evaluate_function(linear([0], [1]), t)

    def test_mislabeled_profile_is_rejected(self):
        data = bytearray(132)
        data[:4] = len(data).to_bytes(4, 'big')
        data[36:40], data[16:20] = b'acsp', b'RGB '
        with self.assertRaisesRegex(ValueError, 'four CMYK channels'):
            cmyk.profile_channels(data)

    def test_controlled_pass_cannot_relabel_default_failure(self):
        result = {'fixed_bounds': dict(cmyk.BOUNDS), 'passed': False, 'all_inputs_unchanged': True, 'files': []}
        for mode, digest in cmyk.BASELINE_HASHES.items():
            pages = []
            for page in (1, 2, 3):
                failed = page == 2 or (mode == 'pdfx4' and page == 1)
                pages.append({'page': page, 'passed': not failed, 'metrics': {
                    'mean_channel_difference': 1., 'p95_max_channel_difference': 23. if failed else 2.,
                    'pixels_above_10_percent': 9. if failed else .2}})
            result['files'].append({'mode': mode, 'sha256': digest, 'pages': pages})
        cmyk.audit_default_comparison(result)
        for change in ('bounds', 'overall', 'page', 'identity', 'metrics'):
            bad = copy.deepcopy(result)
            if change == 'bounds':
                bad['fixed_bounds']['p95_max_channel_difference_max'] = 25
            elif change == 'overall':
                bad['passed'] = True
            elif change == 'page':
                bad['files'][0]['pages'][1]['passed'] = True
            elif change == 'identity':
                bad['files'][0]['sha256'] = '0'*64
            else:
                bad['files'][0]['pages'][1]['metrics']['p95_max_channel_difference'] = float('nan')
            with self.subTest(change=change), self.assertRaises(ValueError):
                cmyk.audit_default_comparison(bad)


if __name__ == '__main__':
    unittest.main()
