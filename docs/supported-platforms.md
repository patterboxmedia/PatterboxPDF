# Supported platforms

This records the execution platform and resource workflows verified for the initial generation release.

The first supported generation and resource-preparation environment is Windows x86_64 (`x86_64-pc-windows-msvc`), using Rust 1.98.0. Font and color preparation use separate executable workers. Release applications must place the required workers beside their own executable. The boundaries pin the executable identity before processing resources and fail when workers are missing, changed, malformed or unable to satisfy confinement.

Ordinary vector drawing, gradients, blend modes and decoded RGB, gray and CMYK image writing do not launch workers. This path uses portable dependencies; current native execution evidence covers Windows. Other platforms have not received equivalent runtime qualification.

Font resources require opaque preparation receipts from the font boundary. Bounded PDF/X output also inspects the supplied output profile through the color boundary. These preparation paths require the Windows sidecars. Preparation accepts exact caller-provided bytes and does not perform paragraph layout or text shaping. Callers supply decoded straight color and alpha planes for direct image resources; encoded image ingestion can use their own decoder or the existing optional confined raster path.

The opt-in `publisher-adapter` feature retains the saved-publication compatibility path and its raster/layout dependencies. The default finished-page API excludes the authoring crates from its normal dependency graph. Legacy worker and executable names remain part of discovery and protocol compatibility.

Other targets may compile portions of the library. Their resource preparation and process confinement have not been established as equivalent production support. Linux or WSL compilation is a development technique; Windows execution supplies the runtime evidence for this release.

Callas is an optional development verification tool. The library and source package do not require or include it. The optional trusted validator uses a separately acquired pinned qpdf runtime on Windows; ordinary finished-page writing does not require qpdf. A binary distribution that includes third-party runtimes needs its own redistribution review.
