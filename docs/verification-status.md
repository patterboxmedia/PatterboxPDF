# Verification status

## October 6 font and metadata maintenance

The focused update rejects ToUnicode destinations above 512 UTF-16BE bytes in
neutral and compatibility construction, with a defensive check at shared
emission. Both ordinary/print font emitters and the legacy candidate emitter
write CMapType 2 directly; Identity-H encoding remains unchanged. XML carriage
return references preserve decoded Info/XMP title equality. The new native
regressions reproduce the previous defects and check the repaired boundary,
including supplementary characters and the 100-entry CMap block boundary.

The focused replay passed 103 native Rust tests across the core, finished-page
and compatibility-font suites. Two independent pypdf/XML regressions decode six
new sample PDFs, check full boundary text extraction and title equality in both
output modes. Exact results and source bindings are in
`verification/current/font-map-metadata-20261006/repair.json`.

An exploratory compatibility fixture with 256 combining marks hit a debug
overflow in the existing text-layout dependency before font preparation. It was
replaced by direct compatibility-map boundary checks; this update makes no claim
to repair or establish support for that shaping stress case.

New font-bearing bytes require their own checker analysis. The October 4 corpus
and callas results below remain evidence for the original exact files; they are
not inherited by newly generated candidates. Profile limitations, the failed
CMYK gallery comparison, unfinished normative inventory remain as recorded. The later licensing decision is described below. The source archive's adjacent isolation receipt
records the new extracted-package build and exact output identities.

## October 4 implementation baseline

The initial general-purpose finished-page generator is implemented. It supports ordinary RGB/Gray/CMYK output, native axial/radial gradients, all sixteen PDF blend modes, local composition spaces/resources and a bounded CMYK PDF/X mode. The public API, independent examples and optional compatibility adapter build from this workspace. Windows x86_64 is the verified execution platform; font/profile preparation uses its confined workers.

Root's canonical native replay passed 150 affected Rust tests, including 24 finished-page and eight gradient checks. One existing child helper is intentionally ignored and exercised by its parent. The optional pdfx4_allowed_features target ran zero tests because pdf-oxide-validator was disabled; its advanced validator fixtures are not claimed as current executed coverage. Default and compatibility Clippy passed; owned Rust files are formatted. Imported unrelated formatting debt remains outside this change.

The inherited Unicode suite lacked its referenced text corpus in the imported checkpoint. Five first-party samples now live beside that suite, following its existing expectations; every assertion is unchanged. All eleven native Unicode tests passed, and workspace `--all-targets` checking passed. This establishes compilation of all targets and execution of that repaired suite, rather than execution of every workspace test.

Six real Windows confinement checks passed file/network/child denial, memory and CPU limits, cancellation, drop/pipe cleanup and unrelated-handle isolation. The independent default-feature client generated repeatable bytes, retained a 57-package graph without authoring crates, and refused output in three actual missing/malformed worker cases. A copied gallery executable generated ordinary graphics without sidecars; print preparation used the exact color worker and selected profile.

## October 4 exact output evidence

- All 34 regenerated compatibility cases passed qpdf. Three intentional production-control cases reproduced their reviewed bleed/resolution warnings.
- Callas check-only analysis passed all 14 positive candidates. Strict JSON/XML review checked exact input/checker/profile identities and absence of fixups. Combined analysis and the separate selected-CMYK profile guard met 34/34 expectations.
- Callas alone met 30/34 expectations: it accepted four invalid output-profile component declarations. The separate guard rejected them. Combined success does not mean callas rejected those controls.
- All 190 source pages passed declared boxes, ordered ActualText and extracted text after separator normalization. PDFium/Poppler retained-to-current and current cross-reader comparisons passed the unchanged recorded bounds, at a maximum render dimension of 1,600 pixels.
- The exact independent standalone candidate and new CMYK gallery both passed callas; both ordinary controls failed for their missing PDF/X envelope. The four-case example matrix met 4/4 combined expectations.
- Four standalone pages passed two-reader comparisons and image/clip probes. The six-page feature gallery passed image/function/shading/mask checks, all sixteen blend-name checks, and exact resource ownership for 24 streams in each PDF.
- Fifty authored gallery appearance probes passed independently in both readers: RGB endpoints/multistops/hard stops, radial alpha/padding, independent component/alpha interpolation, group opacity, clipping, Gray transitions and representative group blending. Four malformed inspector controls were rejected: false isolation, numeric isolation, unused resource entry and missing output-intent Info.

## CMYK preview qualification

The full feature-gallery cross-reader comparison remains **failed** on ordinary page 2 and print pages 1–2. Its original limits are mean channel difference at most 5, 95th-percentile maximum channel difference at most 20, and at most 8% of pixels above a difference of 10. Observed maxima include 24 and 12.557% for the latter two metrics. No tolerance or source color was changed to turn those results into passes.

Differences concentrate in smooth native CMYK gradients. On ordinary page 2, 99.21% of pixels differing by more than ten channel values fall inside the authored CMYK regions. Gray gradients and opaque CMYK hard transitions agree closely. Both readers pass the focused authored appearance probes, and exact print bytes pass structural and PDF/X development checking. This establishes those behaviors while leaving device-color preview agreement qualified. Review CMYK appearance in the intended reader; physical printer acceptance remains a downstream check.

## Requirements and verification

Current output checks cover custom output-condition Info, required title, ICC version screening, encoded content strings and object/executed-state limits. The [supported-output review](requirements/supported-output-review.md) describes the generated structures and their limits.

The real printer profile used for current worker/export and checker evidence is ICC 2.2.0. Synthetic fixtures exercise screening of the admitted 4.0.0–4.2.0 encodings and then fail for missing tags. A complete real ICC v4 profile has not received the same end-to-end verification; those screening checks do not establish that broader profile evidence.

The inventory retains 28 authority topics and fifteen public writer contracts, with current implementation and hashed evidence bindings. PX4-NORMATIVE-INVENTORY remains pending. verify-requirements.py --require-normative deliberately fails for that stronger exhaustive assertion. Current checking does not establish unrestricted PDF/X-4 conformance or certification.

Sanitized, hash-bound current receipts are under verification/current/finalization-20261004. Earlier verification/current receipts remain historical checkpoints. Full local PDFs/renders stay in ignored artifacts; raw licensed reports and activation material stay private outside this workspace.

The public `verify-feature-gallery-appearance.py` runner repeats all fifty
authored checks from the source package. Its refreshed receipt retains the
original colors, tolerances, probe results and region metrics, and binds the
packaged runner and exact render identities. The earlier receipt remains intact.
Four controls reject disabled assertions, replacement of an existing receipt,
changed render bytes and an incorrect authored pixel even with an updated hash.

## Source delivery and next step

The curated source archive includes owned source/fixtures, independent examples, public documentation, sanitized evidence and exact dependency/native-library/asset notices. Its auditor rejects private paths, commercial report/profile material, runtime binaries, escaping/duplicate entries, symlinks and changed file identities. Focused package/privacy tests cover those boundaries, licensed policy rejection and assertion protection. Require the adjacent archive receipt and passed isolated-build receipt to establish the unpacked build and matching generated output; a ZIP alone is insufficient.

A skeptical second pass reproduced an optimized-Python assertion bypass in the
gallery inspector. The gallery and public API verifiers now reject `-O` and
`PYTHONOPTIMIZE` before creating output. The standalone guide now includes worker
builds, both example targets, the output parent and portable Windows commands.
Native execution and a fresh extracted-source build verify the repaired handoff;
the failed whole-page CMYK comparison remains recorded.

As of October 16, first-party source and original project material use MIT OR Apache-2.0 under the PatterboxPDF name. Cargo packages retain publish = false. Historical receipts preserve their original license-pending statements. A fresh source-release receipt records the exact licensed source/build identities; earlier callas verdicts do not transfer to new PDF bytes. Next: build the documented Windows standalone examples. A complete standards inventory is separate future assurance work.

## October 16 licensed source build

The [source isolation receipt](verification/current/public-release-20261016/source-isolation.json) records the exact licensed archive and its source-equality checks for the fresh Windows build. The 712 source inventory files plus source manifest survived ZIP-to-Git-to-fresh-checkout comparison byte for byte. Fresh locked Windows worker and separate client/gallery builds passed with dependency caches and new target directories. No sibling source or prebuilt workers were used.

Native example runs, repeatable client bytes, the 57-package default dependency graph, three actual worker failures, independent PDF parsing and qpdf checks passed. Fifty gallery appearance probes passed. Whole-page comparisons retained the same three CMYK failures; no fresh callas or unrestricted conformance verdict is claimed. The five package regressions, three requirements tests and current requirements auditor passed; archive007 still verifies under its historical v1 pending policy and retains its original SHA-256.

The receipt belongs to the frozen source baseline; this section and the receipt are subsequent verification documentation. Public repository/readback and the open release review PR are a separate delivery gate.
