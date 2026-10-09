#!/usr/bin/env python3
"""Collect notices from the exact Cargo graph used by a package build."""

import argparse
import json
import pathlib
import shutil
import subprocess


def main():
    parser = argparse.ArgumentParser()
    parser.add_argument("destination", type=pathlib.Path)
    parser.add_argument("--target", required=True)
    args = parser.parse_args()
    root = pathlib.Path(__file__).resolve().parent.parent
    overrides = root / "packaging" / "license-overrides"
    override_packages = json.loads((overrides / "packages.json").read_text(encoding="utf-8"))
    override_sources = json.loads((overrides / "sources.json").read_text(encoding="utf-8"))
    metadata = json.loads(subprocess.check_output(
        ["cargo", "metadata", "--locked", "--format-version", "1",
         "--filter-platform", args.target], cwd=root, text=True, encoding="utf-8"))
    destination = args.destination
    destination.mkdir(parents=True, exist_ok=True)
    resolved_ids = {node["id"] for node in metadata["resolve"]["nodes"]}
    packages = sorted(
        (p for p in metadata["packages"]
         if p["id"] in resolved_ids and p["id"] not in metadata["workspace_members"]),
        key=lambda p: (p["name"], p["version"]))
    inventory = []
    for package in packages:
        source = pathlib.Path(package["manifest_path"]).parent
        name = f'{package["name"]}-{package["version"]}'
        target = destination / name
        target.mkdir(exist_ok=True)
        candidates = set()
        if package.get("license_file"):
            candidates.add(source / package["license_file"])
        # Include vendored native-library notices as well as crate-level notices.
        for file in source.rglob("*"):
            if not file.is_file():
                continue
            upper = file.name.upper()
            if (upper.startswith(("LICENSE", "LICENCE", "COPYING", "NOTICE", "COPYRIGHT"))
                    or "LICENSE" in upper
                    or upper in ("OFL.TXT", "UFL.TXT", "HACK-REGULAR.TXT", "README.IJG")):
                candidates.add(file)
        copied = []
        for file in sorted(candidates):
            if not file.is_file():
                raise SystemExit(f"Missing declared license file: {file}")
            try:
                relative = file.relative_to(source)
            except ValueError:
                relative = pathlib.Path(file.name)
            output = target / relative
            output.parent.mkdir(parents=True, exist_ok=True)
            shutil.copyfile(file, output)
            copied.append(relative.as_posix())
        upstream_sources = []
        group = override_packages.get(f'{package["name"]}@{package["version"]}')
        if group:
            for file in sorted((overrides / group).rglob("*")):
                if not file.is_file():
                    continue
                relative = pathlib.Path("upstream") / file.relative_to(overrides / group)
                output = target / relative
                output.parent.mkdir(parents=True, exist_ok=True)
                shutil.copyfile(file, output)
                copied.append(relative.as_posix())
            upstream_sources = [record for record in override_sources if record["group"] == group]
        record = {key: package.get(key) for key in
                  ("name", "version", "license", "repository", "homepage", "source")}
        record["files"] = copied
        if upstream_sources:
            record["upstream_notice_sources"] = upstream_sources
        inventory.append(record)
        (target / "package.json").write_text(
            json.dumps(record, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    (destination / "inventory.json").write_text(
        json.dumps(inventory, ensure_ascii=False, indent=2) + "\n", encoding="utf-8")
    lines = ["# Locked dependency notices", "",
             f"Target: `{args.target}`. Includes development/build dependencies in the resolved graph.", "",
             "License expressions below are package metadata. Copied license and notice files are in",
             "the corresponding package directory; see inventory.json for source links and file lists.", "",
             "| Package | Declared license | License/notice files |", "| --- | --- | --- |"]
    for item in inventory:
        lines.append(f'| {item["name"]} {item["version"]} | {item["license"] or "See package files"} | {len(item["files"])} |')
    (destination / "README.md").write_text("\n".join(lines) + "\n", encoding="utf-8")
    without_files = [f'{p["name"]} {p["version"]}' for p in inventory if not p["files"]]
    print(f"Collected {len(packages)} locked dependency records for {args.target}.")
    if without_files:
        print("Packages without bundled license files (review metadata/source): " + ", ".join(without_files))


if __name__ == "__main__":
    main()
