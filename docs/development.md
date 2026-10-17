# Building PatterboxPDF

Use Rust 1.98.0 and Windows x86_64 MSVC build support (Visual Studio Build Tools with the C++ workload and Windows SDK). Install Python 3.11 or later for the source/package auditors. Cargo downloads the pinned dependencies on the first build; add `--offline` only after they are cached.

From the repository root in PowerShell:

```powershell
cargo build --locked --release -p publisher-font-worker -p publisher-color-worker
cargo build --manifest-path examples/standalone-client/Cargo.toml --locked --release --target-dir target/client --examples --bins
python scripts/stage-workers.py --runtime target/release --caller target/client/release/pdf-library-standalone-client.exe
New-Item -ItemType Directory -Path artifacts -Force | Out-Null
target/client/release/pdf-library-standalone-client.exe --output artifacts/first-run
```

Choose a new output directory for each run. The client writes ordinary and prepared-resource print PDFs. The worker executable names are retained compatibility identifiers. See the [standalone guide](../examples/standalone-client/README.md) for the worker-free gallery and its print workflow, and [platform support](supported-platforms.md) for the resource-preparation and kernel requirements.

The main source/test workspace and separate client each have a pinned lockfile. Dependency caches may be reused; source-isolation verification builds the candidate into new targets and stages only workers built from that same source.

Focused package and requirements checks need only Python 3.11 or later:

```powershell
python tests/unit/source-package.test.py
python tests/unit/requirements-inventory.test.py
python scripts/verify-requirements.py
python tests/unit/output-rules.test.py
python scripts/verify-output-rules.py
python tests/unit/accepted-inputs.test.py
```

To run native drawing tests, first build the required workers and test executable, then stage the workers beside it. Resource admission is process-wide, so run the resource tests sequentially:

```powershell
cargo build --locked --release -p publisher-font-worker -p publisher-color-worker -p publisher-raster-worker
cargo test --locked -p print-core --test finished_pages --test finished_gradients --no-run
$testExe = Get-ChildItem target/debug/deps -Filter 'finished_pages-*.exe' | Sort-Object LastWriteTime -Descending | Select-Object -First 1
python scripts/stage-workers.py --runtime target/release --caller $testExe.FullName --worker font --worker color --worker raster --test-layout
cargo test --locked -p print-core --test finished_pages -- --test-threads=1
cargo test --locked -p print-core --test finished_gradients -- --test-threads=1
```

Independent PDF inspection additionally uses `pypdf`, Pillow, `pypdfium2`, numpy, Poppler and qpdf, obtained separately. These are development tools and are excluded from the source archive. The public gallery/API verifiers accept their tool paths explicitly. The font-map/metadata regression script uses `pypdf`; set `PDF_LIBRARY_COMPOSITING_ARTIFACTS` to an existing fresh directory when running `finished_pages`, then pass that directory to `tests/unit/font-map-metadata.test.py`.

WSL users with a configured Cargo/xwin toolchain can cross-compile the same Windows target with `scripts/cargo-xwin-wsl.cmd build --target x86_64-pc-windows-msvc`, adding the build arguments above. Use the resulting `target/x86_64-pc-windows-msvc/release` worker directory and corresponding target subdirectory for the client. This wrapper derives the source root from the current checkout; WSL compilation does not establish Linux runtime support.

The public source contains the supported client, native fixtures and focused auditors. Retained sanitized receipts record historical exact-file verdicts; new PDFs require their own analysis. Callas is optional development verification, outside the runtime and source package.


## Native Ubuntu

Use the pinned Rust toolchain, a C build toolchain, pkg-config and the native
development libraries required by the locked dependencies. The current backend
qualifies an x86_64 Ubuntu environment with the primitives described in
[platform support](supported-platforms.md). Current evidence includes real ELF execution from a fresh extracted-source build.

Run from the repository root:

```sh
cargo build --locked --release --target x86_64-unknown-linux-gnu -p publisher-font-worker -p publisher-color-worker -p publisher-raster-worker
cargo build --manifest-path examples/standalone-client/Cargo.toml --locked --release --target x86_64-unknown-linux-gnu --target-dir target/client --examples --bins
python3 scripts/stage-workers.py --runtime target/x86_64-unknown-linux-gnu/release --caller target/client/x86_64-unknown-linux-gnu/release/pdf-library-standalone-client
mkdir -p artifacts
target/client/x86_64-unknown-linux-gnu/release/pdf-library-standalone-client --output artifacts/ubuntu-first-run
```

The output directory must be new. Staging recognizes the caller's ELF identity.
The trusted validator remains explicitly unsupported on Linux.

The native qualification runner rebuilds matching workers and test programs. Its required suites cover confinement, resource boundaries, worker binary/process tests, ICC structures, print metadata and numeric emission. It lists and executes every case in each selected suite, rejects missing required names and nonzero ignored/filtered/failed counts, exercises the separate caller and binds shared-library/kernel/tool identities:

```sh
python3 scripts/verify-linux-runtime.py --qpdf /path/to/qpdf --target-dir target/linux-qualification --output artifacts/linux-proof
```

Its output folder must be new. Python needs pypdf; the broader font/color/output
inspection uses fontTools, Pillow, freetype-py, pypdfium2, numpy, qpdf, Poppler and
LittleCMS as documented by each verifier. These are development dependencies.
Libraries and resource workers do not acquire or invoke a commercial checker.

## Current generation verification

Rebuild and restage workers together after upgrading. Font preparation uses protocol 9; a sidecar from the earlier protocol is incompatible. The staging manifest binds actual executables, while preparation receipts bind the exact admitted bytes and policy. Python inspectors are development tools and are not called by generation.

The public examples reproduce supported input families through the same caller
boundary. After the Ubuntu builds above, prepare and inspect a retained CFF control:

```sh
python3 scripts/stage-workers.py --runtime target/x86_64-unknown-linux-gnu/release --caller target/client/x86_64-unknown-linux-gnu/release/examples/accepted-font-input
target/client/x86_64-unknown-linux-gnu/release/examples/accepted-font-input apps/font-worker/src/tests/fixtures/default.otf 1 H artifacts/checked-font used --profile crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc
python3 scripts/verify-pdfx-supported-output.py artifacts/checked-font/print.pdf --mode pdfx4 --qpdf /path/to/qpdf --output artifacts/checked-font/outer.json
python3 scripts/verify-font-metrics.py --pdf artifacts/checked-font/print.pdf --source-font apps/font-worker/src/tests/fixtures/default.otf --preparation-report artifacts/checked-font/prepared.json --output artifacts/checked-font/metrics.json
python3 scripts/verify-icc-profile.py --profile crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc --output artifacts/checked-font/profile.json
```

Use new output directories for subsequent runs. The [standalone guide](../examples/standalone-client/README.md)
documents selected faces, logical text and separately authored font facts. Native
font-control generators and their process probes live beside the checked fixtures
under `apps/font-worker/src/tests`. The composition example has a separate
`verify-composition-corpus.py` inspector for authored path, image, gradient and
metadata measurements. Each inspector reports the exact input and evaluator hashes;
the neutral catalog audit checks the saved evidence bindings:

```sh
python3 scripts/verify-accepted-inputs.py
python3 scripts/verify-output-rules.py
```

The source archive includes the independent ICC/numeric/family helpers, tiny font controls, their generator/provenance and applicable asset notices. Standalone generation does not need those development tools. Final qualification must be repeated from the extracted source rather than relying on a previously built worker.
