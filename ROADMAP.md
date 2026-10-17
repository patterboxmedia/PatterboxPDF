# PatterboxPDF roadmap

The independent library was established October 3, 2026. This file records implemented scope, current qualification and future capabilities.

## Implemented generation scope

The reusable finished-page interface serves unrelated applications. Application UI, document integration and physical printer acceptance belong to downstream applications.

- Ordinary RGB, Gray and CMYK paints and decoded eight-bit images, including straight alpha and shared resource lifetime accounting.
- Axial and radial gradient fills, multiple stops, hard interior transitions, endpoint padding, constant or varying opacity, transforms and clips.
- All sixteen blend modes, isolated group opacity applied once, local blending spaces and direct resource dictionaries.
- Prepared TrueType and CFF programs, positioned glyphs, Unicode extraction mappings and logical ActualText.
- CMYK PDF/X-4 generation with exact admitted output profiles, consistent metadata, encoded string limits and checked object/state limits.
- Confined resource workers, standalone drawing and print examples, public documentation and curated source packaging.

## Completed accepted-input generation

The current milestone covers the documented successful generation routes and all supported input families. Native resource checks and writer construction enforce the contract; independent evaluators measure actual output. Intentional diagnostic candidates have their own stated scope.

Font preparation now preserves composite geometry and required metrics through subsetting, including retained component tails. Full-font embedding preserves source bytes. Font descriptors account for complete outlines, effective CFF transformations, flex curves, fractional coordinates and independently established design authority. Valid name-table formats are supported, and malformed retained names are rejected. Independent metric checks select the documented Unicode/MacRoman PostScript identity and ignore other permitted identity records without guessing their meaning.

ICC preparation checks supported editions, headers, tag/type roles and retained payloads before native usability checks. Numeric encoding preserves the required relationships in the actual emitted decimals. Metadata, Unicode, image planes, gradient functions and resource graphs have independent output controls.

Current source passes 368 native Windows tests and 319 native Ubuntu tests, with no ignored cases. Both platforms pass the 131-case font corpus, the 17-case profile corpus and the four-document composition corpus. The output catalog exercises all 37 targeted negative rules against four positive PDFs. [Verification status](docs/verification-status.md) explains the exact scope and qualifications.

Independent source and exact-output review accepted the documented domain and its explicit authoring premises. Public family/catalog audits pass. The source handoff separately verifies its archive inventory, current compiled inputs and fresh build/execution on both targets.

The three original CMYK reader comparisons remain failed at their unchanged bounds. The completed controlled investigation verifies component/color behavior and explains the retained reader differences. See [color behavior](docs/color-behavior.md).

Historical receipts retain their original source and exact output identities. Changes that affect generation or inspection require fresh relevant checks and current bindings.

## Controlled CMYK screen previews

The optional Python companion is implemented with a reusable module and CLI.
It renders the actual PDF through an explicitly supplied CMS-enabled Poppler,
requires matching embedded CMYK OutputIntent bytes when present, and writes RGB
PNGs containing the selected display ICC profile. Ordinary PDFs without an
OutputIntent record the caller's explicit DefaultCMYK condition.

Native Windows and Ubuntu preview qualification passes with explicit profiles,
independent color measurements and repeatability controls. Source delivery also
requires the adjacent exact-archive isolation receipt. See the
[proofing guide](docs/proofing.md) for use, completion manifests and limitations.
The three historical failed reader comparisons retain their original verdicts
and bounds. Physical printer acceptance remains a downstream check.

## Source release

First-party source and original material use MIT OR Apache-2.0. Dependency, font and profile terms remain separate. All Cargo packages retain `publish = false`. Registry and binary releases require separate work.

The generation contract records the supported caller/resource domain and its specific authoring premises. Current independent output, platform and source-package evidence bind that domain and the exact tested files. Unrestricted PDF/X-4 conformance is a broader claim.

## Future capability work

Reading, editing and merging existing PDFs; paragraph layout and shaping; annotations, forms and signatures; spot colors; additional ICC workflows; gradient strokes and repeating, reflecting, conical or mesh gradients; and resource preparation on additional platforms require separate scope and qualification.

Next: use the standalone examples for the recorded environment. Choose a future capability through its own scoped plan.
