"""Decode focused native samples independently with pypdf and XML parsers.

Set PDF_LIBRARY_COMPOSITING_ARTIFACTS to a fresh directory when running the
finished_pages regressions, then pass that directory to this test runner.
Requires the development-only pypdf package; no library runtime dependency.
"""
from pathlib import Path
import re
import sys
import unittest
import xml.etree.ElementTree as ET

from pypdf import PdfReader

SAMPLES = Path(sys.argv.pop(1))


class FontMapMetadataTests(unittest.TestCase):
    def test_to_unicode_type_size_and_text_extraction(self):
        for name, expected in [('bmp-512', 'A' * 256), ('surrogate-512', '😀' * 128)]:
            with self.subTest(name=name):
                reader = PdfReader(SAMPLES / f'tounicode-{name}.pdf', strict=True)
                fonts = reader.pages[0]['/Resources']['/Font']
                self.assertEqual(len(fonts), 1)
                font = next(iter(fonts.values())).get_object()
                self.assertEqual(font['/Encoding'], '/Identity-H')
                body = font['/ToUnicode'].get_data().decode('ascii')
                self.assertEqual(re.findall(r'/CMapType\s+(\d+)\s+def', body), ['2'])
                block = re.search(r'1 beginbfchar\s+<([0-9A-F]{4})>\s+<([0-9A-F]+)>\s+endbfchar', body)
                self.assertIsNotNone(block)
                destination = bytes.fromhex(block[2])
                self.assertEqual(len(destination), 512)
                self.assertEqual(destination.decode('utf-16-be'), expected)
                self.assertEqual(reader.pages[0].extract_text(), expected)

    def test_decoded_info_and_xmp_titles_agree(self):
        for name, title in [('ascii', 'a\rb\r\nc'), ('unicode', '試\r\n験\r & <title>')]:
            for mode in ('ordinary', 'print'):
                with self.subTest(name=name, mode=mode):
                    reader = PdfReader(SAMPLES / f'metadata-{name}-{mode}.pdf', strict=True)
                    self.assertEqual(reader.metadata.title, title)
                    xml = ET.fromstring(reader.trailer['/Root']['/Metadata'].get_data())
                    value = xml.find('.//{http://purl.org/dc/elements/1.1/}title/{http://www.w3.org/1999/02/22-rdf-syntax-ns#}Alt/{http://www.w3.org/1999/02/22-rdf-syntax-ns#}li')
                    self.assertIsNotNone(value)
                    self.assertEqual(value.text, reader.metadata.title)


if __name__ == '__main__':
    unittest.main()
