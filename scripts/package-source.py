#!/usr/bin/env python3
"""Archive the reviewed source tree, never local tools, inputs or build output."""
import argparse
import hashlib
import json
import pathlib
import re
import zipfile


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", type=pathlib.Path, default=pathlib.Path("dist"))
    args = parser.parse_args()
    root = pathlib.Path(__file__).resolve().parent.parent
    version = re.search(r'^version\s*=\s*"([^"]+)"',
                        (root / "Cargo.toml").read_text(encoding="utf-8"), re.M).group(1)
    directories = (".github", "assets", "crates", "docs", "packaging", "scripts")
    root_files = ("Cargo.toml", "Cargo.lock", "rust-toolchain.toml", "README.md",
                  "CONTRIBUTING.md", "CHANGELOG.md", "LICENSE",
                  "THIRD_PARTY_NOTICES.md", "CONTEXT.md", ".gitignore",
                  ".gitattributes", "requirements-dev.txt")
    excluded_directories = {".git", ".tools", "target", "artifacts", "dist",
                            "__pycache__", ".venv", ".cache", "cache", "temp", "tmp"}
    excluded_suffixes = (".full.otf", ".full.ttf", ".pyc", ".pyo", ".tmp", ".log")
    files = [root / name for name in root_files]
    for directory in directories:
        for path in (root / directory).rglob("*"):
            relative = path.relative_to(root)
            if (excluded_directories.intersection(relative.parts)
                    or path.name.lower().endswith(excluded_suffixes)):
                continue
            if path.is_symlink() or not path.resolve().is_relative_to(root):
                raise SystemExit(f"Refusing source symlink or external path: {relative}")
            if path.is_file():
                files.append(path)
    missing = [str(path.relative_to(root)) for path in files if not path.is_file()]
    if missing:
        raise SystemExit(f"Required source files missing: {missing}")
    output = args.output.resolve()
    output.mkdir(parents=True, exist_ok=True)
    base = f"local-image-tools-{version}-source"
    archive = output / f"{base}.zip"
    manifest = output / f"{base}-files.json"
    checksums = output / f"{base}-SHA256SUMS.txt"
    for path in (archive, manifest, checksums):
        if path.exists():
            raise SystemExit(f"Package already exists: {path}")
    records = []
    with zipfile.ZipFile(archive, "x", compression=zipfile.ZIP_DEFLATED, compresslevel=9) as bundle:
        for path in sorted(set(files)):
            relative = path.relative_to(root).as_posix()
            data = path.read_bytes()
            bundle.writestr(f"{base}/{relative}", data)
            records.append({"path": relative, "bytes": len(data),
                            "sha256": hashlib.sha256(data).hexdigest()})
    manifest.write_text(json.dumps(records, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    checksums.write_text("".join(f"{hashlib.sha256(path.read_bytes()).hexdigest()}  {path.name}\n"
                                  for path in (archive, manifest)), encoding="utf-8")
    print(f"Archived {len(records)} source files: {archive}")
    print(manifest)
    print(checksums)


if __name__ == "__main__":
    main()
