# Verification status

Current generator and resource-worker qualification uses free tools and passes the recorded native Windows and Ubuntu workflows. The results below describe current source execution and exact emitted files.

Independent source and exact-output review accepted the documented family contract and specific authoring premises. The public family/catalog audits pass and bind current code, native controls and 3,726 actual measured comparisons. Corpus size is one part of that evidence.

## Current generator and output evidence

| Check | Windows x86_64 | Ubuntu x86_64 |
| --- | --- | --- |
| Native tests executed | 368; none ignored | 319 in 17 suites; none ignored or filtered |
| Public font corpus | 131 cases; 218 PDFs | 131 cases; 218 PDFs |
| Complete font-descriptor qualification | 212 PDFs | 212 PDFs |
| Profile corpus | 17 cases; 8 admitted profiles; 9 expected refusals | 17 cases; 8 admitted profiles; 9 expected refusals |
| Profile-bearing print outputs | 16 PDFs | 16 PDFs |
| Composition corpus | 4 authored documents, repeated | 4 authored documents, repeated |

The font corpus has 106 successful ordinary/print cases, 24 expected refusals and one retained normalization characterization. Six of those refusals follow an ordinary compatibility output whose source lacks facts required for print. Their print requests correctly return fact-required errors. Those six ordinary PDFs remain outside complete descriptor qualification. The characterization is recorded separately and does not count as a valid-input positive.

The font checks cover TrueType subsets and untouched full programs, point-attached components, inherited advances, component tails, required vertical context, CFF flex/affine/fractional geometry, source/face-bound design facts, valid name formats, Macintosh Roman and Windows Unicode names, and invalid retained names. The naming cases exercise both full embedding and subsetting. Known multi-scalar Unicode mappings and ActualText preserve Latin, supplementary and accented scalar order. Independent measurements use source programs and separately authored facts rather than producer metric reports.

All 218 actual font PDFs match byte for byte across these two runs. The 125 cases retained from the preceding run also preserve all 206 earlier PDF bytes and prepared font programs; their new preparation witnesses identify the current worker. The four composition outputs also have equal bytes and decoded semantics. This records observed equality for the corpus, rather than a universal cross-platform determinism requirement.

The output-rule catalog has four exact positive PDFs and actual negative controls for all 37 rules. Additional emitted controls exercise wrong ascent, capital height, widths, bounding boxes, resource witness identities and encoded matrix relationships. An adapter-specific ascent alteration is detected against the actual allocated font, including unused registration. The serialized-xref offset control retains qpdf's recovery warning and is not described as a qpdf-clean negative.

Composition checks independently decode path/stroke combinations, image and straight-alpha planes, gradient functions, blend groups, metadata, page boxes and direct resources. Repeat and registration-order controls preserve the measured semantics; targeted mutations produce the expected disagreements.

These examples probe the [generation contract](requirements/generation-contract.json). Source rules and independent review must establish the complete documented domain; corpus size alone does not.

## Runtime and source delivery

The Windows run includes six actual AppContainer checks. Ubuntu tests exercise file, network, process, descriptor, memory, CPU, wall-time and cancellation boundaries using real kernel primitives. Real malformed-worker and preparation failures preserve typed errors and allow fresh retries. Mandatory tests cannot be skipped, and unavailable confinement fails before resource parsing.

Both platforms pass strict lint checks, prepare resources, stage matching workers and run a separate public client. The optional trusted validator supports Windows and deliberately refuses Linux.

A usable source handoff requires an audited archive extracted into a new directory, built from its own source and lockfiles, then exercised on both targets. Its adjacent isolation receipt must bind that exact archive, source and output. A development build or archive inventory alone does not establish this gate.

The source handoff separately passed archive inventory/source equality and the advertised builds and workflows from initially absent targets on both systems. Full native/resource-corpus qualification uses the same exact compiled inputs. Its adjacent isolation receipt identifies that archive and the actual resulting files.

## CMYK preview evidence

The controlled CMYK investigation passed on the recorded Windows and Ubuntu executables. Both produced identical diagnostic PDFs. It passed 288 decoded component/opacity checks, 156 gradient-versus-flat samples, ten matched ICC samples and sixty retained-gallery samples. Fifty authored appearance probes pass separately. [Color behavior](color-behavior.md) records the original tools, settings and replay commands.

The original default PDFium/Poppler comparison remains failed on ordinary page 2 and print pages 1-2. Its limits are unchanged: mean difference at most 5, 95th-percentile maximum difference at most 20, and at most 8% of pixels above 10. Controlled measurements establish correct components and reader conversion/sampling differences. They do not establish identical previews or physical printer acceptance.

## Retained records and scope

Historical receipts retain their exact original source and output identities. Earlier commercial-checker verdicts apply only to the files actually checked; this update claims no new commercial-checker verdict. Changes affecting generation or inspection require fresh relevant checks and current bindings.

First-party source and original material use MIT OR Apache-2.0. Dependencies, fonts, profiles and character mapping data retain their separate terms. Registry publication stays disabled. Current source execution and output measurements do not establish unrestricted PDF/X-4 feature coverage, universal reader fidelity or downstream printer acceptance.
