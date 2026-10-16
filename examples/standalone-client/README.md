# Standalone finished-page client

This is a separate Cargo workspace with `print-core` as its only project
dependency. The original client demonstrates prepared fonts and finished pages;
the `feature-gallery` example demonstrates drawing, decoded images, native
gradients and compositing. `ttf-parser` positions the original client's fixed text
line. The dependency graph contains no document, layout or application crate.

Build it from the project root with Rust 1.98.0 and native Windows MSVC build
support. The first build downloads Cargo dependencies:

```powershell
cargo build --locked --release -p publisher-font-worker -p publisher-color-worker
cargo build --manifest-path examples/standalone-client/Cargo.toml --locked --release --target-dir target/client --examples --bins
python scripts/stage-workers.py --runtime target/release --caller target/client/release/pdf-library-standalone-client.exe
New-Item -ItemType Directory -Path artifacts -Force | Out-Null
target/client/release/pdf-library-standalone-client.exe --output artifacts/new-standalone-run
```

The output directory must be new and its parent must exist. The caller creates
`ordinary.pdf` and `candidate.pdf` using the same explicit metadata. Both contain
native TrueType/CFF text, ordered cubic paths, CMYK/alpha images, clipping,
transforms, isolated group opacity and two different sets of page boxes.
`--profile` can supply another supported CMYK printer profile. Candidate output
still requires independent checking of the exact bytes.

Generate the gallery as an ordinary PDF without any worker sidecars:

```powershell
target/client/release/examples/feature-gallery.exe --output artifacts/new-gallery-ordinary --mode ordinary
```

For print output, stage the color worker built from this workspace and select an
exact supported CMYK profile:

```powershell
python scripts/stage-workers.py --runtime target/release --caller target/client/release/examples/feature-gallery.exe --worker color
target/client/release/examples/feature-gallery.exe --output artifacts/new-gallery-print --mode pdfx4 --profile crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc
```

`--output` must name a fresh directory with an existing parent. The gallery
creates only `ordinary.pdf` or `candidate.pdf`, after resource admission, profile
checking and serialization finish. Both examples set an explicit fixed title and
UTC timestamp, so repeated generation with the same inputs is reproducible.

The gallery uses three 610 by 650 point pages. Read each row left to right:

| Page | Visible content from top to bottom |
| --- | --- |
| 1 | RGB (print: CMYK), Gray and CMYK decoded images with straight alpha over gray; multistop axial and radial gradients; a hard axial transition and a radial alpha fade; isolated overlapping paints at half opacity and an axial alpha fade. |
| 2 | Gray axial and radial gradients; CMYK axial and radial gradients; Gray and CMYK hard transitions; transformed triangular clips around gradients with varying alpha. |
| 3 | A four by four grid of blend modes, each applied once to an isolated overlapping group with 0.75 opacity over colored backdrop shapes. |

The blend grid's rows are Normal, Multiply, Screen, Overlay; Darken, Lighten,
ColorDodge, ColorBurn; HardLight, SoftLight, Difference, Exclusion; Hue,
Saturation, Color, Luminosity. The gallery intentionally uses no text fonts. The
original client above covers native TrueType/CFF font preparation and positioned
glyphs.

Image constructors receive caller-decoded, top-to-bottom 8-bit color planes and
an optional independent straight-alpha plane. They do not decode PNG/JPEG file
bytes. Place each image's unit square with its item transform. Text shaping and
positioning also belong to the caller; the original client's simple advance-based
line does not implement paragraph layout or full script shaping.

Gradients interpolate authored color components and opacity independently.
Their unique stops at 0 and 1 pad beyond the geometry's endpoints; equal interior
offsets create hard transitions. These semantics can differ from premultiplied
web gradients when color fades to transparency. Nonuniform transforms stretch
radial circles into ellipses, and the fill path and item clip constrain both color
and alpha. Group opacity applies once after its children combine; the item's
blend mode also applies to the completed group.

Ordinary pages/groups choose RGB when their own painted subtree contains RGB;
otherwise they choose CMYK. Mixed colors convert into that local blending space.
Print output accepts DeviceCMYK and DeviceGray and rejects unmanaged RGB,
including registered RGB image resources. Page 2 remains CMYK in both modes.
The retained PDFium/Poppler gallery comparison exceeded its declared bounds on
three pages containing smooth CMYK gradients. Differences concentrate in the
interpolated CMYK colors; the opaque hard-stop colors agree closely. Both readers
passed focused RGB/Gray, hard-edge, alpha, clip and group/blend appearance probes.
Exact structural checks and those probes establish the tested behavior; review
CMYK previews in the intended reader. Physical printer acceptance is a separate
downstream check. Print profile
preparation currently uses the confined Windows color worker. Ordinary graphics
generation itself needs no worker. See [platform support](../../docs/supported-platforms.md).

Fixture bytes are included through relative paths, so no private machine path is
required. DejaVu Sans is covered by
`crates/layout-core/tests/fixtures/LICENSE-DejaVu`; Noto CJK by its adjacent
`LICENSE-NotoSansCJK` (OFL 1.1). The ICC profile is covered by
`crates/print-core/tests/fixtures/LICENSE-CC0-1.0.txt`. Original attribution and
hashes remain in those fixture README files. First-party source and generated fixtures use MIT OR Apache-2.0; this
example retains `publish = false` because registry publication is separate.

Save target-filtered Cargo metadata from this manifest, then run the verifier:

```powershell
cmd /d /c "cargo metadata --manifest-path examples/standalone-client/Cargo.toml --filter-platform x86_64-pc-windows-msvc --locked --format-version 1 > artifacts\standalone-metadata.json"
python scripts/verify-public-api.py --client target/client/release/pdf-library-standalone-client.exe --cargo-metadata artifacts/standalone-metadata.json --output artifacts/new-public-api-proof
```

The verifier checks repeatability, text, geometry, exact decoded image planes and
bounded candidate structure. `--adapter-cli` adds three representative retained
saved publications. It records the exact PDF identities for independent checking;
generation alone supplies no conformance verdict.

The `cmd` redirection preserves Cargo's UTF-8 JSON in Windows PowerShell 5.1 too.

For development with an already configured WSL Ubuntu/xwin toolchain, the
workspace wrapper can run the two build commands with
`cmd /c scripts\cargo-xwin-wsl.cmd build`, adding
`--target x86_64-pc-windows-msvc`. Use `target/x86_64-pc-windows-msvc/release`
for workers and `target/client/x86_64-pc-windows-msvc/release` for callers in the
commands above. Add `--offline` only after dependencies are cached. See
[local development](../../docs/development.md) for that setup and focused tests.

`scripts/verify-feature-gallery.py` independently inspects native images,
shadings, opacity masks, all sixteen serialized blend names and exact resource
ownership for each stream. It also records PDFium/Poppler appearance comparisons
under declared bounds. Supply development-tool paths through `--qpdf` and
`--poppler`, both generated PDFs through `--ordinary` and `--print`, the exact
profile through `--profile`, and a fresh receipt directory through `--output`.
Retain a failed comparison and its renders for investigation; reader agreement is
an observed result for those bytes, separate from PDF/X checking. Commercial
checkers are development tools outside the caller's runtime and source package.

Run the fifty authored appearance checks against the verifier's retained renders:

```powershell
python scripts/verify-feature-gallery-appearance.py --renders artifacts/new-gallery-proof --output artifacts/new-gallery-proof/appearance-probes.json
```

Use the receipt directory passed to the gallery verifier's `--output`. The
appearance runner checks render hashes, gallery dimensions, expected colors,
clipping and selected group/blend behavior. Its receipt records the runner hash
and each probe's page and sampling coordinates. It preserves the whole-page
comparison verdict. Run it on retained renders even when that comparison exits
with a failure; passing authored probes does not replace the failed comparison.
This script checks the specific authored gallery and supplies no general PDF/X
conformance verdict. Its output must be a fresh file. Run all three Python
verifiers without `-O` or `PYTHONOPTIMIZE`; they reject optimized execution so
their validation assertions cannot be skipped.
