# Supported platforms

PatterboxPDF generates ordinary PDFs directly and prepares fonts, color profiles
and encoded raster inputs through separate resource workers. Applications stage
the required workers beside their executable. Missing, changed, malformed or
unconfined workers cause preparation to fail.

| Target | Generation and preparation | Confinement |
| --- | --- | --- |
| Windows x86_64 MSVC | Native generation and font/color/raster workers | AppContainer and Windows job limits |
| Ubuntu x86_64 GNU | Native generation and font/color/raster workers | Landlock ABI 3+, seccomp, sealed executable and pidfd ownership |

Current source passed 146 native Windows tests/cases and 67 native Ubuntu tests, including actual confinement and resource preparation. Both workflows were repeated from a freshly extracted source package.
See [verification status](verification-status.md).

Ordinary paths, gradients, blend modes and decoded RGB/Gray/CMYK images need no
workers. Fonts require opaque preparation receipts. Selected-CMYK print output also
checks its exact supplied profile through the color worker. Callers provide
positioned glyphs; paragraph layout and shaping remain caller responsibilities.

The Linux backend pins Ubuntu loader and shared-library objects. It requires
Landlock ABI 3 or later, seccomp, clone3/pidfd and sealed memfd execution.
Unavailable primitives fail before parsing, without an unconfined retry. Native
WSL2 runs exercise the actual Linux kernel and ELF programs; results qualify the
recorded environment. They do not establish every distribution, kernel,
architecture or bare-metal configuration. See the
[Linux boundary](../crates/linux-worker-job/README.md) for its policy.

The optional trusted validator supports Windows. Linux refuses it before launch.
The separate Python output inspector is a development check; the PDF writer
itself requires neither qpdf nor a commercial checker. Enable publisher-adapter
only for the retained saved-publication compatibility path. Default library
dependencies exclude application authoring crates. Existing crate, protocol and
worker names remain compatibility identifiers. Other platforms require their
own preparation and confinement qualification.
