# Library architecture

The product is a general-purpose PDF library. Its current core is a finished-page generator with supporting resource workers. Public capabilities and architecture serve unrelated callers; the imported application adapter is optional compatibility support.

The Cargo workspace has no path dependency outside this project. It contains the full dependency closure of the PDF command and color, font, raster and validator workers, including their regression-test dependencies.

| Location | Responsibility |
| --- | --- |
| `crates/print-core` | PDF object writing, resources, page content, metadata and supported PDF/X-4 generation |
| `apps/pdf-cli` | Current saved-publication input adapter and command; package/binary is still named `publisher-pdf` |
| `crates/print-core/src/finished*` | Public finished-page inputs, validation, traversal and serialization |
| `crates/print-core/src/publisher_adapter.rs` | Opt-in lowering of saved-publication scenes into finished-page inputs |
| `crates/document-core`, `crates/layout-core` | Saved-publication input model and scene production, used only by compatibility consumers |
| `crates/application-core` | Compatibility support for validator regression tests; not the intended public PDF API |
| `crates/*-boundary`, `crates/raster-capacity` | Worker protocols, resource limits and lifecycle checks |
| `apps/*-worker` | Isolated color, font, raster and PDF profile processing |
| `crates/windows-worker-job`, `crates/windows-atomic-file` | Windows process/resource and atomic-file support |
| `crates/linux-worker-job` | Native Ubuntu process/resource confinement for font/color/raster workers |
| `examples/standalone-client` | Separate Cargo workspace consuming the default public interface |
| `scripts`, `tests/unit`, `examples/pdf-candidate` | Development corpus construction, checker review and supporting fixtures |
| `artifacts` | Frozen imported evidence and newly generated local setup outputs; excluded from Git by default |

Keeping the legacy names avoids a protocol and executable-discovery change during the project migration. Names are not evidence of a live source dependency on Publisher.

The public boundary accepts finished pages, resources and export options. It does not open an application document or lay out paragraphs. The default `print-core` dependency graph excludes `document-core`, `layout-core` and `application-core`. Compatibility callers explicitly enable `publisher-adapter`; their scene serialization lowers into the same finished-page writer.

Prepared exact font programs and compressed raster planes can share ownership between the compatibility view and finished resources. Raster memory admission remains alive until every owning view is dropped. Traversal shares the low-level font and image emitters, rather than retaining a second PDF serializer. Coordinates are converted from compatibility micrometres/Y-down geometry into PDF points/Y-up at that adapter boundary.

The public PDF/X entry rejects unmanaged RGB. A private compatibility diagnostic entry preserves the old preflight probe that can deliberately emit an unmanaged candidate for a blocking finding. Managed production output uses the public fail-closed entry. The exception is inaccessible to external callers.

Callas is an independent development checker. It is outside generation, the runtime and the source package. qpdf is a pinned tool for optional inspection/validation paths; the writer itself does not require it. The optional `TrustedValidatorBoundary` implements the imported compatibility profile, with its narrower content assumptions. That profile can reject a supported native structure outside its scope; it is not a universal validator for this library's generation features. New output receives focused independent parsing and exact-byte development analysis. The source archive includes no native runtime binaries. Cargo dependencies, workers and optional inspection tools are built or acquired separately.

The finished-page boundary generates ordinary RGB/Gray/CMYK content and bounded CMYK print output. Native axial/radial gradients use component functions and matching gray opacity shadings. Items select defined PDF blend modes; each page or isolated group selects its own composition space from painted content. Shared objects are allocated deterministically, while page/Form resource dictionaries contain their direct stream usage.

Font/profile preparation has retained Windows evidence and an implemented native Ubuntu backend awaiting the current candidate replay. Linux refuses the optional trusted validator before launching it. Reading/editing, paragraph layout and additional color/gradient families have separate future scope. See `../ROADMAP.md` for completion evidence and deferred decisions. Application UI, integration and document formats do not determine that queue.
