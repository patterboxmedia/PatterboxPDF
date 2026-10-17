"""Collect local Cargo license evidence for the writer, workers or source workspace.

Run on the same filesystem as Cargo metadata (WSL metadata has WSL paths).
No downloads, license inference, or changes to source packages are performed.
"""

import argparse
import hashlib
import json
import os
import re
import shutil
from pathlib import Path


def digest(data):
    return hashlib.sha256(data).hexdigest()


SUPPORTED_TARGETS = ("x86_64-pc-windows-msvc", "x86_64-unknown-linux-gnu")


def metadata_union(inputs, *, all_workspace, include_dev):
    """Walk each platform graph before merging; one platform cannot hide another."""
    packages, nodes, closures, receipts = {}, {}, {}, []
    roots = set()
    package_targets = {}
    required = {"publisher-pdf", "publisher-color-worker", "publisher-font-worker",
                "publisher-raster-worker", "publisher-validator-worker"}
    for target, filename, additional in inputs:
        if target not in SUPPORTED_TARGETS:
            raise ValueError('unsupported notice target: '+target)
        data = filename.read_bytes()
        metadata = json.loads(data)
        current_packages = {row['id']: row for row in metadata['packages']}
        current_nodes = {row['id']: row for row in metadata['resolve']['nodes']}
        root_ids = (metadata['workspace_members'] if all_workspace or additional else
                    [key for key, row in current_packages.items() if row['name'] in required])
        if not additional and not all_workspace and {current_packages[key]['name'] for key in root_ids} != required:
            raise ValueError('Cargo metadata is missing a required command or worker')
        roots.update(current_packages[key]['name'] for key in root_ids)
        reachable, pending = set(), list(root_ids)
        while pending:
            key = pending.pop()
            if key in reachable:
                continue
            reachable.add(key)
            node = current_nodes[key]
            for edge in node['deps']:
                if include_dev or any(kind['kind'] != 'dev' for kind in edge['dep_kinds']):
                    pending.append(edge['pkg'])
        closures.setdefault(target, set()).update(reachable)
        receipts.append({'target': target, 'sha256': digest(data), 'additional_workspace': additional,
                         'roots': sorted(current_packages[key]['name'] for key in root_ids)})
        for key in reachable:
            row = current_packages[key]
            if key in packages:
                for field in ('name', 'version', 'source', 'license', 'license_file'):
                    if packages[key].get(field) != row.get(field):
                        raise ValueError('platform metadata package identity disagreement')
            else:
                packages[key] = row
                nodes[key] = {'features': []}
            nodes[key]['features'] = sorted(set(nodes[key]['features']) | set(current_nodes[key]['features']))
            package_targets.setdefault(key, set()).add(target)
    if set(closures) != set(SUPPORTED_TARGETS):
        raise ValueError('source notice collection requires Windows and Linux metadata')
    return packages, nodes, sorted(roots), closures, receipts, package_targets


def collect(metadata_file, destination, rust_sysroot, *, all_workspace=False, include_dev=False,
            additional_metadata=(), target=SUPPORTED_TARGETS[0], target_metadata=(), additional_target_metadata=()):
    inputs = [(target, metadata_file, False), *((target, path, True) for path in additional_metadata),
              *((name, path, False) for name, path in target_metadata),
              *((name, path, True) for name, path in additional_target_metadata)]
    packages, nodes, roots, closures, metadata_receipts, package_targets = metadata_union(
        inputs, all_workspace=all_workspace, include_dev=include_dev)
    destination.mkdir()  # Preserve earlier license collections.
    root = Path(__file__).resolve().parent.parent
    canonical = destination / "canonical"
    shutil.copytree(root / "third_party/cargo-notices/canonical", canonical)
    shutil.copyfile(root / "Cargo.lock", destination / "Cargo.lock")
    shutil.copyfile(root / "examples/standalone-client/Cargo.lock", destination / "client-Cargo.lock")
    rust_docs = rust_sysroot / "share/doc/rust"
    shutil.copytree(rust_docs / "licenses", destination / "rust-standard-library/licenses")
    shutil.copyfile(rust_docs / "COPYRIGHT-library.html", destination / "rust-standard-library/COPYRIGHT-library.html")
    records = []
    for key in sorted(packages, key=lambda item: (packages[item]["name"], packages[item]["version"])):
        package = packages[key]
        base = Path(package["manifest_path"]).parent
        record = {field: package.get(field) for field in
                  ("name", "version", "license", "source", "repository", "links", "authors")}
        record["cargo_manifest_sha256"] = digest(Path(package["manifest_path"]).read_bytes())
        record["enabled_features"] = nodes[key]["features"]
        record["targets"] = sorted(package_targets[key])
        record["license_texts"] = []
        if package["source"] is None:
            record["scope"] = "project-owned; MIT OR Apache-2.0"
        else:
            record["scope"] = ("Cargo source/test dependency; conservative feature union" if include_dev
                               else "Cargo normal/build dependency; conservative feature union")
            if not package.get("license") and not package.get("license_file"):
                raise ValueError(f"Missing license declaration: {package['name']}")
            candidates = []
            if package.get("license_file"):
                declared = Path(package["license_file"])
                candidates.append(declared if declared.is_absolute() else base / declared)
            # Also retain embedded C-library notices, not just the Rust wrapper.
            for directory, children, names in os.walk(base):
                directory = Path(directory)
                if len(directory.relative_to(base).parts) >= 4:
                    children.clear()  # Files at depth five are the existing notice boundary.
                for name in names:
                    if re.match(r"^(licen[sc]e|copying|notice|copyright)([._-].*)?$", name, re.I):
                        path = directory / name
                        if path.is_file():
                            candidates.append(path)
            for path in sorted(set(candidates)):
                if not path.is_file():
                    raise ValueError(f"Declared license file missing: {path}")
                relative = path.relative_to(base)
                output = Path("texts") / f"{package['name']}-{package['version']}" / relative
                target = destination / output
                target.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(path, target)
                record["license_texts"].append({"path": output.as_posix(),
                                               "sha256": digest(target.read_bytes())})
            # Some registry archives omit notices. Exact upstream revisions are
            # separately retained and pinned before this offline collection.
            upstream = root / "third_party/upstream" / f"{package['name']}-{package['version']}"
            if upstream.is_dir():
                receipts = json.loads((root / "third_party/upstream/receipt.json").read_text(encoding="utf-8"))
                notices = [row for row in receipts["notices"]
                           if row["package"] == f"{package['name']}-{package['version']}"]
                for notice in notices:
                    path = root / notice["path"]
                    if path.parent != upstream or digest(path.read_bytes()) != notice["sha256"]:
                        raise ValueError("upstream notice identity or path drift")
                    output = Path("texts") / f"{package['name']}-{package['version']}" / path.name
                    target = destination / output
                    target.parent.mkdir(parents=True, exist_ok=True)
                    if target.exists() and target.read_bytes() != path.read_bytes():
                        raise ValueError("upstream and registry notice disagreement")
                    shutil.copyfile(path, target)
                    record["license_texts"].append({"path": output.as_posix(), "sha256": notice["sha256"],
                                                   "upstream_url": notice["url"],
                                                   "upstream_revision": notice["source_revision"]})
            if not record["license_texts"]:
                record["notice_gap"] = "Published archive omits standalone license/copyright notices. Retained declaration and canonical terms are a license record; obtain upstream notices before public redistribution."
                record["declaration_evidence"] = []
                for path in [base / "Cargo.toml", *base.glob("README*")]:
                    output = Path("declarations") / f"{package['name']}-{package['version']}" / path.name
                    target = destination / output
                    target.parent.mkdir(parents=True, exist_ok=True)
                    shutil.copyfile(path, target)
                    record["declaration_evidence"].append({"path": output.as_posix(), "sha256": digest(target.read_bytes())})
                record["canonical_license_terms"] = [
                    f"canonical/{term}.txt" for term in ("MIT", "BSD-3-Clause", "Apache-2.0")
                    if term in (package.get("license") or "")
                ]
        records.append(record)
    assets = []
    asset_inputs = []
    fonts = root / "crates/layout-core/tests/fixtures"
    for path in sorted([*fonts.glob("*.ttf"), *fonts.glob("*.otf")]):
        if path.name.startswith("DejaVu"):
            notice = "LICENSE-DejaVu"
        elif path.name.startswith("NotoSansCJK"):
            notice = "LICENSE-NotoSansCJK"
        elif path.name.startswith("NotoSansDevanagari"):
            notice = "LICENSE-NotoSansDevanagari"
        elif path.name.startswith("NotoSansThai"):
            notice = "LICENSE-NotoSansThai"
        elif path.name.startswith(("NotoEmoji", "NotoColorEmoji", "Noto-COLR")):
            notice = "LICENSE-NotoEmoji"
        else:
            raise ValueError(f"Uninventoried fixture font: {path.name}")
        asset_inputs.append((path.name, "crates/layout-core/tests/fixtures", notice))
    asset_inputs.append(("SWOP2006coated_300_mth.icc", "crates/print-core/tests/fixtures", "LICENSE-CC0-1.0.txt"))
    for name, directory, license_name in asset_inputs:
        path = root / directory / name
        license_path = root / directory / license_name
        target = destination / "assets" / license_name
        target.parent.mkdir(exist_ok=True)
        shutil.copyfile(license_path, target)
        readme = destination / "assets" / ("fonts-provenance.md" if name.endswith((".ttf", ".otf")) else "profile-provenance.md")
        shutil.copyfile(root / directory / "README.md", readme)
        assets.append({"path": f"{directory}/{name}", "bytes": path.stat().st_size,
                       "sha256": digest(path.read_bytes()), "license_notice": f"assets/{license_name}",
                       "license_notice_sha256": digest(target.read_bytes()),
                       "provenance": f"assets/{readme.name}", "provenance_sha256": digest(readme.read_bytes())})
    # Original synthetic test pixels and generated profiles carry the owner's
    # MIT OR Apache-2.0 terms. They are inventoried separately from upstream art.
    generated = root / "crates/print-core/tests/fixtures"
    for name in ("generated-srgb.icc", "embedded-rgb-alpha-3x2.tif", "oriented-alpha-3x2.tif"):
        path = generated / name
        assets.append({"path": path.relative_to(root).as_posix(), "bytes": path.stat().st_size,
                       "sha256": digest(path.read_bytes()), "scope": "project-generated fixture; MIT OR Apache-2.0",
                       "provenance": "assets/profile-provenance.md",
                       "provenance_sha256": digest((destination / "assets/profile-provenance.md").read_bytes())})
    output = {
        "schema": "patterboxpdf-licenses/v2",
        "roots": roots,
        "supported_targets": list(SUPPORTED_TARGETS),
        "metadata_inputs": metadata_receipts,
        "target_closures": [{'target': name,
                             'packages': sorted([packages[key]['name'], packages[key]['version']] for key in keys)}
                            for name, keys in sorted(closures.items())],
        "cargo_lock_sha256": digest((root / "Cargo.lock").read_bytes()),
        "client_cargo_lock_sha256": digest((root / "examples/standalone-client/Cargo.lock").read_bytes()),
        "rust_standard_library": {"toolchain": rust_sysroot.name,
                                  "notice": "rust-standard-library/COPYRIGHT-library.html",
                                  "sha256": digest((rust_docs / "COPYRIGHT-library.html").read_bytes()),
                                  "license_texts": [{'path': path.relative_to(destination).as_posix(),
                                                     'sha256': digest(path.read_bytes())}
                                                    for path in sorted((destination/'rust-standard-library/licenses').rglob('*'))
                                                    if path.is_file()]},
        "includes_dev_dependencies": include_dev,
        "all_workspace_members": all_workspace,
        "scope": ("Union of Windows and Linux Cargo closures from the selected roots, including optional features unified by the workspace and "
                  + ("development dependencies" if include_dev else "normal/build dependencies")
                  + "; may overinclude build-only crates. First-party source is MIT OR Apache-2.0; external terms remain separately recorded."),
        "dependencies": records,
        "example_assets": assets,
        "canonical_terms": [{"path": f"canonical/{path.name}", "sha256": digest(path.read_bytes())}
                            for path in sorted(canonical.iterdir()) if path.is_file()],
    }
    (destination / "dependencies.json").write_text(json.dumps(output, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"packages": len(records), "third_party": sum(row["source"] is not None for row in records),
                      "notice_gaps": [row["name"] for row in records if "notice_gap" in row]}))


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("metadata", type=Path)
    parser.add_argument("output", type=Path)
    parser.add_argument("--rust-sysroot", type=Path, required=True,
                        help="Installed pinned Rust sysroot containing standard-library notices")
    parser.add_argument("--all-workspace", action="store_true", help="Include every first-party workspace root")
    parser.add_argument("--include-dev", action="store_true", help="Include source/test development dependencies")
    parser.add_argument("--additional-metadata", type=Path, action="append", default=[],
                        help="Include roots from a separate example/client workspace")
    parser.add_argument('--target', choices=SUPPORTED_TARGETS, default=SUPPORTED_TARGETS[0],
                        help='Target used for the primary and additional metadata inputs')
    parser.add_argument('--target-metadata', nargs=2, action='append', default=[], metavar=('TARGET', 'FILE'),
                        help='Metadata generated with --filter-platform TARGET for another target')
    parser.add_argument('--additional-target-metadata', nargs=2, action='append', default=[], metavar=('TARGET', 'FILE'),
                        help='Separate example/client workspace metadata for another target')
    arguments = parser.parse_args()
    collect(arguments.metadata, arguments.output, arguments.rust_sysroot,
            all_workspace=arguments.all_workspace, include_dev=arguments.include_dev,
            additional_metadata=arguments.additional_metadata, target=arguments.target,
            target_metadata=[(name, Path(path)) for name, path in arguments.target_metadata],
            additional_target_metadata=[(name, Path(path)) for name, path in arguments.additional_target_metadata])
