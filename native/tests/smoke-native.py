#!/usr/bin/env python3
"""Exercise the native GTK application with disposable data and a local provider."""

import csv
import io
import json
import os
from pathlib import Path
import signal
import shutil
import socket
import subprocess
import sys
import tempfile
import threading
import time
import uuid
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer


ROOT = Path(__file__).resolve().parents[2]
KEY = "native-smoke-synthetic-key-not-a-provider-secret"
STREAM = "NATIVE_STREAM"
CANCEL = "NATIVE_CANCEL"
ERROR = "NATIVE_ERROR"
SAVE = "NATIVE_SAVE"
FIRST = "Native streaming fragment"
COMPLETE = "Native streaming completed"
PARTIAL = "Native cancellation fragment"
RETRIED = "Native retry completed"
SAVE_FIRST = "Native unsaved fragment"
SAVE_COMPLETE = "Native save completed"
LONG_REPLY = "A native Rust example:\n\n```rust\nfn main() {\n" + "\n".join(f'    let line_{index:02} = "native line {index:02}";' for index in range(36)) + "\n}\n```\n\n" + COMPLETE


def require(condition, message):
    if not condition:
        raise AssertionError(message)


def wait(condition, message, timeout=12):
    deadline = time.monotonic() + timeout
    while time.monotonic() < deadline:
        result = condition()
        if result:
            return result
        time.sleep(0.08)
    raise AssertionError(message)


def test_environment(base):
    environment = dict(os.environ)
    for name in list(environment):
        if name.endswith("_API_KEY"):
            environment.pop(name)
    runtime = base / "runtime"
    runtime.mkdir(mode=0o700, parents=True, exist_ok=True)
    environment.update({"XDG_DATA_HOME": str(base / "data"), "XDG_CONFIG_HOME": str(base / "config"), "XDG_CACHE_HOME": str(base / "cache"), "XDG_STATE_HOME": str(base / "state"), "XDG_RUNTIME_DIR": str(runtime), "GDK_BACKEND": "x11", "GSK_RENDERER": "cairo", "LIBGL_ALWAYS_SOFTWARE": "1", "GTK_A11Y": "atspi", "GDK_DEBUG": "no-portals", "GTK_USE_PORTAL": "0", "GIO_USE_VFS": "local", "GIO_USE_VOLUME_MONITOR": "unix", "NO_PROXY": "127.0.0.1,localhost", "LANG": "C.UTF-8", "LC_ALL": "C.UTF-8"})
    return environment


class Provider(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self):
        self.complete = threading.Event()
        self.save_complete = threading.Event()
        self.stopped = threading.Event()
        self.cancel_started = threading.Event()
        self.failed_once = False
        self.requests = 0
        self.failures = []
        self.closing = threading.Event()
        super().__init__(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.serve_forever, daemon=True).start()

    def close(self):
        self.closing.set()
        self.complete.set()
        self.save_complete.set()
        self.shutdown()
        self.server_close()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def do_POST(self):
        provider = self.server
        try:
            require(self.path == "/chat/completions", "The native provider uses Chat Completions")
            require(self.headers.get("Authorization") == "Bearer " + KEY, "Only the synthetic fixture credential reaches the provider")
            size = int(self.headers.get("Content-Length", "0"))
            require(0 < size <= 1024 * 1024, "Provider requests stay bounded")
            request = json.loads(self.rfile.read(size))
            require(request.get("model") == "native-smoke", "The selected model reaches the provider")
            require(request.get("stream") is True, "The native backend requests streaming")
            prompt = next(message["content"] for message in reversed(request["messages"]) if message["role"] == "user")
            require(prompt in (STREAM, CANCEL, ERROR, SAVE), "Only fixture prompts reach the provider")
            provider.requests += 1
            if prompt == ERROR and not provider.failed_once:
                provider.failed_once = True
                body = json.dumps({"error": {"message": "Untrusted body " + KEY}}).encode()
                self.send_response(503)
                self.send_header("Content-Type", "application/json")
                self.send_header("Content-Length", str(len(body)))
                self.end_headers()
                self.wfile.write(body)
                self.wfile.flush()
                return
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Connection", "close")
            self.end_headers()
            self.close_connection = True
            self.chunk({"role": "assistant"})
            if prompt == STREAM:
                self.chunk({"content": FIRST + "\n\n"})
                require(provider.complete.wait(25), "The UI observes the partial response before completion")
                self.chunk({"content": LONG_REPLY})
            elif prompt == SAVE:
                self.chunk({"content": SAVE_FIRST + "\n\n"})
                require(provider.save_complete.wait(25), "The storage fixture releases completion")
                self.chunk({"content": SAVE_COMPLETE})
            elif prompt == CANCEL:
                self.chunk({"content": PARTIAL})
                provider.cancel_started.set()
                while not provider.closing.wait(0.1):
                    self.chunk({"content": "."})
                return
            else:
                self.chunk({"content": RETRIED})
            self.chunk({}, "stop")
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError, socket.error):
            if provider.cancel_started.is_set():
                provider.stopped.set()
        except Exception as error:
            # Assertion messages contain no request bodies, URLs, or credentials.
            provider.failures.append(type(error).__name__ + ": " + str(error))
            self.close_connection = True

    def chunk(self, delta, finish=None):
        body = {"id": "native-fixture", "object": "chat.completion.chunk", "model": "native-smoke", "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
        self.wfile.write(("data: " + json.dumps(body) + "\n\n").encode())
        self.wfile.flush()


class Desktop:
    def __init__(self, process, environment):
        import gi
        gi.require_version("Atspi", "2.0")
        from gi.repository import Atspi, GLib
        import pyatspi
        Atspi.set_timeout(1000, 1000)
        self.atspi = pyatspi
        self.process = process
        self.environment = environment
        self.context = GLib.MainContext.default()
        self.marker_evidence = {}

    def app(self):
        for _ in range(64):
            if not self.context.pending():
                break
            self.context.iteration(False)
        desktop = self.atspi.Registry.getDesktop(0)
        for item in desktop:
            try:
                if item.get_process_id() == self.process.pid:
                    return item
            except Exception:
                pass
        return None

    def walk(self, root=None):
        root = root or self.app()
        if root is None:
            return
        pending = [(root, 0)]
        visited = 0
        while pending and visited < 3500:
            node, depth = pending.pop()
            visited += 1
            try:
                yield node
                if depth < 32:
                    pending.extend((node[index], depth + 1) for index in reversed(range(node.childCount)))
            except Exception:
                continue

    def visible(self, node):
        try:
            return node.getState().contains(self.atspi.STATE_SHOWING)
        except Exception:
            return False

    def find(self, name, root=None, editable=False, role=None):
        for node in self.walk(root):
            try:
                if node.name != name or not self.visible(node):
                    continue
                if role and node.getRoleName() not in role:
                    continue
                if editable:
                    node.queryEditableText()
                return node
            except Exception:
                continue
        return None

    def control(self, name, root=None, editable=False):
        return wait(lambda: self.find(name, root, editable), "The native control is available: " + name)

    def window(self, title):
        roles = ("frame", "dialog", "window", "file chooser", "alert")
        return wait(lambda: self.find(title, role=roles), "The native window is visible: " + title)

    def click(self, name, root=None):
        node = self.control(name, root)
        require(node.getState().contains(self.atspi.STATE_SENSITIVE), "The native action is enabled: " + name)
        try:
            if node.getRoleName() != "label":
                action = node.queryAction()
                for index in range(action.nActions):
                    if action.getName(index) in ("click", "activate", "press"):
                        require(action.doAction(index), "The native action activates: " + name)
                        return
        except NotImplementedError:
            pass
        bounds = node.queryComponent().getExtents(self.atspi.DESKTOP_COORDS)
        require(bounds.width > 0 and bounds.height > 0, "The native action has visible bounds")
        self.xdo("mousemove", "--sync", str(bounds.x + bounds.width // 2), str(bounds.y + bounds.height // 2), "click", "1")

    def set_text(self, name, value, root=None):
        node = self.control(name, root, editable=True)
        require(node.queryEditableText().setTextContents(value), "The native field accepts input: " + name)

    def text(self, node):
        try:
            value = node.queryText()
            return value.getText(0, value.characterCount)
        except Exception:
            return ""

    def field_text(self, name):
        return self.text(self.control(name, editable=True))

    def select_conversation(self, title):
        saved = self.control("Saved conversations")
        for index in range(saved.childCount):
            row = saved[index]
            if any(node.name == title for node in self.walk(row)):
                try:
                    require(saved.querySelection().selectChild(index), "The saved conversation is selected through GTK accessibility")
                    return
                except NotImplementedError:
                    self.xdo("windowfocus", "--sync", self.main_window_id())
                    self.click(title, row)
                    return
        raise AssertionError("The saved conversation remains available: " + title)

    def has_text(self, value, root=None):
        for node in self.walk(root):
            if self.visible(node):
                try:
                    if value in node.name or value in self.text(node):
                        return True
                except Exception:
                    pass
        return False

    def visible_message(self, value):
        # Text presence alone is insufficient: GTK can expose overscan rows and
        # reports unreliable desktop/range positions on some GTK releases.
        # Confirm the exact marker in actual pixels cropped to the history.
        history = None
        for node in self.walk():
            if self.visible(node) and (value in node.name or value in self.text(node)):
                ancestor = node.parent
                while ancestor and ancestor.getRoleName() != "scroll pane":
                    ancestor = ancestor.parent
                if ancestor:
                    history = ancestor
                    break
        if history is None:
            return False
        viewport = history.queryComponent().getExtents(self.atspi.WINDOW_COORDS)
        composer = self.control("Message", editable=True)
        composer_height = composer.queryComponent().getExtents(self.atspi.WINDOW_COORDS).height
        ancestor = composer.parent
        composer_area = None
        while ancestor and ancestor.getRoleName() != "frame":
            bounds = ancestor.queryComponent().getExtents(self.atspi.WINDOW_COORDS)
            if bounds.width >= viewport.width - 16 and bounds.height > composer_height + 40:
                composer_area = bounds
                break
            ancestor = ancestor.parent
        require(composer_area is not None, "The composer has a bounded native layout")
        layout_width = viewport.width
        ancestor = history.parent
        while ancestor and ancestor.getRoleName() != "frame":
            bounds = ancestor.queryComponent().getExtents(self.atspi.WINDOW_COORDS)
            layout_width = max(layout_width, bounds.width)
            ancestor = ancestor.parent
        geometry = dict(line.split("=", 1) for line in self.xdo("getwindowgeometry", "--shell", self.main_window_id()).splitlines() if "=" in line)
        window_x, window_y, window_width, window_height = (int(geometry[key]) for key in ("X", "Y", "WIDTH", "HEIGHT"))
        inset = max(0, (window_width - layout_width) // 2)
        right = window_x + window_width - inset
        bottom = window_y + window_height - inset - composer_area.height
        crop = (right - viewport.width, bottom - viewport.height, right, bottom)
        require(crop[0] >= window_x and crop[1] >= window_y and crop[2] <= window_x + window_width and crop[3] < window_y + window_height, "The pixel check stays inside the owned history viewport")
        from PIL import Image, ImageGrab
        image = ImageGrab.grab(xdisplay=os.environ["DISPLAY"]).crop(crop)
        image = image.resize((image.width * 2, image.height * 2), Image.Resampling.LANCZOS)
        encoded = io.BytesIO()
        image.save(encoded, format="PNG")
        result = subprocess.run(["tesseract", "stdin", "stdout", "--psm", "6", "-l", "eng", "tsv"], input=encoded.getvalue(), stdout=subprocess.PIPE, stderr=subprocess.PIPE, env={**self.environment, "OMP_THREAD_LIMIT": "1"}, check=True, timeout=8)
        lines = {}
        for word in csv.DictReader(io.StringIO(result.stdout.decode("utf-8")), delimiter="\t"):
            if word["level"] == "5" and word["text"].strip() and float(word["conf"]) >= 50:
                key = (word["block_num"], word["par_num"], word["line_num"])
                lines.setdefault(key, []).append(word)
        expected = value.lower().split()
        for words in lines.values():
            normalized = ["".join(character for character in word["text"].lower() if character.isalnum()) for word in words]
            for index in range(len(words) - len(expected) + 1):
                if normalized[index:index + len(expected)] == expected:
                    matched = words[index:index + len(expected)]
                    marker_top = min(int(word["top"]) for word in matched)
                    marker_bottom = max(int(word["top"]) + int(word["height"]) for word in matched)
                    if marker_top > 3 and marker_bottom < image.height - 3:
                        self.marker_evidence[value] = {"method": "local screenshot OCR", "historyCrop": list(crop), "markerBottomInCrop": marker_bottom / 2, "historyPixelHeight": viewport.height}
                        return True
        return False

    def xdo(self, *args, text=None):
        return subprocess.run(["xdotool", *args], input=text, text=True, env=self.environment, check=True, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=5).stdout.strip()

    def main_window_id(self):
        return wait(lambda: self.xdo("search", "--onlyvisible", "--pid", str(self.process.pid), "--name", "DeepSeek Harness for Linux").splitlines()[0], "The owned native window exists")

    def focus_message(self):
        # Settings restores the composer's native focus. Xvfb needs the X11
        # application focus explicitly; GTK4 does not implement AT-SPI grabFocus.
        self.xdo("windowfocus", "--sync", self.main_window_id())

    def request_close(self):
        # Use the real GTK titlebar action. Xvfb has no window manager, so an
        # Alt+F4 shortcut or a hand-built X11 event is not reliable evidence.
        main = self.window("DeepSeek Harness for Linux — Native")
        self.click("Close", main)
    def close(self):
        self.request_close()
        require(self.process.wait(timeout=8) == 0, "The native app closes gracefully")


def snapshot(path):
    from PIL import ImageGrab
    ImageGrab.grab(xdisplay=os.environ["DISPLAY"]).save(path)


def check_process_tree(process):
    rows = subprocess.check_output(["ps", "-eo", "pid=,ppid=,comm="], text=True).splitlines()
    processes = []
    owned = {process.pid}
    for line in rows:
        pid, parent, name = line.split(None, 2)
        processes.append((int(pid), int(parent), name))
    for _ in range(16):
        for pid, parent, _name in processes:
            if parent in owned:
                owned.add(pid)
    require(not any(pid in owned and (name.lower().startswith("node") or "webkit" in name.lower()) for pid, _parent, name in processes), "The native app starts no Node or WebKit process")
    for pid in owned:
        try:
            require(KEY.encode() not in Path(f"/proc/{pid}/cmdline").read_bytes(), "Credentials are absent from app process arguments")
        except FileNotFoundError:
            pass
    pss_kib = 0
    for pid in owned:
        try:
            lines = Path(f"/proc/{pid}/smaps_rollup").read_text().splitlines()
            pss_kib += next(int(line.split()[1]) for line in lines if line.startswith("Pss:"))
        except FileNotFoundError:
            pass
    return {"processCount": len(owned), "pssMiB": round(pss_kib / 1024, 1)}


def run(application):
    artifacts = ROOT / "artifacts"
    artifacts.mkdir(exist_ok=True)
    (artifacts / "native-smoke.json").unlink(missing_ok=True)
    fixture = Path(os.environ.get("HARNESS_NATIVE_PLUGIN_FIXTURE", ROOT / "native/examples/uppercase")).resolve()
    require((fixture / "plugin.wasm").is_file(), "Build or select the harmless example plugin before running the native smoke test")
    provider = Provider()
    process = None
    output = None
    stage = "startup"
    with tempfile.TemporaryDirectory(prefix="harness-native-smoke-") as scratch:
        scratch = Path(scratch)
        environment = test_environment(scratch)
        profile = scratch / "data/deepseek-harness-native"

        def state():
            path = profile / "state.json"
            return json.loads(path.read_text()) if path.exists() else None

        def launch():
            nonlocal process, output
            output = tempfile.TemporaryFile()
            process = subprocess.Popen([str(application)], cwd=scratch, env=environment, stdout=output, stderr=output, start_new_session=True)
            desktop = Desktop(process, environment)
            wait(lambda: desktop.find("Message", editable=True), "The native composer becomes accessible", timeout=25)
            return desktop

        def messages():
            return [message for session in state()["sessions"] for message in session["messages"]]

        def credential_check():
            for path in profile.rglob("*"):
                if path.is_file():
                    require(KEY.encode() not in path.read_bytes(), "The native profile never persists the API key")
            output.flush()
            output.seek(0)
            require(KEY.encode() not in output.read(), "The native app never writes the key to stdout or stderr")
            output.seek(0, os.SEEK_END)

        try:
            desktop = launch()
            require(desktop.has_text("What would you like to work on?"), "A fresh native profile shows the useful empty state")
            require(not desktop.control("Send").getState().contains(desktop.atspi.STATE_SENSITIVE), "Send is disabled for an empty message")
            ready_metrics = check_process_tree(process)
            snapshot(artifacts / "native-empty.png")
            stage = "single app instance"
            second = subprocess.run([str(application)], cwd=scratch, env=environment, stdout=subprocess.PIPE, stderr=subprocess.PIPE, timeout=10)
            require(second.returncode == 0 and process.poll() is None, "A second launch returns to the running native application")
            windows = [node for node in desktop.walk() if node.name == "DeepSeek Harness for Linux — Native" and node.getRoleName() in ("frame", "window")]
            require(len(windows) == 1, "A second launch leaves exactly one native main window")
            require(desktop.field_text("Message") == "", "A second launch preserves the existing composer")
            stage = "settings"
            desktop.click("Settings")
            settings = desktop.window("Settings")
            desktop.set_text("API endpoint", "http://example.invalid", settings)
            desktop.click("Save settings", settings)
            wait(lambda: desktop.has_text("Enter an HTTPS API base address", settings), "An insecure remote API address is rejected inline")
            require(state()["settings"]["base_url"] == "https://api.deepseek.com", "Invalid settings leave the saved connection unchanged")
            desktop.set_text("Model", "native-smoke", settings)
            local_endpoint = f"http://127.0.0.1:{provider.server_port}"
            desktop.set_text("API endpoint", local_endpoint, settings)
            desktop.set_text("API key", KEY, settings)
            desktop.click("Save settings", settings)
            wait(lambda: state()["settings"]["model"] == "native-smoke", "Valid connection settings persist")
            require(state()["settings"]["base_url"] == local_endpoint, "The saved endpoint is the owned loopback fixture before sending")
            stage = "streaming"
            desktop.set_text("Message", "keyboard line")
            desktop.focus_message()
            desktop.xdo("key", "--clearmodifiers", "ctrl+End", "Return")
            wait(lambda: desktop.field_text("Message") == "keyboard line\n", "Plain Enter inserts a newline in the composer")
            require(provider.requests == 0, "Plain Enter does not submit a model request")
            desktop.set_text("Message", STREAM)
            desktop.focus_message()
            desktop.xdo("key", "--clearmodifiers", "ctrl+Return")
            wait(lambda: desktop.visible_message(FIRST), "The first streamed fragment is inside the actual GTK history viewport")
            require(not desktop.has_text(COMPLETE), "The GTK view renders before the provider finishes")
            require(desktop.find("Stop"), "Stop replaces Send while generating")
            provider.complete.set()
            wait(lambda: desktop.visible_message(COMPLETE), "The final marker after the long code block is visible above the composer")
            wait(lambda: messages()[-1]["status"] == "complete", "The completed reply is saved")
            wait(lambda: not desktop.has_text("Generating…"), "Completion clears the message's generating state")
            snapshot(artifacts / "native-long-reply.png")
            first_id = next(session["id"] for session in state()["sessions"] if session["messages"])
            stage = "conversation drafts"
            desktop.set_text("Message", "first draft café")
            desktop.click("＋  New conversation")
            wait(lambda: len(state()["sessions"]) == 2, "A second conversation is created")
            desktop.set_text("Message", "second draft")
            desktop.select_conversation(STREAM)
            wait(lambda: desktop.field_text("Message") == "first draft café", "Switching back restores the first unsent draft")
            desktop.select_conversation("New conversation")
            wait(lambda: desktop.field_text("Message") == "second draft", "Switching restores the second unsent draft")
            desktop.select_conversation(STREAM)
            wait(lambda: desktop.field_text("Message") == "first draft café", "The first draft survives repeated switching")
            stage = "cancellation"
            desktop.set_text("Message", CANCEL)
            desktop.click("Send")
            wait(lambda: desktop.visible_message(PARTIAL), "The cancellable response is visible inside the history viewport")
            desktop.click("Stop")
            wait(provider.stopped.is_set, "Stop aborts the provider connection")
            wait(lambda: messages()[-1]["status"] == "cancelled", "The cancelled partial response persists")
            stage = "provider error and retry"
            desktop.set_text("Message", ERROR)
            desktop.click("Send")
            wait(lambda: desktop.has_text("The provider is temporarily unavailable"), "Provider failure explains recovery in the interface")
            require(not desktop.has_text(KEY), "The untrusted provider error body is absent from the interface")
            desktop.set_text("Message", "draft kept while retrying")
            desktop.click("Retry reply")
            wait(lambda: desktop.visible_message(RETRIED), "The retried response is visible inside the history viewport")
            wait(lambda: messages()[-1]["status"] == "complete", "The retried response persists")
            require(desktop.field_text("Message") == "draft kept while retrying", "Retry preserves a newly typed draft")
            wait(lambda: any(session["draft"] == "draft kept while retrying" for session in state()["sessions"]), "Retry also saves the fresh draft")
            stage = "failed storage and retry save"
            desktop.set_text("Message", SAVE)
            desktop.click("Send")
            wait(lambda: desktop.visible_message(SAVE_FIRST), "The storage fixture fragment is visible above the composer")
            snapshot(artifacts / "native-save-partial.png")
            state_path = profile / "state.json"
            backup = profile / "state.before-storage-fixture.json"
            state_path.rename(backup)
            state_path.mkdir()
            provider.save_complete.set()
            wait(lambda: desktop.find("Retry save"), "A failed final checkpoint offers Retry save")
            desktop.request_close()
            time.sleep(0.25)
            require(process.poll() is None, "Normal close is blocked while a reply is unsaved")
            require(desktop.find("Retry save"), "The unsaved reply and recovery action remain visible")
            wait(lambda: desktop.visible_message(SAVE_COMPLETE), "The unsaved completed reply remains in the history viewport")
            snapshot(artifacts / "native-save-error.png")
            state_path.rmdir()
            backup.rename(state_path)
            requests_before_save = provider.requests
            desktop.click("Retry save")
            wait(lambda: messages()[-1]["status"] == "complete" and SAVE_COMPLETE in messages()[-1]["content"], "Retry save persists the completed response after storage repair")
            require(provider.requests == requests_before_save, "Retry save makes no additional model request")
            wait(lambda: not desktop.find("Retry save"), "Successful persistence clears Retry save")
            stage = "extension chooser"
            desktop.set_text("Message", "Rust client café")
            desktop.click("Extensions")
            extensions = desktop.window("Extensions")
            require(desktop.has_text("No extensions installed yet.", extensions), "The extension manager has a useful empty state")
            desktop.click("Install extension…", extensions)
            chooser = desktop.window("Choose an extension folder")
            chooser_id = desktop.xdo("search", "--onlyvisible", "--pid", str(process.pid), "--name", "Choose an extension folder").splitlines()[-1]
            desktop.xdo("windowfocus", "--sync", chooser_id)
            desktop.xdo("key", "--clearmodifiers", "ctrl+l")
            desktop.xdo("type", "--clearmodifiers", "--delay", "0", "--file", "-", text=str(fixture))
            desktop.xdo("key", "Return")
            wait(lambda: desktop.find("Make uppercase", extensions) or desktop.has_text(fixture.name, chooser), "The native chooser resolves the supplied fixture folder")
            if not desktop.find("Make uppercase", extensions):
                desktop.click("Install extension", chooser)
            stage = "extension execution"
            wait(lambda: desktop.find("Make uppercase", extensions), "The installed extension contributes its command")
            desktop.click("Make uppercase", extensions)
            wait(lambda: desktop.field_text("Message") == "RUST CLIENT CAFé", "The real WASM extension changes the draft and preserves non-ASCII text")
            desktop.click("Remove", extensions)
            confirm = desktop.window("Remove extension?")
            desktop.click("Keep extension", confirm)
            wait(lambda: not desktop.find("Remove extension?", role=("dialog", "frame")), "Cancelling removal closes its confirmation")
            require(desktop.find("Make uppercase", extensions), "Cancelling removal keeps the extension")
            desktop.click("Remove", extensions)
            confirm = desktop.window("Remove extension?")
            desktop.click("Remove extension", confirm)
            wait(lambda: desktop.has_text("No extensions installed yet.", extensions), "Confirmed removal removes the extension")
            desktop.click("Close", extensions)
            stage = "persistence after close"
            desktop.set_text("Message", "saved draft after restart")
            process_metrics = check_process_tree(process)
            credential_check()
            snapshot(artifacts / "native-workflow.png")
            marker_evidence = dict(desktop.marker_evidence)
            desktop.close()
            credential_check()
            output.close()
            desktop = launch()
            wait(lambda: desktop.field_text("Message") == "saved draft after restart", "The native draft survives a graceful app restart")
            wait(lambda: desktop.visible_message(SAVE_COMPLETE), "The latest completed reply is visible above the composer after an app restart")
            snapshot(artifacts / "native-restarted.png")
            require(any(message["content"] == RETRIED and message["status"] == "complete" for message in messages()), "Earlier completed replies remain in durable history even when virtualized offscreen")
            require(any(session["id"] == first_id for session in state()["sessions"]), "The original conversation identity survives")
            require(len(state()["sessions"]) == 2, "Both saved conversations remain after restarting")
            credential_check()
            require(provider.failures == [], "The local provider recorded no protocol failures")
            check_process_tree(process)
            stage = "narrow window and memory-only key"
            desktop.xdo("windowsize", desktop.main_window_id(), "640", "480")
            main = desktop.window("DeepSeek Harness for Linux — Native")
            wait(lambda: main.queryComponent().getExtents(desktop.atspi.DESKTOP_COORDS).width <= 680 and main.queryComponent().getExtents(desktop.atspi.DESKTOP_COORDS).height <= 520, "The native layout supports a 640 by 480 desktop window")
            wait(lambda: desktop.visible_message(SAVE_COMPLETE), "The latest reply remains visible after shrinking the window")
            send_bounds = desktop.control("Send").queryComponent().getExtents(desktop.atspi.DESKTOP_COORDS)
            main_bounds = main.queryComponent().getExtents(desktop.atspi.DESKTOP_COORDS)
            require(send_bounds.x + send_bounds.width <= main_bounds.x + main_bounds.width and send_bounds.y + send_bounds.height <= main_bounds.y + main_bounds.height, "Send remains inside the narrow window")
            snapshot(artifacts / "native-narrow.png")
            desktop.click("Settings")
            settings = desktop.window("Settings")
            require(desktop.field_text("API key") == "", "Restarting does not restore the memory-only API key")
            settings_id = desktop.xdo("search", "--onlyvisible", "--pid", str(process.pid), "--name", "^Settings$").splitlines()[-1]
            desktop.xdo("windowsize", settings_id, "520", "400")
            wait(lambda: settings.queryComponent().getExtents(desktop.atspi.DESKTOP_COORDS).height <= 430, "Settings supports a short desktop window")
            settings_bounds = settings.queryComponent().getExtents(desktop.atspi.DESKTOP_COORDS)
            save_bounds = desktop.control("Save settings", settings).queryComponent().getExtents(desktop.atspi.DESKTOP_COORDS)
            require(save_bounds.y + save_bounds.height <= settings_bounds.y + settings_bounds.height, "The settings action stays pinned within the short modal")
            desktop.xdo("windowfocus", "--sync", settings_id)
            desktop.xdo("mousemove", str(settings_bounds.x + settings_bounds.width // 2), str(settings_bounds.y + 130), "click", "--repeat", "6", "--delay", "50", "5")
            wait(lambda: desktop.find("Custom instructions", settings, editable=True), "The settings content scrolls to its lower fields")
            snapshot(artifacts / "native-settings-narrow.png")
            desktop.click("Save settings", settings)
            wait(lambda: not desktop.find("Settings", role=("dialog", "frame")), "The native settings window closes")
            desktop.close()
            marker_evidence.update(desktop.marker_evidence)
            credential_check()
            output.close()
            stage = "bounded history paging"
            # Seed only this disposable profile, after its writer has exited.
            # The real controls must page without loading more than 50 rows or
            # changing the draft, session identity, or provider request count.
            seeded = state()
            session = next(session for session in seeded["sessions"] if session["id"] == first_id)
            now = int(time.time() * 1000)
            session["title"] = "Paging fixture"
            session["updated_at"] = now
            session["draft"] = "paging draft preserved"
            session["messages"] = [{"id": str(uuid.uuid4()), "role": "user" if index % 2 == 0 else "assistant", "content": f"Native page row {index:02}", "status": "complete", "created_at": now + index} for index in range(60)]
            (profile / "state.json").write_text(json.dumps(seeded))
            requests_before_paging = provider.requests
            desktop = launch()
            desktop.select_conversation("Paging fixture")

            def rendered_page():
                return {node.name for node in desktop.walk() if node.getRoleName() == "label" and node.name.startswith("Native page row ")}

            latest_rows = {f"Native page row {index:02}" for index in range(10, 60)}
            earlier_rows = {f"Native page row {index:02}" for index in range(10)}
            wait(lambda: desktop.has_text("Messages 11–60 of 60") and rendered_page() == latest_rows, "The latest history page renders exactly 50 rows")
            wait(lambda: desktop.visible_message("Native page row 59"), "The last history page is visible in actual pixels")
            desktop.click("Earlier messages")
            wait(lambda: desktop.has_text("Messages 1–10 of 60") and rendered_page() == earlier_rows, "Earlier messages renders the remaining ten rows without overlap")
            wait(lambda: desktop.visible_message("Native page row 09"), "The preceding history page is visible in actual pixels")
            require(not desktop.control("Earlier messages").getState().contains(desktop.atspi.STATE_SENSITIVE), "Paging stops at the first saved message")
            desktop.click("Latest messages")
            wait(lambda: desktop.has_text("Messages 11–60 of 60") and rendered_page() == latest_rows, "Latest messages restores the bounded latest page")
            wait(lambda: desktop.visible_message("Native page row 59"), "Returning to the latest page follows its final row")
            require(desktop.field_text("Message") == "paging draft preserved", "Paging preserves the unsent draft")
            require(provider.requests == requests_before_paging, "History paging makes no model request")
            require(any(session["id"] == first_id and len(session["messages"]) == 60 for session in state()["sessions"]), "Paging preserves all durable messages and conversation identity")
            snapshot(artifacts / "native-paging.png")
            marker_evidence.update(desktop.marker_evidence)
            credential_check()
            check_process_tree(process)
            desktop.close()
            report = {"provider": "synthetic loopback fixture; no paid model request", "nativeEmptyAndSettings": "passed", "singleAppInstance": "passed", "composerEnterAndCtrlEnter": "passed", "uiStreamingAndStop": "passed", "longMarkdownAndVisibleFinalMarker": "passed", "providerFailureAndRetry": "passed", "conversationSwitchingAndDrafts": "passed", "failedSaveRecoveryAndCloseGuard": "passed", "closeAndRestartPersistence": "passed", "localWasmInstallRunRemove": "passed", "narrowWindowAndModalScroll": "passed", "boundedHistoryPaging": "passed", "credentialPrivacy": "passed", "noNodeOrWebKit": "passed", "providerRequests": provider.requests, "readyProcessTree": ready_metrics, "afterWorkflowProcessTree": process_metrics, "markerViewportEvidence": marker_evidence, "memoryMethod": "PSS summed over the owned app process tree; Xvfb/software renderer, synthetic provider. No comparison or percentage claim."}
            (artifacts / "native-smoke.json").write_text(json.dumps(report, indent=2) + "\n")
            for stale in ("native-failure.json", "native-failure.png", "native-accessibility.json"):
                (artifacts / stale).unlink(missing_ok=True)
            print(json.dumps(report))
        except Exception:
            # Do not publish UI trees, provider bodies, or process output. A
            # chooser can contain local filenames, so failure screenshots are
            # intentionally omitted while it is open.
            (artifacts / "native-failure.json").write_text(json.dumps({"stage": stage, "appExited": process.poll() is not None if process else True, "providerRequests": provider.requests}, indent=2) + "\n")
            if stage != "extension chooser":
                if process and process.poll() is None:
                    controls = []
                    for node in desktop.walk():
                        try:
                            name = node.name
                            if name and desktop.visible(node) and KEY not in name and node.getRoleName() != "password text":
                                controls.append({"name": name[:160], "role": node.getRoleName()})
                        except Exception:
                            pass
                    (artifacts / "native-accessibility.json").write_text(json.dumps(controls, indent=2) + "\n")
                try:
                    snapshot(artifacts / "native-failure.png")
                except Exception:
                    pass
            raise
        finally:
            provider.close()
            if process and process.poll() is None:
                try:
                    os.killpg(process.pid, signal.SIGTERM)
                    process.wait(timeout=5)
                except subprocess.TimeoutExpired:
                    os.killpg(process.pid, signal.SIGKILL)
                    process.wait(timeout=5)
            if output:
                output.close()


if __name__ == "__main__":
    require(len(sys.argv) == 2, "Usage: smoke-native.py PATH_TO_NATIVE_APP")
    app = Path(sys.argv[1]).resolve()
    require(app.is_file(), "Build or install the native application before running its smoke test")
    require(shutil.which("tesseract"), "Install the tesseract-ocr test tool before running the native smoke test")
    if os.environ.get("HARNESS_NATIVE_TEST_BUS") != "1":
        # Own both display and session bus, even on direct invocation: snapshots
        # must never capture a user's windows, and single-instance activation
        # must never reach an already-open native application.
        require(shutil.which("xvfb-run"), "Install xvfb before running the native smoke test")
        with tempfile.TemporaryDirectory(prefix="harness-native-session-") as session:
            environment = test_environment(Path(session))
            environment["HARNESS_NATIVE_TEST_BUS"] = "1"
            result = subprocess.run(["xvfb-run", "-a", "dbus-run-session", "--", sys.executable, str(Path(__file__).resolve()), str(app)], env=environment, timeout=180)
            raise SystemExit(result.returncode)
    run(app)
