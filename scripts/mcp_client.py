#!/usr/bin/env python3
"""Small stdio JSON-RPC client for the idea_db MCP server.

Each tool call owns its short-lived subprocess.  This keeps concurrent
acceptance writes independent and avoids sharing JSON-RPC streams between test
threads.
"""

from __future__ import annotations

import json
import os
import subprocess
import selectors
import time
import uuid
from pathlib import Path
from dataclasses import dataclass
from typing import Any


Json = dict[str, Any]


class McpProtocolError(RuntimeError):
    pass


@dataclass
class McpToolError(RuntimeError):
    error: Json

    def __post_init__(self) -> None:
        super().__init__(str(self.error.get("message") or self.error))


def command_from_env() -> list[str]:
    raw = os.environ.get("IDEA_DB_MCP_COMMAND_JSON")
    if not raw:
        raise McpProtocolError("IDEA_DB_MCP_COMMAND_JSON is required for MCP mutations")
    try:
        command = json.loads(raw)
    except json.JSONDecodeError as exc:
        raise McpProtocolError("IDEA_DB_MCP_COMMAND_JSON must be a JSON command array") from exc
    if not isinstance(command, list) or not command or not all(isinstance(item, str) and item for item in command):
        raise McpProtocolError("IDEA_DB_MCP_COMMAND_JSON must be a nonempty string array")
    return command


def _messages(tool: str, arguments: Json) -> bytes:
    requests = (
        {
            "jsonrpc": "2.0",
            "id": 1,
            "method": "initialize",
            "params": {
                "protocolVersion": "2025-11-25",
                "capabilities": {},
                "clientInfo": {"name": "idea-db-client", "version": "1"},
            },
        },
        {"jsonrpc": "2.0", "method": "notifications/initialized", "params": {}},
        {"jsonrpc": "2.0", "id": 2, "method": "tools/call", "params": {"name": tool, "arguments": arguments}},
    )
    return b"".join(
        json.dumps(message, ensure_ascii=False, separators=(",", ":")).encode("utf-8") + b"\n"
        for message in requests
    )


def _reply(stdout: bytes, stderr: bytes) -> Json:
    responses: dict[int, Json] = {}
    for line in stdout.decode("utf-8", errors="replace").splitlines():
        try:
            value = json.loads(line)
        except json.JSONDecodeError:
            continue
        if isinstance(value, dict) and isinstance(value.get("id"), int):
            responses[value["id"]] = value
    response = responses.get(2)
    if not response:
        detail = stderr.decode("utf-8", errors="replace").strip()
        raise McpProtocolError(f"MCP tools/call response missing{': ' + detail if detail else ''}")
    if "error" in response:
        error = response["error"]
        raise McpProtocolError(f"MCP JSON-RPC error: {error!r}")
    result = response.get("result")
    if not isinstance(result, dict):
        raise McpProtocolError(f"MCP tools/call result is not an object: {result!r}")
    return result


def _structured_error(result: Json) -> Json:
    structured = result.get("structuredContent")
    if isinstance(structured, dict) and isinstance(structured.get("error"), dict):
        return structured["error"]
    for item in result.get("content", []):
        if not isinstance(item, dict) or item.get("type") != "text" or not isinstance(item.get("text"), str):
            continue
        try:
            parsed = json.loads(item["text"])
        except json.JSONDecodeError:
            continue
        if isinstance(parsed, dict) and isinstance(parsed.get("error"), dict):
            return parsed["error"]
    return {"status": 500, "code": "mcp_tool_error", "message": "MCP tool reported an unstructured error"}


def call_tool(tool: str, arguments: Json, timeout: float) -> Json:
    """Call one MCP tool and return its structured content.

    Domain failures are preserved as :class:`McpToolError` with the server's
    ``status``, ``code``, ``message``, and ``details`` object.
    """
    # Keep stdin open until the result: EOF cancels in-flight SDK requests.
    log_directory = os.environ.get("IDEA_DB_MCP_LOG_DIR")
    log_file = None
    if log_directory:
        directory = Path(log_directory)
        directory.mkdir(parents=True, exist_ok=True)
        log_path = directory / (str(time.time_ns()) + "-" + uuid.uuid4().hex + ".stderr.log")
        log_file = os.fdopen(os.open(log_path, os.O_WRONLY | os.O_CREAT | os.O_EXCL, 0o600), "wb")
    process = subprocess.Popen(command_from_env(), stdin=subprocess.PIPE,
                               stdout=subprocess.PIPE, stderr=log_file or subprocess.DEVNULL, bufsize=0)
    deadline = time.monotonic() + timeout
    selector = selectors.DefaultSelector()
    selector.register(process.stdout, selectors.EVENT_READ)
    pending = bytearray()
    def receive(expected_id):
        while True:
            while b"\n" in pending:
                line, _, rest = pending.partition(b"\n")
                pending[:] = rest
                try:
                    message = json.loads(line)
                except (ValueError, UnicodeDecodeError) as exc:
                    raise McpProtocolError("Non-JSON message on MCP stdout") from exc
                if message.get("id") == expected_id:
                    return message
            remaining = deadline - time.monotonic()
            if remaining <= 0 or not selector.select(remaining):
                raise McpProtocolError("MCP call timed out")
            chunk = os.read(process.stdout.fileno(), 65536)
            if not chunk:
                raise McpProtocolError("MCP process exited before its response")
            pending.extend(chunk)
            if len(pending) > 70 * 1024 * 1024:
                raise McpProtocolError("MCP response exceeded bound")
    try:
        frames = _messages(tool, arguments).splitlines(keepends=True)
        process.stdin.write(frames[0])
        initialized = receive(1)
        if "error" in initialized or initialized.get("result", {}).get("protocolVersion") != "2025-11-25":
            raise McpProtocolError("MCP initialization/version negotiation failed")
        process.stdin.write(frames[1])
        process.stdin.write(frames[2])
        response = receive(2)
        if "error" in response:
            raise McpProtocolError("MCP JSON-RPC error: " + repr(response["error"]))
        result = response.get("result")
        if not isinstance(result, dict):
            raise McpProtocolError("MCP tool result must be an object")
    finally:
        selector.close()
        if process.stdin:
            process.stdin.close()
        try:
            process.wait(timeout=5)
        except subprocess.TimeoutExpired:
            process.kill()
            process.wait(timeout=5)
        if process.stdout:
            process.stdout.close()
        if log_file:
            log_file.close()
    if result.get("isError"):
        raise McpToolError(_structured_error(result))
    structured = result.get("structuredContent")
    if isinstance(structured, dict):
        return structured
    for item in result.get("content", []):
        if isinstance(item, dict) and item.get("type") == "text" and isinstance(item.get("text"), str):
            try:
                parsed = json.loads(item["text"])
            except json.JSONDecodeError:
                continue
            if isinstance(parsed, dict):
                return parsed
    raise McpProtocolError(f"MCP tool {tool} returned no structured JSON content")
