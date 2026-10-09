#!/usr/bin/env python3
"""Sample an executable and observed descendants; defaults to a 100 ms interval.

Windows: rss = working set, private_bytes = process private commit (Private Bytes).
Linux/macOS: rss is reported; Windows Private Bytes is intentionally unavailable.
Requires psutil==7.2.2. A project-local interpreter is installed at
.tools/metrics-venv/Scripts/python.exe on the development Windows machine.

Examples:
  python scripts/measure-process.py --json run.json --csv run.csv -- lit.exe ...
  python scripts/measure-process.py --duration 15 --warmup 5 --json idle.json -- app.exe
  python scripts/measure-process.py --pid 1234 --duration 15 --csv attached.csv

Duration limits observation, not the target lifetime. Explicit --terminate-after
can stop a tree launched by this script; it is not allowed for attached processes.
JSON is written to stdout and optionally a new file. Workload output goes to
stderr unless --workload-output is specified. Output files are never overwritten.
"""

import argparse
import csv
import datetime as dt
import json
import os
from pathlib import Path
import platform
import statistics
import subprocess
import sys
import time

try:
    import psutil
except ImportError:
    raise SystemExit("psutil is required: use .tools/metrics-venv/Scripts/python.exe, or install psutil==7.2.2 in a local virtual environment.")


def positive(value):
    number = float(value)
    if number <= 0:
        raise argparse.ArgumentTypeError("must be greater than zero")
    return number


def output_file(path):
    if path is None:
        return None
    path.parent.mkdir(parents=True, exist_ok=True)
    return path.open("x", encoding="utf-8", newline="")


def run(args):
    windows = sys.platform == "win32"
    command = args.command[1:] if args.command[:1] == ["--"] else args.command
    if bool(args.pid) == bool(command):
        raise ValueError("Specify either --pid or a command after --, but not both.")
    if args.terminate_after and (args.pid or not args.duration):
        raise ValueError("--terminate-after requires a launched command and --duration.")
    if args.warmup < 0:
        raise ValueError("--warmup cannot be negative.")

    csv_file = json_file = workload_file = None
    try:
        csv_file = output_file(args.csv)
        json_file = output_file(args.json)
        workload_file = output_file(args.workload_output)
        started_utc = dt.datetime.now(dt.timezone.utc).isoformat()
        started = time.perf_counter()
        if command:
            kwargs = dict(cwd=args.cwd, stdin=subprocess.DEVNULL,
                          stdout=workload_file or sys.stderr,
                          stderr=workload_file or sys.stderr)
            if windows:
                kwargs["creationflags"] = subprocess.CREATE_NO_WINDOW
                if args.hidden:
                    startup = subprocess.STARTUPINFO()
                    startup.dwFlags |= subprocess.STARTF_USESHOWWINDOW
                    startup.wShowWindow = subprocess.SW_HIDE
                    kwargs["startupinfo"] = startup
            root = psutil.Popen(command, **kwargs)
        else:
            root = psutil.Process(args.pid)

        known = {}
        per_process = {}
        errors = {}

        def remember(process):
            try:
                key = (process.pid, process.create_time())
            except psutil.Error:
                return
            known.setdefault(key, process)
            per_process.setdefault(key, dict(
                pid=key[0], creation_time_unix=key[1],
                measured_sample_count=0,
                sampled_peak_rss_bytes=0,
                sampled_peak_windows_private_bytes=0 if windows else None,
                os_peak_working_set_bytes=None,
                os_peak_pagefile_usage_bytes=None))

        remember(root)
        root_pid = root.pid
        root_exit_code = None
        root_exit_elapsed = None
        samples = []
        interval = args.interval_ms / 1000
        next_sample = started
        stop_reason = "tree_exited"
        interrupted = False
        fields = ["elapsed_seconds", "process_count", "rss_bytes",
                  "windows_private_bytes", "root_rss_bytes",
                  "root_windows_private_bytes", "sample_read_errors"]
        writer = csv.DictWriter(csv_file, fieldnames=fields) if csv_file else None
        if writer:
            writer.writeheader()

        try:
            while True:
                # Retain already-observed descendants even after their parent exits.
                live_known = {p.pid: p for p in known.values() if p.is_running()}
                for process in list(live_known.values()):
                    try:
                        # Walk each living tree root once rather than rescanning
                        # the entire process table for every known descendant.
                        if process.pid == root_pid or process.ppid() not in live_known:
                            for child in process.children(recursive=True):
                                remember(child)
                    except psutil.AccessDenied:
                        errors["discovery_access_denied"] = errors.get("discovery_access_denied", 0) + 1
                    except psutil.Error:
                        pass

                row = dict(elapsed_seconds=round(time.perf_counter() - started, 6),
                           process_count=0, rss_bytes=0,
                           windows_private_bytes=0 if windows else None,
                           root_rss_bytes=0,
                           root_windows_private_bytes=0 if windows else None,
                           sample_read_errors=0)
                live = []
                for key, process in list(known.items()):
                    try:
                        if not process.is_running() or process.status() == psutil.STATUS_ZOMBIE:
                            continue
                        live.append(process)
                        memory = process.memory_info()
                        rss = memory.rss
                        private = getattr(memory, "private", None) if windows else None
                        row["process_count"] += 1
                        row["rss_bytes"] += rss
                        if windows and private is not None:
                            row["windows_private_bytes"] += private
                        if process.pid == root_pid:
                            row["root_rss_bytes"] = rss
                            row["root_windows_private_bytes"] = private
                        record = per_process[key]
                        record["measured_sample_count"] += 1
                        record["sampled_peak_rss_bytes"] = max(record["sampled_peak_rss_bytes"], rss)
                        if private is not None:
                            record["sampled_peak_windows_private_bytes"] = max(record["sampled_peak_windows_private_bytes"], private)
                        for attribute, field in [("peak_wset", "os_peak_working_set_bytes"),
                                                 ("peak_pagefile", "os_peak_pagefile_usage_bytes")]:
                            value = getattr(memory, attribute, None)
                            if value is not None:
                                record[field] = max(record[field] or 0, value)
                    except (psutil.NoSuchProcess, psutil.ZombieProcess):
                        pass
                    except psutil.AccessDenied:
                        row["sample_read_errors"] += 1
                        errors["access_denied"] = errors.get("access_denied", 0) + 1

                root_alive = root.is_running()
                if isinstance(root, psutil.Popen):
                    root_exit_code = root.poll()
                    root_alive = root_exit_code is None
                elif not root_alive:
                    try:
                        root_exit_code = root.wait(timeout=0)
                    except psutil.Error:
                        pass
                if not root_alive and root_exit_elapsed is None:
                    root_exit_elapsed = round(time.perf_counter() - started, 6)

                # A final zero caused only by termination is not an idle-memory sample.
                if live or root_alive:
                    samples.append(row)
                    if writer:
                        writer.writerow(row)
                        csv_file.flush()
                if not root_alive and not live:
                    break
                if args.duration and time.perf_counter() - started >= args.duration:
                    stop_reason = "duration_reached"
                    break
                next_sample += interval
                now = time.perf_counter()
                if next_sample < now:
                    next_sample = now
                time.sleep(max(0, next_sample - now))
        except KeyboardInterrupt:
            stop_reason = "observer_interrupted"
            interrupted = True

        observation_seconds = round(time.perf_counter() - started, 6)
        terminated = False
        if args.terminate_after and stop_reason == "duration_reached":
            # Only this script's launched tree; never select by executable name.
            observed = [p for p in known.values() if p.pid != root_pid] + [root]
            for process in observed:
                try:
                    if process.is_running():
                        process.terminate()
                        terminated = True
                except psutil.Error:
                    pass
            psutil.wait_procs(observed, timeout=3)
            root_exit_code = root.poll()

        still_running = []
        for process in known.values():
            try:
                if process.is_running() and process.status() != psutil.STATUS_ZOMBIE:
                    still_running.append(process.pid)
            except psutil.Error:
                pass

        def aggregate(selected):
            result = {"sample_count": len(selected)}
            for field in ["rss_bytes", "windows_private_bytes"]:
                values = [row[field] for row in selected if row[field] is not None]
                result[field] = (dict(sampled_peak=max(values), minimum=min(values),
                                      mean=round(statistics.fmean(values), 2), last=values[-1])
                                 if values else None)
            return result

        spacings = [b["elapsed_seconds"] - a["elapsed_seconds"] for a, b in zip(samples, samples[1:])]
        report = dict(
            schema_version=1, started_utc=started_utc,
            platform=platform.platform(), psutil_version=psutil.__version__,
            root_pid=root_pid, command=command or None, attached=bool(args.pid),
            hidden_window_requested=bool(args.hidden),
            requested_interval_ms=args.interval_ms,
            actual_mean_interval_ms=round(statistics.fmean(spacings) * 1000, 3) if spacings else None,
            actual_max_interval_ms=round(max(spacings) * 1000, 3) if spacings else None,
            observation_seconds=observation_seconds,
            root_exit_observed_at_seconds=root_exit_elapsed,
            root_exit_code=root_exit_code, stop_reason=stop_reason,
            terminated_by_observer=terminated, still_running_pids=still_running,
            observed_process_count=len(known), errors=errors,
            all_samples=aggregate(samples),
            post_warmup=dict(warmup_seconds=args.warmup, **aggregate(
                [s for s in samples if s["elapsed_seconds"] >= args.warmup])),
            per_process_peaks=list(per_process.values()),
            metric_notes=[
                "rss_bytes is resident memory; on Windows it is Working Set, not Private Bytes.",
                "windows_private_bytes is Windows process private commit, not private resident working set; unavailable on other platforms.",
                "Tree totals sum memory read from observed live processes in each sample. Shared resident pages can be counted more than once.",
                "Tree peaks are maxima of sampled simultaneous totals; individual OS process high-water marks are reported separately and never summed as a tree peak.",
                "Processes or memory spikes entirely between samples can be missed. This is sampled measurement, not proof of an absolute memory cap.",
                "Sampling overhead, observer memory and dedicated GPU memory are not part of the reported target totals.",
                "Post-warmup means time filtering only; the operator must ensure the application is actually idle for an idle measurement.",
            ])
        encoded = json.dumps(report, ensure_ascii=False, indent=2) + "\n"
        if json_file:
            json_file.write(encoded)
        sys.stdout.write(encoded)
        return 130 if interrupted else 0
    finally:
        for file in (csv_file, json_file, workload_file):
            if file:
                file.close()


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--pid", type=int)
    parser.add_argument("--duration", type=positive, help="Observation duration in seconds; default: wait for observed tree to exit")
    parser.add_argument("--warmup", type=float, default=0, help="Exclude initial seconds only from post_warmup statistics")
    parser.add_argument("--interval-ms", type=positive, default=100)
    parser.add_argument("--json", type=Path)
    parser.add_argument("--csv", type=Path)
    parser.add_argument("--workload-output", type=Path)
    parser.add_argument("--cwd")
    parser.add_argument("--hidden", action="store_true", help="Request a hidden window on Windows; record separately from visible-GUI tests")
    parser.add_argument("--terminate-after", action="store_true", help="Explicitly terminate the launched tree after the duration; may discard unsaved work")
    parser.add_argument("command", nargs=argparse.REMAINDER)
    args = parser.parse_args()
    if hasattr(sys.stdout, "reconfigure"):
        sys.stdout.reconfigure(encoding="utf-8")
    try:
        return run(args)
    except (OSError, ValueError, psutil.Error) as error:
        parser.error(str(error))


if __name__ == "__main__":
    sys.exit(main())
