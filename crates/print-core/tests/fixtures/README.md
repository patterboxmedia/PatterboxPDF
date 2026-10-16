# CMYK profile test fixture

`SWOP2006coated_300_mth.icc` is a real CMYK output profile used only to prove
that the production ICC boundary can build working device-to-PCS and
PCS-to-device transforms.

- Upstream commit: `5a36861414d0b634d6f7a1d34ffa4dd2a895a92e`
- Source file: https://github.com/pmjdebruijn/amethyst-cmyk-icc-profiles/blob/5a36861414d0b634d6f7a1d34ffa4dd2a895a92e/SWOP2006coated_300_mth.icc
- License at that commit: https://github.com/pmjdebruijn/amethyst-cmyk-icc-profiles/blob/5a36861414d0b634d6f7a1d34ffa4dd2a895a92e/LICENSE
- License: CC0-1.0. A redistribution notice and exact legal-code links are retained as `LICENSE-CC0-1.0.txt`.
- SHA-256: `e30370d47804f281071c9786970714b1dbf4e62b4aac03096d3c257304f962b1`

This fixture is evidence that the parser accepts a complete, transformable
profile. It is not a recommendation to use this experimental characterization
for production printing.

## Project-generated TIFF and sRGB fixtures

`generated-srgb.icc`, `embedded-rgb-alpha-3x2.tif` and
`oriented-alpha-3x2.tif` were generated in this workspace on October 4, 2026
(UTC), using the bundled Python runtime with Pillow 12.3.0 and LittleCMS 2.19.
They replace test references to TIFFs in the unimported desktop application.
No sibling checkout files were used. They are development fixtures owned by
this project and licensed under MIT OR Apache-2.0. The upstream CMYK profile retains its separate CC0-1.0 terms.

Both TIFFs contain an uncompressed 3 by 2 RGBA image, with straight alpha
samples 255, 192, 128, 64, 0 and 255 in row order. TIFF orientation tag 274 is
6 (90 degrees clockwise), so the decode worker returns a 2 by 3 image.
The tagged TIFF embeds the exact generated sRGB bytes; the untagged TIFF has
no ICC profile and must still trigger the explicit untagged-source policy.
The ICC creation date is fixed to January 1, 2000 at 00:00:00 for repeatability.
These fixtures exercise source-profile recovery, alpha, orientation, crops,
flips and shadow alignment. They are not output profiles for PDF/X.

Exact SHA-256 values:

- `generated-srgb.icc`: `217fe48ec958c667f8eef725aa27198f465df95d7662593b90d0a1cc30114356`
- `embedded-rgb-alpha-3x2.tif`: `10ded8bf59b5a89142dbcedbc70a5dbda4165d0e2e8a4d85164ef71e203019e3`
- `oriented-alpha-3x2.tif`: `abb14012134d3aee0cf4bfeab6c6710b5073cd2b38c9b3e05584bd9bf6e1e9af`

To recreate them with the versions above, run this Python recipe from the
workspace root. Other LittleCMS/Pillow versions may produce different bytes.

```python
from pathlib import Path
from PIL import Image, ImageCms, TiffImagePlugin

output = Path("crates/print-core/tests/fixtures")
profile = bytearray(ImageCms.ImageCmsProfile(ImageCms.createProfile("sRGB")).tobytes())
profile[24:36] = bytes.fromhex("07d000010001000000000000")
profile = bytes(profile)
(output / "generated-srgb.icc").write_bytes(profile)
image = Image.new("RGBA", (3, 2))
image.putdata([
    (255, 0, 0, 255), (0, 255, 0, 192), (0, 0, 255, 128),
    (255, 255, 0, 64), (255, 0, 255, 0), (0, 255, 255, 255),
])
tags = TiffImagePlugin.ImageFileDirectory_v2()
tags[274] = 6
image.save(output / "embedded-rgb-alpha-3x2.tif", tiffinfo=tags,
           icc_profile=profile, compression="raw")
image.save(output / "oriented-alpha-3x2.tif", tiffinfo=tags, compression="raw")
```
