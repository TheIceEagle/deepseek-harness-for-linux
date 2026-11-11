"""A local MCP fixture, never a provider or browser connection."""
import json
import os
from pathlib import Path
import subprocess
import sys
import time

mode = sys.argv[1]
marker = Path(sys.argv[2]) if len(sys.argv) > 2 else None
if marker:
    marker.write_text(str(os.getpid()))
    with marker.with_suffix(".pids").open("a") as output:
        output.write(str(os.getpid()) + "\n")


def respond(request, result=None, error=None):
    value = {"jsonrpc": "2.0", "id": request["id"]}
    value["error" if error is not None else "result"] = error if error is not None else result
    print(json.dumps(value), flush=True)


def tool(name):
    return {"name": name, "description": "Fixture text tool", "inputSchema": {"type": "object", "properties": {"text": {"type": "string"}}}}


for line in sys.stdin:
    request = json.loads(line)
    method = request.get("method")
    if marker:
        with marker.with_suffix(".methods").open("a") as output:
            output.write(method + "\n")
    if mode.startswith("modern") and method in ("server/discover", "tools/list", "tools/call"):
        meta = request.get("params", {}).get("_meta", {})
        assert meta["io.modelcontextprotocol/protocolVersion"] == "2026-07-28"
        assert meta["io.modelcontextprotocol/clientInfo"]["name"] == "deepseek-harness-native"
        assert meta["io.modelcontextprotocol/clientCapabilities"] == {}
    if method == "server/discover":
        if mode in ("ignore_discovery", "silent_startup"):
            continue
        if mode == "malformed_discovery":
            print("synthetic-private-discovery-marker", flush=True)
            continue
        if mode == "partial_discovery":
            sys.stdout.write("{")
            sys.stdout.flush()
            continue
        if mode == "modern_rejection":
            respond(request, error={"code": -32021, "message": "synthetic-private-discovery-marker"})
        elif mode == "modern_header_rejection":
            respond(request, error={"code": -32020, "message": "synthetic-private-discovery-marker"})
        elif mode.startswith("modern"):
            versions = ["2025-11-25"] if mode == "modern_legacy_version" else ["2026-07-28"]
            respond(request, {"resultType": "complete", "supportedVersions": versions, "capabilities": {"tools": {}}, "ttlMs": 0, "cacheScope": "private"})
        else:
            respond(request, error={"code": -32601, "message": "synthetic-private-discovery-marker"})
    elif method == "initialize":
        if mode.startswith("modern"):
            respond(request, error={"code": -32601, "message": "legacy handshake forbidden"})
            continue
        if mode == "silent_startup":
            continue
        if mode == "legacy_slow_init":
            time.sleep(1.2)
        if mode in ("legacy_slow_init", "ignore_discovery"):
            assert request["params"]["protocolVersion"] == "2025-11-25"
        if mode == "stall":
            time.sleep(60)
        if mode == "huge_line":
            sys.stdout.write("x" * (2 * 1024 * 1024))
            sys.stdout.flush()
            time.sleep(60)
        respond(request, {"protocolVersion": "2025-03-26", "capabilities": {"tools": {}}, "serverInfo": {"name": "local-fixture", "version": "1"}})
    elif method == "tools/list":
        if mode == "modern_slow_catalog" and not request.get("params", {}).get("cursor"):
            time.sleep(1.2)
        if mode == "duplicate":
            respond(request, {"tools": [tool("echo"), tool("echo")]})
        elif mode == "many":
            respond(request, {"tools": [tool("tool_" + str(index)) for index in range(65)]})
        elif mode == "cycle":
            respond(request, {"tools": [], "nextCursor": "same"})
        elif mode == "large_schema":
            value = tool("echo")
            value["inputSchema"]["description"] = "x" * (17 * 1024)
            respond(request, {"tools": [value]})
        elif request.get("params", {}).get("cursor") == "second":
            respond(request, {"tools": [tool("second")]})
        else:
            respond(request, {"tools": [tool("echo")], "nextCursor": "second"})
    elif method == "tools/call":
        arguments = request["params"].get("arguments", {})
        if mode in ("hang", "modern_hang"):
            if marker:
                marker.with_suffix(".called").write_text("called")
            time.sleep(60)
        elif mode == "error":
            respond(request, error={"code": -32000, "message": "synthetic-private-error-marker"})
        elif mode == "tool_error":
            respond(request, {"content": [{"type": "text", "text": "synthetic-private-error-marker"}], "isError": True})
        elif mode == "image":
            respond(request, {"content": [{"type": "image", "data": "AA==", "mimeType": "image/png"}]})
        elif mode == "large_output":
            respond(request, {"content": [{"type": "text", "text": "x" * (65536 + 1)}]})
        elif mode in ("environment", "modern_environment"):
            values = {"modelKey": "DEEPSEEK_API_KEY" in os.environ, "ambientSecret": "AWS_SECRET_ACCESS_KEY" in os.environ, "explicit": os.environ.get("HARNESS_MCP_EXPLICIT") == "per-server-marker"}
            respond(request, {"content": [{"type": "text", "text": json.dumps(values)}]})
        elif mode == "grandchild":
            process = subprocess.Popen([sys.executable, "-c", "import time; time.sleep(60)"])
            if marker:
                marker.with_suffix(".grandchild").write_text(str(process.pid))
            respond(request, {"content": [{"type": "text", "text": "spawned"}]})
            os._exit(0)
        elif mode == "structured":
            respond(request, {"content": [], "structuredContent": {"value": "café 🌊"}})
        else:
            respond(request, {"content": [{"type": "text", "text": arguments.get("text", "fixture result")}], "isError": False})
    elif "id" in request:
        respond(request, error={"code": -32601, "message": "unsupported"})
