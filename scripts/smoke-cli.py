"""Exercise a built CLI end to end; Pillow independently decodes its outputs.

Usage: python scripts/smoke-cli.py --exe path/to/lit --output NEW_DIRECTORY
Requires Pillow for verification only. Produces a JSON verification record.
"""

import argparse
import hashlib
import json
import subprocess
from pathlib import Path

from PIL import Image


def digest(path):
    with path.open("rb") as stream:
        return hashlib.file_digest(stream, "sha256").hexdigest()


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--exe", type=Path, required=True)
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    exe = args.exe.resolve()
    destination = args.output.resolve()
    destination.mkdir(parents=True, exist_ok=False)
    records = []

    def run(arguments, expected=0):
        result = subprocess.run([str(exe), *map(str, arguments)], capture_output=True, text=True, encoding="utf-8")
        records.append({"arguments": list(map(str, arguments)), "exit_code": result.returncode,
                        "stdout": result.stdout, "stderr": result.stderr})
        if result.returncode != expected:
            raise AssertionError(f"Unexpected exit {result.returncode}: {result.stderr}\n{result.stdout}")
        return result

    source = destination / "originals"
    run(["fixtures", "--output", source, "--count", 8, "--width", 320, "--height", 240, "--gif-frames", 6])
    hashes = {path: digest(path) for path in source.rglob("*") if path.is_file()}
    with Image.open(next(source.rglob("*.gif"))) as animation:
        original_frames = animation.n_frames
        original_loop = animation.info.get("loop")

    operations = [
        ("compress", []),
        ("resize", ["--percent", 50]),
        ("jpeg", ["--format", "jpeg", "--long-edge", 100, "--gif-first-frame"]),
        ("png", ["--format", "png", "--long-edge", 100, "--gif-first-frame"]),
        ("webp", ["--format", "webp", "--long-edge", 100, "--gif-first-frame"]),
        ("gif", ["--format", "gif", "--long-edge", 100]),
    ]
    for name, arguments in operations:
        report_path = destination / f"{name}.json"
        run(["process", source, "--recursive", "--output", destination / name, "--report", report_path, *arguments])
        report = json.loads(report_path.read_text(encoding="utf-8"))
        assert report["summary"]["success"] == 8, report["summary"]
        for result in report["results"]:
            with Image.open(result["input"]) as before, Image.open(result["output"]) as after:
                # Verify actual bytes, not only the engine's declared format and dimensions.
                if name in ("jpeg", "png", "webp", "gif"):
                    assert after.format.lower() == name, (name, after.format)
                    assert max(after.size) == 100, after.size
                elif name == "resize":
                    assert after.size == tuple(max(1, dimension // 2) for dimension in before.size)
                else:
                    assert after.size == before.size
                if before.format == "GIF" and after.format == "GIF":
                    assert after.n_frames == original_frames
                    assert after.info.get("loop") == original_loop
                    for frame in range(original_frames):
                        before.seek(frame)
                        after.seek(frame)
                        assert before.info.get("duration") == after.info.get("duration")
                for frame in range(getattr(after, "n_frames", 1)):
                    after.seek(frame)
                    after.load()

    existing = {path: digest(path) for path in (destination / "resize").rglob("*") if path.is_file()}
    run(["process", source, "--recursive", "--output", destination / "resize", "--percent", 50])
    assert all(digest(path) == value for path, value in existing.items()), "A previous output was overwritten"
    assert len([path for path in (destination / "resize").rglob("*") if path.is_file()]) == 16

    gif_path = next(source.rglob("*.gif"))
    result = run(["process", gif_path, "--output", destination / "no-silent-first-frame", "--format", "png"], expected=2)
    assert "first frame" in result.stderr.lower(), result.stderr
    run(["process", source, "--recursive", "--output", destination / "invalid-size", "--percent", 0], expected=1)
    run(["process", source, "--recursive", "--output", destination / "incomplete-bounds", "--width", 100], expected=2)
    assert all(digest(path) == value for path, value in hashes.items()), "An original was changed"
    assert not list(destination.rglob("*.tmp")), "Partial outputs were left behind"
    output = {"status": "passed", "input_images": len(hashes), "verified_operations": [name for name, _ in operations],
              "checks": ["independent decoding", "animation timing/loop", "no silent animation flattening", "original hashes",
                         "collision numbering", "argument validation"], "runs": records}
    (destination / "verification.json").write_text(json.dumps(output, ensure_ascii=False, indent=2), encoding="utf-8")
    print(json.dumps({key: value for key, value in output.items() if key != "runs"}, ensure_ascii=False))


if __name__ == "__main__":
    main()
