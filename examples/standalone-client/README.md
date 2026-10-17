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

## Export a selected font glyph

The `accepted-font-input` example prepares an exact source face through the font
worker and writes a one-glyph finished page. Stage matching font and color
workers beside the example built above:

```powershell
python scripts/stage-workers.py --runtime target/release --caller target/client/release/examples/accepted-font-input.exe --worker font --worker color
target/client/release/examples/accepted-font-input.exe apps/font-worker/src/tests/fixtures/default.otf 1 H artifacts/new-font-input used --profile crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc
```

Arguments are `SOURCE GID TEXT NEW_OUTPUT used|unused`, followed by optional
`--profile PROFILE`, `--face-index N` and `--font-facts FILE.json`. The existing
`--ascent-facts` flag is an alias. The face
index defaults to zero; select the intended face when the source is a supported
font collection. `GID` is the source glyph ID. `TEXT` supplies the caller's Unicode
mapping and extraction text. `unused` registers the font without painting it,
which still exercises print resource admission.

The new output directory receives `prepared.font`, `prepared.json` and
`ordinary.pdf`. Supplying a print profile also requests `print.pdf`. The JSON
records the exact source, selected face, prepared bytes, worker and preparation
policy, plus the source-to-CID mapping. The native Ubuntu example accepts the
same arguments without the `.exe` suffix; stage ELF workers built for that target.

PDF's Ascent metric excludes accented-character glyphs. Some source fonts do not
provide enough information to determine those roles automatically. The prepared
resource exposes `requires_ascent_facts()`; PDF/X-4 generation reports
`FontAscentFactsRequired` when those facts are missing, including for unused
registered fonts. This example writes ordinary output first, so that error can
leave `ordinary.pdf` and the preparation files while producing no `print.pdf`.
The ordinary compatibility metric in that case has no correct-Ascent guarantee.

Capital height describes the font's intended flat capital line. The worker uses
a usable source declaration or an established flat-capital reference, including
L. A known empty capital can have height zero. When the source cannot establish
this design value, `requires_cap_height_fact()` remains true and print generation
requests a fact before returning bytes. This also applies to unused registered
fonts. Ordinary output can retain the unresolved compatibility behavior; its
capital-height descriptor has no correctness guarantee in that case.

Callers who know the authored roles can use
`TrustedFontBoundary::prepare_publication_with_ascent_facts`. Its accented source
GIDs must be strictly increasing, unique and nonzero. The set must account for
all retained glyphs, including composite components and the whole font when full
embedding is required. An explicitly empty set asserts that none are accented.
The library checks source/face identity, ranges and transport; the caller is
responsible for the semantic truth of the supplied roles. It continues to check
the font program, geometry, widths, embedding rights and process confinement.

`TrustedFontBoundary::prepare_publication_with_font_facts` accepts the optional
accent set and an optional `cap_height_pdf_units` independently. Capital height
is a font-wide author design fact in the final PDF glyph coordinates, measured
from the transformed baseline. It is not a raw source-font unit count or the
top of any chosen round letter. A value of zero is an explicit design assertion;
omitting it requests native inference. A conflicting value cannot replace an
already computable source metric. The caller supplies the truth of the design
fact, while the library checks its finite range and identity binding.

For the included `ascent-unencoded-high.otf` control, GID2 is the authored
H-with-circumflex glyph. Create the strict JSON for its exact bytes
and supply it to the example:

```powershell
python -c "import hashlib,json,pathlib; p=pathlib.Path('apps/font-worker/src/tests/fixtures/ascent-unencoded-high.otf'); pathlib.Path('artifacts/font-ascent-facts.json').write_text(json.dumps({'source_sha256':hashlib.sha256(p.read_bytes()).hexdigest(),'face_index':0,'accented_source_glyph_ids':[2]}),encoding='utf-8')"
target/client/release/examples/accepted-font-input.exe apps/font-worker/src/tests/fixtures/ascent-unencoded-high.otf 2 Ĥ artifacts/new-font-with-facts used --profile crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc --ascent-facts artifacts/font-ascent-facts.json
```

Use an existing parent directory and a fresh output directory. Facts apply to the
exact source bytes and face; changing either requires a new assertion. The
opaque `ascent_facts_sha256()` binds that assertion to the prepared resource.
The JSON also accepts `cap_height_pdf_units`; supply either fact field or both,
alongside `source_sha256` and `face_index`. An empty accented-GID array is an
actual assertion, while an omitted field supplies no assertion. Unknown,
duplicate or wrongly typed fields are refused. `font_facts_sha256()` binds both
optional facts and their presence to the exact source and face.
Use independently authored facts when inspecting the resulting PDF; a producer's
metric values or success report do not independently establish correctness.

## Draw the feature gallery

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
preparation uses the confined color worker for the recorded Windows or Ubuntu target. Current native Ubuntu execution and fresh extracted-source builds are recorded in the platform receipts. Ordinary graphics
generation itself needs no worker. See [platform support](../../docs/supported-platforms.md).

Fixture bytes are included through relative paths, so no private machine path is
required. DejaVu Sans is covered by
`crates/layout-core/tests/fixtures/LICENSE-DejaVu`; Noto CJK by its adjacent
`LICENSE-NotoSansCJK` (OFL 1.1). The ICC profile is covered by
`crates/print-core/tests/fixtures/LICENSE-CC0-1.0.txt`. Original attribution and
hashes remain in those fixture README files. First-party source and original
project controls use MIT OR Apache-2.0. Renamed Noto-derived font controls retain
the OFL terms in `apps/font-worker/src/tests/fixtures/LICENSE-OFL`. This example
retains `publish = false` because registry publication is separate.

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


## Native Ubuntu client

Build and stage the native ELF workers/client using
[development instructions](../../docs/development.md#native-ubuntu).
The client accepts the same arguments and fixed fixtures as Windows. The
executables have no .exe suffix. The worker-free gallery is:

```sh
target/client/x86_64-unknown-linux-gnu/release/examples/feature-gallery --output artifacts/ubuntu-gallery --mode ordinary
```

For print output, stage the native color worker beside that exact gallery
executable and supply the profile as in the Windows example. Use a fresh output
directory each time. Native WSL2 execution qualifies the actual recorded Linux
environment; cross-compiling a Windows executable alone does not establish Linux
support. See the current candidate status before relying on platform qualification.

The cmyk-diagnostic example generates a worker-free controlled color PDF. Its
replay, independent controls and reader-setting assumptions are in
[color behavior](../../docs/color-behavior.md).
