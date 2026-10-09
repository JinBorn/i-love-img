"""Generate deterministic long GIFs without retaining all frames in memory.

Requires Pillow. Use a new output directory; existing files are never replaced.
The 640x480 logical canvas and changing partial frames exercise all three common
disposal modes. Frame count changes independently of canvas dimensions.
"""

import argparse
import hashlib
import json
import struct
from pathlib import Path

from PIL import GifImagePlugin, Image, ImageDraw


def generate(path, count):
    width, height, tile = 640, 480, 120
    colors = [0, 0, 0, 20, 160, 130, 240, 180, 60, 50, 100, 210]
    palette = colors + [0] * (768 - len(colors))
    with path.open("xb") as output:
        output.write(b"GIF89a")
        output.write(struct.pack("<HHBBB", width, height, 0xF1, 0, 0))
        output.write(bytes(colors))
        output.write(b"!\xff\x0bNETSCAPE2.0\x03\x01" + struct.pack("<H", 3) + b"\0")
        for index in range(count):
            with Image.new("P", (tile, tile), 0) as frame:
                frame.putpalette(palette)
                ImageDraw.Draw(frame).ellipse((8, 8, tile - 9, tile - 9), fill=index % 3 + 1)
                blocks = GifImagePlugin.getdata(
                    frame,
                    offset=((index * 7) % (width - tile), (index * 3) % (height - tile)),
                    duration=40,
                    transparency=0,
                    disposal=index % 3 + 1,
                    include_color_table=False,
                )
                for block in blocks:
                    output.write(block)
        output.write(b";")
    with path.open("rb") as source:
        sha256 = hashlib.file_digest(source, "sha256").hexdigest()
    with Image.open(path) as image:
        if image.n_frames != count or image.size != (width, height):
            raise AssertionError("Generated GIF header/frame count mismatch")
        loop = image.info.get("loop")
    return {"path": path.name, "sha256": sha256, "bytes": path.stat().st_size, "format": "GIF",
            "width": width, "height": height, "frames": count, "loop": loop,
            "frame_duration_ms": 40}


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--output", required=True, type=Path)
    parser.add_argument("--frames", nargs="+", type=int, default=[200, 2000])
    args = parser.parse_args()
    if any(count <= 0 for count in args.frames) or len(set(args.frames)) != len(args.frames):
        parser.error("frame counts must be unique positive integers")
    args.output.mkdir(parents=True, exist_ok=False)
    records = [generate(args.output / f"animation-{count}.gif", count) for count in args.frames]
    with (args.output / "manifest.json").open("x", encoding="utf-8") as output:
        json.dump({"note": "Synthetic resource workload, not a visual quality benchmark.",
                   "files": records}, output, indent=2)
        output.write("\n")
    print(json.dumps(records))


if __name__ == "__main__":
    main()
