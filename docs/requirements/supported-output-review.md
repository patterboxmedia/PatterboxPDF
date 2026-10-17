# Supported PDF/X-4 output

The finished-page API emits PDF 1.6, with ordinary output or a bounded selected-CMYK
PDF/X-4 mode. The output-rule catalog and current receipts identify checked
structures and exact bytes. Unrestricted conformance and acceptance by a
particular printer require additional evidence.

| Area | Generation contract |
| --- | --- |
| Pages | Complete composed pages with finite, nested media, bleed and trim boxes |
| Fonts | Embedded prepared TrueType/CFF programs, positioned glyphs, CID mappings, precise widths, descriptors and Unicode extraction |
| Color | One exact embedded accepted CMYK printer profile; CMYK/Gray print content; RGB also supported in ordinary output |
| Streams | Direct lengths, supported Flate encoding and unfiltered metadata; self-contained resources |
| Transparency | Native axial/radial shadings, Gray opacity masks, sixteen blend modes and isolated group opacity applied once |
| Resources | Stable shared objects and each stream's direct resource dictionary |
| Metadata | Matching Info/XMP title, dates and identifiers; nonblank print title, output-intent description and consistent trapping state |
| Limits | Bounded names, content strings, CIDs, objects and executed graphics-state depth; nonstream lines at most 255 bytes |
| Admission | Invalid geometry, missing resources, unmanaged RGB in print mode and invalid preparation cause failure |

The font worker records metrics from the actual prepared program. TrueType subsets
refresh geometry/metric headers and required program tables. Full-font preparation
preserves admitted original bytes. Independent checks inspect binary structures,
decoded geometry and descriptor metrics. FreeType executes TrueType hint programs
at the recorded finite sizes/configuration; those trials do not establish behavior
at every possible raster size.

ToUnicode destinations contain at most 512 UTF-16BE bytes and use CMapType 2.
Content strings include encoded byte-order marks and surrogate pairs in the
32,767-byte limit. Titles retain their separate aggregate budget. Form invocation
and soft-mask evaluation contribute to the depth limit of 28 state saves.
References remain within 8,388,607 indirect objects. Long Info titles and arrays
wrap without changing decoded strings or stream payloads; xref offsets are updated.

Accepted ICC encodings are 2.2.0, 4.0.0, 4.1.0 and 4.2.0. Header, tag and transform
screening precedes confined native parsing. Version admission alone does not
establish a usable printing condition. Callers choose the profile appropriate
to the destination process.

Current records bind ordinary output, selected-CMYK print with the retained v2 profile, print with a freshly serialized v4.2 development control, and the native gradient diagnostic. Source-font measurements pass independently. The v4 control has separate physical-PCS/CLUT measurements and exact embedded-profile identity on both platforms; it is a development fixture, not a recommendation for a printing condition.

Native gradients interpolate authored color and straight opacity independently.
Readers differ in their screen conversion and sampling. Three retained default
CMYK comparisons still fail their original bounds. The
[controlled investigation](../color-behavior.md) verifies components and explains
those differences while preserving the failed results.

Current inputs provide no PDF reading/editing/merging, paragraph layout/shaping,
annotations/forms/signatures, arbitrary operator/dictionary injection, scripts,
attachments, encryption, spot-color authoring, external-profile PDF/X-4p or
additional gradient families. Those capabilities need separate designs.

[Technical references](public-technical-sources.md) describe public PDF, font,
XML and ICC formats. [Verification status](../verification-status.md) distinguishes
current checks from historical exact-file verdicts. New bytes require new analysis.
