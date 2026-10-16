# Technical sources for the PDF library

Public format references relevant to the supported generation API.

## Publicly accessible references

| Reference | Useful content | Scope |
| --- | --- | --- |
| [Adobe PDF Reference 1.6](https://printtechnologies.org/standards/files/pdf-reference-1.6-1.pdf), authored by Adobe and hosted by APTech | The detailed underlying file format: color spaces, embedded fonts, images, gradients, transparency and output profiles. | PDF/X-4 is based on PDF 1.6. This reference describes PDF generally; PDF/X adds restrictions. Read with the [1.6 errata](https://printtechnologies.org/standards/files/pdf-reference-1.6-errata.pdf) and [blend-mode addendum](https://printtechnologies.org/standards/files/pdf-reference-1.6-addendum-blend-modes.pdf). |
| [PDF/X in a Nutshell](https://pdfa.org/resource/pdfx-in-a-nutshell/), PDF Association, 2017 | Explains the print-specific rules, complete resource embedding, color management and live transparency. | Publicly offered under CC-BY 4.0. Useful PDF/X-4 guidance, rather than a complete clause checklist. Its account of newer standards reflects its publication date. |
| [PDFX-ready preflight comparison V1.7/V2.7](https://www.pdfx-ready.ch/download/1677/preflight-dokumentation/11341/pdfx_ready-preflight-vergleich_v17d27d), 2022 | A detailed German table of checks for CMYK and CMYK+RGB workflows, including transparency color spaces, overprint, ink coverage, small text and image resolution. | Production profiles add requirements and warnings beyond PDF/X. The table includes an aggregate PDF/X conformance check; it does not establish complete conformance. [PDFX-ready's download page](https://www.pdfx-ready.ch/preflight_v27-17/) supplies the actual profiles for development checking. |
| [PDF/X: The key facts](https://pdfa.org/pdfx-the-key-facts/), Dietrich von Seggern of callas, 2017 | A concise public explanation of embedded resources, page boxes, metadata and prohibited interactive features. | Helpful cross-check for PDF/X-4 topics. Minimum image resolution and required bleed amounts depend on production needs. Do not apply this older article indiscriminately to newer PDF/X versions. |

## Apply the information to this project

1. **Review supported output first.** Compare the current emitted fonts, profile, page boxes, metadata, CMYK paints and transparency groups with the relevant references. Fix a concrete discrepancy when one is demonstrated, then independently check the changed PDF bytes. Keep production warnings distinct from invalid file structures.
2. **Use the format reference for native gradients.** Section 4.6.3 (printed pages 272-301) specifies axial, radial and mesh shading. Section 3.9.3 (pages 147-148) describes functions that join multiple color transitions. CMYK is a supported shading color space. The writer now emits native axial/radial functions and shadings, including a matching gray luminosity mask for varying alpha. Chapter 7 determines transparency behavior; mesh shading remains future work.
3. **Add printer-specific warnings where useful to a consumer.** The PDFX-ready matrix can inform warnings about excessive ink, small text using several inks and unsuitable image resolution. Select those rules for the intended printing process; do not impose them universally on the reusable writer.

The current writer embeds its exact screened CMYK profile, writes `/N 4`, embeds prepared fonts and images, and explicitly declares CMYK for PDF/X page and isolated group blending. Current verification is described in [verification status](../verification-status.md).

## Output-intent metadata

The preserved baseline emitted a custom `/OutputConditionIdentifier` and embedded `/DestOutputProfile`, but omitted output-intent `/Info`. That identifier was a custom description rather than an ICC registry printing-condition entry. PDF Reference 1.6 Table 10.50 requires `/Info` for that case.

`/OutputCondition` is a separate recommended descriptive field. The retained standalone candidate demonstrates the omission despite its checker pass. The current writer supplies a nonempty profile-linked `/Info` and validates a nonblank title. Focused regression tests cover those fields; current exact-byte evidence is identified in [verification status](../verification-status.md).

## Version traps found during the search

- [APTech's link labelled “PDF/X-4 application notes”](https://printtechnologies.org/standards/pdf-x) currently opens a September 2006 document covering PDF/X-1a, PDF/X-2 and PDF/X-3. Do not treat it as PDF/X-4-specific instructions.
- The [callas 2025 poster about shared PDF subset requirements](https://pdfa.org/download-area/posters/PDFDaysEurope2025-TechnicalPoster-callas.pdf) covers upcoming PDF/A-4 and PDF/X-6 revisions. Do not import its rules into this PDF/X-4 writer solely because it is newer.
- The old Adobe-hosted PDF 1.6 URL returned 404 during this search. The APTech-hosted reference linked above was accessible.

The [supported-output review](supported-output-review.md) describes the implemented generation contracts. Ordinary and print generation have separate admission policies. Tested outputs do not establish unrestricted PDF/X-4 conformance.
