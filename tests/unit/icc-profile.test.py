#!/usr/bin/env python3
"""Independent binary controls for the standalone ICC evaluator."""
import importlib.util
import struct
import unittest
from pathlib import Path

ROOT = Path(__file__).resolve().parents[2]
SPEC = importlib.util.spec_from_file_location('icc_rules', ROOT/'scripts/verify-icc-profile.py')
ICC = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(ICC)


def typed(kind, body=b''):
    return kind+bytes(4)+body


def lut(inputs, outputs, eight=False):
    matrix=struct.pack('>9i',65536,0,0,0,65536,0,0,0,65536)
    samples=inputs*(256 if eight else 2)+2**inputs*outputs+outputs*(256 if eight else 2)
    return typed(b'mft1' if eight else b'mft2',bytes((inputs,outputs,2,0))+matrix+
                 (b'' if eight else struct.pack('>HH',2,2))+bytes(samples*(1 if eight else 2)))


def localized(text='x', locale=b'enUS'):
    encoded=text.encode('utf-16-be')
    return typed(b'mluc',struct.pack('>II',1,12)+locale+struct.pack('>II',len(encoded),28)+encoded)


def make_profile(edition, extra=()):
    legacy=edition[0]==2
    description=typed(b'desc',struct.pack('>I',2)+b'x\0'+bytes(8+70))
    tags=[(b'desc',description if legacy else localized()),(b'cprt',typed(b'text',b'x\0') if legacy else localized()),
          (b'wtpt',typed(b'XYZ ',bytes(12)))]
    tags += [(name,lut(4,3)) for name in (b'A2B0',b'A2B1',b'A2B2')]
    tags += [(name,lut(3,4)) for name in (b'B2A0',b'B2A1',b'B2A2')]
    tags += [(b'gamt',lut(3,1)),*extra]
    header=bytearray(128)
    header[8:12]=edition;header[12:24]=b'prtrCMYKLab '
    header[24:36]=struct.pack('>6H',2024,2,29,23,59,59)
    header[36:40]=b'acsp';header[68:80]=bytes.fromhex('0000f6d6000100000000d32d')
    directory=bytearray(struct.pack('>I',len(tags)))
    bodies=bytearray();start=132+12*len(tags)
    for name,payload in tags:
        directory+=struct.pack('>4sII',name,start+len(bodies),len(payload))
        bodies+=payload+bytes((-len(payload))%4)
    result=header+directory+bodies
    result[:4]=struct.pack('>I',len(result))
    return result


EDITIONS=[bytes.fromhex(v) for v in ('02200000','04000000','04100000','04200000')]


class ExactIccControls(unittest.TestCase):
    def test_parametric_power_has_no_interior_domain_hole(self):
        for edition in EDITIONS[1:]:
            def profile_with(parameters):
                return make_profile(edition,[(b'priv',typed(b'para',struct.pack('>HH5i',3,0,*parameters)))])
            self.assertTrue(ICC.inspect_profile_bytes(profile_with((-65536,-131072,65536,0,0))))
            self.assertEqual(ICC.inspect_profile_bytes(profile_with((-65536,65536,65536,0,0))),[])
            self.assertEqual(ICC.inspect_profile_bytes(profile_with((-65536,65536,-32768,0,49152))),[])
            self.assertEqual(ICC.inspect_profile_bytes(profile_with((65536,-131072,65536,0,0))),[])

    def test_viewing_and_luminance_y_are_consistent_in_v4(self):
        viewing=typed(b'view',struct.pack('>7I',0,65536,0,0,0,0,0))
        luminance=typed(b'XYZ ',struct.pack('>3I',0,65536,0))
        mismatch=typed(b'XYZ ',struct.pack('>3I',0,131072,0))
        for edition in EDITIONS:
            for tags in ([(b'view',viewing)],[(b'lumi',luminance)],[(b'view',viewing),(b'lumi',luminance)]):
                self.assertEqual(ICC.inspect_profile_bytes(make_profile(edition,tags)),[])
            findings=ICC.inspect_profile_bytes(make_profile(edition,[(b'view',viewing),(b'lumi',mismatch)]))
            self.assertEqual(not findings,edition[0]==2)

    def test_original_and_all_edition_grammar_controls(self):
        original=(ROOT/'crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc').read_bytes()
        self.assertEqual(ICC.inspect_profile_bytes(original),[])
        for edition in EDITIONS:
            with self.subTest(edition=edition.hex()):
                data=make_profile(edition)
                self.assertEqual(ICC.inspect_profile_bytes(data),[])
                report=ICC.inspect_profile(data)
                self.assertEqual(report['observations']['version_bytes'],list(edition))
                self.assertEqual(report['observations']['device_class'],'prtr')
                self.assertEqual(report['observations']['declared_size'],len(data))
                self.assertEqual(len(report['observations']['tags']),10)

    def test_header_boundaries_include_u16_year_and_vendor_bits(self):
        for edition in EDITIONS:
            good=make_profile(edition)
            for offset,value in ((27,13),(29,30),(31,24),(33,60),(35,60),(47,4),(63,16),(67,4),(79,0),(127,1)):
                changed=good.copy();changed[offset]=value
                with self.subTest(edition=edition.hex(),offset=offset):self.assertTrue(ICC.inspect_profile_bytes(changed))
            good[24:26]=struct.pack('>H',65535);good[26:30]=struct.pack('>HH',12,31)
            good[44]=255;good[56]=255
            self.assertEqual(ICC.inspect_profile_bytes(good),[])

    def test_dormant_lut8_and_private_data(self):
        for edition in EDITIONS:
            good=lut(4,3,True)
            self.assertEqual(ICC.inspect_profile_bytes(make_profile(edition,[(b'priv',good)])),[])
            self.assertTrue(ICC.inspect_profile_bytes(make_profile(edition,[(b'priv',good[:-1])])))
            self.assertEqual(ICC.inspect_profile_bytes(make_profile(edition,[(b'priv',typed(b'vndr',b'\xff\x80'))])),[])

    def test_locales_unicode_and_substring_aliases(self):
        for edition in EDITIONS[1:]:
            self.assertEqual(ICC.inspect_profile_bytes(make_profile(edition,[(b'priv',localized('Zulu 😀',b'zuZA'))])),[])
        data=typed(b'mluc',struct.pack('>II',2,12)+b'enUS'+struct.pack('>II',6,40)+b'zuZA'+struct.pack('>II',4,42)+'abc'.encode('utf-16-be'))
        self.assertEqual(ICC._localized(data),46)
        bad=bytearray(data);bad[42:44]=b'\xdc\x00'
        with self.assertRaises(ValueError):ICC._localized(bad)

    def test_profile_id_editions_and_zero(self):
        for edition in EDITIONS[1:]:
            good=make_profile(edition);good[44]=1;good[56]=2;good[67]=3
            normalized=good.copy();normalized[64:68]=bytes(4)
            if edition[1]==0:normalized[56:64]=bytes(8)
            else:normalized[44:48]=bytes(4)
            good[84:100]=ICC.hashlib.md5(normalized).digest()
            self.assertEqual(ICC.inspect_profile_bytes(good),[])
            good[84]^=1;self.assertTrue(ICC.inspect_profile_bytes(good))
            good[84:100]=bytes(16);self.assertEqual(ICC.inspect_profile_bytes(good),[])

    def test_required_roles_and_complete_aliases(self):
        good=make_profile(EDITIONS[0]);at=132+3*12
        alias=good.copy();alias[at+12+4:at+12+12]=good[at+4:at+12]
        self.assertEqual(ICC.inspect_profile_bytes(alias),[])
        wrong=good.copy();reverse=132+6*12
        wrong[reverse+4:reverse+12]=good[at+4:at+12]
        self.assertTrue(ICC.inspect_profile_bytes(wrong))

    def test_v4_padding_conditionals_and_optional_roles(self):
        matrix=typed(b'sf32',struct.pack('>9i',65536,0,0,0,65536,0,0,0,65536))
        measurement=typed(b'meas',bytes(24)+struct.pack('>I',2))
        for edition in EDITIONS[1:]:
            good=make_profile(edition);offset,size=struct.unpack_from('>II',good,136)
            good[offset+size]=1;self.assertTrue(ICC.inspect_profile_bytes(good))
            self.assertTrue(ICC.inspect_profile_bytes(make_profile(edition,[(b'meas',measurement)])))
            self.assertEqual(ICC.inspect_profile_bytes(make_profile(edition,[(b'meas',measurement),(b'chad',matrix)])),[])
            self.assertTrue(ICC.inspect_profile_bytes(make_profile(edition,[(b'chad',typed(b'sf32',bytes(36)))])))
            self.assertTrue(ICC.inspect_profile_bytes(make_profile(edition,[(b'tech',typed(b'text',b'x\0'))])))

    def test_retained_type_families_require_their_complete_extent(self):
        for edition in EDITIONS:
            cases=[typed(b'curv',bytes(4)),typed(b'sf32',bytes(4)),typed(b'uf32',bytes(4)),typed(b'ui16',bytes(2)),
                   typed(b'ui32',bytes(4)),typed(b'ui64',bytes(8)),typed(b'sig ',bytes(4)),typed(b'XYZ ',bytes(12)),
                   typed(b'meas',bytes(28)),typed(b'view',bytes(28)),typed(b'ncl2',struct.pack('>III',0,1,0)+bytes(64)+b'x'+bytes(37))]
            response=typed(b'rcs2',struct.pack('>HHI',4,1,16)+b'StaA'+bytes(64));cases.append(response)
            desc=typed(b'desc',struct.pack('>I',2)+b'x\0'+bytes(78)) if edition[0]==2 else localized()
            cases.append(typed(b'pseq',struct.pack('>I',1)+bytes(20)+desc+desc))
            if edition[0]==4:
                cases.append(typed(b'chrm',struct.pack('>HH',4,0)+bytes(32)))
                cases.append(typed(b'clrt',struct.pack('>I',4)+b''.join(bytes((65+i,))+bytes(37) for i in range(4))))
                cases.append(typed(b'para',struct.pack('>HHi',0,0,65536)))
            if edition[0]==2 or edition[1]==0:
                cases += [typed(b'crdi',bytes(20)),typed(b'scrn',bytes(8)),typed(b'bfd ',bytes(9))]
                cases.append(typed(b'devs',struct.pack('>I',1)+b'MSFT'+struct.pack('>IIII',36,1,24,1)+b'mtyp'+struct.pack('>III',4,1,1)))
            if edition[0]==2:cases.append(typed(b'ncol',struct.pack('>II',0,1)+b'\0\0x\0'+bytes(4)))
            for good in cases:
                with self.subTest(edition=edition.hex(),kind=good[:4]):
                    self.assertEqual(ICC.inspect_profile_bytes(make_profile(edition,[(b'priv',good)])),[])
                    self.assertTrue(ICC.inspect_profile_bytes(make_profile(edition,[(b'priv',good[:-1])])))

    def test_edition_strength_and_extensible_name_records(self):
        for edition in EDITIONS[1:]:
            self.assertEqual(ICC.inspect_profile_bytes(make_profile(edition,[(b'priv',localized('x\0'))])),[])
        alternate=typed(b'mluc',struct.pack('>II',1,12)+b'enUS'+struct.pack('>II',3,28)+b'abc')
        for edition in EDITIONS[1:]:
            self.assertEqual(not ICC.inspect_profile_bytes(make_profile(edition,[(b'priv',alternate)])),edition==EDITIONS[-1])
        for edition in EDITIONS:
            good=make_profile(edition);good[40:44]=b'TGNT'
            self.assertEqual(not ICC.inspect_profile_bytes(good),edition!=EDITIONS[-1])
            good[40:44]=bytes(4);good[64]=1
            self.assertEqual(not ICC.inspect_profile_bytes(good),edition!=EDITIONS[-1])
        extended=typed(b'mluc',struct.pack('>II',1,16)+b'zuZA'+struct.pack('>II',2,32)+b'extn'+b'\0x')
        for edition in EDITIONS[1:]:self.assertEqual(ICC.inspect_profile_bytes(make_profile(edition,[(b'priv',extended)])),[])
        self.assertEqual(ICC._localized(typed(b'mluc',bytes(8))),16)


if __name__=='__main__':unittest.main()
