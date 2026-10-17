# PatterboxPDF source release

First-party source and original material are available under your choice of MIT or Apache 2.0 (`MIT OR Apache-2.0`). The complete grants are in `LICENSE-MIT` and `LICENSE-APACHE`. Upstream notices remain intact. Dependencies, fonts, profiles and character mapping data retain their separate terms in `third_party/cargo-notices/dependencies.json`. Original project fixtures use the first-party dual license; derivatives of upstream fonts retain their recorded upstream terms.

This release distributes source through GitHub. All first-party Cargo packages, including the standalone client, retain `publish = false`. Existing crate, worker and compatibility names remain available. [Development](development.md) and the [standalone guide](../examples/standalone-client/README.md) describe Windows and Ubuntu setup.

Current source qualification passes 368 native Windows tests and 319 native Ubuntu tests. Both platforms pass the expanded font, profile and composition workflows. [Verification status](verification-status.md) records the actual outputs, controls and limitations.

Independent source and exact-output review accepted the documented input contract and specific authoring premises. Public family/catalog audits pass. The source handoff separately qualifies the exact archive and both advertised native workflows.

## Package and verify source

The package allowlist includes source, required redistribution-safe fixtures, both root licenses, public documentation, neutral evidence and exact third-party notices. Dependencies are acquired separately through the pinned Cargo lockfiles. Build outputs, commercial tools, raw licensed reports, personal instructions, credentials and machine-specific provenance history are excluded.

Build an archive and verify its inventory without extracting:

```powershell
New-Item -ItemType Directory -Path artifacts -Force | Out-Null
python scripts/build-source-package.py artifacts/source-review-001
python scripts/build-source-package.py --verify artifacts/source-review-001/patterboxpdf-0.0.0-source.zip
```

Use a new output directory for another package. The verifier checks complete root license texts, first-party manifest and notice identities, disabled registry publication, target-specific dependency notices, asset provenance, exact inventory lengths/hashes, duplicate or escaping paths, symlinks and excluded payloads. Fixed ZIP timestamps and `* -text` Git attributes preserve exact source bytes.

The notice inventory records the union of Windows and Linux workspace/source-test dependencies and the separate client, with target membership and both lockfiles. Upstream license texts and external font/profile hashes remain pinned. Original component controls have their own project provenance and dual-license record, separate from upstream font derivatives. The Macintosh Roman mapping data retains its Apple attribution, Unicode data notice and Python extraction provenance. Package guards verify all 128 scalar identities and reject missing or reassigned notices.

## Qualify the extracted package

Extract the audited archive into a new directory. Build the required workers and separate client from that source using its own manifests and lockfiles. Stage matching workers, then execute native confinement/resource tests and the ordinary/print workflows on both targets. Run independent output checks against the exact resulting PDFs.

An adjacent isolation receipt must identify the archive and source manifest, confirm extracted source equality, record actual native execution and bind the resulting output identities. Packaging alone does not prove a usable build. A newly prepared package cannot borrow an earlier archive's qualification.

The current handoff passed extraction/source equality and builds/workflows from initially absent targets on Windows and Ubuntu. Its compiled source inputs match the full native and independent resource/output qualification. Keep the adjacent isolation receipt with the exact archive; it identifies the actual inputs, execution and outputs.

Ordinary drawing and decoded images need no workers. Font and CMYK profile preparation use confined workers. Current generation and independent verification use free tools; the writer needs neither qpdf nor a commercial checker.

## Retained evidence

The earlier [source isolation receipt](verification/current/public-release-20261016/source-isolation.json) remains attached to its original archive and source baseline. It is historical evidence rather than qualification of this changed source. Historical commercial-checker results likewise remain attached to their original exact PDFs.

The three original CMYK whole-page reader comparisons retain their failed verdicts at unchanged bounds. Controlled component/color evidence explains the reader differences. See [color behavior](color-behavior.md) and [verification status](verification-status.md). Source-release qualification does not establish unrestricted PDF/X-4 feature coverage, identical reader previews or physical printer acceptance.
