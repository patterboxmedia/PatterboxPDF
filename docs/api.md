# Finished-page API

This documents the application-neutral interface for generating finished pages. Supported operations belong to the initial generation release; future reading, editing and layout features have their own scope.

Add `print-core` as a dependency with its default features. The `finished` module contains application-neutral page inputs; `FinishedPdfWriter` writes their bytes. A caller supplies composed content and resources. Paragraph layout, line breaking, font selection and encoded image ingestion belong to the caller or an explicit adapter.

All page geometry uses PDF points (72 points per inch), with the origin at the bottom left and positive Y upward. `PdfBox` uses minimum and maximum coordinates. Media, bleed and trim boxes must be finite, have positive area, and nest in that order. Item transforms are PDF affine matrices `[a, b, c, d, e, f]`. Image placement maps a unit square into the desired page area through the item transform.

```rust
use print_core::finished::*;
use print_core::{FinishedPdfWriter, PdfCandidateMetadata};

let area = PdfBox::new(0.0, 0.0, 612.0, 792.0);
let document = FinishedDocument {
    pages: vec![FinishedPage {
        boxes: FinishedPageBoxes { media: area, bleed: area, trim: area },
        items: vec![FinishedItem::new(FinishedContent::Path(FinishedPathPaint {
            path: FinishedPath {
                commands: vec![
                    PathCommand::MoveTo(Point::new(72.0, 72.0)),
                    PathCommand::LineTo(Point::new(240.0, 72.0)),
                    PathCommand::LineTo(Point::new(240.0, 180.0)),
                    PathCommand::Close,
                ],
                fill_rule: FillRule::NonZero,
            },
            fill: Some(Paint::cmyk([0.8, 0.1, 0.0, 0.0], 1.0)),
            stroke: None,
        }))],
    }],
    resources: FinishedResources::default(),
};
let options = FinishedPdfOptions {
    metadata: PdfCandidateMetadata::at_utc("Example", "2026-10-04T00:00:00Z").unwrap(),
    mode: FinishedOutputMode::Ordinary,
};
let pdf = FinishedPdfWriter::write(&document, &options)?;
std::fs::write("example.pdf", pdf.bytes())?;
# Ok::<(), Box<dyn std::error::Error>>(())
```

Items preserve the supplied paint order. Paths support moves, lines, cubic curves and closure, either fill rule, and stroke settings. `clip` clips an item in its transformed local space. `Paint::rgb`, `Paint::gray` and `Paint::cmyk` supply components in [0,1] and independent alpha. Gray zero is black and one is white. The documented PDF/X mode accepts CMYK and gray paint and requires the caller's exact embedded CMYK output profile; unmanaged RGB is rejected in that mode.

`FontResource::from_prepared` accepts an opaque `PreparedPublicationFont` from `TrustedFontBoundary` plus CID-to-Unicode mappings. The same boundary types are reexported by `print-core`. A `PositionedGlyphRun` names that resource, supplies its font size, CID and matrix for every positioned glyph, and supplies `actual_text` for the logical text represented by the run. U+001B is rejected in `actual_text` because language escape inputs are unsupported; a U+001B scalar in a ToUnicode mapping remains permitted. Prepared font programs remain exact; the writer embeds them with native PDF font dictionaries and extraction mappings. Prepared geometry and widths refer to the actual retained program and its effective coordinate system. Full-font rights preserve the exact complete program; malformed retained structural data is not repaired by silently rewriting it. The resource contract describes supported font formats and preparation errors.

Font preparation accepts supported TrueType and CFF1 OpenType sources, including a selected collection face for subsetting. Rights-required full embedding uses `prepare_publication` on a supported standalone TrueType face 0 and preserves the complete source bytes. It validates every retained glyph, including unpainted component tails and every font registered with the writer. CFF2, full collection/CFF embedding and arbitrary nondefault variable instances remain outside these routes.

`prepare_publication_with_font_facts` adds optional source/face-bound author facts: the complete accented source-GID set and an otherwise unresolved capital design height in final PDF glyph units. Missing differs from an explicitly empty set or a height of zero. These facts supply design meaning that geometry cannot infer; they do not override a conflicting computable value. Opaque prepared resources expose separate ascent/capital authority flags and identity hashes. Print generation requires both authorities for every registered font. Ordinary output preserves unresolved compatibility behavior and reports that limitation. The strict adapter offers `prepare_publication_fonts_with_font_facts` for the same facts. See the [font example](../examples/standalone-client/README.md) for the typed JSON and selected-face CLI.

TrueType component geometry supports horizontal phantom anchors and preserves recursively selected original metrics. A genuinely needed unknown vertical coordinate returns a specific unsupported-context error; missing unused vertical metadata alone does not. A selected metric component whose transform moves its horizontal axis outside the documented positive horizontal context is explicitly unsupported. Full-source bytes are never repaired into a different font to bypass these rules.

Naming-table versions 0 and 1 preserve their platform metadata. Required Unicode bytes, record extents/order, language indices and lexical language-tag rules are checked. Registry membership, extension meaning and genuinely uninterpreted platform metadata require the declared resource context. Family inspection reads the required Unicode records, including Windows encoding 10, and Apple's default MacRoman mapping. Exact spelling in a historical regional or currency variant needs its actual context or a usable Unicode alias. PostScript identity uses the selected standard Unicode/MacRoman records and rejects malformed or disagreeing retained identities; other permitted records are explicitly ignored for identity. These specific metadata premises do not waive computable font geometry or structure checks.


`ImageResource::from_decoded_rgb`, `from_decoded_gray` and `from_decoded_cmyk` accept width, height, respectively three, one or four interleaved bytes per pixel, and optional one-byte alpha per pixel. Inputs are decoded eight-bit samples. Alpha is straight: supply original color values without multiplying them by alpha. Gray zero is black and 255 is white. The caller chooses the decoder for PNG/JPEG/TIFF or uses an explicit preparation adapter.

The constructors validate dimensions, plane lengths and shared resource limits before retaining compressed planes. Their resources own their memory admission until dropped. An image placement names its resource and supplies an additional scalar alpha; the item's transform and clipping define placement and crop. `ImageAlpha` paints a constant color through the resource's exact alpha plane. Every registered resource is validated and emitted. PDF/X rejects a registered RGB image even when unused or used only for its alpha; use a gray alpha resource in that case.

`FinishedGroup` has local bounds, child items and opacity. The writer emits an isolated transparency group, then applies opacity to its completed appearance once. This preserves overlap between child paints. Paint alpha is independent of group opacity.

`FinishedItem::new` sets `blend_mode` to `BlendMode::Normal`. Change that field to select Multiply, Screen, Overlay, Darken, Lighten, ColorDodge, ColorBurn, HardLight, SoftLight, Difference, Exclusion, Hue, Saturation, Color or Luminosity. A group's placement blend mode applies to its completed appearance; children use their own item blend modes.

Ordinary pages and isolated groups choose RGB when their own recursively painted content contains RGB, and CMYK otherwise. A resource that is registered but not painted does not choose the blending space. A separate RGB page therefore does not alter an existing CMYK page. Mixed RGB/CMYK content converts into the selected group space when composited; this does not promise numerical preservation of CMYK components through RGB composition. Print groups compose in CMYK, while internal opacity masks use Gray. Device-color previews can differ between readers' color-management implementations.

Page and Form resource dictionaries contain only resources directly referenced by their own stream. Registered shared font/image objects are emitted once and reused by reference. Each group lists its direct children; grandchildren belong to their containing Form.

`FinishedContent::Gradient` fills a `FinishedPath` with a `Gradient`. `GradientGeometry::Axial` supplies start/end points for a linear gradient. `GradientGeometry::Radial` supplies two centers and radii; item transforms can turn circular geometry into an ellipse. Stops contain an offset and `Paint`. Each gradient uses one color space throughout, with authored color components and alpha interpolated independently.

For a path already held in `path`, a horizontal three-stop fill looks like this:

```rust
let fill = FinishedContent::Gradient(FinishedGradientFill {
    path,
    gradient: Gradient {
        geometry: GradientGeometry::Axial {
            start: Point::new(72.0, 72.0),
            end: Point::new(240.0, 72.0),
        },
        stops: vec![
            GradientStop { offset: 0.0, paint: Paint::rgb([1.0, 0.2, 0.0], 1.0) },
            GradientStop { offset: 0.45, paint: Paint::rgb([0.8, 0.0, 0.6], 0.5) },
            GradientStop { offset: 1.0, paint: Paint::rgb([0.1, 0.2, 1.0], 1.0) },
        ],
    },
});
```

Provide unique endpoint stops at 0 and 1, with finite nondecreasing interior offsets. Repeated interior offsets create a hard transition: the first color is the left limit and the last is the right value; intermediate colors at that same position are skipped. Distinct close offsets remain distinct. Endpoint colors pad beyond the gradient's ends. The fill path's control-point hull must have positive width and height. Coincident axial points, identical radial circles, two zero radii, mixed stop color spaces and invalid component ranges return `InvalidGradient`. PDF/X also rejects tiny values when a standards reader's zeroing would collapse a required interval or geometry.

The writer uses native PDF functions and shadings. Constant alpha uses a scalar state; changing alpha uses a matching gray luminosity mask bounded by the fill's path. Transforms and clips affect both color and opacity. This component interpolation can differ from premultiplied web gradients; authoring tools should choose their stop colors accordingly. Gradient strokes, repeating/reflecting spread, conical and mesh gradients are future work.

Each retained CID-to-Unicode destination is limited to 512 UTF-16BE bytes in both
output modes, without a byte-order mark: 256 basic characters or 128 supplementary
characters. `FontResource::from_prepared` returns `InvalidFont` for a longer
mapping; the compatibility font preparer returns `UnicodeMappingTooLong`.
Mappings are never truncated. Later occurrences of the same glyph retain their
own `ActualText`, which has a separate content-string limit. ToUnicode resources
use CMapType 2; the font's Identity-H encoding stays separate.

`FinishedPdfWriter::write` returns a structured error before exposing PDF bytes when input geometry, resource references, glyphs, planes, paints, output profiles or limits are invalid. Equal input, options and explicit metadata produce repeatable bytes. `PdfCandidateMetadata::for_export` uses the current time; use `at_utc` for reproducibility. Metadata preserves title carriage returns in both decoded Info and XMP values by escaping them as XML character references. Info and XMP dates share one whole-second value, including consistent handling of accepted leap and fractional seconds; the original parsed timestamp remains part of deterministic input identity.

Ordinary mode omits PDF/X identity and output intent. `FinishedOutputMode::PdfX4` generates the documented CMYK print form with an embedded output profile and PDF/X metadata. It requires a nonblank title and supports ICC 2.2.0 and 4.0.0 through 4.2.0. Native checks inspect the exact profile's binary header, retained tag structures and edition-specific roles before its bytes are embedded unchanged. The caller chooses the destination printing condition.

Profile registrations and original measurement conditions must be accurate. Optional characterization data must name an actual registered data set or contain a valid file in its declared ANSI/ISO interchange format. Retained legacy PostScript data must contain the valid edition-specific CRD, CSA or rendering-intent operands and relationships it declares. ICC framing checks do not validate those nested languages or establish real-world registration/measurement facts. These are specific resource preconditions; the remaining byte-verifiable profile checks stay enforced by native preparation.

The successful generation contract covers the finished-page entry point and the documented strict compatibility routes. Adapter authoring/diagnostic routes that intentionally omit prepared-font enforcement or allow unmanaged RGB remain diagnostic candidates. Their return of bytes does not confer the strict generation guarantee. See [generation checks](requirements/README.md) for the exact entry-point contract. Strict print generation uses `FinishedPdfWriter::write` with `FinishedOutputMode::PdfX4`. The `PublicationCandidateWriter::write_with_rasters_and_fonts`, `write_with_resources` and `write_with_resources_and_metadata` routes participate only with enforced prepared fonts (`Some`, including an empty collection) and the managed print branch. `ScenePdfWriter` public overloads produce ordinary output. Intentional diagnostic branches remain outside the positive print claim.

PDF/X limits each encoded content `ActualText` string to 32,767 bytes, counts automatic Form state saves toward the 28-level graphics-state limit, and bounds indirect objects to 8,388,607. Non-ASCII text uses UTF-16BE plus a byte-order marker, so character count alone does not determine its encoded size. Document titles are outside that content-string limit. The separate aggregate text/resource budget and ordinary nesting policy still apply. Development checker results apply to the exact tested print bytes; a new independent conformance verdict needs analysis of the new output. Checkers are outside generation and the runtime. Supported generation checks and evidence limits are described in `requirements/README.md`.

The initial release generates composed PDFs. Reading, editing, merging, paragraph layout/shaping, forms, signatures, annotations, spot colors and arbitrary operator insertion are outside this interface. Font and output-profile preparation use confined Windows or Ubuntu workers; consult platform support for the recorded execution environments. Vector/image/gradient ordinary drawing uses the direct finished-page path.

`examples/standalone-client` is a separate Cargo workspace that consumes this public interface. The opt-in `publisher-adapter` feature retains imported saved-publication compatibility. New public API choices serve library users independently of any application document model.
