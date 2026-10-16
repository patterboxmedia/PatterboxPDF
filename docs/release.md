# PatterboxPDF source release

First-party source and original project material are available under your choice of MIT or Apache 2.0 (`MIT OR Apache-2.0`). The complete grants are in the root `LICENSE-MIT` and `LICENSE-APACHE`. Existing upstream copyright notices remain intact. Dependencies, fonts and the upstream CMYK fixture retain their separate terms in `third_party/cargo-notices/dependencies.json`; project-generated fixtures use the first-party dual license.

This is a GitHub source release. All first-party Cargo packages, including the separate example client, retain `publish = false`. Rust crate, worker and compatibility names are retained. See [development](development.md) and the [standalone guide](../examples/standalone-client/README.md) for Windows setup and example commands.

The explicit package allowlist includes source, required redistribution-safe fixtures, both root licenses, current public documentation, sanitized evidence and exact third-party notices. Dependencies are acquired separately through the pinned Cargo lockfiles. The archive contains no build outputs, commercial tools/profiles, raw licensed reports, personal instructions, credentials or machine-specific provenance history.

Build a new package and verify it without extracting:

```powershell
python scripts/build-source-package.py artifacts/source-review-001
python scripts/build-source-package.py --verify artifacts/source-review-001/patterboxpdf-0.0.0-source.zip
```

The licensed v2 policy requires both complete root license texts, matching first-party manifest and notice identities, and disabled registry publication. The verifier also supports retained v1 archives with their original `license_pending` policy; it rejects unknown schemas and conflicting policies. Every policy checks inventory lengths/hashes, notices/assets, escaping/duplicate paths, symlinks, private paths and forbidden binary/report payloads. Fixed ZIP timestamps and `* -text` Git attributes preserve exact source bytes, including retained CRLF receipts and notices.

Extract the audited archive into a new directory. For a public Git baseline, initialize only this curated tree and compare every Git blob and a fresh checkout to the archive inventory before building. Build required workers and the separate client from that checkout into fresh targets, then run the ordinary and print examples. Record the archive/manifest identity, baseline commit/tree, source equality and exact output identities in an adjacent isolation receipt. Packaging alone does not prove a usable build.

Historical archives and verification receipts remain immutable checkpoints. Archive007 predates the October 16 licensing decision and retains its original pending policy. The current notice metadata is regenerated from the Windows Cargo source/test dependency closure plus the standalone client; exact registry/upstream notices and external asset hashes remain pinned. Notice collection is a private development operation; the public package includes its audited output.

Windows x86_64 is the verified runtime platform. Ordinary drawing and decoded images need no workers; exact fonts and bounded CMYK PDF/X preparation use confined Windows workers. Earlier CMYK gallery reader comparisons remain failed on three pages, and the complete normative PDF/X-4 inventory remains unfinished. The fresh release build and structural inspection do not transfer any earlier callas verdict to new PDF bytes or establish unrestricted PDF/X conformance. See [verification status](verification-status.md).
