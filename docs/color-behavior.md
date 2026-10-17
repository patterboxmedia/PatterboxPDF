# CMYK screen previews

PatterboxPDF writes native CMYK gradients, preserving their components and straight
opacity. A PDF reader converts those components to RGB for a screen. Readers can
use different conversion tables, profile settings and sampling precision, producing
different previews of the same correct component values.

The retained feature gallery exceeds its original default PDFium/Poppler comparison
bounds on ordinary page 2 and print pages 1-2. Those are still failed default
comparisons. The separate color investigation establishes their cause and verifies
the component output; it does not change the failed results or their limits.

| Retained output | Page | Mean channel difference | 95th percentile maximum channel difference | Pixels above 10 |
| --- | ---: | ---: | ---: | ---: |
| Ordinary | 2 | 1.244 | 23 | 8.823% |
| Print | 1 | 1.615 | 24 | 12.557% |
| Print | 2 | 1.244 | 23 | 8.823% |

The original limits remain mean <=5, percentile <=20 and pixels above 10 <=8%.
The fifty authored appearance probes pass separately. Reader agreement is not a
substitute for verifying CMYK components, profiles or intended printing conditions.

Actual Windows and Ubuntu executables generated the same diagnostic PDF bytes
with fixed metadata. The controlled replay passed 288 decoded component/opacity
checks, 156 gradient-versus-flat samples, ten matched ICC samples and sixty
retained-gallery samples. The three failed default comparisons remain recorded.

## Reproduce the investigation

Build the standalone client's `cmyk-diagnostic` example using the normal native
build instructions. It needs no resource workers. Its `--output` argument names a
new PDF file in an existing directory; existing files are preserved.

```text
cargo build --manifest-path examples/standalone-client/Cargo.toml --locked --release --target-dir target/client --example cmyk-diagnostic
target/client/release/examples/cmyk-diagnostic --output output/diagnostic.pdf
```

On Windows, the executable has an `.exe` extension. The two pages have identical
CMYK panels. A one-point RGB marker outside the panels selects RGB page blending
in the second page dictionary; the first page declares CMYK page blending.

Each row places a gradient on the left and five independently authored flat
component controls on the right. From top to bottom, the rows exercise opaque
axial, opaque radial, hard transition, constant alpha, variable alpha, radial
variable alpha, group opacity, and variable alpha combined with group opacity.

The development verifier needs Python with `pypdf`, `pypdfium2`, `Pillow` and
`numpy`, plus qpdf, Poppler's `pdftoppm`, and a LittleCMS 2 shared library. These
tools are supplied explicitly and are not library runtime dependencies. Use the
retained ordinary and print feature-gallery PDFs, or regenerate their exact bytes
with the current `feature-gallery` example and its fixed metadata. Current native
regeneration reproduces both frozen baseline identities. Build and stage the color
worker for the print gallery:

```text
cargo build --locked --release -p publisher-color-worker
cargo build --manifest-path examples/standalone-client/Cargo.toml --locked --release --target-dir target/client --example feature-gallery
python scripts/stage-workers.py --runtime target/release --caller target/client/release/examples/feature-gallery --worker color
target/client/release/examples/feature-gallery --mode ordinary --output output/baseline-ordinary
target/client/release/examples/feature-gallery --mode pdfx4 --profile crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc --output output/baseline-print
```

On Windows, append `.exe` to the example executable in staging and execution.
When using an explicit Rust target, include that target subdirectory in both
runtime and caller paths. Both output directories must be new. The expected
SHA-256 identities are `ac8a687a3f91cde2cf768d0099ff37e1a8ce722448a6b4062c51ef2aada10fb8`
for ordinary output and `5b6a94bf69381718c1eff572305a13ce71b78f608657ab34246d96750206ecab`
for print output. The verifier requires these identities and preserves their failed
default-comparison verdicts.

```text
python scripts/verify-cmyk-gradients.py --diagnostic output/diagnostic.pdf --profile crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc --lcms PATH_TO_LCMS_SHARED_LIBRARY --poppler PATH_TO_PDFTOPPM --qpdf PATH_TO_QPDF --baseline-ordinary output/baseline-ordinary/ordinary.pdf --baseline-print output/baseline-print/candidate.pdf --output output/color-proof
```

The proof directory must be new. The verifier freezes input identities and records
the reader/CMM versions, library and executable hashes, exact profile, generated
reference PDFs, renders, and separate default and controlled verdicts. It rejects
wrong component functions and a profile labeled with the wrong channel space.
Its tests also reject relaxed bounds and default verdicts that disagree with the
measured values.

## What the controls establish

The decoded output is checked against independently calculated component and alpha
functions, including both sides of a hard transition. Constant and variable alpha
are checked separately from the color components, and group opacity applies once.
In each reader, the native diagnostic gradients match flat controls within four
8-bit channel values. The original gallery's opaque axial/radial samples match
independently authored flat colors at that reader's sampled parameters.

The investigated builds are PDFium 156.0.8076.0, exposed by pypdfium2 5.14.0, and
Poppler 26.07.0. PDFium evaluates a 256-entry shading table, selecting
`floor(t * 255)` and evaluating that bucket at `bucket / 256`. This accounts for
small gradient-versus-continuous-swatch differences. Its default DeviceCMYK
conversion differs from Poppler's. The
[matching PDFium shading source](https://pdfium.googlesource.com/pdfium/+/8ca5b735df4263f43c830479b78213854a392c22/core/fpdfapi/render/cpdf_rendershading.cpp)
and [matching DeviceCMYK source](https://pdfium.googlesource.com/pdfium/+/8ca5b735df4263f43c830479b78213854a392c22/core/fpdfapi/page/cpdf_devicecs.cpp)
describe those paths. Poppler's matching source is available in its
[26.07.0 source archive](https://poppler.freedesktop.org/poppler-26.07.0.tar.xz).

An explicit ICC profile alone does not make all other reader settings equal. The
reference experiment therefore matches each reader's actual conversion conditions:

| Reader | CMYK input quantization | Conversion intent | LittleCMS flags |
| --- | --- | --- | --- |
| PDFium | Truncate to 8 bits | Perceptual | None |
| Poppler | Round to 8 bits | Relative colorimetric | No optimization; black point compensation |

The PDF requests relative colorimetric rendering. PDFium's investigated ICC path
uses a perceptual transform regardless of that request; its
[matching ICC source](https://pdfium.googlesource.com/pdfium/+/8ca5b735df4263f43c830479b78213854a392c22/core/fxcodec/icc/icc_transform.cpp)
documents the selected settings. The verifier also records a separate full-precision
relative-colorimetric LittleCMS reference. It does not label unlike settings as a
matched comparison. Changing reader builds requires reviewing these assumptions
and repeating the checks.

This investigation required no change to the native CMYK gradient encoding.
Applications should distinguish an unmanaged screen preview from an explicitly
profile-managed proof, and verify the destination printing condition they intend
to use. These fixture checks establish neither universal reader agreement nor
physical printer acceptance.

## Optional controlled preview workflow

The [proofing companion](proofing.md) offers a reusable Python API and CLI for
rendering an existing PDF with explicit printing and display profiles. It requires
a native CMS-enabled Poppler executable, a printer-class CMYK profile and a
display-class RGB profile. An embedded CMYK OutputIntent must match the exact
selected printing-profile bytes. Without an OutputIntent, the ordinary PDF uses
the caller's explicit DefaultCMYK condition.

Poppler interprets PDF rendering intents and embedded color spaces.
The qualified Poppler PNG path composites ordinary transparency groups in RGB,
including groups whose source PDF declares a CMYK blending space. The reference
checks that RGB appearance. CMYK group-colorimetry and press simulation need
separate qualification.
The CLI does not override rendering intent or black point compensation. The
qualification reference for relative-colorimetric samples uses LittleCMS intent
`1` and flags `0x2100` (no optimization and black point compensation); renderer
build changes require fresh checks of those conditions. The companion tags the
rendered RGB pixels with the exact display ICC bytes without reconverting them.
A viewer must honor that tag, and the display condition still affects appearance.

Native Windows and Ubuntu previews pass the recorded color, opacity, profile-sensitivity and repeatability checks. Exact source-archive qualification is recorded separately in its adjacent isolation receipt. New previews have
their own PDF, profile, renderer and output identities in a completion manifest.
They do not replace the three original failed comparisons, their unchanged
thresholds or the historical PDF identities above. Controlled CMYK-to-display
preview does not establish general RGB press simulation, universal reader
agreement or physical printer approval.
