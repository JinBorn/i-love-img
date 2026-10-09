#!/usr/bin/env python3
"""Independently verify benchmark outputs with Pillow (run after measurement).

Example:
  python scripts/verify-benchmarks.py --source artifacts/performance/input \
    --manifest artifacts/performance/workload.json \
    --cli-report report.json --gui-log capture/run.jsonl \
    --output-root processed --output verification.json

Repeat --cli-report, --gui-log and --output-root as needed. Every report's
originals must belong to the supplied source manifest. Different source sets
need separate invocations. Output JSON is created exclusively, never overwritten.
Image verification is sequential; GIF frames are composed and compared lazily,
not collected into an animation-sized pixel array. Requires Pillow with WebP.
"""

import argparse
from collections import Counter, defaultdict
import hashlib
from itertools import zip_longest
import json
import math
import os
from pathlib import Path
import sys

from PIL import Image, __version__ as pillow_version


FORMATS = {"JPEG", "PNG", "WEBP", "GIF"}
EXTENSIONS = {".jpg", ".jpeg", ".png", ".webp", ".gif"}


def path(value):
    value = os.fspath(value)
    if os.name == "nt":
        if value.startswith("\\\\?\\UNC\\"):
            value = "\\\\" + value[8:]
        elif value.startswith("\\\\?\\"):
            value = value[4:]
    else:
        value = value.replace("\\", "/")
    return Path(value).resolve()


def format_name(value):
    value = str(value).upper()
    return "JPEG" if value == "JPG" else value


def require(condition, message):
    if not condition:
        raise ValueError(message)


def sha256(file):
    digest = hashlib.sha256()
    with file.open("rb") as source:
        while block := source.read(1024 * 1024):
            digest.update(block)
    return digest.hexdigest()


def pixel_digest(image, mode="RGBA"):
    digest = hashlib.sha256()
    for top in range(0, image.height, 128):
        with image.crop((0, top, image.width, min(top + 128, image.height))) as band:
            with band.convert(mode) as normalized:
                digest.update(normalized.tobytes())
    return digest.digest()


def png_depth(file):
    with file.open("rb") as stream:
        header = stream.read(29)
    require(header[:8] == b"\x89PNG\r\n\x1a\n" and header[12:16] == b"IHDR", "Invalid PNG header")
    return header[24], header[25]


def compare_png(source, output, original, result):
    require(original.size == result.size, "Lossless PNG dimensions changed")
    depth, color = png_depth(source)
    require(png_depth(output)[0] == depth, "Lossless PNG bit depth changed")
    require(not (depth == 16 and color in (2, 4, 6)),
            "Pillow cannot certify full precision of 16-bit multi-channel PNG pixels")
    mode = "I" if depth == 16 else "RGBA"
    require(pixel_digest(original, mode) == pixel_digest(result, mode), "Lossless PNG pixels changed")


def gif_segments(image, stats):
    """Coalesce equal consecutive canvases so valid frame merging is allowed."""
    previous = None
    duration = 0
    index = 0
    while True:
        try:
            image.seek(index)
        except EOFError:
            break
        image.load()  # Pillow applies GIF offsets, disposal and prior-frame composition.
        current = pixel_digest(image)
        delay = int(image.info.get("duration", 0))
        stats["frames"] += 1
        stats["duration_ms"] += delay
        if previous is not None and current != previous:
            yield previous, duration
            duration = 0
        previous = current
        duration += delay
        index += 1
    if previous is not None:
        yield previous, duration


def compare_gif(original, result):
    require(original.size == result.size, "Lossless GIF canvas dimensions changed")
    require(original.info.get("loop") == result.info.get("loop"), "GIF loop behavior changed")
    before = {"frames": 0, "duration_ms": 0}
    after = {"frames": 0, "duration_ms": 0}
    for index, pair in enumerate(zip_longest(gif_segments(original, before), gif_segments(result, after))):
        require(pair[0] == pair[1], f"GIF composed RGBA/timing differs at playback segment {index}")
    return {"input": before, "output": after}


def decode_all(image):
    count = 0
    while True:
        try:
            image.seek(count)
        except EOFError:
            return count
        image.load()
        count += 1


class Verifier:
    def __init__(self, args):
        self.source = path(args.source)
        self.roots = [path(root) for root in args.output_root]
        self.originals = {}
        self.errors = []
        self.error_count = 0

    def error(self, context, error):
        self.error_count += 1
        if len(self.errors) < 100:
            self.errors.append({"context": str(context), "error": str(error)})

    def check_sources(self, manifest_file):
        manifest = json.loads(manifest_file.read_text(encoding="utf-8-sig"))
        entries = manifest["files"]
        require(isinstance(entries, list) and entries, "Manifest must contain source files")
        total = 0
        checked = 0
        counts = Counter()
        for entry in entries:
            try:
                file = path(self.source / entry["path"].replace("\\", "/"))
                require(file.is_relative_to(self.source), "Manifest entry escapes --source")
                require(file not in self.originals, "Duplicate source in manifest")
                self.originals[file] = entry
                before = file.stat()
                require(before.st_size == entry["bytes"], "Original byte count differs from manifest")
                require(sha256(file) == entry["sha256"].lower(), "Original SHA256 differs from manifest")
                after = file.stat()
                require((before.st_size, before.st_mtime_ns) == (after.st_size, after.st_mtime_ns),
                        "Original changed during hash verification")
                checked += 1
                total += after.st_size
                counts[format_name(entry["format"])] += 1
            except (OSError, KeyError, ValueError) as error:
                self.error(entry.get("path", "manifest entry"), error)
        actual = {path(p) for p in self.source.rglob("*") if p.is_file() and p.suffix.lower() in EXTENSIONS}
        if actual != set(self.originals):
            self.error(manifest_file, f"Source/manifest image sets differ: {len(actual - set(self.originals))} unlisted, {len(set(self.originals) - actual)} missing")
        summary = manifest.get("summary", {})
        for key, value in [("images", len(entries)), ("input_bytes", total)]:
            if key in summary and summary[key] != value:
                self.error(manifest_file, f"Manifest summary {key}={summary[key]} differs from verified {value}")
        if "formats" in summary:
            expected_formats = {format_name(k): v for k, v in summary["formats"].items()}
            if expected_formats != dict(counts):
                self.error(manifest_file, "Manifest per-format counts differ from verified source entries")
        return {"source": str(self.source), "manifest": str(manifest_file),
                "manifest_images": len(entries), "sha256_verified_images": checked,
                "verified_bytes": total, "formats": dict(counts)}

    def success(self, row, options):
        original = path(row["original"])
        require(original in self.originals, f"Original is outside the verified manifest: {original}")
        source, output = path(row["input"]), path(row["output"])
        require(source != output and original != output, "Output aliases an input/original path")
        require(not os.path.samefile(source, output), "Output is the same filesystem file as its input")
        require(not os.path.samefile(original, output), "Output is the same filesystem file as its original")
        report = row["processing"]
        input_bytes, output_bytes = source.stat().st_size, output.stat().st_size
        require(input_bytes == report["input_bytes"], "Reported input bytes differ from disk")
        require(output_bytes == report["output_bytes"] and output_bytes > 0, "Reported output bytes differ from disk")
        pure = options.get("resize", {}).get("mode") == "Keep" and options.get("format") is None
        with Image.open(source) as before, Image.open(output) as after:
            input_format, output_format = format_name(before.format), format_name(after.format)
            require(input_format in FORMATS and output_format in FORMATS, "Unexpected decoded format")
            require(output_format == format_name(report["format"]), "Decoded output format differs from processing report")
            expected = input_format if options.get("format") is None else format_name(options["format"])
            require(output_format == expected, "Output format does not match the requested format")
            require(after.size == (report["width"], report["height"]), "Decoded dimensions differ from processing report")
            comparison = None
            if pure and input_format == "PNG":
                compare_png(source, output, before, after)
                comparison = "png_pixels_and_bit_depth"
            elif pure and input_format == "GIF":
                comparison = {"gif_playback": compare_gif(before, after)}
            else:
                decode_all(after)
        return {"input_format": input_format, "output_format": output_format,
                "input_bytes": input_bytes, "output_bytes": output_bytes,
                "processing_elapsed_ms": report["elapsed_ms"], "comparison": comparison,
                "output": output, "original": original}

    def batch(self, label, rows, options, summary=None, kind="cli", step=None):
        start_errors = self.error_count
        status_counts = Counter(row["status"].lower() for row in rows)
        totals = dict(input_bytes=0, output_bytes=0, processing_elapsed_ms=0)
        by_format = defaultdict(lambda: dict(count=0, input_bytes=0, output_bytes=0,
                                            processing_elapsed_ms=0, output_formats=Counter()))
        outputs, covered = set(), set()
        comparisons = Counter()
        verified = 0
        seen_ids = set()
        for row in rows:
            try:
                require(row["id"] not in seen_ids, "Duplicate item ID in a batch")
                seen_ids.add(row["id"])
                require(path(row["original"]) in self.originals, "Original lacks source-manifest SHA256 coverage")
                status = row["status"].lower()
                require(status in {"success", "cancelled", "failed", "skipped"}, f"Incomplete or unknown final state: {status}")
                if status != "success":
                    require(not row.get("output"), "Non-successful item exposes an output")
                    require(status == "cancelled", f"Benchmark item {status}: {row.get('reason', '')}")
                    continue
                result = self.success(row, options)
                require(result["output"] not in outputs, "Two successful items claim the same output file")
                outputs.add(result["output"])
                covered.add(result["original"])
                verified += 1
                bucket = by_format[result["input_format"]]
                bucket["count"] += 1
                bucket["output_formats"][result["output_format"]] += 1
                for key in totals:
                    totals[key] += result[key]
                    bucket[key] += result[key]
                if result["comparison"]:
                    comparisons["gif_playback" if isinstance(result["comparison"], dict) else result["comparison"]] += 1
            except (OSError, KeyError, ValueError, TypeError, SyntaxError, Image.DecompressionBombError) as error:
                self.error(f"{label}: item {row.get('id')}", error)

        if summary is not None:
            expected = {"total": len(rows), **{k: status_counts[k] for k in ["success", "failed", "skipped", "cancelled"]}}
            expected.update(input_bytes=totals["input_bytes"], output_bytes=totals["output_bytes"])
            for key, actual in expected.items():
                if key in summary and not isinstance(summary[key], bool) and summary[key] != actual:
                    self.error(label, f"Summary {key}={summary[key]} differs from verified {actual}")
        wall_ms = summary.get("elapsed_ms") if summary else None
        throughput = status_counts["success"] * 1000 / wall_ms if wall_ms and wall_ms > 0 else None
        if summary and "images_per_second" in summary:
            expected = throughput if throughput is not None else 0
            if not math.isclose(summary["images_per_second"], expected, rel_tol=1e-6, abs_tol=1e-6):
                self.error(label, "Reported images_per_second does not match batch wall time/success count")

        roots = self.roots
        scope = "explicit output roots"
        if not roots and outputs:
            roots = [path(os.path.commonpath([str(p.parent) for p in outputs]))]
            scope = "inferred common output ancestor; may be narrower than the configured output root"
        if not roots:
            self.error(label, "No output root can be inferred; provide --output-root to check cancellation cleanup")
        temporary = []
        for root in roots:
            if root == Path(root.anchor):
                self.error(label, f"Refusing to recursively scan a filesystem root: {root}")
                continue
            if not root.is_dir():
                self.error(label, f"Output root does not exist: {root}")
                continue
            temporary.extend(str(p) for p in root.rglob(".local-image-*.tmp"))
        if self.roots and any(not any(p.is_relative_to(root) for root in self.roots) for p in outputs):
            self.error(label, "Some outputs are outside the supplied --output-root directories")
        if temporary:
            self.error(label, f"Leftover tool temporary files: {temporary[:20]}")
        return {"kind": kind, "report": str(label), "step": step,
                "verified": self.error_count == start_errors, "status_counts": dict(status_counts),
                "verified_successes": verified, "manifest_originals_with_valid_outputs": len(covered),
                **totals, "by_input_format": dict(by_format), "lossless_comparisons": dict(comparisons),
                "batch_wall_ms": wall_ms, "images_per_second": throughput,
                "batch_cancelled_flag": summary.get("cancelled") if summary and isinstance(summary.get("cancelled"), bool) else None,
                "wall_time_source": "batch report elapsed_ms" if wall_ms is not None else "unavailable in this log; not inferred from item timings",
                "processing_time_note": "processing_elapsed_ms is the SUM of per-item processing times, not batch wall time",
                "temporary_scan_scope": scope, "temporary_scan_roots": [str(p) for p in roots],
                "leftover_temporary_files": temporary}

    def gui(self, file):
        batches = []
        selected = None
        timing = None
        completed = False
        for line_number, line in enumerate(file.read_text(encoding="utf-8-sig").splitlines(), 1):
            if not line.strip():
                continue
            event = json.loads(line)
            name = event.get("event")
            if name == "ready":
                completed = False
            elif name == "complete":
                completed = True
            elif name == "before-process":
                selected = {r["id"] for r in event["rows"] if r.get("selected", True)}
                timing = None
            elif name == "batch-finished":
                timing = event.get("summary", event)
            elif name == "after-process":
                source_rows = event["rows"]
                require(selected is not None, "GUI after-process has no matching before-process snapshot")
                source_rows = [r for r in source_rows if r["id"] in selected]
                require({r["id"] for r in source_rows} == selected, "GUI final snapshot is missing selected items")
                rows = [{"id": r["id"], "original": r["original"], "input": r["input"],
                         "output": r.get("output"), "processing": r.get("report"),
                         "status": r["state"].lower(), "reason": r.get("detail", "")}
                        for r in source_rows]
                batches.append(self.batch(f"{file}:line {line_number}", rows, event["options"], timing,
                                          kind="gui", step=event.get("step")))
                selected, timing = None, None
        require(batches, f"No after-process batches found in {file}")
        require(selected is None and completed, f"GUI log did not reach a complete diagnostic run: {file}")
        return batches


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--source", type=Path, required=True)
    parser.add_argument("--manifest", type=Path, required=True)
    parser.add_argument("--cli-report", type=Path, action="append", default=[])
    parser.add_argument("--gui-log", type=Path, action="append", default=[])
    parser.add_argument("--output-root", type=Path, action="append", default=[])
    parser.add_argument("--output", type=Path, required=True)
    args = parser.parse_args()
    if not args.cli_report and not args.gui_log:
        parser.error("At least one --cli-report or --gui-log is required")
    args.output.parent.mkdir(parents=True, exist_ok=True)
    try:
        destination = args.output.open("x", encoding="utf-8")
    except FileExistsError:
        parser.error(f"Refusing to overwrite {args.output}")
    verifier = Verifier(args)
    result = {"schema_version": 1, "pillow_version": pillow_version, "batches": []}
    with destination:
        try:
            result["originals"] = verifier.check_sources(args.manifest)
            for file in args.cli_report:
                try:
                    report = json.loads(file.read_text(encoding="utf-8-sig"))
                    result["batches"].append(verifier.batch(file, report["results"], report["options"], report["summary"]))
                except (OSError, KeyError, ValueError) as error:
                    verifier.error(file, error)
            for file in args.gui_log:
                try:
                    result["batches"].extend(verifier.gui(file))
                except (OSError, KeyError, ValueError) as error:
                    verifier.error(file, error)
        except (OSError, KeyError, ValueError) as error:
            verifier.error(args.manifest, error)
        result.update(verified=verifier.error_count == 0, error_count=verifier.error_count,
                      errors=verifier.errors,
                      notes=["Reports are independent runs; byte counts/timings are not added across repeated runs.",
                             "JPEG/WebP lossy outputs are decoded and checked structurally, not certified visually lossless.",
                             "Only Keep resize plus null output format triggers PNG/GIF pure-lossless comparison.",
                             "GIF comparison uses composited RGBA and durations, allowing identical adjacent frames to be merged."])
        json.dump(result, destination, ensure_ascii=False, indent=2)
        destination.write("\n")
    print(f"{'PASS' if result['verified'] else 'FAIL'}: {len(result['batches'])} batches; {verifier.error_count} verification errors; {args.output}")
    return 0 if result["verified"] else 1


if __name__ == "__main__":
    sys.exit(main())
