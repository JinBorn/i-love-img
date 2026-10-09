#!/usr/bin/env python3
"""Verify separate native GUI tools, operation isolation, and explicit chaining.

    python scripts/verify-independent-tools.py --portable-dir "dist/candidate" \
        --artifacts artifacts/tools-ui-validation/independent-tools

Requires Windows, Python 3.10+, Pillow, and the adjacent verify-portable.py.
The portable candidate must be pristine; all settings and outputs belong to a
new copy under --artifacts. Screenshots are captured by the app's opt-in native
diagnostics. This exercises the actual navigation/actions/rendering, not mouse
hit testing. Only the directly spawned GUI PID can receive WM_CLOSE/terminate.
"""

import argparse
import datetime as dt
import hashlib
import importlib.util
import json
import os
from pathlib import Path
import sys
import traceback

from PIL import Image, ImageDraw, __version__ as pillow_version


# Reuse the portable verifier's audited process ownership/profile guards. The
# subclass below changes only diagnostic configuration, not launch/close rules.
_spec = importlib.util.spec_from_file_location(
    "portable_verifier", Path(__file__).with_name("verify-portable.py")
)
portable = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(portable)
require = portable.require
write_json = portable.write_json


def decode(path):
    """Force an independent decode of every output frame using Pillow."""
    with Image.open(path) as image:
        result = {
            "path": str(path), "format": image.format, "size": list(image.size),
            "frames": getattr(image, "n_frames", 1), "frame_details": [],
            "sha256": portable.digest(path), "bytes": path.stat().st_size,
        }
        for index in range(result["frames"]):
            image.seek(index)
            image.load()
            pixels = image.convert("RGBA")
            result["frame_details"].append({
                "size": list(pixels.size),
                "duration_ms": image.info.get("duration", 0),
                "rgba_sha256": hashlib.sha256(pixels.tobytes()).hexdigest(),
            })
    return result


def make_fixtures(root):
    root.mkdir()
    rgb = Image.new("RGB", (192, 128))
    rgb.putdata([
        ((x * 13 + y * 7) % 256, (x * 3 + y * 17) % 256, (x * 19 + y * 11) % 256)
        for y in range(128) for x in range(192)
    ])
    rgb.save(root / "photo.jpg", quality=97)
    rgb.transpose(Image.Transpose.FLIP_LEFT_RIGHT).save(root / "photo-alias.jpeg", quality=96)
    alpha = Image.new("RGBA", (160, 120))
    alpha.putdata([
        ((x * 3) % 256, (y * 5) % 256, (x + y) % 256, 96 if x < 40 else 255)
        for y in range(120) for x in range(160)
    ])
    alpha.save(root / "transparent.png")
    alpha.resize((144, 108)).save(root / "static.webp", lossless=True)
    frames = []
    for index, color in enumerate(("#d34b58", "#32a06d", "#497ce0")):
        frame = Image.new("RGB", (128, 96), color)
        draw = ImageDraw.Draw(frame)
        draw.rectangle((8 + index * 22, 12, 38 + index * 22, 78), fill="#fff4b8")
        frames.append(frame)
    frames[0].save(
        root / "animated.gif", save_all=True, append_images=frames[1:],
        duration=[40, 70, 110], loop=0, disposal=2, optimize=False,
    )
    result = {path.name: decode(path) for path in sorted(root.iterdir())}
    require(len(result) == 5, "Fixture creation did not produce all five inputs")
    require(result["animated.gif"]["frames"] == 3, "GIF fixture lost frames")
    return result


class IndependentRun(portable.GuiRun):
    def __init__(self, folder, app, cwd, timeout, *, inputs, steps, save=False):
        self.folder, self.app, self.cwd, self.timeout = folder, app, cwd, timeout
        self.process = None
        self.handles = []
        self.record = {"application": str(app), "cwd": str(cwd), "wm_close": []}
        self.capture = folder / "capture"
        self.capture.mkdir()
        self.config = {
            # An omitted initial tool must open Home, even with input paths.
            "input_paths": inputs, "steps": steps,
            "capture_directory": str(self.capture), "idle_ms": 1500,
            "settle_ms": 250, "exit_when_done": False,
            "use_saved_settings": True, "save_settings_on_exit": save,
        }
        self.config_path = folder / "diagnostics.json"
        write_json(self.config_path, self.config)


STEPS = [
    {"action": "open", "tool": "compress"},
    # Inapplicable fields must never escape the selected tool's whitelist.
    {"action": "process", "tool": "compress", "resize_percent": 9,
     "format": "gif", "gif_first_frame": True},
    {"action": "open", "tool": "resize"},
    {"action": "open", "tool": "convert"},
    {"action": "open", "tool": "compress"},
    {"action": "continue_ready", "tool": "resize", "resize_percent": 50},
    {"action": "process", "tool": "resize", "lossless": True,
     "format": "webp", "gif_first_frame": True},
    {"action": "continue", "tool": "convert", "format": "webp",
     "lossless": True, "gif_first_frame": True},
    {"action": "readjust", "tool": "convert"},
    {"action": "open", "tool": "resize"},
    {"action": "open", "tool": "compress"},
    {"action": "home"},
    {"action": "open", "tool": "resize"},
    {"action": "continue_ready", "tool": "compress"},
    {"action": "home"},
]


def one(events, name, step=None):
    matches = [event for event in events if event.get("event") == name
               and (step is None or event.get("step") == step)]
    require(len(matches) == 1, f"Expected one {name} at step {step}; found {len(matches)}")
    return matches[0]


def snapshot(events, step):
    action = STEPS[step]["action"]
    return one(events, "after-navigation" if action in ("open", "home", "continue_ready")
               else "after-process", step)


def queue_counts(event):
    queues = event.get("tool_queues")
    require(isinstance(queues, dict), "Snapshot is missing independent tool queue counts")
    return {tool: queues[tool] for tool in ("compress", "resize", "convert")}


def home(event, pending):
    require(event.get("page") == "home" and event.get("tool") is None,
            "Home snapshot exposes an active tool")
    require(event.get("options") is None and event["rows"] == [],
            "Home must not expose a hidden operation or active file list")
    actual_pending = event.get("pending_input_count", event.get("pending_paths"))
    require(actual_pending == pending, f"Unexpected Home pending input count: {actual_pending}")


def tool(event, expected, rows):
    require(event.get("page") in ("tool", expected) and event.get("tool") == expected,
            f"Expected separate {expected} page: {event.get('page')}/{event.get('tool')}")
    require(len(event["rows"]) == rows, f"Unexpected row count in {expected}")
    options = event.get("options")
    require(isinstance(options, dict), f"Missing actual {expected} process options")
    if expected in ("compress", "resize"):
        require(options["format"] is None, f"{expected} inherited a conversion format")
        require(options["gif_first_frame"] is False, f"{expected} inherited GIF first-frame export")
    if expected in ("compress", "convert"):
        require(options["resize"]["mode"] == "Keep", f"{expected} inherited resize parameters")
    if expected == "resize":
        require(options["compression"] == "Auto", "Resize inherited Compress lossless mode")
    return options


def by_original(event):
    rows = {Path(row["original"]).name: row for row in event["rows"]}
    require(len(rows) == len(event["rows"]), "Duplicate original lineage in this verification stage")
    return rows


def inspect_outputs(event, fixtures, app, *, scale, converted=False):
    rows = by_original(event)
    require(set(rows) == set(fixtures), "Batch lost an original input")
    inspected = {}
    for name, row in rows.items():
        require(row["state"] == "Success" and row.get("output"), f"Task failed: {row}")
        output = Path(row["output"]).resolve(strict=True)
        require(portable.within(output, (app / "Output").resolve(strict=True)),
                f"Output escaped portable output directory: {output}")
        require(not portable.same_path(row["output"], row["input"])
                and not portable.same_path(row["output"], row["original"]),
                "Output overwrote an input/original path")
        actual = decode(output)
        original = fixtures[name]
        expected_size = [int(dimension * scale) for dimension in original["size"]]
        require(actual["size"] == expected_size, f"{name} dimensions: {actual['size']} != {expected_size}")
        require(actual["format"] == ("WEBP" if converted else original["format"]),
                f"{name} has the wrong output format: {actual['format']}")
        require(actual["frames"] == (1 if converted else original["frames"]),
                f"{name} animation frame count changed unexpectedly")
        require(all(frame["size"] == expected_size for frame in actual["frame_details"]),
                f"{name} contains an incorrectly sized animation frame")
        if original["frames"] > 1 and not converted:
            require([frame["duration_ms"] for frame in actual["frame_details"]]
                    == [frame["duration_ms"] for frame in original["frame_details"]],
                    f"{name} frame timing changed")
            require(len({frame["rgba_sha256"] for frame in actual["frame_details"]}) == 3,
                    f"{name} lost distinct animation content")
        inspected[name] = actual
    return inspected


def assert_lineage(event, previous, *, original=False):
    previous_rows = by_original(previous)
    for name, row in by_original(event).items():
        expected = row["original"] if original else previous_rows[name]["output"]
        require(portable.same_path(row["input"], expected), f"Wrong input lineage for {name}")
        require(row["source"] == ("Original" if original else "PreviousOutput"),
                f"Wrong input source semantics for {name}")


def verify_flow(events, fixtures, app, capture):
    count = len(fixtures)
    ready = one(events, "ready")
    home(ready, count)
    require(queue_counts(ready) == dict.fromkeys(("compress", "resize", "convert"), 0),
            "Homepage inputs were silently assigned to a tool")
    imported = snapshot(events, 0)
    tool(imported, "compress", count)
    for row in imported["rows"]:
        require(row["state"] == "Ready" and row["output"] is None,
                "Opening Compress unexpectedly ran an operation")
        expected_input = fixtures[Path(row["original"]).name]["path"]
        require(portable.same_path(row["original"], expected_input)
                and portable.same_path(row["input"], expected_input)
                and row["source"] == "Original",
                "Homepage import lost its original identity")
    require(imported["settings"]["resize"]["percent"] == 25
            and imported["settings"]["convert"]["format"] == "WebP",
            "Poisoned dormant profiles were not loaded")
    compressed = snapshot(events, 1)
    options = tool(compressed, "compress", count)
    require(options["compression"] == "Lossless", "Compress lost its own selected lossless setting")
    require(compressed["settings"] == imported["settings"], "Compress changed another tool's settings")
    result = {"compress": inspect_outputs(compressed, fixtures, app, scale=1)}
    for step, kind in ((2, "resize"), (3, "convert")):
        empty = snapshot(events, step)
        tool(empty, kind, 0)
        require(empty["settings"] == compressed["settings"], "Opening a tool mutated preferences")
        require(queue_counts(empty) == {"compress": count, "resize": 0, "convert": 0},
                "A tool inherited another tool's file queue")
    returned = snapshot(events, 4)
    tool(returned, "compress", count)
    require(returned["rows"] == compressed["rows"], "Returning to Compress lost its results")

    transfer = snapshot(events, 5)
    options = tool(transfer, "resize", count)
    require(options["resize"]["mode"] == {"Percent": 50.0}, "Resize did not use its own 50% setting")
    assert_lineage(transfer, compressed)
    require(all(row["selected"] and row["state"] == "Ready" and row["output"] is None
                for row in transfer["rows"]), "Continue-ready processed outputs before a Start action")
    require(not any(event.get("event") == "before-process" and event.get("step") == 5 for event in events),
            "Continue-ready implicitly started processing")
    for other in ("compress", "convert"):
        require(transfer["settings"][other] == compressed["settings"][other],
                f"Transfer to Resize mutated {other} preferences")
    resized = snapshot(events, 6)
    tool(resized, "resize", count)
    assert_lineage(resized, compressed)
    result["resize"] = inspect_outputs(resized, fixtures, app, scale=0.5)
    converted = snapshot(events, 7)
    options = tool(converted, "convert", count)
    require(options["format"] == "WebP" and options["gif_first_frame"] is True,
            "Conversion did not use explicit WebP/first-frame choices")
    assert_lineage(converted, resized)
    result["convert"] = inspect_outputs(converted, fixtures, app, scale=0.5, converted=True)
    require(result["convert"]["animated.gif"]["frame_details"][0]["rgba_sha256"]
            == result["resize"]["animated.gif"]["frame_details"][0]["rgba_sha256"],
            "Explicit first-frame export did not preserve the input GIF's first frame")
    readjusted = snapshot(events, 8)
    tool(readjusted, "convert", count)
    assert_lineage(readjusted, converted, original=True)
    result["readjust"] = inspect_outputs(readjusted, fixtures, app, scale=1, converted=True)
    require(result["readjust"]["animated.gif"]["frame_details"][0]["rgba_sha256"]
            == fixtures["animated.gif"]["frame_details"][0]["rgba_sha256"],
            "Readjust did not export the original GIF's first frame")
    require(readjusted["settings"] == converted["settings"], "Readjust restored unrelated tool preferences")
    for step, previous, kind in ((9, resized, "resize"), (10, compressed, "compress")):
        restored = snapshot(events, step)
        tool(restored, kind, count)
        require(restored["rows"] == previous["rows"], f"{kind} lost its independent batch results")
        require(restored["options"] == previous["options"], f"{kind} lost its independent options")
    retained_home = snapshot(events, 11)
    home(retained_home, 0)
    require(queue_counts(retained_home) == dict.fromkeys(("compress", "resize", "convert"), count),
            "Home discarded independent tool queues")
    reopened_resize = snapshot(events, 12)
    tool(reopened_resize, "resize", count)
    require(reopened_resize["rows"] == resized["rows"]
            and all(row["selected"] for row in reopened_resize["rows"]),
            "Reopening Resize did not retain its five selected successful outputs")
    appended = snapshot(events, 13)
    tool(appended, "compress", count * 2)
    previous_by_id = {row["id"]: row for row in compressed["rows"]}
    all_ids = [row["id"] for row in appended["rows"]]
    require(len(set(all_ids)) == count * 2, "Appending to Compress reused an existing row ID")
    retained = [row for row in appended["rows"] if row["id"] in previous_by_id]
    incoming = [row for row in appended["rows"] if row["id"] not in previous_by_id]
    require(len(retained) == count and len(incoming) == count, "Transfer replaced the destination's old queue")
    for row in retained:
        require(row == {**previous_by_id[row["id"]], "selected": False},
                "Transfer changed an existing Compress result or left it selected")
    previously_used_ids = {row["id"] for event in (compressed, resized, readjusted) for row in event["rows"]}
    require(all(row["id"] not in previously_used_ids for row in incoming),
            "Transfer reused a row ID belonging to another tool")
    require(all(row["selected"] and row["state"] == "Ready" and row["output"] is None
                for row in incoming), "Transfer implicitly processed incoming rows or failed to select them")
    incoming_event = {"rows": incoming}
    require(set(by_original(incoming_event)) == set(fixtures), "Appended queue lost an original input")
    assert_lineage(incoming_event, resized)
    originals = by_original(resized)
    for name, row in by_original(incoming_event).items():
        require(portable.same_path(row["original"], originals[name]["original"]),
                "Appending to an existing queue changed original lineage")
    require(appended["options"] == compressed["options"]
            and appended["settings"] == retained_home["settings"],
            "Append transfer changed destination parameters or another tool's preferences")
    final_home = snapshot(events, 14)
    home(final_home, 0)
    require(queue_counts(final_home) == {"compress": count * 2, "resize": count, "convert": count},
            "Final Home did not retain the appended destination queue")
    processing_steps = [event["step"] for event in events if event.get("event") == "before-process"]
    require(processing_steps == [1, 6, 7, 8], f"Unexpected implicit processing: {processing_steps}")
    screenshots = [event for event in events if event.get("event") == "screenshot"]
    require(len(screenshots) == len(STEPS) + 1, "Missing a Home/tool/action screenshot")
    for screenshot in screenshots:
        require(screenshot.get("ok") is True and Path(screenshot["path"]).is_file(),
                f"Screenshot capture failed: {screenshot}")
    require((capture / "00-ready.png").is_file(), "Homepage screenshot is missing")
    return result, final_home["settings"]


def verify(args):
    require(os.name == "nt", "This native GUI verifier requires Windows")
    candidate = args.portable_dir.resolve(strict=True)
    artifacts = args.artifacts.resolve()
    require(candidate.is_dir() and (candidate / portable.EXE_NAME).is_file()
            and (candidate / "portable.flag").is_file(), "Candidate must be an extracted portable build")
    require(not (candidate / "Data").exists(), "Candidate must be pristine (no Data directory)")
    require(not artifacts.exists(), "--artifacts must name a new directory")
    require(not portable.within(artifacts, candidate), "Artifacts must be outside the candidate")
    artifacts.mkdir(parents=True)
    baseline = portable.tree_snapshot(candidate)
    profiles = portable.profile_roots()
    before_profiles = {name: portable.tree_snapshot(path) for name, path in profiles.items()}
    report = {
        "started_utc": dt.datetime.now(dt.timezone.utc).isoformat(), "status": "running",
        "candidate": str(candidate), "candidate_sha256": portable.digest(candidate / portable.EXE_NAME),
        "python": sys.version, "pillow": pillow_version, "steps": STEPS,
        "scope": ["Home and separate tool pages", "Per-tool parameters and batch queues",
                  "All-frame independent output decoding", "Explicit continue and original-based readjust",
                  "Schema-2 preference save/reload", "App-specific profile and original-file hash guards"],
        "limitations": ["Diagnostics invoke actual app actions; mouse hit testing is reviewed separately",
                        "Small fixtures verify behavior, not 1000-image throughput or memory targets",
                        "Windows host only; no macOS/Linux or physical USB runtime claim"],
        "profile_paths_read_only": {name: str(path) for name, path in profiles.items()},
    }
    caught_error = None
    fixtures = None
    try:
        fixtures = make_fixtures(artifacts / "fixtures")
        report["fixtures"] = fixtures
        primary = artifacts / "01-independent-flow"
        primary.mkdir()
        app = primary / "Local Image Tools"
        portable.shutil.copytree(candidate, app)
        (app / "Data").mkdir()
        write_json(app / "Data/settings.json", {
            "schema_version": 2, "language": "Chinese", "output_directory": "Output",
            "recursive": True,
            "compress": {"lossless": True, "jpeg_quality": 81, "webp_quality": 77, "strip_metadata": True},
            "resize": {"mode": 3, "percent": 25, "jpeg_quality": 94, "webp_quality": 89,
                       "strip_metadata": False},
            "convert": {"format": "WebP", "webp_lossless": True, "jpeg_quality": 69,
                        "webp_quality": 64, "gif_first_frame": True, "strip_metadata": False,
                        "background": [11, 22, 33]},
        })
        cwd = primary / "unrelated-cwd"
        cwd.mkdir()
        inputs = [str(artifacts / "fixtures" / name) for name in fixtures]
        with IndependentRun(primary, app, cwd, args.timeout, inputs=inputs, steps=STEPS, save=True) as run:
            events = run.complete()
            report["outputs"], expected_settings = verify_flow(events, fixtures, app, run.capture)
            run.close()
            events = run.exited()
            require(one(events, "settings-saved")["ok"] is True, "Closing did not save preferences")
            saved = portable.read_json(app / "Data/settings.json")
            require(saved.pop("schema_version", None) == 2, "Saved preferences have the wrong schema")
            require(saved == expected_settings, "Saved tool preferences differ from the visible final state")
        print("PASS separate pages, isolated options/queues, five-format batch, animation, continue/readjust", flush=True)
        reload_dir = artifacts / "02-preference-reload"
        reload_dir.mkdir()
        with IndependentRun(reload_dir, app, cwd, args.timeout, inputs=[], steps=[], save=False) as run:
            events = run.complete()
            home(one(events, "ready"), 0)
            require(one(events, "settings-loaded")["settings"] == expected_settings,
                    "Preferences did not reload independently")
            run.close()
            events = run.exited()
            require(not any(event.get("event") == "settings-saved" for event in events),
                    "Read-only diagnostic reload unexpectedly saved preferences")
        require(not list((app / "Data").glob(".settings-*.tmp")), "Settings left temporary files")
        require(not (cwd / "Output").exists(), "Portable outputs were written under caller CWD")
        report["status"] = "passed"
        report["preferences_round_trip"] = True
        report["capture_directory"] = str(primary / "capture")
        print("PASS schema-2 save/reload and targeted clean GUI exits", flush=True)
    except BaseException as error:
        caught_error = error
        report.update(status="failed", error=str(error), traceback=traceback.format_exc())
    finally:
        report["finished_utc"] = dt.datetime.now(dt.timezone.utc).isoformat()
        try:
            report["candidate_unchanged"] = portable.tree_snapshot(candidate) == baseline
            report["profile_unchanged"] = {
                name: portable.tree_snapshot(path) == before_profiles[name]
                for name, path in profiles.items()
            }
            report["originals_unchanged"] = fixtures is not None and all(
                portable.digest(artifacts / "fixtures" / name) == metadata["sha256"]
                for name, metadata in fixtures.items()
            )
            require(report["candidate_unchanged"] and all(report["profile_unchanged"].values())
                    and report["originals_unchanged"], "Candidate, app profile, or original hashes changed")
        except BaseException as error:
            report.update(status="failed", integrity_error=str(error))
        write_json(artifacts / "verification.json", report)
        print(str(artifacts / "verification.json"), flush=True)
    if caught_error is not None:
        raise caught_error
    require(report["status"] == "passed", report.get("integrity_error", "Verification failed"))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--portable-dir", type=Path, required=True)
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--timeout", type=float, default=120, help="Per-event GUI wait timeout, seconds (default: 120)")
    args = parser.parse_args()
    if not 10 <= args.timeout <= 300:
        parser.error("--timeout must be between 10 and 300 seconds")
    try:
        verify(args)
    except (Exception, KeyboardInterrupt) as error:
        print(f"FAIL: {error}", file=sys.stderr)
        return 1
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
