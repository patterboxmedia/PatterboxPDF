# Supported platforms

PatterboxPDF generates ordinary PDFs directly and prepares fonts, color profiles and encoded raster inputs through separate workers. Applications stage matching workers beside their executable. Build and restage workers when upgrading; font preparation uses protocol 9. Missing, changed, malformed or unconfined workers cause preparation to fail.

| Target | Generation and preparation | Confinement |
| --- | --- | --- |
| Windows x86_64 MSVC | Native generation and font/color/raster workers | AppContainer and Windows job limits |
| Ubuntu x86_64 GNU | Native generation and font/color/raster workers | Landlock ABI 3+, seccomp, sealed executable and pidfd ownership |

Current qualification executed 368 native Windows tests and 319 native Ubuntu tests, with no ignored cases. The Windows run includes six AppContainer isolation tests. All mandatory Ubuntu cases ran with no ignored or filtered tests. Real resources, matching workers, ordinary/print examples and expected preparation refusals were exercised on both systems.

Each platform also passed 131 public font cases, 17 profile cases and four composition documents with independent output checks. [Verification status](verification-status.md) gives the output counts and limits.

The source handoff also verifies the exact archive inventory and builds/runs the advertised workflows from new directories with initially absent targets. Its compiled inputs match the separately executed full native and resource-corpus qualification. Keep the adjacent isolation receipt with that archive.

Ordinary paths, gradients, blend modes and decoded RGB/Gray/CMYK images need no workers. Fonts require opaque preparation receipts. CMYK print output also checks the exact supplied profile through the color worker. Callers provide positioned glyphs and any required truthful font design facts. Paragraph layout and shaping remain caller responsibilities.

The qualified Windows environment is Windows 11 x86_64. The Ubuntu run uses actual x86_64 Linux ELF executables under WSL2 with the recorded Linux kernel and native libraries. The Linux backend pins its loader and shared-library objects and requires Landlock ABI 3 or later, seccomp, clone3/pidfd and sealed memfd execution. Missing primitives fail before parsing. See the [Linux boundary](../crates/linux-worker-job/README.md) for its policy.

Qualification applies to those recorded environments. Other distributions, kernels, architectures and bare-metal configurations need their own execution and confinement evidence.

The optional trusted validator supports Windows. Linux refuses it before launch. The Python output inspector is a development check; the PDF writer needs neither qpdf nor a commercial checker. Enable `publisher-adapter` for the retained saved-publication compatibility path. Default library dependencies exclude application authoring crates. Existing crate, protocol and worker names remain compatibility identifiers.

Native receipts show which code executed under confinement. Independent output receipts show what the exact emitted files contain. The generation evidence must bind both.
