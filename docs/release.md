# PatterboxPDF source release

First-party source and original project material are available under your choice of MIT or Apache 2.0 (`MIT OR Apache-2.0`). The complete grants are in the root `LICENSE-MIT` and `LICENSE-APACHE`. Existing upstream copyright notices remain intact. Dependencies, fonts and the upstream CMYK fixture retain their separate terms in `third_party/cargo-notices/dependencies.json`; project-generated fixtures use the first-party dual license.

This is a GitHub source release. All first-party Cargo packages, including the separate example client, retain `publish = false`. Rust crate, worker and compatibility names are retained. See [development](development.md) and the [standalone guide](../examples/standalone-client/README.md) for Windows and Ubuntu setup and the current qualification boundary.

The explicit package allowlist includes source, required redistribution-safe fixtures, both root licenses, current public documentation, sanitized evidence and exact third-party notices. Dependencies are acquired separately through the pinned Cargo lockfiles. The archive contains no build outputs, commercial tools/profiles, raw licensed reports, personal instructions, credentials or machine-specific provenance history.

Build a new package and verify it without extracting:

```powershell
New-Item -ItemType Directory -Path artifacts -Force | Out-Null
python scripts/build-source-package.py artifacts/source-review-001
python scripts/build-source-package.py --verify artifacts/source-review-001/patterboxpdf-0.0.0-source.zip
```

The licensed v2 policy requires both complete root license texts, matching first-party manifest and notice identities, and disabled registry publication. The verifier also supports retained v1 archives with their original `license_pending` policy; it rejects unknown schemas and conflicting policies. Every policy checks inventory lengths/hashes, notices/assets, escaping/duplicate paths, symlinks, private paths and forbidden binary/report payloads. Fixed ZIP timestamps and `* -text` Git attributes preserve exact source bytes, including retained CRLF receipts and notices.

Extract the audited archive into a new directory. For a public Git baseline, initialize only this curated tree and compare every Git blob and a fresh checkout to the archive inventory before building. Build required workers and the separate client from that checkout into fresh targets, then run the ordinary and print examples. Record the archive/manifest identity, baseline commit/tree, source equality and exact output identities in an adjacent isolation receipt. Packaging alone does not prove a usable build.

Historical build results identify exact source archives and generated output bytes. Archive007 predates the October 16 licensing decision and retains its original pending policy. The current notice metadata records the union of Windows and Linux Cargo source/test dependency closures plus the separate client, with exact target membership and both lockfiles; exact registry/upstream notices and external asset hashes remain pinned. The public notice collector and pinned notice output are included for reproducibility.

Ordinary drawing and decoded images need no workers. Exact fonts and bounded CMYK print preparation use confined resource workers. Current native qualification passes on Windows and Ubuntu, including real resource preparation, confinement and worker refusals. Earlier CMYK gallery comparisons remain failed on three pages; the controlled investigation explains their cause. No historical exact-file checker verdict transfers to changed bytes. See [verification status](verification-status.md).

## October 16 source isolation

The [source isolation receipt](verification/current/public-release-20261016/source-isolation.json) records the licensed archive identity and the source-equality checks for the fresh Windows build. All 713 payloads, including the source manifest, matched the ZIP, Git blobs and fresh checkout before and after the fresh Windows builds. The separate workers/client and ordinary/print gallery ran successfully. Independent structure/resource and qpdf checks passed; fifty appearance probes passed while the three documented CMYK whole-page comparisons remained failed.

This record is follow-up verification documentation for that frozen baseline. Its licensing and build evidence does not include remote publication; repository/readback verification is recorded separately when performed.
