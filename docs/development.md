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

Choose a new output directory for each run. The client writes ordinary and prepared-resource print PDFs. The worker executable names are retained compatibility identifiers. See the [standalone guide](../examples/standalone-client/README.md) for the worker-free gallery and its print workflow, and [platform support](supported-platforms.md) for the Windows resource-preparation boundary.

The main source/test workspace and separate client each have a pinned lockfile. Dependency caches may be reused; source-isolation verification builds the candidate into new targets and stages only workers built from that same source.

Focused package and requirements checks need only Python 3.11 or later:

```powershell
python tests/unit/source-package.test.py
python tests/unit/requirements-inventory.test.py
python scripts/verify-requirements.py
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

Earlier full-checker matrices and operational helper scripts belong to the private development workspace. The public source contains the supported client, native fixtures and focused auditors. Retained sanitized receipts record historical exact-file verdicts; new PDFs require their own analysis. Callas is optional development verification, outside the runtime and source package.
