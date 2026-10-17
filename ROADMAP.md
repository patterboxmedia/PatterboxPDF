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

The October 16 candidate adds native Ubuntu font/color/raster workers and
fail-closed process confinement. Prepared fonts use precise widths and
outline-derived descriptors. Independent evaluators check emitted PDF, font,
profile, metadata and stream structures; nonstream syntax lines wrap at255 bytes.

The CMYK investigation is complete: decoded components and controlled samples
pass on the recorded Windows and Ubuntu executables. The three original default
reader-comparison failures remain recorded with their unchanged bounds.
[Color behavior](docs/color-behavior.md) explains the reader conversion/sampling
differences and replay.

Current source passed supported-output and font/profile inspection, 146 native Windows tests/cases and 67 native Ubuntu tests. Fresh extracted-source builds repeated both platform workflows. The public catalog binds 37 checked rules, four positive outputs and 37 actual negative controls.
[Verification status](docs/verification-status.md) records this boundary.

Historical receipts retain the exact source/output identities and verdicts of
their original baselines. Current output requires current analysis.

## Source release

On October 16, 2026 the project selected MIT OR Apache-2.0 for first-party source and original material, and the PatterboxPDF public source identity. Dependency/font/profile terms remain separate. All Cargo packages retain publish = false; registry and binary releases are separate future decisions. Earlier archives and receipts retain the licensing policy in effect when they were produced.

The supported-output catalog and native platform checks cover the implemented generation contracts and exact tested outputs. Unrestricted PDF/X-4 conformance is not established.

## Future capability work

Reading/editing/merging PDFs, paragraph layout/shaping, annotations/forms/signatures, spot-color authoring, additional ICC workflows, gradient strokes/repeating/reflecting/conical/mesh gradients and resource preparation beyond the qualified Windows/Ubuntu environments require their own scoped plans. They are not unfinished units of this initial generation implementation.

Next: build the documented standalone examples. Future capability work needs a separate scope and verification contract.
