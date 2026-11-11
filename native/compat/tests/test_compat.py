#!/usr/bin/env python3
"""Actual optional Node/Cordis host checks; no browser, provider or user profile."""
import json
import os
from pathlib import Path
import queue
import shutil
import subprocess
import tempfile
import threading
import time
import unittest

ROOT = Path(__file__).resolve().parents[3]
HOST = ROOT / "native/compat/host.mjs"
RUNTIME = Path(os.environ.get("HARNESS_COMPAT_RUNTIME", ROOT / "runtime")).resolve()
FIXTURE = ROOT / "native/compat/fixtures/echo-plugin.mjs"
KEY = "native-compat-synthetic-credential-not-real"


class Host:
    def __init__(self, scratch, plugin=FIXTURE, profile=None, runtime=RUNTIME, env_names=()):
        self.profile = profile or scratch / "profile"
        workspace = scratch / "workspace"
        workspace.mkdir(exist_ok=True)
        environment = dict(os.environ)
        environment.update({"DEEPSEEK_API_KEY": KEY, "GITHUB_TOKEN": KEY, "UNAPPROVED_SECRET": KEY, "DSH_UNTRUSTED_TOKEN": KEY, "DSH_HOME": str(scratch / "original-harness"), "XDG_DATA_HOME": str(scratch / "data"), "XDG_CONFIG_HOME": str(scratch / "config"), "XDG_CACHE_HOME": str(scratch / "cache")})
        node = str(RUNTIME / "node") if (RUNTIME / "node").is_file() else shutil.which("node")
        command = [node, str(HOST), "--runtime", str(runtime), "--profile", str(self.profile), "--plugin", str(plugin)]
        for name in env_names:
            command.extend(["--env-name", name])
        self.process = subprocess.Popen(command, cwd=workspace, env=environment, stdin=subprocess.PIPE, stdout=subprocess.PIPE, stderr=subprocess.PIPE)
        self.frames = queue.Queue()
        self.lines = []
        self.errors = []
        self.reader = threading.Thread(target=self.read, daemon=True)
        self.reader.start()
        self.error_reader = threading.Thread(target=lambda: self.errors.append(self.process.stderr.read()), daemon=True)
        self.error_reader.start()

    def read(self):
        for line in self.process.stdout:
            self.lines.append(line)
            try:
                self.frames.put(json.loads(line))
            except (UnicodeError, ValueError):
                self.frames.put({"bad_frame": True})
        self.frames.put({"eof": True})

    def send(self, message):
        self.process.stdin.write(json.dumps(message).encode() + b"\n")
        self.process.stdin.flush()

    def result(self, identity, timeout=10):
        limit = time.monotonic() + timeout
        while time.monotonic() < limit:
            frame = self.frames.get(timeout=max(0.01, limit - time.monotonic()))
            if frame.get("id") == identity:
                return frame
            if "bad_frame" in frame or "eof" in frame:
                raise AssertionError("The host emitted invalid protocol output or exited.")
        raise AssertionError("The host did not respond in time.")

    def rpc(self, method, params=None, identity=1):
        self.send({"jsonrpc": "2.0", "id": identity, "method": method, "params": params or {}})
        return self.result(identity)

    def initialize(self):
        response = self.rpc("initialize", {"protocolVersion": "2025-06-18", "capabilities": {}, "clientInfo": {"name": "native-compat-test", "version": "1"}})
        assert response["result"]["protocolVersion"] == "2025-06-18"
        self.send({"jsonrpc": "2.0", "method": "notifications/initialized"})

    def close(self):
        if self.process.poll() is None:
            self.process.stdin.close()
            try:
                self.process.wait(timeout=8)
            except subprocess.TimeoutExpired:
                self.process.kill()
                self.process.wait(timeout=5)
                raise
        if not self.process.stdin.closed:
            self.process.stdin.close()
        self.reader.join(timeout=2)
        self.error_reader.join(timeout=2)
        self.process.stdout.close()
        self.process.stderr.close()


class CompatibilityTests(unittest.TestCase):
    def setUp(self):
        self.temporary = tempfile.TemporaryDirectory(prefix="native-js-compat-")
        self.scratch = Path(self.temporary.name)
        self.hosts = []
        original = self.scratch / "original-harness"
        original.mkdir()
        (original / "untouched").write_text("original synthetic profile")

    def tearDown(self):
        for host in self.hosts:
            host.close()
        self.assertEqual((self.scratch / "original-harness/untouched").read_text(), "original synthetic profile")
        self.temporary.cleanup()

    def host(self, **options):
        host = Host(self.scratch, **options)
        self.hosts.append(host)
        return host

    def test_unchanged_upstream_plugin_executes_through_real_session(self):
        host = self.host(plugin="@deepseek-ai/dsh-tool-working-directory")
        host.initialize()
        tools = host.rpc("tools/list")["result"]["tools"]
        self.assertEqual([tool["name"] for tool in tools], ["working_directory"])
        result = host.rpc("tools/call", {"name": "working_directory", "arguments": {}})["result"]
        self.assertFalse(result["isError"])
        self.assertEqual(result["content"], [{"type": "text", "text": str(self.scratch / "workspace")}])
        denied = host.rpc("tools/call", {"name": "bash", "arguments": {}})["result"]
        self.assertTrue(denied["isError"])
        host.close()
        self.assertEqual(host.process.returncode, 0)
        self.assertFalse((host.profile / "native-compat.lock").exists())
        self.assertEqual(host.profile.stat().st_mode & 0o777, 0o700)
        self.assertEqual((host.profile / "native-compat.json").stat().st_mode & 0o777, 0o600)

    def test_a_bundle_folder_needs_no_javascript_main_entry(self):
        bundle = self.scratch / "bundle-only"
        bundle.mkdir()
        (bundle / "package.json").write_text(json.dumps({"name": "compat-tool-bundle", "version": "1.0.0", "dsh": {"bundle": {"patch": "cordis.patch.yml"}}}))
        (bundle / "cordis.patch.yml").write_text(json.dumps([{"insert": [{"id": "selected-working-directory", "name": "@deepseek-ai/dsh-tool-working-directory"}]}]))
        host = self.host(plugin=bundle)
        host.initialize()
        self.assertEqual([tool["name"] for tool in host.rpc("tools/list")["result"]["tools"]], ["working_directory"])
        self.assertFalse(host.rpc("tools/call", {"name": "working_directory", "arguments": {}})["result"]["isError"])

    def test_real_bundle_loads_its_existing_tools_without_default_roster(self):
        host = self.host(plugin="@deepseek-ai/dsh-experimental-session-search")
        host.initialize()
        names = {tool["name"] for tool in host.rpc("tools/list")["result"]["tools"]}
        self.assertEqual(names, {"session_search", "session_event_search", "session_trace", "session_event_trace", "session_event_read"})
        result = host.rpc("tools/call", {"name": "session_trace", "arguments": {}})["result"]
        self.assertFalse(result["isError"])
        self.assertTrue(result["content"])

    def test_plain_plugin_peer_imports_and_logging_preserve_mcp_frames(self):
        host = self.host()
        host.initialize()
        tools = host.rpc("tools/list")["result"]["tools"]
        self.assertEqual({tool["name"] for tool in tools}, {"compat_echo", "compat_wait", "compat_environment"})
        response = host.rpc("tools/call", {"name": "compat_echo", "arguments": {"text": "hello café 世界"}})["result"]
        self.assertFalse(response["isError"])
        self.assertEqual(response["content"][0]["text"], "hello café 世界")
        host.close()
        self.assertFalse(b"".join(host.errors))
        self.assertTrue(all(json.loads(line)["jsonrpc"] == "2.0" for line in host.lines))
        self.assertNotIn(b"Ordinary plugin logging", b"".join(host.lines))

    def test_ambient_credentials_are_absent_and_never_logged(self):
        host = self.host()
        host.initialize()
        result = host.rpc("tools/call", {"name": "compat_environment", "arguments": {}})["result"]
        self.assertEqual(result["content"][0]["text"], "false")
        host.close()
        self.assertNotIn(KEY.encode(), b"".join(host.lines + host.errors))
        for path in host.profile.rglob("*"):
            if path.is_file():
                self.assertNotIn(KEY.encode(), path.read_bytes())

    def test_explicit_approved_credentials_reach_only_the_selected_plugin(self):
        host = self.host(env_names=["GITHUB_TOKEN"])
        host.initialize()
        result = host.rpc("tools/call", {"name": "compat_environment", "arguments": {"approved_github": True}})["result"]
        self.assertEqual(result["content"][0]["text"], "true")
        host.close()
        self.assertNotIn(KEY.encode(), b"".join(host.lines + host.errors))
        for path in host.profile.rglob("*"):
            if path.is_file():
                self.assertNotIn(KEY.encode(), path.read_bytes())

    def test_model_credentials_and_host_configuration_cannot_be_forwarded(self):
        for index, name in enumerate(["DEEPSEEK_API_KEY", "DSH_HOME", "DSH_UNTRUSTED_TOKEN", "deepseek_api_key"]):
            with self.subTest(name=name):
                profile = self.scratch / ("forbidden-" + str(index))
                host = self.host(profile=profile, env_names=[name])
                self.assertEqual(host.process.wait(timeout=8), 1)
                self.assertFalse(profile.exists())
                host.close()
                self.assertNotIn(KEY.encode(), b"".join(host.lines + host.errors))

    def test_cancellation_and_output_limits_are_visible_failures(self):
        host = self.host()
        host.initialize()
        host.send({"jsonrpc": "2.0", "id": 10, "method": "tools/call", "params": {"name": "compat_wait", "arguments": {}}})
        host.send({"jsonrpc": "2.0", "method": "notifications/cancelled", "params": {"requestId": 10}})
        self.assertTrue(host.result(10, timeout=4)["result"]["isError"])
        response = host.rpc("tools/call", {"name": "compat_echo", "arguments": {"text": "x" * 70000}})["result"]
        self.assertTrue(response["isError"])
        self.assertLess(len(json.dumps(response)), 500)
        self.assertEqual(host.rpc("ping")["result"], {})

    def test_an_existing_profile_and_live_lock_are_not_overwritten(self):
        host = self.host()
        host.initialize()
        second = self.host(profile=host.profile)
        self.assertEqual(second.process.wait(timeout=8), 1)
        self.assertEqual(host.rpc("ping")["result"], {})
        occupied = self.scratch / "occupied"
        occupied.mkdir()
        (occupied / "settings.json").write_text("private synthetic data")
        original_mode = occupied.stat().st_mode
        third = self.host(profile=occupied)
        self.assertEqual(third.process.wait(timeout=8), 1)
        self.assertEqual(list(occupied.iterdir()), [occupied / "settings.json"])
        self.assertEqual(occupied.stat().st_mode, original_mode)

    def test_oversized_frames_close_and_release_profile(self):
        host = self.host()
        host.initialize()
        host.process.stdin.write(b"x" * (1024 * 1024 + 1))
        host.process.stdin.flush()
        self.assertEqual(host.process.wait(timeout=8), 1)
        self.assertFalse((host.profile / "native-compat.lock").exists())

    def test_a_crashed_host_can_reopen_only_its_own_marked_profile(self):
        host = self.host()
        host.initialize()
        host.process.kill()
        host.process.wait(timeout=5)
        self.assertTrue((host.profile / "native-compat.lock").exists())
        replacement = self.host(profile=host.profile)
        replacement.initialize()
        self.assertEqual(replacement.rpc("ping")["result"], {})

    def test_a_plugin_without_server_tools_is_not_misreported_as_connected(self):
        plugin = self.scratch / "ui-only.mjs"
        plugin.write_text('export const name="ui-only"; export function apply() {}')
        host = self.host(plugin=plugin)
        self.assertEqual(host.process.wait(timeout=8), 1)
        host.close()
        self.assertFalse(host.lines)
        self.assertIn(b"could not start", b"".join(host.errors))
        self.assertFalse((host.profile / "native-compat.lock").exists())

    def test_a_wrong_runtime_is_rejected_before_creating_profile(self):
        runtime = self.scratch / "wrong-runtime"
        package = runtime / "node_modules/@deepseek-ai/dsh"
        package.mkdir(parents=True)
        (runtime / "package.json").write_text('{}')
        (package / "package.json").write_text('{"name":"@deepseek-ai/dsh","version":"999.0.0"}')
        host = self.host(runtime=runtime)
        self.assertEqual(host.process.wait(timeout=8), 1)
        self.assertFalse(host.profile.exists())
        host.close()
        self.assertNotIn(KEY.encode(), b"".join(host.lines + host.errors))


if __name__ == "__main__":
    if not (RUNTIME / "node_modules/@deepseek-ai/dsh/package.json").is_file():
        raise SystemExit("Prepare the pinned optional Harness runtime before running compatibility tests.")
    unittest.main(verbosity=2)
