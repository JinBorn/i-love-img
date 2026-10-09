#!/usr/bin/env python3
"""Verify a pristine Windows portable package through its native GUI diagnostics.

    python scripts/verify-portable.py --portable-dir "dist/extracted/Local Image Tools" \
        --artifacts artifacts/portable-gui --with-subst

The artifacts directory must not exist and must be outside the candidate. All
scenarios copy the entire candidate; original binaries and user settings are
never seeded, edited, or restored. Only spawned GUI PIDs receive WM_CLOSE or,
on failure/timeout, terminate(). Screenshots/logs/copies remain in artifacts.
Requires Windows and Python 3.10+. No third-party Python packages are needed.
"""

import argparse
from contextlib import contextmanager
import ctypes
from ctypes import wintypes
import datetime as dt
import hashlib
import json
import ntpath
import os
from pathlib import Path
import shutil
import struct
import subprocess
import sys
import time
import traceback
import uuid
import zlib


EXE_NAME = "local-image-tools.exe"


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def write_json(path, value):
    with path.open("x", encoding="utf-8") as stream:
        json.dump(value, stream, ensure_ascii=False, indent=2)
        stream.write("\n")


def read_json(path):
    return json.loads(path.read_text(encoding="utf-8"))


def digest(path):
    result = hashlib.sha256()
    with path.open("rb") as stream:
        for block in iter(lambda: stream.read(1024 * 1024), b""):
            result.update(block)
    return result.hexdigest()


def tree_snapshot(root):
    if not root.exists():
        return {"exists": False, "directories": [], "files": {}}
    return {
        "exists": True,
        "directories": sorted(p.relative_to(root).as_posix() for p in root.rglob("*") if p.is_dir()),
        "files": {
            p.relative_to(root).as_posix(): {"size": p.stat().st_size, "sha256": digest(p)}
            for p in sorted(root.rglob("*")) if p.is_file()
        },
    }


def normalized(value):
    """Lexical Windows comparison: deliberately never resolve SUBST drives."""
    value = os.fspath(value)
    if value.startswith("\\\\?\\UNC\\"):
        value = "\\\\" + value[8:]
    elif value.startswith("\\\\?\\"):
        value = value[4:]
    return ntpath.normcase(ntpath.normpath(value))


def same_path(actual, expected):
    return isinstance(actual, str) and normalized(actual) == normalized(expected)


def within(actual, root):
    try:
        return ntpath.commonpath([normalized(actual), normalized(root)]) == normalized(root)
    except ValueError:
        return False


def make_png(path):
    path.parent.mkdir(parents=True, exist_ok=True)

    def chunk(kind, data):
        return struct.pack(">I", len(data)) + kind + data + struct.pack(">I", zlib.crc32(kind + data))

    pixels = b"".join(
        b"\0" + bytes(channel for x in range(64) for channel in (x * 3, y * 5, (x + y) * 2, 255))
        for y in range(48)
    )
    path.write_bytes(
        b"\x89PNG\r\n\x1a\n"
        + chunk(b"IHDR", struct.pack(">IIBBBBB", 64, 48, 8, 6, 0, 0, 0))
        + chunk(b"IDAT", zlib.compress(pixels))
        + chunk(b"IEND", b"")
    )


def png_size(path):
    with path.open("rb") as stream:
        header = stream.read(24)
    require(header[:8] == b"\x89PNG\r\n\x1a\n", f"Output is not PNG: {path}")
    require(header[12:16] == b"IHDR", f"Output has no PNG IHDR: {path}")
    return struct.unpack(">II", header[16:24])


def hidden_options():
    startup = subprocess.STARTUPINFO()
    startup.dwFlags |= subprocess.STARTF_USESHOWWINDOW
    startup.wShowWindow = subprocess.SW_HIDE
    return {"startupinfo": startup, "creationflags": subprocess.CREATE_NO_WINDOW}


def profile_roots():
    """Use the same Windows known folders as the Rust directories crate."""
    shell = ctypes.WinDLL("shell32", use_last_error=True)
    ole = ctypes.WinDLL("ole32", use_last_error=True)
    shell.SHGetKnownFolderPath.argtypes = [ctypes.c_void_p, wintypes.DWORD, wintypes.HANDLE, ctypes.POINTER(ctypes.c_void_p)]
    shell.SHGetKnownFolderPath.restype = ctypes.c_long
    ole.CoTaskMemFree.argtypes = [ctypes.c_void_p]
    result = {}
    for name, identifier in {
        "roaming": "3EB685DB-65F9-4CF6-A03A-E3EF65729F3D",
        "local": "F1B32785-6FBA-4FCF-9D55-7B8E7F157091",
    }.items():
        guid = ctypes.create_string_buffer(uuid.UUID(identifier).bytes_le)
        pointer = ctypes.c_void_p()
        status = shell.SHGetKnownFolderPath(guid, 0, None, ctypes.byref(pointer))
        require(status == 0, f"Cannot read Windows {name} known folder: HRESULT {status:#x}")
        try:
            result[name] = Path(ctypes.wstring_at(pointer)) / "LocalImageTools"
        finally:
            ole.CoTaskMemFree(pointer)
    return result


def close_owned_window(process):
    require(process.poll() is None, f"GUI PID {process.pid} exited before WM_CLOSE")
    user = ctypes.WinDLL("user32", use_last_error=True)
    callback_type = ctypes.WINFUNCTYPE(wintypes.BOOL, wintypes.HWND, wintypes.LPARAM)
    user.EnumWindows.argtypes = [callback_type, wintypes.LPARAM]
    user.GetWindowThreadProcessId.argtypes = [wintypes.HWND, ctypes.POINTER(wintypes.DWORD)]
    user.GetWindowTextW.argtypes = [wintypes.HWND, wintypes.LPWSTR, ctypes.c_int]
    user.PostMessageW.argtypes = [wintypes.HWND, wintypes.UINT, wintypes.WPARAM, wintypes.LPARAM]
    user.PostMessageW.restype = wintypes.BOOL
    windows = []

    @callback_type
    def visit(window, _):
        pid = wintypes.DWORD()
        user.GetWindowThreadProcessId(window, ctypes.byref(pid))
        if pid.value == process.pid:
            title = ctypes.create_unicode_buffer(1024)
            user.GetWindowTextW(window, title, len(title))
            if "Local Image Tools" in title.value:
                windows.append((window, title.value))
        return True

    user.EnumWindows(visit, 0)
    require(len(windows) == 1, f"Expected one application window for PID {process.pid}, got {windows}")
    window, title = windows[0]
    pid = wintypes.DWORD()
    user.GetWindowThreadProcessId(window, ctypes.byref(pid))
    require(process.poll() is None and pid.value == process.pid, "GUI ownership changed before WM_CLOSE")
    require(user.PostMessageW(window, 0x0010, 0, 0), f"WM_CLOSE failed: {ctypes.get_last_error()}")
    return {"pid": process.pid, "hwnd": int(window), "title": title, "message": "WM_CLOSE"}


@contextmanager
def lock_settings(path):
    kernel = ctypes.WinDLL("kernel32", use_last_error=True)
    kernel.CreateFileW.argtypes = [wintypes.LPCWSTR, wintypes.DWORD, wintypes.DWORD, ctypes.c_void_p, wintypes.DWORD, wintypes.DWORD, wintypes.HANDLE]
    kernel.CreateFileW.restype = wintypes.HANDLE
    kernel.CloseHandle.argtypes = [wintypes.HANDLE]
    # Allow reading; deny write/delete sharing so MoveFileEx replacement fails.
    handle = kernel.CreateFileW(str(path), 0x80000000, 1, None, 3, 0x80, None)
    require(handle != ctypes.c_void_p(-1).value, f"Cannot lock scenario settings: {ctypes.get_last_error()}")
    try:
        yield
    finally:
        kernel.CloseHandle(handle)


class GuiRun:
    def __init__(self, folder, app, cwd, timeout, *, use_saved=True, save_on_exit=True, output=None):
        self.folder = folder
        self.app = app  # Preserve a mapped drive; never Path.resolve() here.
        self.cwd = cwd
        self.timeout = timeout
        self.process = None
        self.handles = []
        self.record = {"application": str(app), "cwd": str(cwd), "wm_close": []}
        self.capture = folder / "capture"
        self.capture.mkdir()
        self.config = {
            "tool": "resize",
            "input_paths": ["inputs/sample.png"],
            "capture_directory": str(self.capture),
            "steps": [{"action": "process", "tool": "resize", "resize_percent": 50.0}],
            "idle_ms": 1500,
            "settle_ms": 300,
            # The verifier validates portability before explicitly asking to close.
            "exit_when_done": False,
        }
        if use_saved or save_on_exit:
            self.config.update(use_saved_settings=use_saved, save_settings_on_exit=save_on_exit)
        if output is not None:
            self.config["output_directory"] = str(output)
        self.config_path = folder / "diagnostics.json"
        write_json(self.config_path, self.config)

    def events(self):
        path = self.capture / "run.jsonl"
        if not path.exists():
            return []
        lines = path.read_bytes().splitlines(keepends=True)
        result = []
        for line in lines:
            if not line.endswith(b"\n"):
                continue  # The active writer may still be flushing the final record.
            event = json.loads(line)
            if event.get("process_id") == self.process.pid and event.get("process_role") == "parent":
                result.append(event)
        return result

    def wait_for(self, predicate, description):
        deadline = time.monotonic() + self.timeout
        while time.monotonic() < deadline:
            events = self.events()
            if predicate(events):
                return events
            require(self.process.poll() is None, f"GUI exited ({self.process.returncode}) while waiting for {description}")
            time.sleep(0.1)
        raise TimeoutError(f"PID {self.process.pid}: timed out waiting for {description}")

    def __enter__(self):
        environment = os.environ.copy()
        for key in list(environment):
            if key.startswith("LIT_") or key.startswith("LOCAL_IMAGE_TOOLS_DIAGNOSTICS"):
                environment.pop(key)
        environment["LOCAL_IMAGE_TOOLS_DIAGNOSTICS"] = str(self.config_path)
        system = Path(os.environ["SystemRoot"])
        environment["PATH"] = os.pathsep.join(str(p) for p in (system / "System32", system, system / "System32/Wbem"))
        executable = self.app / EXE_NAME
        require(executable.is_file(), f"Application executable is missing: {executable}")
        require((self.app / "portable.flag").is_file(), "Scenario portable marker is missing")
        try:
            self.handles = [(self.folder / name).open("xb") for name in ("stdout.txt", "stderr.txt")]
            self.process = subprocess.Popen(
                [str(executable)], executable=str(executable), cwd=str(self.cwd), env=environment,
                stdin=subprocess.DEVNULL, stdout=self.handles[0], stderr=self.handles[1], **hidden_options(),
            )
            self.record.update(pid=self.process.pid, executable=str(executable), system_only_path=environment["PATH"])
            loaded_events = self.wait_for(lambda events: any(e.get("event") == "settings-loaded" for e in events), "settings-loaded")
            loaded = next(e for e in loaded_events if e.get("event") == "settings-loaded")
            require(loaded.get("portable") is True, "GUI did not select portable mode; stopping before requested exit/save")
            require(same_path(loaded.get("settings_path"), self.app / "Data/settings.json"), f"Wrong settings path: {loaded.get('settings_path')}")
            return self
        except BaseException:
            self.__exit__(*sys.exc_info())
            raise

    def complete(self):
        return self.wait_for(lambda events: any(e.get("event") == "complete" for e in events), "native diagnostic completion")

    def close(self):
        self.record["wm_close"].append(close_owned_window(self.process))

    def exited(self):
        try:
            code = self.process.wait(timeout=self.timeout)
        except subprocess.TimeoutExpired as error:
            raise TimeoutError(f"PID {self.process.pid} did not exit after WM_CLOSE") from error
        require(code == 0, f"GUI exited with code {code}")
        events = self.events()
        self.record.update(exit_code=code, events=events)
        return events

    def __exit__(self, kind, value, trace):
        if self.process is not None:
            if self.process.poll() is None:
                self.record["terminated_only_spawned_pid"] = self.process.pid
                self.process.terminate()
                self.process.wait(timeout=10)
            self.record["exit_code"] = self.process.returncode
            self.record["events"] = self.events()
        for handle in self.handles:
            handle.close()
        write_json(self.folder / "run-result.json", self.record)


def find_event(events, name):
    matches = [event for event in events if event.get("event") == name]
    require(len(matches) == 1, f"Expected one {name} event, received {len(matches)}")
    return matches[0]


def assert_processed(events, app, cwd, expected_output):
    ready = find_event(events, "ready")
    after = find_event(events, "after-process")
    for snapshot in (ready, after):
        require(snapshot.get("portable") is True, "Snapshot does not report portable mode")
        require(same_path(snapshot.get("resolved_output_directory"), expected_output), f"Output resolved incorrectly: {snapshot.get('resolved_output_directory')}")
        require(len(snapshot["rows"]) == 1, f"Expected one imported image: {snapshot['rows']}")
        require(same_path(snapshot["rows"][0]["original"], cwd / "inputs/sample.png"), "Relative input was not resolved against caller CWD")
        require(snapshot["options"]["format"] is None, "Resize unexpectedly inherited another tool's conversion format")
        require(snapshot["options"]["compression"] == "Auto", "Resize unexpectedly inherited lossless compression mode")
        require(snapshot["options"]["gif_first_frame"] is False, "Resize unexpectedly inherited first-frame export")
    row = after["rows"][0]
    require(row["state"] == "Success", f"Processing failed: {row}")
    # Batch commits are canonicalized by the image core. Resolve only these
    # existing filesystem objects so a physical E: path can match a SUBST Z:
    # destination. Settings and GUI-resolved paths above stay lexically strict.
    committed_output = Path(row["output"]).resolve(strict=True)
    physical_destination = expected_output.resolve(strict=True)
    require(within(committed_output, physical_destination), f"Output escaped requested directory: {row['output']}")
    require(png_size(Path(row["output"])) == (32, 24), "Independent output dimensions do not match requested resize")
    require(not (cwd / "Output").exists(), "Portable Output was incorrectly created under caller CWD")
    require(not (cwd / "LocalImageTools-output").exists(), "Input-derived output was incorrectly created under caller CWD")
    require(not any((app / "Data").glob(".settings-*.tmp")), "A temporary settings file was left behind")


def assert_saved(events, physical_app, expected_output, *, quality=None):
    saves = [event for event in events if event.get("event") == "settings-saved"]
    require(saves and saves[-1].get("ok") is True, "No successful settings-saved event")
    settings = read_json(physical_app / "Data/settings.json")
    require(settings.get("schema_version") == 2, "Settings were not saved using schema version 2")
    require(all(isinstance(settings.get(tool), dict) for tool in ("compress", "resize", "convert")), "Typed tool preferences are missing")
    require("jpeg_quality" not in settings and "resize_mode" not in settings, "Legacy operational fields remained at the settings root")
    require(normalized(settings["output_directory"]) == normalized(expected_output), f"Incorrect persisted output: {settings['output_directory']}")
    if quality is not None:
        require(settings["language"] == "English" and settings["resize"]["jpeg_quality"] == quality, "Language/Resize quality preferences were not preserved")
    require(sorted(p.name for p in (physical_app / "Data").iterdir()) == ["settings.json"], "Unexpected persistent files in Data")
    return settings


def copy_scenario(artifacts, name, source):
    folder = artifacts / name
    require(within(folder.resolve(), artifacts), "Scenario directory escapes artifacts")
    folder.mkdir()
    app = folder / "Local Image Tools"
    require(within(app.resolve(), artifacts), "Copy destination escapes artifacts")
    shutil.copytree(source, app)
    cwd = folder / "unrelated-cwd"
    cwd.mkdir()
    make_png(cwd / "inputs/sample.png")
    return folder, app, cwd


def seed(app, output, quality=73):
    (app / "Data").mkdir()
    write_json(app / "Data/settings.json", {
        "schema_version": 2,
        "language": "English", "output_directory": str(output),
        "compress": {"lossless": True, "jpeg_quality": quality - 1, "webp_quality": 64},
        "resize": {"jpeg_quality": quality, "webp_quality": 90},
        # These settings must remain dormant while the Resize page processes PNG.
        "convert": {"format": "WebP", "webp_lossless": True, "jpeg_quality": quality + 1,
                    "webp_quality": 66, "gif_first_frame": True, "background": [11, 22, 33]},
    })


def ordinary_case(folder, app, cwd, timeout, expected_output, saved_output, *, quality=None, warning=False, fresh=False):
    before = digest(cwd / "inputs/sample.png")
    with GuiRun(folder, app, cwd, timeout) as run:
        events = run.complete()
        loaded = find_event(events, "settings-loaded")
        if fresh:
            require(loaded["settings"]["output_directory"] == "Output", "Fresh portable defaults did not use Output")
            require(loaded["settings"]["language"] == "Chinese", "Unexpected fresh language default")
            require(loaded["settings"]["compress"]["jpeg_quality"] == 85
                    and loaded["settings"]["resize"]["jpeg_quality"] == 95
                    and loaded["settings"]["convert"]["jpeg_quality"] == 85,
                    "Unexpected fresh per-tool quality defaults")
        if quality is not None:
            require(loaded["settings"]["language"] == "English" and loaded["settings"]["resize"]["jpeg_quality"] == quality, "Saved Resize preferences were not loaded")
        require(bool(loaded.get("warning")) == warning, f"Unexpected loaded warning: {loaded.get('warning')}")
        assert_processed(events, app, cwd, expected_output)
        run.close()
        events = run.exited()
        saved = assert_saved(events, app, saved_output, quality=quality)
        for other_tool in ("compress", "convert"):
            require(saved[other_tool] == loaded["settings"][other_tool], f"Resize changed {other_tool} preferences")
    require(digest(cwd / "inputs/sample.png") == before, "The original input was modified")
    return {"scenario": folder.name, "status": "passed", "application": str(app), "run_result": str(folder / "run-result.json")}


class SubstMappings:
    def __init__(self):
        self.owned = []
        self.operations = []
        self.kernel = ctypes.WinDLL("kernel32", use_last_error=True)
        self.kernel.GetLogicalDrives.restype = wintypes.DWORD
        self.kernel.QueryDosDeviceW.argtypes = [wintypes.LPCWSTR, wintypes.LPWSTR, wintypes.DWORD]
        self.kernel.QueryDosDeviceW.restype = wintypes.DWORD
        self.command = Path(os.environ["SystemRoot"]) / "System32/subst.exe"
        require(self.command.is_file(), f"SUBST executable not found: {self.command}")

    def target(self, letter):
        buffer = ctypes.create_unicode_buffer(32768)
        count = self.kernel.QueryDosDeviceW(letter, buffer, len(buffer))
        return buffer.value if count else None

    def create(self, directory):
        mask = self.kernel.GetLogicalDrives()
        for character in "ZYXWVUTSRQPONMLKJIHGFED":
            letter = character + ":"
            if mask & (1 << (ord(character) - ord("A"))) or self.target(letter) is not None:
                continue
            args = [str(self.command), letter, str(directory)]
            result = subprocess.run(args, capture_output=True, text=True, timeout=10, **hidden_options())
            self.operations.append({"command": args, "exit_code": result.returncode, "stdout": result.stdout, "stderr": result.stderr})
            if result.returncode != 0:
                continue
            expected = "\\??\\" + str(directory)
            actual = self.target(letter)
            self.owned.append((letter, expected))
            require(actual is not None and normalized(actual) == normalized(expected), f"Unexpected SUBST mapping for {letter}: {actual}")
            return Path(letter + "\\")
        raise RuntimeError("Could not create an unused SUBST drive letter")

    def cleanup(self):
        failures = []
        for letter, expected in reversed(self.owned):
            actual = self.target(letter)
            if actual is None:
                continue
            if normalized(actual) != normalized(expected):
                failures.append(f"Mapping {letter} changed externally; left it untouched")
                continue
            args = [str(self.command), letter, "/D"]
            result = subprocess.run(args, capture_output=True, text=True, timeout=10, **hidden_options())
            self.operations.append({"command": args, "exit_code": result.returncode, "stdout": result.stdout, "stderr": result.stderr})
            if result.returncode != 0 or self.target(letter) is not None:
                failures.append(f"Could not remove own mapping {letter}: {result.stderr}")
        return failures


def drive_case(candidate, artifacts, timeout):
    folder = artifacts / "07-drive-relocation"
    require(within(folder.resolve(), artifacts), "Drive scenario escapes artifacts")
    folder.mkdir()
    mappings = SubstMappings()
    records = []
    cleanup_errors = []
    try:
        first_volume = folder / "volume-a"
        first_volume.mkdir()
        first_physical = first_volume / "Local Image Tools"
        require(within(first_physical.resolve(), artifacts), "First drive copy escapes artifacts")
        shutil.copytree(candidate, first_physical)
        first_drive = mappings.create(first_volume)
        first_app = first_drive / "Local Image Tools"
        seed(first_physical, first_app / "TravelOutput", quality=69)
        first_run = folder / "first-run"
        first_run.mkdir()
        first_cwd = first_run / "unrelated-cwd"
        first_cwd.mkdir()
        make_png(first_cwd / "inputs/sample.png")
        records.append(ordinary_case(first_run, first_app, first_cwd, timeout, first_app / "TravelOutput", "TravelOutput", quality=69))

        second_volume = folder / "volume-b"
        second_volume.mkdir()
        require(within(first_physical.resolve(), artifacts), "Relocation source escapes artifacts")
        require(within((second_volume / "Local Image Tools").resolve(), artifacts), "Relocation destination escapes artifacts")
        shutil.copytree(first_physical, second_volume / "Local Image Tools")
        second_drive = mappings.create(second_volume)
        require(second_drive.drive != first_drive.drive, "SUBST relocation did not change drive letters")
        second_app = second_drive / "Local Image Tools"
        second_run = folder / "second-run"
        second_run.mkdir()
        second_cwd = second_run / "unrelated-cwd"
        second_cwd.mkdir()
        make_png(second_cwd / "inputs/sample.png")
        records.append(ordinary_case(second_run, second_app, second_cwd, timeout, second_app / "TravelOutput", "TravelOutput", quality=69))
    finally:
        cleanup_errors = mappings.cleanup()
        write_json(folder / "subst.json", {"operations": mappings.operations, "cleanup_errors": cleanup_errors, "runs": records})
    require(not cleanup_errors, "; ".join(cleanup_errors))
    return {"scenario": folder.name, "status": "passed", "runs": records, "subst_evidence": str(folder / "subst.json")}


def verify(args):
    require(os.name == "nt", "This native GUI verifier requires Windows")
    candidate = args.portable_dir.resolve(strict=True)
    artifacts = args.artifacts.resolve()
    require(candidate.is_dir(), "--portable-dir must be an extracted application directory")
    require((candidate / EXE_NAME).is_file() and (candidate / "portable.flag").is_file(), "Candidate must contain the GUI executable and portable.flag")
    require(not (candidate / "Data").exists(), "Candidate must be pristine: Data already exists")
    require(not artifacts.exists(), "--artifacts must name a new directory")
    require(not within(artifacts, candidate), "Artifacts must be outside the candidate to prevent recursive copies")
    artifacts.mkdir(parents=True)
    baseline = tree_snapshot(candidate)
    profiles = profile_roots()
    profile_before = {name: tree_snapshot(root) for name, root in profiles.items()}
    report = {
        "started_utc": dt.datetime.now(dt.timezone.utc).isoformat(),
        "status": "running", "candidate": str(candidate), "artifacts": str(artifacts),
        "python": sys.version, "platform": sys.platform, "timeout_seconds_per_wait": args.timeout,
        "settings_schema_version": 2, "diagnostic_tool": "resize",
        "selection": "drive-only" if args.only_drive else "normal-and-drive" if args.with_subst else "normal",
        "candidate_manifest": baseline,
        "profile_paths_read_only": {name: str(root) for name, root in profiles.items()},
        "profile_before": profile_before, "scenarios": [],
        "scope": [
            "Native Windows GUI, actual image import/process/export, and WM_CLOSE settings persistence",
            "Portable settings/output paths, relocation, caller-relative diagnostic input paths",
            "Schema-2 typed preferences; dormant Compress/Convert settings do not alter Resize or get overwritten",
            "Read-only before/after hashes of candidate and app-specific Local/Roaming AppData directories",
            "System-only PATH; original package and source-image bytes remain unchanged",
        ],
        "limitations": [
            "Runs on this host, not a fresh OS or physical USB device; no power-loss, unplug, or full-disk simulation",
            "Diagnostics supply the relative input path; positional GUI command-line argument parsing is not exercised",
            "No automatic modal-button interaction; save retry uses WM_CLOSE to the same verified GUI PID",
            "No system-wide filesystem tracing: Windows shell/driver caches and unrelated application writes are outside scope",
            "No user profile files are created, edited, restored, or deleted by the verifier; monitored profile changes fail verification",
            "Failure cleanup terminates only the directly spawned GUI PID; no process-name or process-tree kill",
        ],
    }
    if not args.only_drive:
        report["scope"].append("Unique temporary settings replacement failure and retry using Windows share locks")
    else:
        report["limitations"].append("Drive-only selection does not rerun the six ordinary scenarios; their results belong to the earlier verification artifact")
    error = None
    try:
        if not args.only_drive:
            folder, app, cwd = copy_scenario(artifacts, "01-fresh", candidate)
            report["scenarios"].append(ordinary_case(folder, app, cwd, args.timeout, app / "Output", "Output", fresh=True))
            print("PASS fresh defaults, relative input, portable output, close/save", flush=True)

            folder, seeded, cwd = copy_scenario(artifacts, "02-seeded-descendant", candidate)
            seed(seeded, seeded / "Pictures/finished")
            report["scenarios"].append(ordinary_case(folder, seeded, cwd, args.timeout, seeded / "Pictures/finished", "Pictures/finished", quality=73))
            print("PASS preference round-trip and absolute descendant stored relative", flush=True)

            folder, app, cwd = copy_scenario(artifacts, "03-folder-relocation", seeded)
            report["scenarios"].append(ordinary_case(folder, app, cwd, args.timeout, app / "Pictures/finished", "Pictures/finished", quality=73))
            print("PASS full-folder copy/relocation", flush=True)

            folder, app, cwd = copy_scenario(artifacts, "04-unavailable-external", candidate)
            external = folder / "unavailable-old-machine/output"
            seed(app, external)
            report["scenarios"].append(ordinary_case(folder, app, cwd, args.timeout, app / "Output", "Output", quality=73, warning=True))
            require(not external.parent.exists(), "Unavailable external directory was recreated")
            print("PASS unavailable external absolute resets without recreation", flush=True)

            folder, app, cwd = copy_scenario(artifacts, "05-diagnostics-default", candidate)
            with GuiRun(folder, app, cwd, args.timeout, use_saved=False, save_on_exit=False, output=app / "DiagnosticOutput") as run:
                events = run.complete()
                assert_processed(events, app, cwd, app / "DiagnosticOutput")
                run.close()
                events = run.exited()
                require(not any(e.get("event") == "settings-saved" for e in events), "Default diagnostics unexpectedly saved preferences")
            require(not (app / "Data").exists(), "Default diagnostics created portable Data")
            report["scenarios"].append({"scenario": folder.name, "status": "passed", "flags": "omitted: both default false", "run_result": str(folder / "run-result.json")})
            print("PASS diagnostics defaults create no settings Data", flush=True)

            folder, app, cwd = copy_scenario(artifacts, "06-lock-failure-retry", candidate)
            seed(app, "Output", quality=67)
            before = (app / "Data/settings.json").read_bytes()
            with GuiRun(folder, app, cwd, args.timeout) as run:
                # Lock after successful load to test replacement, not read failure.
                with lock_settings(app / "Data/settings.json"):
                    events = run.complete()
                    assert_processed(events, app, cwd, app / "Output")
                    run.close()
                    events = run.wait_for(lambda events: any(e.get("event") == "settings-close-blocked" for e in events), "save failure and close cancellation")
                    require(any(e.get("event") == "settings-saved" and e.get("ok") is False for e in events), "No failed settings-saved event")
                    require((app / "Data/settings.json").read_bytes() == before, "Failed replacement changed previous JSON")
                    require(run.process.poll() is None, "GUI closed despite failed settings save")
                    require(not list((app / "Data").glob(".settings-*.tmp")), "Failed replacement left a settings temporary file")
                run.close()
                events = run.exited()
                assert_saved(events, app, "Output", quality=67)
                require((app / "Data/settings.json").read_bytes() != before, "Retry did not persist current complete preferences")
            report["scenarios"].append({"scenario": folder.name, "status": "passed", "previous_json_preserved": True, "retry_saved": True, "run_result": str(folder / "run-result.json")})
            print("PASS locked settings preservation and targeted close retry", flush=True)
        else:
            report["scenarios"].append({"scenario": "01-06-normal-scenarios", "status": "skipped", "reason": "--only-drive requested; previous results are not rerun or claimed by this artifact"})

        if args.with_subst:
            report["scenarios"].append(drive_case(candidate, artifacts, args.timeout))
            print("PASS actual SUBST drive-letter relocation and mapping cleanup", flush=True)
        else:
            report["scenarios"].append({"scenario": "07-drive-relocation", "status": "skipped", "reason": "Pass --with-subst to exercise two actual temporary drive mappings"})
        report["status"] = "passed"
    except BaseException as caught:
        error = caught
        report["status"] = "failed"
        report["error"] = str(caught)
        report["traceback"] = traceback.format_exc()
    finally:
        report["finished_utc"] = dt.datetime.now(dt.timezone.utc).isoformat()
        try:
            report["candidate_unchanged"] = tree_snapshot(candidate) == baseline
            report["profile_after"] = {name: tree_snapshot(root) for name, root in profiles.items()}
            report["profile_unchanged"] = report["profile_after"] == profile_before
            if not report["candidate_unchanged"] or not report["profile_unchanged"]:
                report["status"] = "failed"
                report["integrity_error"] = "Original candidate or monitored application profile paths changed"
        except BaseException as caught:
            report["status"] = "failed"
            report["integrity_error"] = str(caught)
        write_json(artifacts / "verification.json", report)
        print(str(artifacts / "verification.json"), flush=True)
    if error is not None:
        raise error
    require(report["status"] == "passed", report.get("integrity_error", "Verification failed"))


def main():
    parser = argparse.ArgumentParser(description=__doc__, formatter_class=argparse.RawDescriptionHelpFormatter)
    parser.add_argument("--portable-dir", type=Path, required=True)
    parser.add_argument("--artifacts", type=Path, required=True)
    parser.add_argument("--timeout", type=float, default=90, help="Timeout for each GUI event/exit wait, in seconds (default: 90)")
    parser.add_argument("--with-subst", action="store_true", help="Also verify relocation across two unused temporary Windows drive letters")
    parser.add_argument("--only-drive", action="store_true", help="Run only the drive-letter relocation scenario; implies --with-subst")
    args = parser.parse_args()
    if args.only_drive:
        args.with_subst = True
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
