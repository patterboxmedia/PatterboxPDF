These six tiny 3 × 2 RGB rasters are authored test fixtures, generated from six
synthetic pixels with Pillow 12.3.0. No external artwork is included.

Each valid control decodes under the explicit assume-sRGB policy. Its paired
malformed file retains the format header and 3 × 2 dimensions: PNG has a
shortened IDAT stream with updated chunk length/CRC, JPEG has a damaged Huffman
table marker, and TIFF retains its declared strip length but is cut short. The
Windows boundary test verifies that inspection succeeds, confined pixel decode
rejects, and no output files are produced.
