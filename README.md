# PatterboxPDF

An independent Rust PDF generation library for unrelated applications. Its initial finished-page interface supports ordinary PDFs and a bounded CMYK PDF/X-4 mode. Publisher is a future consumer; its document model and UI do not define this library.

The initial generation implementation is complete. Callers supply composed pages, paths, positioned glyphs, decoded eight-bit RGB/Gray/CMYK images, transforms and clips. Native axial/radial gradients support multiple stops, hard transitions and varying alpha. Items support all sixteen PDF blend modes, isolated group opacity and local blending spaces. Shared resources are reused, and each page/Form lists its direct resource usage.

Ordinary graphics and decoded images work without worker executables. Exact font and print-profile preparation use confined Windows workers. The default library excludes application authoring dependencies; the optional publisher-adapter preserves imported document compatibility. Reading/editing/merging, paragraph layout/shaping, forms/signatures, spot colors and additional gradient families have separate future scope.

## Use the library

- [Public API](docs/api.md): coordinates, resources, gradients, compositing and output modes.
- [Standalone examples](examples/standalone-client/README.md): worker-free graphics and prepared-resource print workflows.
- [Local development](docs/development.md): build and test commands.
- [Platform support](docs/supported-platforms.md): verified Windows execution and resource preparation.
- [Architecture](docs/architecture.md): module ownership and optional compatibility support.
- [Roadmap](ROADMAP.md): completed scope and deferred decisions.

## Verification and delivery

The October 6 maintenance update corrects ToUnicode destination limits and
resource type, and preserves carriage returns in decoded Info/XMP titles.
Focused checks and a fresh extracted-source build are recorded in
[verification status](docs/verification-status.md). Font-bearing PDFs have new
bytes; the earlier checker results below apply to their retained baseline files.

Current evidence covers 150 affected Rust tests, six actual Windows confinement checks, three real preparation-worker failures, and a separate client with a 57-package default dependency graph. The regenerated 34-case corpus met all combined checker/profile expectations, including 14 positive print candidates. All 190 source pages passed geometry, ordered text and recorded PDFium/Poppler comparisons.

The new six-page ordinary/print gallery passed structural and exact local-resource inspection. Fifty authored appearance probes passed in both readers, and its exact print bytes passed callas analysis. Three pages containing smooth CMYK gradients exceeded the declared whole-page reader-comparison bounds. Those failed metrics remain recorded; CMYK screen previews need review in the intended reader. See [verification status](docs/verification-status.md) for exact scope and qualifications.

The writer checks output-intent metadata, titles, ICC editions, encoded string lengths and executed graphics-state/object limits. [Supported-output review](docs/requirements/supported-output-review.md) records applicability. A complete normative statement inventory remains unfinished; tested outputs do not establish unrestricted PDF/X-4 conformance.

The source package includes pinned dependency/asset notices and sanitized evidence. Its adjacent isolated-build receipt establishes the actual unpacked build; packaging alone does not. Callas is a development verification tool outside the runtime and source archive.

PatterboxPDF's first-party source and original project material are available under your choice of [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE). Third-party dependencies, fonts and profiles retain their separately recorded terms in `third_party/cargo-notices/dependencies.json`. Cargo packages retain `publish = false`; this release distributes source through GitHub.

Next: follow the standalone examples on Windows to build the library and its resource workers. See [release details](docs/release.md) for source verification and evidence limits.
