#!/usr/bin/env python3
"""Emit NSIS commands that remove only files present in the package stage."""
import argparse
import pathlib


def nsis_path(relative):
    # NSIS expands dollars and quotes even inside quoted string arguments.
    value = str(relative).replace("/", "\\").replace("$", "$$").replace('"', '$\\"')
    if "\n" in value or "\r" in value:
        raise ValueError("NSIS paths cannot contain newlines")
    return f'"$INSTDIR\\{value}"'


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("stage", type=pathlib.Path)
    parser.add_argument("output", type=pathlib.Path)
    args = parser.parse_args()
    stage = args.stage.resolve(strict=True)
    output = args.output.resolve()
    if not stage.is_dir() or output.is_relative_to(stage):
        raise SystemExit("Stage must be a directory; manifest must be outside it")
    files, directories = [], []
    for path in stage.rglob("*"):
        relative = path.relative_to(stage)
        if path.is_symlink() or not path.resolve().is_relative_to(stage):
            raise SystemExit(f"Refusing package symlink or external path: {relative}")
        if path.is_file():
            files.append(relative)
        elif path.is_dir():
            directories.append(relative)
    lines = ["; Generated from the exact package stage. Preserve additional user files."]
    lines.extend(f"Delete {nsis_path(path)}" for path in sorted(files))
    lines.extend(f"RMDir {nsis_path(path)}" for path in
                 sorted(directories, key=lambda path: (-len(path.parts), str(path))))
    with output.open("x", encoding="utf-8") as stream:
        stream.write("\n".join(lines) + "\n")
    print(f"Uninstall manifest: {len(files)} files, {len(directories)} nonrecursive directories")


if __name__ == "__main__":
    main()
