"""Stage only explicit PDF Library sidecars beside a caller, with exact hashes."""
from pathlib import Path
import argparse
import hashlib
import json
import shutil

WORKERS = ("font", "color", "raster", "validator")


def target_platform(caller, target=None):
    """Choose the caller's target, independently of the machine staging it."""
    if target:
        if "windows" in target:
            return "windows"
        if "linux" in target:
            return "linux"
        raise ValueError("workers require a Windows or Linux caller target")
    if caller.suffix.lower() == ".exe":
        return "windows"
    with caller.open("rb") as source:
        if source.read(4) == b"\x7fELF":
            return "linux"
    raise ValueError("cannot infer caller target; provide --target")


def digest(path):
    return hashlib.sha256(path.read_bytes()).hexdigest()


def stage(runtime, caller, workers, test_layout=False, target=None):
    runtime = runtime.resolve(strict=True)
    caller = caller.resolve(strict=True)
    if not runtime.is_dir() or not caller.is_file():
        raise ValueError("runtime must be a directory and caller must be an existing executable")
    platform = target_platform(caller, target)
    suffix = ".exe" if platform == "windows" else ""
    if any(name not in WORKERS for name in workers):
        raise ValueError("unknown worker")
    sources = [(runtime / f"publisher-{name}-worker{suffix}").resolve(strict=True) for name in workers]
    if any(not source.is_file() for source in sources):
        raise ValueError("every requested sidecar must be an existing regular file")
    receipt = []
    directories = [caller.parent]
    if test_layout:
        if caller.parent.name != "deps":
            raise ValueError("--test-layout requires a Cargo test executable inside deps")
        # The debug font boundary deliberately uses deps/../; color and raster
        # tests still resolve adjacent workers. Stage the explicit set in both.
        directories.append(caller.parent.parent)
    for directory in directories:
        for source in sources:
            target = directory / source.name
            expected = digest(source)
            if source != target.resolve() and (not target.exists() or digest(target) != expected):
                if platform == "linux" and target.exists():
                    target.chmod(0o700)
                shutil.copyfile(source, target)
            actual = digest(target)
            if actual != expected:
                raise ValueError(f"staged sidecar hash mismatch: {source.name}")
            if platform == "linux":
                target.chmod(0o500)
            receipt.append({"name": source.name, "sha256": actual, "bytes": target.stat().st_size,
                            "location": "adjacent" if directory == caller.parent else "debug-parent"})
    return {"schema": "pdf-library-worker-stage/v1", "caller": caller.name, "target_platform": platform,
            "caller_sha256": digest(caller), "workers": receipt}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--runtime", type=Path, required=True)
    parser.add_argument("--caller", type=Path, required=True)
    parser.add_argument("--worker", choices=tuple(WORKERS), action="append")
    parser.add_argument("--test-layout", action="store_true", help="also stage the explicit sidecars in the Cargo debug parent")
    parser.add_argument("--target", help="caller Rust target triple; required when executable identity is ambiguous")
    args = parser.parse_args()
    receipt = stage(args.runtime, args.caller, sorted(set(args.worker or ["font", "color"])), args.test_layout, args.target)
    path = args.caller.with_suffix(".workers.json")
    path.write_text(json.dumps(receipt, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(receipt, indent=2))


if __name__ == "__main__":
    main()
