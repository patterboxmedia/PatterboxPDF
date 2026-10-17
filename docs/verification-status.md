# Verification status

Current qualification passes 37 supported-output rules on four exact positive PDFs, 37 exercised negative output controls and three source-font metric measurements. The four current records and exact implementation/evaluator identities pass the neutral catalog audit. Earlier checker verdicts remain attached to their original bytes.

The update implements native Ubuntu font/color/raster workers, fail-closed Linux
confinement, precise prepared-font metrics and binary font/profile checks. Long
nonstream lines wrap without changing decoded titles or stream payloads.
Independent inspection covers ordinary and selected-CMYK print PDFs, positive
output and exercised negative controls. A separate metric oracle uses explicit
source fonts and independent geometry/raster measurements.

## CMYK preview investigation

The controlled investigation is complete on the recorded Windows and native Ubuntu
executables. Both produced identical diagnostic PDFs. The replay passed 288 decoded
component/opacity checks, 156 gradient-versus-flat samples, ten matched ICC samples
and sixty retained-gallery samples. Fifty authored appearance probes pass separately.
[Color behavior](color-behavior.md) records tool versions, settings and replay commands.

The default PDFium/Poppler comparison remains failed on ordinary page 2 and print
pages 1-2. Unchanged limits are mean difference at most 5, 95th-percentile maximum
difference at most 20, and at most 8% of pixels above 10. Independent controls
establish correct components and reader conversion/sampling differences. They
do not claim identical previews or physical printer acceptance.

## Runtime and source delivery

Current qualification executed 146 native Windows PE tests/cases and 67 native Ubuntu ELF tests, prepared real resources, staged matching workers and exercised actual refusals. The Windows result includes six AppContainer checks and nine explicitly selected isolated packaging cases. All mandatory Linux cases ran with zero ignored or filtered tests. Fresh extracted-source builds repeated both platform workflows.
Linux regressions additionally check timestamp denial, a 1024 descriptor limit and
typed deadline errors after independent reaping. ICC checks bound shared-payload
work, and inline ActualText controls check encoded-byte limits.
Linux tests check real file, network, process, descriptor, memory, CPU, wall-time
and cancellation behavior. Missing tools, skipped tests, compile-only results
and unconstrained fallbacks do not count as success. The optional trusted
validator deliberately refuses Linux.

The source package was extracted into fresh directories and built from its own source and pinned lockfiles on both targets. An adjacent isolation receipt binds archive,
manifest, source and output identities. Packaging and a development-checkout
build alone do not establish that delivery gate.

## Retained evidence

Earlier receipts remain under verification/current/finalization-20261004,
font-map-metadata-20261006 and public-release-20261016. They record the tested
baselines: generated output, caller contracts, six Windows confinement checks,
three actual preparation-worker refusals, Unicode/metadata corrections,
renderer comparisons and a licensed source build. Historical commercial-checker
results apply only to their original exact PDFs. This update claims no fresh
commercial-checker verdict.

First-party source and original project material use MIT OR Apache-2.0.
Dependencies, fonts and profiles retain their separately recorded terms.
Registry publication stays disabled. Current checks qualify supported generation;
unrestricted PDF/X-4 conformance, universal reader fidelity and downstream printer
acceptance require their own evidence.
