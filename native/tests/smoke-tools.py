#!/usr/bin/env python3
"""Drive real native MCP controls against a disposable local tool/provider pair."""

import json
import importlib.util
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
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer

HELPERS = importlib.util.spec_from_file_location("native_smoke_helpers", Path(__file__).with_name("smoke-native.py"))
helpers = importlib.util.module_from_spec(HELPERS)
HELPERS.loader.exec_module(helpers)
Desktop, require, wait = helpers.Desktop, helpers.require, helpers.wait
snapshot, test_environment = helpers.snapshot, helpers.test_environment
ROOT = Path(__file__).resolve().parents[2]
KEY = "native-tool-smoke-synthetic-key-not-a-provider-secret"
DENIED, ALLOWED, HANG, FAILED_SAVE = "TOOL_DENIED", "TOOL_ALLOWED", "TOOL_HANG", "TOOL_SAVE_FAIL"
DENIED_ANSWER, ALLOWED_ANSWER = "Native tool request declined", "Native tool execution completed"


def mcp_fixture(folder):
    """An executable local MCP server. Only an approved call writes the log."""
    folder = Path(folder)
    folder.mkdir(parents=True, exist_ok=True)
    (folder / "pid").write_text(str(os.getpid()))
    for line in sys.stdin:
        request = json.loads(line)
        method = request.get("method")
        result = None
        if method == "initialize":
            result = {"protocolVersion": "2025-03-26", "capabilities": {"tools": {}}, "serverInfo": {"name": "native-gui-fixture", "version": "1"}}
        elif method == "tools/list":
            result = {"tools": [{"name": "echo", "description": "Local GUI smoke echo", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}, "required": ["text"], "additionalProperties": False}}]}
        elif method == "tools/call":
            arguments = request["params"]["arguments"]
            with (folder / "calls.jsonl").open("a") as output:
                output.write(json.dumps(arguments) + "\n")
            if arguments["text"] == "hang":
                time.sleep(120)
            text = json.dumps({"text": arguments["text"], "inherited_model_key": "DEEPSEEK_API_KEY" in os.environ})
            result = {"content": [{"type": "text", "text": text}], "isError": False}
        elif "id" not in request:
            continue
        else:
            print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "error": {"code": -32601, "message": "Unsupported fixture request"}}), flush=True)
            continue
        print(json.dumps({"jsonrpc": "2.0", "id": request["id"], "result": result}), flush=True)


class Provider(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self):
        self.requests = 0
        self.failures = []
        self.save_plan = threading.Event()
        self.save_request = threading.Event()
        super().__init__(("127.0.0.1", 0), Handler)
        threading.Thread(target=self.serve_forever, daemon=True).start()

    def close(self):
        self.save_plan.set()
        self.shutdown()
        self.server_close()


class Handler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"

    def log_message(self, *_args):
        pass

    def chunk(self, delta, finish=None):
        body = {"choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}
        self.wfile.write(("data: " + json.dumps(body) + "\n\n").encode())
        self.wfile.flush()

    def do_POST(self):
        provider = self.server
        try:
            require(self.path == "/chat/completions", "Only the fixture chat endpoint is called")
            require(self.headers.get("Authorization") == "Bearer " + KEY, "Only the synthetic model key reaches the fixture")
            size = int(self.headers.get("Content-Length", "0"))
            require(0 < size < 1024 * 1024, "The fixture request is bounded")
            request = json.loads(self.rfile.read(size))
            require(request["model"] == "native-tool-smoke", "Only the test model is requested")
            tools = request.get("tools", [])
            require(len(tools) == 1, "The connected MCP server contributes exactly one model tool")
            name = tools[0]["function"]["name"]
            require(name.startswith("mcp__fixture__"), "The model tool belongs to the selected connection")
            messages = request["messages"]
            prompt = next(message["content"] for message in reversed(messages) if message["role"] == "user")
            require(prompt in (DENIED, ALLOWED, HANG, FAILED_SAVE), "Only fixture prompts are sent")
            provider.requests += 1
            tool_result = messages[-1] if messages[-1]["role"] == "tool" else None
            self.send_response(200)
            self.send_header("Content-Type", "text/event-stream")
            self.send_header("Connection", "close")
            self.end_headers()
            self.close_connection = True
            if tool_result:
                require(prompt in (DENIED, ALLOWED), "Stopped or unsaved calls never continue to another provider request")
                if prompt == DENIED:
                    require("declined" in tool_result["content"], "Denial returns a clear tool error to the model")
                    answer = DENIED_ANSWER
                else:
                    result = json.loads(tool_result["content"])
                    require(result == {"text": "approved café", "inherited_model_key": False}, "The real MCP result returns with no inherited model key")
                    answer = ALLOWED_ANSWER
                self.chunk({"reasoning_content": "Fixture final reasoning", "content": answer})
                self.chunk({}, "stop")
            else:
                if prompt == FAILED_SAVE:
                    provider.save_request.set()
                    require(provider.save_plan.wait(25), "The plan is released after storage becomes unwritable")
                text = {DENIED: "denied text", ALLOWED: "approved café", HANG: "hang", FAILED_SAVE: "must not run"}[prompt]
                arguments = json.dumps({"text": text})
                midpoint = len(arguments) // 2
                self.chunk({"reasoning_content": "Fixture tool reasoning", "tool_calls": [{"index": 0, "id": "call_" + prompt.lower(), "type": "function", "function": {"name": name, "arguments": arguments[:midpoint]}}]})
                self.chunk({"tool_calls": [{"index": 0, "function": {"arguments": arguments[midpoint:]}}]})
                self.chunk({}, "tool_calls")
            self.wfile.write(b"data: [DONE]\n\n")
            self.wfile.flush()
        except (BrokenPipeError, ConnectionResetError, socket.error):
            pass
        except Exception as error:
            provider.failures.append(type(error).__name__ + ": " + str(error))
            self.close_connection = True


def process_gone(pid):
    try:
        return Path(f"/proc/{pid}/stat").read_text().split()[2] == "Z"
    except FileNotFoundError:
        return True


def run(application):
    artifacts = ROOT / "artifacts"
    artifacts.mkdir(exist_ok=True)
    report = artifacts / "native-tools-smoke.json"
    report.unlink(missing_ok=True)
    provider = Provider()
    process = None
    output = None
    stage = "startup"
    with tempfile.TemporaryDirectory(prefix="harness-native-tools-") as scratch:
        scratch = Path(scratch)
        environment = test_environment(scratch)
        environment["DEEPSEEK_API_KEY"] = KEY
        environment["G_DEBUG"] = "fatal-criticals"
        profile = scratch / "data/deepseek-harness-native"
        marker = scratch / "fixture"

        def state():
            return json.loads((profile / "state.json").read_text())

        def last():
            return state()["sessions"][0]["messages"][-1]

        def calls():
            path = marker / "calls.jsonl"
            return [json.loads(line) for line in path.read_text().splitlines()] if path.exists() else []

        def child_pid():
            return int((marker / "pid").read_text())

        def launch():
            nonlocal process, output
            output = tempfile.TemporaryFile()
            process = subprocess.Popen([str(application)], cwd=scratch, env=environment, stdout=output, stderr=output, start_new_session=True)
            desktop = Desktop(process, environment)
            wait(lambda: desktop.find("Message", editable=True), "The native composer opens", timeout=25)
            return desktop

        def close_connections(desktop, window):
            desktop.click("Close", window)
            wait(lambda: not desktop.find("Tool connections", role=("dialog", "frame")), "Tool connections closes")

        def send(desktop, prompt):
            desktop.set_text("Message", prompt)
            desktop.click("Send")

        def private_state():
            for path in profile.rglob("*"):
                if path.is_file():
                    require(KEY.encode() not in path.read_bytes(), "The model key is absent from persistent state and tool config")
            output.flush()
            output.seek(0)
            require(KEY.encode() not in output.read(), "The model key is absent from stdout and stderr")
            output.seek(0, os.SEEK_END)

        try:
            desktop = launch()
            desktop.click("Settings")
            settings = desktop.window("Settings")
            desktop.set_text("Model", "native-tool-smoke", settings)
            desktop.set_text("API endpoint", f"http://127.0.0.1:{provider.server_port}", settings)
            desktop.set_text("API key", KEY, settings)
            desktop.click("Save settings", settings)
            stage = "MCP configuration"
            desktop.click("Tool connections")
            connections = desktop.window("Tool connections")
            desktop.click("Add MCP server", connections)
            form = desktop.window("Add MCP server")
            desktop.set_text("Connection ID", "fixture", form)
            desktop.set_text("Program", sys.executable, form)
            desktop.set_text("Arguments", "\n".join([str(Path(__file__).resolve()), "--mcp-fixture", str(marker)]), form)
            desktop.click("Save connection", form)
            wait(lambda: desktop.has_text("fixture", connections) and desktop.has_text("Stopped", connections), "Saving leaves the server stopped")
            require(not (marker / "pid").exists(), "Saving a connection does not execute its program")
            desktop.click("Connect", connections)
            wait(lambda: desktop.has_text("Connected · 1 tools", connections), "Connect discovers the fixture's real MCP tool")
            first_pid = child_pid()
            close_connections(desktop, connections)
            stage = "denied tool call"
            send(desktop, DENIED)
            permission = desktop.window("Allow tool call?")
            require(calls() == [], "A proposed tool is not run before permission")
            require(last()["tool_trace"][0]["tool_calls"][0]["id"] == "call_tool_denied", "The planned call is durable before its permission prompt")
            desktop.click("Deny", permission)
            wait(lambda: last()["status"] == "complete" and DENIED_ANSWER in last()["content"], "The model continues after a declined tool")
            require(calls() == [], "Declining never executes the external tool")
            require(last()["tool_trace"][0]["results"][0]["is_error"], "The declined result is saved as an error")
            stage = "allowed tool call"
            send(desktop, ALLOWED)
            permission = desktop.window("Allow tool call?")
            require(desktop.has_text("approved café", permission), "Permission shows the actual model arguments")
            require(calls() == [], "The second proposed tool also waits for approval")
            snapshot(artifacts / "native-tools-approval.png")
            desktop.click("Allow once", permission)
            wait(lambda: last()["status"] == "complete" and ALLOWED_ANSWER in last()["content"], "The real MCP result reaches the follow-up model request")
            require(calls() == [{"text": "approved café"}], "The approved call executes exactly once")
            wait(lambda: desktop.visible_message(ALLOWED_ANSWER), "The final tool response is drawn in the native history")
            require(state()["version"] == 2, "Canonical tool history uses the guarded profile version")
            desktop.click("Tool activity")
            activity = desktop.window("Tool activity")
            require(desktop.has_text("mcp__fixture__", activity), "Saved tool activity is inspectable in the native app")
            snapshot(artifacts / "native-tools-activity.png")
            desktop.click("Close", activity)
            stage = "disconnect and reconnect"
            desktop.click("Tool connections")
            connections = desktop.window("Tool connections")
            desktop.click("Disconnect", connections)
            wait(lambda: desktop.has_text("Stopped", connections), "Disconnect updates the visible status")
            wait(lambda: process_gone(first_pid), "Disconnect reaps the owned server process")
            desktop.click("Connect", connections)
            wait(lambda: desktop.has_text("Connected · 1 tools", connections), "An explicit reconnect refreshes the MCP tool")
            close_connections(desktop, connections)
            stage = "cancel an active tool"
            send(desktop, HANG)
            permission = desktop.window("Allow tool call?")
            desktop.click("Allow once", permission)
            wait(lambda: calls()[-1] == {"text": "hang"}, "The long-running fixture started only after approval")
            hanging_pid = child_pid()
            desktop.click("Stop")
            wait(lambda: last()["status"] == "cancelled", "Stop saves the interrupted tool conversation")
            wait(lambda: process_gone(hanging_pid), "Stop terminates and reaps the hanging MCP process")
            require(len(calls()) == 2, "Cancellation never retries the external action")
            desktop.click("Tool connections")
            connections = desktop.window("Tool connections")
            require(desktop.has_text("Stopped", connections), "A cancelled server is shown as stopped")
            desktop.click("Connect", connections)
            wait(lambda: desktop.has_text("Connected · 1 tools", connections), "Reconnect is explicit after cancellation")
            close_connections(desktop, connections)
            stage = "failed plan checkpoint"
            send(desktop, FAILED_SAVE)
            wait(provider.save_request.is_set, "The provider waits before emitting its tool plan")
            state_path = profile / "state.json"
            backup = profile / "tool-smoke-state-backup"
            state_path.rename(backup)
            state_path.mkdir()
            before = len(calls())
            provider.save_plan.set()
            wait(lambda: desktop.find("Retry save"), "The failed plan save exposes recovery")
            require(not desktop.find("Allow tool call?", role=("dialog", "frame")), "An unsaved tool plan never prompts execution")
            require(len(calls()) == before, "An unsaved plan cannot run a tool")
            state_path.rmdir()
            backup.rename(state_path)
            requests_before_save = provider.requests
            desktop.click("Retry save")
            wait(lambda: not desktop.find("Retry save"), "Repair and Retry save persist the interrupted plan")
            require(provider.requests == requests_before_save and len(calls()) == before, "Retry save repeats neither model nor external action")
            private_state()
            stage = "restart keeps connections stopped"
            idle_pid = child_pid()
            desktop.close()
            wait(lambda: process_gone(idle_pid), "App close shuts down its MCP server")
            output.close()
            desktop = launch()
            desktop.click("Tool connections")
            connections = desktop.window("Tool connections")
            require(desktop.has_text("fixture", connections) and desktop.has_text("Stopped", connections), "Reopening restores settings but leaves the connection stopped")
            require(child_pid() == idle_pid and process_gone(idle_pid), "Reopening does not auto-start a configured program")
            close_connections(desktop, connections)
            require(any(message.get("tool_trace") for message in state()["sessions"][0]["messages"]), "Tool transcripts survive a graceful restart")
            require(provider.failures == [], "The fixture observed no invalid model/tool protocol")
            require(provider.requests == 6, "Exactly six synthetic model requests cover denial, execution, stop and failed storage")
            private_state()
            desktop.close()
            report.write_text(json.dumps({"provider": "synthetic loopback only", "mcpConfigurationAndDiscovery": "passed", "permissionDenyAndAllow": "passed", "modelToolContinuation": "passed", "toolCheckpointFailure": "passed", "stopAndDisconnect": "passed", "restartWithoutAutoStart": "passed", "credentialPrivacy": "passed", "providerRequests": provider.requests, "actualToolExecutions": len(calls())}, indent=2) + "\n")
        except Exception:
            captured = ""
            if output:
                output.flush()
                output.seek(0, os.SEEK_END)
                output.seek(max(0, output.tell() - 8192))
                captured = output.read().decode("utf-8", errors="replace").replace(KEY, "[synthetic key redacted]")
            (artifacts / "native-tools-failure.json").write_text(json.dumps({"stage": stage, "providerRequests": provider.requests, "actualToolExecutions": len(calls()), "providerFailures": provider.failures, "processReturncode": process.poll() if process else None, "appOutputTail": captured}, indent=2) + "\n")
            try:
                snapshot(artifacts / "native-tools-failure.png")
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
    if len(sys.argv) == 3 and sys.argv[1] == "--mcp-fixture":
        mcp_fixture(sys.argv[2])
    else:
        require(len(sys.argv) == 2, "Usage: smoke-tools.py PATH_TO_NATIVE_APP")
        application = Path(sys.argv[1]).resolve()
        require(application.is_file(), "Build or install the native application first")
        require(shutil.which("tesseract"), "Install the local OCR test tool")
        if os.environ.get("HARNESS_NATIVE_TOOLS_TEST_BUS") != "1":
            require(shutil.which("xvfb-run"), "Install xvfb for an isolated desktop test")
            with tempfile.TemporaryDirectory(prefix="harness-tool-session-") as session:
                environment = test_environment(Path(session))
                environment["HARNESS_NATIVE_TOOLS_TEST_BUS"] = "1"
                result = subprocess.run(["xvfb-run", "-a", "dbus-run-session", "--", sys.executable, str(Path(__file__).resolve()), str(application)], env=environment, timeout=180)
                raise SystemExit(result.returncode)
        run(application)
