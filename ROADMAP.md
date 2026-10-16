# PatterboxPDF roadmap

Independent library established October 3, 2026. This file records completed scope and future work.

## Completed initial generation scope

The initial reusable finished-page library is implemented and verified for unrelated callers. Publisher is an incidental future consumer. Application UI, document integration and physical printer acceptance are downstream work.

- Ordinary RGB, Gray and CMYK paints and decoded eight-bit images, including straight alpha and resource lifetime accounting.
- Native axial/radial gradient fills, multiple stops, hard interior transitions, endpoint padding, constant/varying opacity, transforms and clips.
- All sixteen standard blend modes, isolated group opacity applied once, per-page/group painted-color discovery and exact direct-stream resource dictionaries.
- Exact worker-prepared TrueType/CFF programs, positioned glyphs, Unicode extraction mappings and ordered ActualText.
- Supported CMYK PDF/X output with exact screened output profiles, corrected custom-condition Info, nonblank title, serialized content-string limits, checked object allocation and executed graphics-state depth.
- Worker-free drawing and prepared-resource print examples, accurate API/platform documentation, current requirements bindings and curated source packaging.

Finalization covered standards corrections, ordinary color inputs, gradients, compositing, neutral examples and current evidence/source delivery. Current results are in [verification status](docs/verification-status.md). Require the source archive's adjacent passed isolation receipt for the final source handoff.

## Current verification

October 6 maintenance: bound every retained ToUnicode destination to 512
UTF-16BE bytes, emit Type 2 ToUnicode resources directly in both writer paths,
and preserve CR/CRLF titles in decoded Info/XMP metadata. Focused native and
independent decoding checks accompany a new source handoff. Archive 006 remains
the earlier immutable checkpoint. New font-bearing output has different bytes;
the October 4 checker results below are retained baseline evidence.

The affected native replay passed 150 Rust tests. Six actual Windows confinement checks and three real preparation-worker failure cases passed. The separate client retains a 57-package default graph without application authoring crates. The regenerated corpus met 34/34 combined expectations, with 14 positive callas passes; the separate CMYK guard rejects four malformed profile declarations that callas accepts. All 190 source pages passed geometry, text and fixed two-reader comparisons.

The new ordinary/print gallery passed native structure/resource checks and fifty meaningful appearance probes in both readers. Its exact CMYK output passed callas. Whole-page comparison exceeded its unchanged bounds on ordinary page 2 and print pages 1–2, concentrated in smooth CMYK gradient previews. Preserve those failures and their qualification; do not describe the full gallery comparison as passed.

The inherited Unicode test corpus is now supplied inside its crate. All eleven
existing native assertions passed, and every workspace target compiles. Actual
printer-profile worker/export evidence uses ICC 2.2.0; admitted v4 encodings have
screening coverage without a complete real v4 end-to-end run.

The authored gallery appearance runner is included in public source. Its exact
fifty checks are reproducible from retained renders; the original evidence is
preserved and the refreshed receipt retains failed whole-page comparison flags.

The second pass corrected the recipient setup instructions and made the gallery
and public API verifiers refuse optimized Python, which disables their checks.
The source handoff includes a regression check for that refusal and requires a
fresh extracted-package build after these repairs.

Earlier corpus receipts, archives and the original 1,350 imported files remain preserved checkpoints. New evidence is under docs/verification/current/finalization-20261004; old receipts retain their historical identities.

## Source release

On October 16, 2026 the project selected MIT OR Apache-2.0 for first-party source and original material, and the PatterboxPDF public source identity. Dependency/font/profile terms remain separate. All Cargo packages retain publish = false; registry and binary releases are separate future decisions. Earlier archives and receipts retain the licensing policy in effect when they were produced.

The `normative_pdfx4_coverage_complete` flag remains false. Current verification covers the supported generation contracts and exact tested outputs; unrestricted PDF/X-4 conformance is not established.

## Future capability work

Reading/editing/merging PDFs, paragraph layout/shaping, annotations/forms/signatures, spot-color authoring, additional ICC workflows, gradient strokes/repeating/reflecting/conical/mesh gradients and equivalent resource preparation on other platforms require their own scoped plans. They are not unfinished units of this initial generation implementation.

Next: build the documented standalone examples. Future capability work needs a separate scope and verification contract.
