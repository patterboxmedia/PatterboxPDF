"""Exercise the separate finished-page client and retained compatibility callers."""
if not __debug__:
    raise SystemExit('Run this verifier without -O; its validation assertions must remain enabled.')

from pathlib import Path
import argparse
from datetime import datetime, timezone
import hashlib
import json
import os
import shutil
import subprocess
from pypdf import PdfReader
from pypdf.generic import ContentStream

ROOT = Path(__file__).resolve().parents[1]
CMYK = bytes([0,255,255,0, 255,0,255,0, 255,255,0,0, 0,0,0,255, 0,0,0,0, 0,128,255,32])
ALPHA = bytes([255,160,80,32,128,255])


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def execute(command, evidence, name, expect_success=True):
    result = subprocess.run([str(value) for value in command], cwd=ROOT,
                            capture_output=True, text=True, timeout=180,
                            creationflags=getattr(subprocess, "CREATE_NO_WINDOW", 0))
    (evidence / f"{name}.stdout.log").write_text(result.stdout, encoding="utf-8")
    (evidence / f"{name}.stderr.log").write_text(result.stderr, encoding="utf-8")
    if (result.returncode == 0) != expect_success:
        raise AssertionError(f"{name} exit {result.returncode}: {result.stderr}")
    return result


def object_tree(resources, seen=None):
    seen = set() if seen is None else seen
    for reference in resources.get("/XObject", {}).values():
        obj = reference.get_object()
        key = id(obj)
        if key in seen:
            continue
        seen.add(key)
        yield obj
        if obj.get("/Subtype") == "/Form":
            yield from object_tree(obj.get("/Resources", {}), seen)


def actual_text_segments(reader, page):
    """Follow painted Forms in order, preserving the exact authored clusters."""
    segments = []

    def visit(stream, resources, active):
        for operands, operator in ContentStream(stream, reader).operations:
            if operator == b"BDC" and hasattr(operands[1], "get"):
                text = operands[1].get("/ActualText")
                if text is not None:
                    segments.append(str(text))
            elif operator == b"Do":
                obj = resources["/XObject"][operands[0]].get_object()
                if obj.get("/Subtype") == "/Form":
                    assert id(obj) not in active, "recursive Form resources"
                    visit(obj, obj.get("/Resources", resources), active | {id(obj)})

    visit(page.get_contents(), page["/Resources"], set())
    return segments


def standalone_pdf(path, candidate):
    reader = PdfReader(path)
    assert len(reader.pages) == 2
    assert reader.metadata.title == "Standalone finished pages"
    expected = [([-10,-10,310,290],[-5,-5,305,285],[0,0,300,280]),
                ([0,0,420,240],[8,8,412,232],[12,12,408,228])]
    for page, boxes in zip(reader.pages, expected):
        assert tuple(list(value) for value in (page.mediabox, page.bleedbox, page.trimbox)) == boxes
    text = reader.pages[0].extract_text()
    assert "Standalone TrueType ffi" in text and "Standalone CFF 中文" in text
    fonts = reader.pages[0]["/Resources"]["/Font"]
    subtypes = {font.get_object()["/DescendantFonts"][0].get_object()["/Subtype"] for font in fonts.values()}
    assert subtypes == {"/CIDFontType0", "/CIDFontType2"}
    objects = list(object_tree(reader.pages[0]["/Resources"]))
    images = [obj for obj in objects if obj.get("/Subtype") == "/Image" and obj.get("/ColorSpace") == "/DeviceCMYK"]
    assert images and all(obj.get_data() == CMYK for obj in images)
    assert all(obj["/SMask"].get_object().get_data() == ALPHA for obj in images)
    assert any(bool(getattr(obj.get("/Group", {}).get("/I"), "value", False))
               for obj in objects if obj.get("/Subtype") == "/Form")
    # Ordered curves, clipping and ActualText remain in actual streams, including Forms.
    streams = b"\n".join([page.get_contents().get_data() for page in reader.pages]
                         + [obj.get_data() for obj in objects if obj.get("/Subtype") == "/Form"])
    assert b"20 70 80 -20 110 35 c" in streams
    assert b"W\nn" in streams and b"/ActualText" in streams
    catalog = reader.trailer["/Root"]
    claims = b"GTS_PDFXVersion" in path.read_bytes()
    assert claims == candidate and ("/OutputIntents" in catalog) == candidate
    return {"file": path.name, "sha256": digest(path), "pages": 2,
            "native_text": True, "exact_cmyk_alpha": True, "geometry": True,
            "pdfx_candidate": candidate}


def dependency_graph(path):
    metadata = json.loads(path.read_text(encoding="utf-8"))
    packages = {package["id"]: package for package in metadata["packages"]}
    nodes = {node["id"]: node for node in metadata["resolve"]["nodes"]}
    root = metadata["resolve"]["root"]
    assert packages[root]["name"] == "pdf-library-standalone-client"
    pending, visited = [root], set()
    while pending:
        current = pending.pop()
        if current in visited:
            continue
        visited.add(current)
        for dep in nodes[current]["deps"]:
            if any(kind["kind"] != "dev" for kind in dep["dep_kinds"]):
                pending.append(dep["pkg"])
    names = sorted({packages[node]["name"] for node in visited})
    assert not {"document-core", "layout-core", "application-core"}.intersection(names)
    assert "publisher-adapter" not in nodes[next(node for node in visited if packages[node]["name"] == "print-core")]["features"]
    return {"no_authoring_dependencies": True, "package_count": len(visited), "packages": names}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--client", type=Path, required=True)
    parser.add_argument("--cargo-metadata", type=Path, required=True,
                        help="cargo metadata for the standalone manifest with default features and Windows target")
    parser.add_argument("--adapter-cli", type=Path)
    parser.add_argument("--matrix", type=Path, default=ROOT / "artifacts/pdfx-callas-validation-20261002/matrix-v5-profile-contract-extension-001")
    parser.add_argument("--profile", type=Path, default=ROOT / "crates/print-core/tests/fixtures/SWOP2006coated_300_mth.icc")
    parser.add_argument("--output", type=Path)
    parser.add_argument("--qpdf", type=Path, help="optional trusted development checker; never staged into the caller")
    args = parser.parse_args()
    client = args.client.resolve(strict=True)
    evidence = args.output or ROOT / "artifacts" / ("public-api-" + datetime.now(timezone.utc).strftime("%Y%m%dT%H%M%S%f"))
    evidence.mkdir()
    census = dependency_graph(args.cargo_metadata)
    runs = []
    for repeat in (1, 2):
        output = evidence / f"standalone-{repeat}"
        execute([client, "--output", output, "--profile", args.profile], evidence, f"standalone-{repeat}")
        runs.append([standalone_pdf(output / "ordinary.pdf", False), standalone_pdf(output / "candidate.pdf", True)])
    assert [row["sha256"] for row in runs[0]] == [row["sha256"] for row in runs[1]]
    failures = []
    # Disposable copies prove actual packaged-release behavior without removing
    # any live worker. Rejected runs must expose no output directory or PDF bytes.
    for kind in ("absent-font", "malformed-font", "absent-color"):
        directory = evidence / kind
        directory.mkdir()
        caller = directory / client.name
        shutil.copyfile(client, caller)
        if kind == "malformed-font":
            (directory / "publisher-font-worker.exe").write_bytes(b"invalid executable")
        if kind == "absent-color":
            shutil.copyfile(client.parent / "publisher-font-worker.exe", directory / "publisher-font-worker.exe")
        output = directory / "output"
        result = execute([caller, "--output", output, "--profile", args.profile], evidence, kind, False)
        assert not output.exists(), "failure exposed an output bundle"
        failures.append({"case": kind, "exit_code": result.returncode, "no_output_exposed": True})
    adapter = []
    if args.adapter_cli:
        matrix = json.loads((args.matrix / "matrix.json").read_text(encoding="utf-8"))
        for case_id in ("truetype-multipage", "cff-exact-text", "artwork-transparency"):
            case = next(row for row in matrix["cases"] if row["id"] == case_id)
            identities = []
            for repeat in (1, 2):
                output = evidence / f"adapter-{case_id}-{repeat}"
                execute([args.adapter_cli, "--input", args.matrix / case["origin"]["source_path"],
                         "--output", output, "--profile", args.profile, "--mode", "pdfx4-candidate",
                         "--metadata-utc", case["origin"]["metadata_utc"], "--allow-warnings"],
                        evidence, f"adapter-{case_id}-{repeat}")
                pdf = output / "publication.pdf"
                receipt = json.loads((output / "receipt.json").read_text(encoding="utf-8"))
                assert receipt["pdf"]["sha256"] == digest(pdf)
                current, retained = PdfReader(pdf), PdfReader(args.matrix / case["pdf"]["path"])
                assert len(current.pages) == len(retained.pages)
                for a, b in zip(current.pages, retained.pages):
                    # Extractors insert different separators at clipped cell
                    # Forms. Exact authored clusters still constrain text and
                    # ordering, including its real spaces and combining text.
                    assert actual_text_segments(current, a) == actual_text_segments(retained, b)
                    assert " ".join(a.extract_text().split()) == " ".join(b.extract_text().split())
                    for key in ("/MediaBox", "/BleedBox", "/TrimBox"):
                        assert list(a[key]) == list(b[key])
                identities.append(digest(pdf))
            assert identities[0] == identities[1]
            adapter.append({"case": case_id, "sha256": identities[0],
                            "ordered_actual_text_matches_retained": True,
                            "extracted_text_matches_after_separator_normalization": True,
                            "page_boxes_match_retained": True})
    if args.qpdf:
        for path in evidence.rglob("*.pdf"):
            execute([args.qpdf, "--check", path], evidence, path.parent.name + "-" + path.stem + "-qpdf")
    result = {"schema": "pdf-library-public-api/v1", "dependency_graph": census,
              "standalone": runs, "repeatable_bytes": True, "worker_failures": failures,
              "adapter": adapter, "fresh_independent_pdfx4_check": False,
              "caller": {"name": client.name, "sha256": digest(client)}}
    (evidence / "results.json").write_text(json.dumps(result, indent=2) + "\n", encoding="utf-8")
    print(json.dumps({"evidence_directory": evidence.name, "standalone_hashes": [row["sha256"] for row in runs[0]],
                      "dependency_packages": census["package_count"], "worker_failures": len(failures),
                      "adapter_cases": len(adapter), "independent_pdfx4_check": False}, indent=2))


if __name__ == "__main__":
    main()
