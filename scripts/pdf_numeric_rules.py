"""Exact decimal matrix checks for the supported generated content subset.

The native writer's nonsingularity contract is stronger than the PDF warning
about noninvertible painting. This is a generator-contract check, not a claim
that every singular matrix makes an arbitrary PDF invalid.
"""
from fractions import Fraction
import re


DECIMAL = re.compile(rb'[+-]?(?:[0-9]+(?:\.[0-9]*)?|\.[0-9]+)\Z')
DELIMITERS = b'\0\t\n\x0c\r ()<>[]{}/%'


def matrix_tokens(data):
    """Keep number spellings, skipping comments, names and complete strings."""
    offset, numbers = 0, []
    while offset < len(data):
        byte = data[offset]
        if byte in b'\0\t\n\x0c\r ':
            offset += 1
            continue
        if byte == 37:
            end = re.search(rb'[\r\n]', data[offset:])
            offset = offset + end.start() if end else len(data)
            continue
        if byte == 40:
            depth = 1; offset += 1
            while offset < len(data) and depth:
                if data[offset] == 92:
                    offset += 2
                    continue
                depth += (data[offset] == 40) - (data[offset] == 41)
                offset += 1
            if depth:
                raise ValueError('Unterminated content string')
            numbers = []
            continue
        if data[offset:offset+2] in (b'<<', b'>>'):
            offset += 2; numbers = []
            continue
        if byte == 60 and data[offset:offset+2] != b'<<':
            end = data.find(b'>', offset + 1)
            if end < 0:
                raise ValueError('Unterminated content hex string')
            offset = end + 1; numbers = []
            continue
        if byte in DELIMITERS:
            offset += 1; numbers = []
            if byte == 47:
                while offset < len(data) and data[offset] not in DELIMITERS:
                    offset += 1
            continue
        end = offset + 1
        while end < len(data) and data[end] not in DELIMITERS:
            end += 1
        token, offset = data[offset:end], end
        if DECIMAL.fullmatch(token):
            numbers.append(token.decode('ascii'))
        else:
            if token in (b'cm', b'Tm'):
                if len(numbers) != 6:
                    raise ValueError('Matrix does not have six decimal operands')
                yield token.decode('ascii'), numbers
            numbers = []


def inspect_content_matrices(data):
    matrices, findings = [], []
    try:
        for operator, operands in matrix_tokens(data):
            a, b, c, d, _, _ = map(Fraction, operands)
            determinant = a*d - b*c
            receiver = [0 if 0 < abs(value) < Fraction(1, 2**126) else value
                        for value in (a, b, c, d)]
            matrices.append({'operator': operator, 'operands': operands,
                             'determinant': str(determinant),
                             'permitted_underflow_determinant': str(receiver[0]*receiver[3]-receiver[1]*receiver[2])})
            if determinant == 0:
                findings.append('Encoded matrix loses the supported nonsingularity relationship')
    except ValueError as error:
        findings.append(str(error))
    return {'passed': not findings, 'findings': findings, 'matrices': matrices}
