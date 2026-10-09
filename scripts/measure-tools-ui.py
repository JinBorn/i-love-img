#!/usr/bin/env python3
r"""Measure the normal visible portable GUI, preserving all evidence.

    python scripts/measure-tools-ui.py \
        --portable-dir dist/candidate --artifacts artifacts/tools-ui-memory \
        --source artifacts/benchmark-input

Requires Windows and psutil==7.2.2. The artifacts directory must be NEW.
Home and the three empty tool pages run sequentially; --source adds a 1000-file
Compress batch. No GUI flags, hidden-window requests, or renderer overrides are
used. Settings saves are disabled. Only this script's verified subprocess tree
can be terminated on failure; successful measurements require a natural exit.
"""

import argparse
import csv
import datetime as dt
import importlib.util
import os
from pathlib import Path
import shutil
import statistics
import subprocess
import sys
import time
import traceback


sys.dont_write_bytecode = True
_spec = importlib.util.spec_from_file_location(
    "portable_verifier", Path(__file__).with_name("verify-portable.py")
)
portable = importlib.util.module_from_spec(_spec)
_spec.loader.exec_module(portable)
require = portable.require
write_json = portable.write_json
MIB = 1024 * 1024
EXTENSIONS = {".jpg", ".jpeg", ".png", ".webp", ".gif"}
DEFAULT_COMPRESS = {
    "lossless": False, "jpeg_quality": 85, "webp_quality": 80,
    "strip_metadata": True,
}


def utc_now():
    return dt.datetime.now(dt.timezone.utc).isoformat()


def child_environment(config_path):
    # Never mutate os.environ or persist changes to the user's environment.
    removed = sorted(key for key in os.environ if key.upper().startswith(
        ("LIT_", "LOCAL_IMAGE_TOOLS_DIAGNOSTICS")))
    environment = {key: value for key, value in os.environ.items() if key not in removed}
    environment["LOCAL_IMAGE_TOOLS_DIAGNOSTICS"] = str(config_path)
    return environment, removed


class OwnedTree:
    """Track descendants by PID plus creation time, never by process name."""

    def __init__(self, process, psutil):
        self.psutil = psutil
        self.root = process
        self.known = {}
        self.remember(process)

    def remember(self, process):
        try:
            self.known.setdefault((process.pid, process.create_time()), process)
        except self.psutil.NoSuchProcess:
            pass

    def live(self):
        living = []
        for (pid, created), process in list(self.known.items()):
            try:
                if (process.is_running() and process.create_time() == created
                        and process.status() != self.psutil.STATUS_ZOMBIE):
                    living.append(process)
            except self.psutil.NoSuchProcess:
                pass
        return living

    def discover(self):
        living = self.live()
        ids = {process.pid for process in living}
        for process in living:
            try:
                if process.pid == self.root.pid or process.ppid() not in ids:
                    for child in process.children(recursive=True):
                        self.remember(child)
            except self.psutil.NoSuchProcess:
                pass

    def cleanup(self):
        record = {"attempted": True, "terminated": [], "errors": []}
        try:
            self.discover()
        except self.psutil.Error as error:
            record["errors"].append(str(error))
        # Retained Process objects verify creation time before signaling on
        # Windows. Terminate children first, keeping the sampler alive briefly
        # so it can record the failed workload's exit and flush its evidence.
        for sampler in (False, True):
            targets = [p for p in self.live() if (p.pid == self.root.pid) == sampler]
            for process in targets:
                try:
                    process.terminate()
                    record["terminated"].append({
                        "pid": process.pid, "creation_time_unix": process.create_time(),
                    })
                except self.psutil.Error as error:
                    record["errors"].append(f"PID {process.pid}: {error}")
            self.psutil.wait_procs(targets, timeout=3)
        record["still_running_pids"] = [process.pid for process in self.live()]
        return record

    def evidence(self):
        return [{"pid": pid, "creation_time_unix": created}
                for pid, created in sorted(self.known)]


def run_sampler(command, environment, folder, timeout, psutil, record):
    tree = None
    try:
        with (folder / "sampler.stdout.log").open("x", encoding="utf-8") as stdout, \
                (folder / "sampler.stderr.log").open("x", encoding="utf-8") as stderr:
            process = psutil.Popen(
                command, cwd=folder, env=environment, stdin=subprocess.DEVNULL,
                stdout=stdout, stderr=stderr,
            )
            record["sampler_pid"] = process.pid
            tree = OwnedTree(process, psutil)
            started = time.monotonic()
            next_progress = started + 30
            while True:
                tree.discover()
                result = process.poll()
                if result is not None:
                    break
                elapsed = time.monotonic() - started
                if elapsed > timeout:
                    raise TimeoutError(f"{folder.name}: exceeded {timeout:g} seconds")
                if time.monotonic() >= next_progress:
                    print(f"{folder.name}: measuring ({elapsed:.0f} s)", flush=True)
                    next_progress += 30
                time.sleep(0.1)
            record["sampler_exit_code"] = result
            record["wrapper_elapsed_seconds"] = round(time.monotonic() - started, 3)
            require(result == 0, f"Sampler exited {result}; see {folder / 'sampler.stderr.log'}")
            require(not tree.live(), "Sampler exited while its owned process tree was still running")
            record["cleanup"] = {"attempted": False, "still_running_pids": []}
    except BaseException:
        if tree is not None:
            record["cleanup"] = tree.cleanup()
        raise
    finally:
        if tree is not None:
            record["owned_processes"] = tree.evidence()


def metric(values):
    require(bool(values), "No memory samples in the selected interval")
    mean, peak = statistics.fmean(values), max(values)
    return {
        "mean_bytes": round(mean, 2), "sampled_peak_bytes": peak,
        "mean_mib": round(mean / MIB, 3), "sampled_peak_mib": round(peak / MIB, 3),
    }


def memory_summary(rows):
    require(bool(rows), "No samples selected")
    return {
        "sample_count": len(rows),
        "first_elapsed_seconds": rows[0]["elapsed_seconds"],
        "last_elapsed_seconds": rows[-1]["elapsed_seconds"],
        "tree_working_set": metric([row["rss_bytes"] for row in rows]),
        "tree_private_bytes": metric([row["windows_private_bytes"] for row in rows]),
        "gui_working_set": metric([row["root_rss_bytes"] for row in rows]),
        "gui_private_bytes": metric([row["root_windows_private_bytes"] for row in rows]),
    }


def load_samples(path):
    with path.open(encoding="utf-8", newline="") as stream:
        return [{key: float(value) if key == "elapsed_seconds" else int(value)
                 for key, value in row.items()} for row in csv.DictReader(stream)]


def read_events(path, root_pid):
    import json
    events = [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines() if line]
    return [event for event in events
            if event.get("process_id") == root_pid and event.get("process_role") == "parent"]


def only_event(events, name):
    matches = [event for event in events if event.get("event") == name]
    require(len(matches) == 1, f"Expected one parent {name} event; found {len(matches)}")
    return matches[0]


def analyze(folder, app, tool, batch, record):
    report = portable.read_json(folder / "memory.json")
    record["sampler_report"] = report
    require(report["root_exit_code"] == 0, "GUI did not exit successfully")
    require(report["terminated_by_observer"] is False, "Sampler terminated the GUI")
    require(report["still_running_pids"] == [], "Sampler left living workload processes")
    require(report["stop_reason"] == "tree_exited", "Observed tree did not exit naturally")
    require(report["hidden_window_requested"] is False and report["attached"] is False,
            "Measurement was hidden or attached")
    require(not report["errors"], f"Sampler errors: {report['errors']}")
    events = read_events(folder / "capture" / "run.jsonl", report["root_pid"])
    ready = only_event(events, "ready")
    resources = only_event(events, "ui-resources")
    record["renderer"] = only_event(events, "renderer")
    record["ui_resources"] = resources
    record["complete"] = only_event(events, "complete")
    record["ready_row_count"] = len(ready["rows"])
    record["ready_settings"] = ready["settings"]
    record["ready_options"] = ready["options"]
    record["ready_page"] = ready["page"]
    record["ready_tool"] = ready["tool"]
    require(ready["tool"] == tool, f"Wrong tool: {ready['tool']}")
    require(ready["page"] == ("home" if tool is None else "tool"), "Wrong landing page")
    require(ready["pending_input_count"] == 0, "Inputs still pending")
    require(ready["portable"] is True, "GUI is not in portable mode")
    require(ready["settings"]["language"] == "Chinese", "GUI did not use default Chinese")
    require(ready["settings"]["compress"] == DEFAULT_COMPRESS, "Compress defaults changed")
    require(portable.same_path(ready["resolved_output_directory"], app / "Output"),
            "Portable output directory is not inside the copied application")
    ppp = resources["pixels_per_point"]
    require(isinstance(ppp, (int, float)) and ppp > 0, "Missing actual pixels-per-point")
    screenshots = [event for event in events if event.get("event") == "screenshot"]
    require(len(screenshots) == (2 if batch else 1), "Wrong screenshot count")
    record["screenshots"] = []
    for event in screenshots:
        require(event["ok"] is True, f"Screenshot failed: {event}")
        path = Path(event["path"]).resolve()
        require(portable.within(path, folder / "capture") and path.is_file(),
                "Screenshot is missing or outside its scenario")
        size = portable.png_size(path)
        logical_size = [round(dimension / ppp, 3) for dimension in size]
        record["screenshots"].append({
            "path": str(path), "pixel_size": list(size), "logical_size": logical_size,
        })
        require(all(abs(actual - expected) <= 2 / ppp
                    for actual, expected in zip(logical_size, (1220, 820))),
                f"Expected normal 1220x820 window; observed {logical_size} at scale {ppp}")
    rows = load_samples(folder / "memory.csv")
    require(all(row["sample_read_errors"] == 0 for row in rows), "Memory sample read failed")
    if batch:
        before = only_event(events, "before-process")
        finished = only_event(events, "batch-finished")
        closed = only_event(events, "batch-closed")
        after = only_event(events, "after-process")
        successes = sum(row["state"] == "Success" for row in after["rows"])
        require(len(ready["rows"]) == len(before["rows"]) == len(after["rows"]) == 1000,
                "Batch did not import and process exactly 1000 rows")
        require(successes == finished["success"] == finished["total"] == 1000,
                f"Batch did not succeed for all 1000 inputs: {successes}")
        require(finished["cancelled"] is False and closed["ok"] is True, "Batch did not close successfully")
        for row in after["rows"]:
            output = Path(row["output"]).resolve()
            require(portable.within(output, app / "Output") and output.is_file(),
                    f"Batch output is missing or outside application/Output: {output}")
        selected = memory_summary(rows)
        record["batch"] = {
            "success_count": successes, "row_count": len(after["rows"]),
            "batch_duration_ms": finished["elapsed_ms"],
            "start_to_closed_ms": round(closed["diagnostic_elapsed_ms"] - before["diagnostic_elapsed_ms"], 3),
            "duration_note": "batch_duration_ms is the engine batch report, excluding screenshot and post-batch settle time",
        }
        threshold = 2048 * MIB
        record["memory"] = selected
        record["target"] = {"mib": 2048, "scope": "whole-tree simultaneous sampled peak",
                            "working_set_peak_within": selected["tree_working_set"]["sampled_peak_bytes"] <= threshold,
                            "private_bytes_peak_within": selected["tree_private_bytes"]["sampled_peak_bytes"] <= threshold}
    else:
        require(not ready["rows"] and all(count == 0 for count in ready["tool_queues"].values()),
                "Idle scenario has imported rows")
        selected_rows = [row for row in rows if 3 <= row["elapsed_seconds"] <= 8]
        require(len(selected_rows) >= 35, "Too few 100 ms idle samples between seconds 3 and 8")
        require(selected_rows[0]["elapsed_seconds"] <= 3.3 and selected_rows[-1]["elapsed_seconds"] >= 7.7,
                "Idle samples do not cover the intended 3–8 second interval")
        require(all(row["root_rss_bytes"] > 0 for row in selected_rows), "GUI absent from idle samples")
        selected = memory_summary(selected_rows)
        record["memory"] = selected
        threshold = 100 * MIB
        record["target"] = {"mib": 100, "scope": "whole-tree samples at 3–8 seconds after sampler launch",
                            "working_set_mean_within": selected["tree_working_set"]["mean_bytes"] <= threshold,
                            "working_set_peak_within": selected["tree_working_set"]["sampled_peak_bytes"] <= threshold,
                            "private_bytes_mean_within": selected["tree_private_bytes"]["mean_bytes"] <= threshold,
                            "private_bytes_peak_within": selected["tree_private_bytes"]["sampled_peak_bytes"] <= threshold}
    return selected


def scenario(artifacts, app, interpreter, sampler, psutil, name, tool, source, timeout, record):
    folder = artifacts / name
    require(portable.within(folder.resolve(), artifacts), "Scenario destination escaped artifacts")
    folder.mkdir()
    capture = folder / "capture"
    capture.mkdir()
    batch = source is not None
    config = {
        "input_paths": [str(source)] if batch else [],
        "steps": [{"action": "process", "tool": "compress"}] if batch else [],
        "capture_directory": str(capture), "language": "zh",
        "idle_ms": 2000 if batch else 9000, "settle_ms": 6500 if batch else 800,
        "exit_when_done": True, "use_saved_settings": True, "save_settings_on_exit": False,
    }
    if tool is not None:
        config["tool"] = tool
    config_path = folder / "diagnostics.json"
    write_json(config_path, config)
    environment, removed = child_environment(config_path)
    command = [str(interpreter), str(sampler), "--interval-ms", "10" if batch else "100",
               "--json", str(folder / "memory.json"), "--csv", str(folder / "memory.csv"),
               "--workload-output", str(folder / "workload.log"), "--cwd", str(app),
               "--", str(app / portable.EXE_NAME)]
    record.update(name=name, status="running", started_utc=utc_now(), command=command,
                  diagnostics_config=config, removed_child_environment_keys=removed,
                  artifacts=str(folder), timeout_seconds=timeout)
    try:
        require(not (app / "Data").exists(), "Copied app acquired Data before scenario")
        print(f"{name}: starting normal visible GUI", flush=True)
        run_sampler(command, environment, folder, timeout, psutil, record)
        memory = analyze(folder, app, tool, batch, record)
        require(not (app / "Data").exists(), "Settings saves were disabled, but Data was created")
        record["status"] = "passed"
        ws, private = memory["tree_working_set"], memory["tree_private_bytes"]
        print(f"{name}: {memory['sample_count']} samples; Working Set mean/max "
              f"{ws['mean_mib']:.2f}/{ws['sampled_peak_mib']:.2f} MiB; Private Bytes mean/max "
              f"{private['mean_mib']:.2f}/{private['sampled_peak_mib']:.2f} MiB; "
              f"scale {record['ui_resources']['pixels_per_point']}; "
              f"renderer {record['renderer']['renderer']}" +
              (f"; {record['batch']['success_count']} successes in {record['batch']['batch_duration_ms'] / 1000:.3f} s"
               if batch else ""), flush=True)
    except BaseException as error:
        record.update(status="failed", error=f"{type(error).__name__}: {error}", traceback=traceback.format_exc())
        raise
    finally:
        record["finished_utc"] = utc_now()
        write_json(folder / "scenario.json", record)


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--portable-dir", required=True, type=Path)
    parser.add_argument("--artifacts", required=True, type=Path)
    parser.add_argument("--source", type=Path, help="Optional existing directory with exactly 1000 supported image files")
    parser.add_argument("--idle-timeout", type=float, default=120)
    parser.add_argument("--batch-timeout", type=float, default=1800)
    args = parser.parse_args()
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    require(sys.platform == "win32", "This script measures Windows Working Set and Private Bytes")
    try:
        import psutil
    except ImportError:
        raise SystemExit("Install measurement dependencies in this interpreter: python -m pip install -r requirements-dev.txt")
    require(psutil.__version__ == "7.2.2", "This measurement requires psutil==7.2.2")
    interpreter = Path(sys.executable)
    sampler = Path(__file__).resolve().with_name("measure-process.py")
    require(interpreter.is_file() and sampler.is_file(), "Missing sampler or current Python interpreter")
    candidate, artifacts = args.portable_dir.resolve(), args.artifacts.resolve()
    source = args.source.resolve() if args.source else None
    require(candidate.is_dir() and (candidate / portable.EXE_NAME).is_file(), "Missing candidate executable")
    require((candidate / "portable.flag").is_file(), "Candidate is not a portable package")
    require(not (candidate / "Data").exists(), "Candidate must be pristine: Data already exists")
    require(not artifacts.exists(), "Artifacts directory must be NEW; nothing will be overwritten")
    require(not portable.within(artifacts, candidate), "Artifacts cannot be inside the candidate")
    require(args.idle_timeout > 9 and args.batch_timeout > 9, "Timeouts must exceed diagnostic idle time")
    if source:
        require(source.is_dir(), "Source must be an existing directory")
        require(not portable.within(artifacts, source), "Artifacts cannot be inside the input tree")
        count = sum(path.is_file() and path.suffix.lower() in EXTENSIONS for path in source.rglob("*"))
        require(count == 1000, f"Expected 1000 supported source images, found {count}")
    profiles = portable.profile_roots()
    guarded = {"candidate": candidate, **{f"profile-{name}": path for name, path in profiles.items()}}
    if source:
        guarded["source"] = source
    for name, root in guarded.items():
        require(not portable.within(artifacts, root), f"Artifacts cannot be inside guarded {name}")
    artifacts.mkdir(parents=True)
    summary = {
        "schema_version": 1, "started_utc": utc_now(), "status": "running",
        "candidate": str(candidate), "application": str(artifacts / "application"),
        "source": str(source) if source else None,
        "expected_window_logical_size": [1220, 820], "scenarios": [], "integrity": {},
        "metric_notes": [
            "Working Set is resident process memory; Private Bytes is private commit, not private resident memory.",
            "Tree totals are simultaneous observed process sums; shared resident pages can be counted more than once.",
            "Peaks or short-lived processes between samples may be missed; thresholds are sampled observations, not absolute caps.",
            "Observer overhead and dedicated GPU memory are excluded. Diagnostic screenshots are retained; batch peaks include capture/settle overhead.",
            "Idle statistics use 3–8 seconds from sampler launch, before the ready screenshot at 9 seconds; actual scale and renderer are recorded.",
            "Scenario status validates the run; target booleans separately compare observed memory with 100 MiB idle / 2048 MiB batch targets.",
        ],
    }
    before = {}
    try:
        for name, root in guarded.items():
            before[name] = portable.tree_snapshot(root)
            write_json(artifacts / f"{name}.before.json", before[name])
        app = artifacts / "application"
        require(portable.within(app.resolve(), artifacts) and not app.exists(), "Unsafe application copy destination")
        shutil.copytree(candidate, app)
        require(portable.tree_snapshot(app) == before["candidate"], "Application copy differs from candidate")
        summary["candidate_executable_sha256"] = portable.digest(app / portable.EXE_NAME)
        for name, tool in [("idle-home", None), ("idle-compress", "compress"),
                           ("idle-resize", "resize"), ("idle-convert", "convert")]:
            record = {}
            summary["scenarios"].append(record)
            scenario(artifacts, app, interpreter, sampler, psutil, name, tool, None, args.idle_timeout, record)
        if source:
            record = {}
            summary["scenarios"].append(record)
            scenario(artifacts, app, interpreter, sampler, psutil, "batch-compress-1000", "compress",
                     source, args.batch_timeout, record)
        summary["status"] = "passed"
    except BaseException as error:
        summary.update(status="failed", error=f"{type(error).__name__}: {error}", traceback=traceback.format_exc())
    finally:
        for name, snapshot in before.items():
            try:
                after = portable.tree_snapshot(guarded[name])
                write_json(artifacts / f"{name}.after.json", after)
                unchanged = after == snapshot
                summary["integrity"][name] = {"path": str(guarded[name]), "unchanged": unchanged}
                if not unchanged:
                    summary["status"] = "failed"
            except BaseException as error:
                summary["integrity"][name] = {"error": str(error), "unchanged": False}
                summary["status"] = "failed"
        summary["copied_application_data_absent"] = not (artifacts / "application" / "Data").exists()
        if not summary["copied_application_data_absent"]:
            summary["status"] = "failed"
        summary["finished_utc"] = utc_now()
        write_json(artifacts / "summary.json", summary)
    print(f"{summary['status']}: {artifacts / 'summary.json'}", flush=True)
    if summary.get("error"):
        print(summary["error"], file=sys.stderr)
    return 0 if summary["status"] == "passed" else 1


if __name__ == "__main__":
    sys.exit(main())
