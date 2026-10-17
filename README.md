# PatterboxPDF

A reusable Rust library for generating ordinary PDFs and CMYK PDF/X-4 files from composed pages. Callers provide paths, positioned glyphs, decoded eight-bit images, transforms and clips. Axial and radial gradients support multiple stops, hard transitions and varying opacity. All sixteen PDF blend modes and isolated groups are available.

The [generation contract](docs/requirements/generation-contract.json) describes the supported input families, resource requirements and generation checks. The library serves unrelated applications. Publisher is one possible consumer.

Ordinary graphics and decoded images work directly. Font and print-profile preparation use confined native workers on the recorded Windows and Ubuntu environments. Reading, editing, merging, paragraph layout, shaping, forms, signatures, spot colors and additional gradient families have separate future scope.

## Use the library

- [Public API](docs/api.md): coordinates, resources, gradients, compositing and output modes.
- [Standalone examples](examples/standalone-client/README.md): drawing and prepared-resource workflows.
- [Local development](docs/development.md): builds and verification commands.
- [Platform support](docs/supported-platforms.md): native worker and environment requirements.
- [Controlled screen previews](docs/proofing.md): optional Python CMYK-to-display rendering with explicit ICC profiles.
- [Generation checks](docs/requirements/README.md): input contracts and independent evidence.
- [Roadmap](ROADMAP.md): delivered scope and future capabilities.

## Verification and delivery

Current generator qualification passes 368 native Windows tests and 319 native Ubuntu tests, with none ignored. On each platform, the font corpus exercises 131 cases and produces 218 PDFs: 212 receive complete descriptor checks, while six ordinary compatibility outputs deliberately omit facts required for print. Their print requests return the expected fact-required errors. Profile tests exercise eight admitted profiles and nine expected refusals. Independent composition and output controls cover geometry, images, gradients, text, metadata and resources. See [verification status](docs/verification-status.md) for the measured scope.

The writer embeds the exact prepared font program and admitted ICC profile. Rights-required full-font embedding preserves the original font bytes. When font data cannot establish accent roles or a capital height, callers supply truthful facts bound to that source and face. Other specific registration, measurement and optional resource-language premises are stated in the generation contract.

The [generation contract](docs/requirements/generation-contract.json) binds the complete documented input domain to current code, independent measurements and actual native controls. The public contract/catalog audits pass. The source handoff has separate archive extraction, build and execution qualification on both supported targets; retain its adjacent isolation receipt.

The three original CMYK screen comparisons retain their failed verdicts at unchanged bounds. Controlled component/color checks and those reader differences are explained in [color behavior](docs/color-behavior.md). These results do not establish unrestricted PDF/X-4 feature coverage, identical previews in every reader or physical printer acceptance.

The optional [screen-preview companion](docs/proofing.md) reads an existing PDF through a caller-supplied, color-managed Poppler executable and writes ICC-tagged RGB PNGs with a completion manifest. It requires Pillow and pypdf separately; Rust generation needs none of these preview dependencies. Native Windows and Ubuntu previews pass the recorded color, opacity, profile-sensitivity and repeatability checks. Exact source-archive qualification is recorded separately in its adjacent isolation receipt.

Current qualification uses free tools. Independent development inspectors stay outside the library runtime. The source package includes dependency and asset notices; its adjacent isolation receipt must confirm extraction, build and execution on both supported targets before release.

First-party source and original material are available under your choice of [MIT](LICENSE-MIT) or [Apache 2.0](LICENSE-APACHE). Dependencies, fonts, profiles and character mapping data retain their recorded terms in `third_party/cargo-notices/dependencies.json`. Cargo packages retain `publish = false`; registry and binary distribution require separate release work.

Next: build a [standalone example](examples/standalone-client/README.md) for your supported environment, then choose the output profile for your intended printing condition.
