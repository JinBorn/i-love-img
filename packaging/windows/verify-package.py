"""Read-only audit of Windows portable archives, source archives and extracted folders."""
import argparse
import hashlib
import json
from pathlib import Path, PurePosixPath
import zipfile


def sha256(data):
    return hashlib.sha256(data).hexdigest()


def archive_files(path, prefix):
    with zipfile.ZipFile(path) as archive:
        files = {}
        for item in archive.infolist():
            assert item.filename.startswith(prefix), item.filename
            if item.is_dir():
                continue
            name = item.filename[len(prefix):]
            relative = PurePosixPath(name)
            assert not relative.is_absolute() and ".." not in relative.parts, name
            assert name and name not in files, f"Duplicate archive file: {name}"
            files[name] = archive.read(item)
        return files


def verify_manifest(files, manifest_path):
    manifest = json.loads(manifest_path.read_text(encoding="utf-8"))
    expected = {item["path"]: item for item in manifest}
    assert len(expected) == len(manifest), "Duplicate manifest entries"
    assert files.keys() == expected.keys(), "Archive and manifest have different file sets"
    for name, data in files.items():
        assert len(data) == expected[name]["bytes"], name
        assert sha256(data) == expected[name]["sha256"], name
    return manifest


def verify_protected_snapshot(root, snapshot_path):
    protected = json.loads(snapshot_path.read_text(encoding="utf-8"))
    for item in protected["protected_files"]:
        data = (root / item["path"]).read_bytes()
        assert len(data) == item["bytes"] and sha256(data) == item["sha256"], item["path"]
    folders = protected.get("protected_application_folders")
    if folders is None:
        folders = [protected["protected_folder"]]
    for relative in folders:
        folder = root / relative
        assert folder.is_dir(), f"Original application folder missing: {relative}"
        prefix = relative + "/"
        actual_files = {p.relative_to(root).as_posix() for p in folder.rglob("*") if p.is_file()}
        expected_files = {item["path"] for item in protected["protected_files"]
                          if item["path"].startswith(prefix)}
        assert actual_files == expected_files, f"Original application folder file set changed: {relative}"
        actual_directories = {p.relative_to(root).as_posix() for p in folder.rglob("*") if p.is_dir()}
        expected_directories = {name for name in protected["protected_directories"]
                                if name.startswith(prefix)}
        assert actual_directories == expected_directories, f"Original directory set changed: {relative}"
    return len(protected["protected_files"])


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--packages", type=Path, required=True)
    parser.add_argument("--version", required=True)
    parser.add_argument("--target", default="x86_64-pc-windows-gnullvm",
                        choices=("x86_64-pc-windows-gnullvm", "x86_64-pc-windows-msvc"))
    parser.add_argument("--extracted", type=Path)
    parser.add_argument("--candidate", type=Path, help="Previously tested candidate verification JSON")
    parser.add_argument("--license-baseline", type=Path, help="Previously reviewed portable ZIP")
    parser.add_argument("--protected-manifest", type=Path, help="Pre-task file and directory snapshot")
    parser.add_argument("--installer-extracted", type=Path)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    root = Path(__file__).resolve().parents[2]
    package_dir = args.packages.resolve()
    suffix = "windows-x64" if args.target.endswith("msvc") else "windows-x64-gnullvm"
    base = f"local-image-tools-{args.version}-{suffix}-portable"
    files = archive_files(package_dir / f"{base}.zip", "Local Image Tools/")
    manifest = verify_manifest(files, package_dir / f"{base}-files.json")
    for name in files:
        assert not set(PurePosixPath(name).parts).intersection(
            {"Data", "Output", ".tools", "target", "artifacts", "__pycache__"}), name
    for name in ("portable.flag", "START-HERE.txt"):
        assert files[name] == (root / "packaging/windows" / name).read_bytes(), name
    assert "dependency-licenses/nsis/COPYING" not in files
    inventory = json.loads(files["dependency-licenses/inventory.json"])
    notice_count = 0
    for item in inventory:
        assert item["files"], item["name"]
        for name in item["files"]:
            assert files[f'dependency-licenses/{item["name"]}-{item["version"]}/{name}']
            notice_count += 1
    if args.license_baseline:
        previous = archive_files(args.license_baseline, "Local Image Tools/")
        notices = lambda entries: {name: data for name, data in entries.items()
                                  if name.startswith("dependency-licenses/") and "/nsis/" not in name}
        assert notices(files) == notices(previous), "Dependency notices differ from reviewed baseline"
    binary_names = ["local-image-tools.exe", "lit.exe"]
    if args.target.endswith("gnullvm"):
        binary_names.append("libunwind.dll")
        runtime = root / ".tools/llvm-mingw-20261006-ucrt-x86_64"
        assert files["libunwind.dll"] == (runtime / "bin/libunwind.dll").read_bytes()
        assert files["dependency-licenses/llvm-runtime/LICENSE.TXT"] == (runtime / "LICENSE.TXT").read_bytes()
    binaries = {name: sha256(files[name]) for name in binary_names}
    for name in binary_names[:2]:
        assert files[name] == (root / "target" / args.target / "release" / name).read_bytes(), name
    if args.candidate:
        candidate = json.loads(args.candidate.read_text(encoding="utf-8"))
        assert candidate["status"] == "passed"
        assert binaries == candidate["binary_sha256"], "Final binaries differ from tested candidate"
    verified_checksums = []
    for checksum in sorted(package_dir.glob(f"local-image-tools-{args.version}-*-SHA256SUMS.txt")):
        for line in checksum.read_text(encoding="utf-8").splitlines():
            expected_hash, name = line.split("  ", 1)
            assert Path(name).name == name, name
            assert sha256((package_dir / name).read_bytes()) == expected_hash, name
            verified_checksums.append(name)
    assert f"{base}.zip" in verified_checksums and f"{base}-files.json" in verified_checksums
    source_count = None
    source_base = f"local-image-tools-{args.version}-source"
    if (package_dir / f"{source_base}.zip").exists():
        source = archive_files(package_dir / f"{source_base}.zip", f"{source_base}/")
        verify_manifest(source, package_dir / f"{source_base}-files.json")
        for name, data in source.items():
            assert not set(PurePosixPath(name).parts).intersection(
                {".tools", "target", "artifacts", "dist", "__pycache__"}), name
            assert data == (root / name).read_bytes(), f"Source differs from workspace: {name}"
        for name, data in files.items():
            if name.startswith("docs/") or name in ("README.md", "LICENSE", "THIRD_PARTY_NOTICES.md"):
                assert data == (root / name).read_bytes(), f"Packaged docs differ from workspace: {name}"
        assert f"{source_base}.zip" in verified_checksums
        assert f"{source_base}-files.json" in verified_checksums
        source_count = len(source)
    extracted_count = None
    if args.extracted:
        folder = args.extracted.resolve()
        extracted = {p.relative_to(folder).as_posix(): p for p in folder.rglob("*") if p.is_file()}
        assert extracted.keys() == files.keys(), "Extracted folder contains missing or additional files"
        for name, path in extracted.items():
            assert path.read_bytes() == files[name], name
        assert not (folder / "Data").exists() and not (folder / "Output").exists()
        extracted_count = len(extracted)
    protected_count = None
    if args.protected_manifest:
        protected_count = verify_protected_snapshot(root, args.protected_manifest)
    installer_count = None
    if args.installer_extracted:
        installer = args.installer_extracted.resolve()
        for name in ("portable.flag", "START-HERE.txt", "Data"):
            assert not (installer / name).exists()
        for name, data in files.items():
            if name not in ("portable.flag", "START-HERE.txt"):
                assert (installer / name).read_bytes() == data, name
        assert (installer / "dependency-licenses/nsis/COPYING").read_bytes() == (root / ".tools/nsis-3.11/COPYING").read_bytes()
        installer_count = sum(p.is_file() for p in installer.rglob("*"))
    result = {
        "status": "passed", "version": args.version, "portable_files": len(files),
        "portable_bytes": sum(item["bytes"] for item in manifest),
        "license_packages": len(inventory), "notice_files": notice_count,
        "license_baseline_matches": bool(args.license_baseline), "binary_sha256": binaries,
        "source_files": source_count, "source_and_docs_match_workspace": source_count is not None,
        "binaries_match_tested_candidate": bool(args.candidate), "extracted_files": extracted_count,
        "extracted_folder_pristine": extracted_count is not None,
        "protected_old_files_unchanged": protected_count, "installer_files_inspected": installer_count,
        "installer_executed": False, "checksum_artifacts_verified": verified_checksums,
        "artifacts": [{"name": path.name, "bytes": path.stat().st_size, "sha256": sha256(path.read_bytes())}
                      for path in sorted(package_dir.glob(f"local-image-tools-{args.version}-*")) if path.is_file()],
    }
    args.output.parent.mkdir(parents=True, exist_ok=True)
    args.output.write_text(json.dumps(result, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    print(json.dumps(result, ensure_ascii=False, indent=2))


if __name__ == "__main__":
    main()
