# Controlled CMYK screen previews

The optional Python companion renders an existing PDF into RGB PNG pages using
an explicit CMYK printing profile and RGB display profile. Each PNG contains the
exact display ICC bytes, so a viewer that honors ICC tags can interpret its colors
for the intended display condition. Your viewing software, monitor and screen
settings still affect what you see.

This is a controlled CMYK-to-display preview. Poppler interprets embedded RGB,
Gray and ICCBased content and PDF rendering intents through its RGB PNG route.

The qualified Poppler PNG path composites ordinary transparency groups in RGB,
including groups whose source PDF declares a CMYK blending space. The reference
checks that RGB appearance. CMYK group-colorimetry and press simulation need
separate qualification.

Arbitrary-PDF fidelity, general RGB-to-press-to-screen simulation, universal
reader agreement, monitor calibration and physical printer approval need
separate qualification.

## Prepare the optional tools

Use Python 3.11 or later with `Pillow` and `pypdf`. Pillow needs its LittleCMS
binding for profile validation. Supply a native Poppler `pdftoppm` executable with
working color management and the `-defaultcmykprofile` and `-displayprofile`
options. Profile flags alone do not prove that a renderer build applies them;
qualification must exercise actual color conversion.

These dependencies are separate from the Rust library and resource workers.
Generating PDFs does not require them. The companion does not download tools or
profiles. Obtain a native Windows Poppler distribution or, on Ubuntu, install
`poppler-utils` through your system package manager or supply an independently
obtained native build. Use an ELF executable for native Ubuntu execution.

Poppler is GPL licensed and supplied separately; this source package includes no
renderer executable. Pillow and pypdf also retain their own licenses. The
library's MIT OR Apache-2.0 choice applies to its first-party source.

From the repository root, a separate Python environment can hold the optional
dependencies. In PowerShell:

```powershell
python -m venv .venv-proofing
.venv-proofing/Scripts/python.exe -m pip install Pillow pypdf
```

In an Ubuntu shell:

```sh
python3 -m venv .venv-proofing
.venv-proofing/bin/python -m pip install Pillow pypdf
```

The system may require its Python venv package before creating that environment.
If you already have an environment with these dependencies, use its interpreter.
No commercial checker is required.

## Choose the profiles

The printing profile must be a usable printer-class CMYK ICC profile (`prtr`,
`CMYK`). The display profile must be a usable monitor/display-class RGB ICC
profile (`mntr`, `RGB`). The companion checks their headers and opens a native
LittleCMS transform through Pillow before rendering.

If the PDF embeds an OutputIntent, its decoded CMYK profile bytes must exactly
match the selected printing profile. All catalog and page OutputIntents are
checked, including unselected pages. Conflicting profiles, missing embedded
profiles or non-CMYK OutputIntents fail. A profile with a similar name is not an
exact match. For an ordinary PDF with no OutputIntent, the selected printing
profile explicitly supplies the renderer's DefaultCMYK condition; the manifest
records that choice.

For the included examples, these fixture profiles are available:

- Printing: `crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc`.
- Display: `crates/print-core/tests/fixtures/generated-srgb.icc`.

Their recorded asset terms still apply; see the fixture notices and
[dependency and asset inventory](../third_party/cargo-notices/dependencies.json).
For production work, select profiles for your actual printing and display
conditions. The example printing profile must also match any embedded
OutputIntent in the selected PDF.

## Render pages from the command line

These commands use the ordinary PDF produced by the
[standalone example](../examples/standalone-client/README.md). First generate that
PDF using the platform's build instructions. Replace the renderer placeholder
with your native executable. Run from the repository root; the output's parent
must exist, and the output directory itself must be new.

PowerShell, after the Windows standalone run:

```powershell
.venv-proofing/Scripts/python.exe scripts/proof-pdf.py 'artifacts/new-standalone-run/ordinary.pdf' --print-profile 'crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc' --display-profile 'crates/print-core/tests/fixtures/generated-srgb.icc' --renderer 'C:/path/to/pdftoppm.exe' --output 'artifacts/preview-windows' --pages '1,2' --dpi 144
```

Ubuntu shell, after the native Ubuntu run in the
[development guide](development.md#native-ubuntu):

```sh
.venv-proofing/bin/python scripts/proof-pdf.py 'artifacts/ubuntu-first-run/ordinary.pdf' --print-profile 'crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc' --display-profile 'crates/print-core/tests/fixtures/generated-srgb.icc' --renderer '/path/to/pdftoppm' --output 'artifacts/preview-ubuntu' --pages '1,2' --dpi 144
```

Omit `--pages` to render all pages. Page numbers are unique, one-based integers;
`--pages '2,1'` preserves that requested order. Omit `--dpi` for 144 DPI, or supply
a positive finite value. Subsequent runs need a different output directory.
Use the same command with `candidate.pdf` and its matching printing profile to
preview the standalone print output.

The renderer produces opaque RGB pages on white paper, using MediaBox and text
and vector antialiasing. PNG names retain their source page numbers, such as
`page-1.png`. Open the completed PNGs in software that honors embedded ICC tags.

## Call the reusable Python module

The companion is a source module in `scripts`, without a separately installed
Python package. Add that directory to your caller's import path, or load the
module with `importlib`. For example, run this from the repository root with the
Python environment prepared above:

```python
from pathlib import Path
import sys

sys.path.insert(0, str(Path("scripts").resolve()))
from pdf_proofing import ProofingError, render_preview

try:
    manifest = render_preview(
        "artifacts/new-standalone-run/ordinary.pdf",
        print_profile="crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc",
        display_profile="crates/print-core/tests/fixtures/generated-srgb.icc",
        renderer="C:/path/to/pdftoppm.exe",  # Ubuntu: /path/to/pdftoppm
        output="artifacts/preview-api",  # Must not exist.
        pages=[1, 2],
        dpi=144,
    )
except ProofingError as error:
    raise SystemExit(f"Preview failed: {error}")
```

The signature is `render_preview(pdf, *, print_profile, display_profile,
renderer, output, pages=None, dpi=144)`. It returns the completed manifest as a
Python dictionary and raises `ProofingError` when the preview cannot complete.
Paths may be strings or path objects and are passed literally to the renderer.

## Read the result

`manifest.json` is written last, only after every selected page has been rendered
and verified. Its `schema_version` is `1` and its `status` is `complete`.
It records:

- PDF, printing profile, display profile and companion source hashes.
- Renderer version and executable hash, Python executable identity, dependency
  versions and the actual platform.
- Embedded OutputIntent matching or explicit DefaultCMYK selection, selected
  pages, DPI, opaque background and renderer settings.
- Each PNG's encoded-file and decoded-pixel hashes, pixel dimensions, PDF page
  boxes, rotation, user unit, renderer arguments and diagnostics.

Pillow attaches the exact display profile without a second color conversion and
checks that tagging preserved the RGB pixels. Inputs and the renderer executable
are checked again before completion. The manifest describes the run; a completed
manifest alone is not an independent fidelity or printing acceptance result.
Renderer arguments contain resolved local paths, so review a generated manifest
before sharing it.

Poppler interprets PDF rendering intents. The CLI offers no rendering-intent or
black-point-compensation override. Black point compensation and transform
optimization follow the renderer build. The qualification reference for the
relative-colorimetric samples uses LittleCMS intent `1` and flags `0x2100`
(no optimization plus black point compensation). These measured conditions must
be reviewed when changing renderer builds; they are not caller-selectable CLI
settings. Document color spaces and blending can affect the resulting pixels.

## Handle failures and qualification

Animated PNGs and transparency metadata are rejected before and after profile tagging.

Missing dependencies, invalid profiles, encrypted or unreadable PDFs, invalid
page selections, conflicting print conditions and an existing destination cause
an actionable error. Renderer failure, profile diagnostics, missing or invalid
PNGs, or changed inputs also prevent completion. Each renderer subprocess has a
300-second limit. The CLI exits nonzero on failure; the API raises
`ProofingError`.

A failure after rendering begins can leave PNGs for diagnosis. Those partial
files are incomplete output, and no completed `manifest.json` is published.
Inspect the error, fix the cause and rerun into a fresh directory.

The recorded Windows and Ubuntu preview runs each pass 78 gradient/opacity,
20 direct LittleCMS color and 30 gallery comparisons, with errors bounded
to four 8-bit values per channel. Ten repeated pages per environment preserve decoded pixels
and exact ICC bytes. Another 156 checks compare every diagnostic gradient and flat sample with
independent color and opacity expectations. The largest observed channel error is one. See the
[Windows receipt](verification/current/proofing-20261017-r3/windows.json) and
[Ubuntu receipt](verification/current/proofing-20261017-r3/ubuntu.json).
Earlier [Windows](verification/current/proofing-20261017/windows.json),
[Ubuntu](verification/current/proofing-20261017/ubuntu.json) and
[source-input](verification/current/proofing-20261017/source-package.json)
records retain their original tool identities. The current records add
independent color and opacity expectations and reject failed or stale evidence.
The earlier [R2 Windows](verification/current/proofing-20261017-r2/windows.json),
[R2 Ubuntu](verification/current/proofing-20261017-r2/ubuntu.json) and
[R2 source-input](verification/current/proofing-20261017-r2/source-package.json)
records retain their source identities from before the transparency fix.
Fresh source delivery requires the adjacent exact-archive isolation receipt.
The three original failed reader comparisons and their limits remain recorded in
[color behavior](color-behavior.md). New controlled previews have their own
identities and require their own measured acceptance.

Next: prepare the tools, choose the matching profiles, render into a new directory
and inspect the completed manifest and tagged pages together.
