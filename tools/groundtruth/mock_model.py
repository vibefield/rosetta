"""Deterministic Anthropic Messages and OpenAI Responses mock server."""

from __future__ import annotations

import argparse
import json
import os
import threading
from http.server import BaseHTTPRequestHandler, ThreadingHTTPServer
from pathlib import Path
from typing import Any

USAGE_IN = 128
USAGE_OUT = 32


def _compact(value: Any) -> str:
    return json.dumps(value, separators=(",", ":"), ensure_ascii=False)


class MockState:
    def __init__(self, log_path: Path | None = None) -> None:
        self._counter = 0
        self._lock = threading.Lock()
        self.log_path = log_path

    def next_id(self) -> int:
        with self._lock:
            value = self._counter
            self._counter += 1
            return value

    def log(self, entry: dict[str, Any]) -> None:
        if self.log_path is None:
            return
        with self._lock:
            self.log_path.parent.mkdir(parents=True, exist_ok=True)
            with self.log_path.open("a") as output:
                output.write(_compact(entry) + "\n")


class MockServer(ThreadingHTTPServer):
    daemon_threads = True

    def __init__(self, address: tuple[str, int], log_path: Path | None = None) -> None:
        self.state = MockState(log_path)
        super().__init__(address, MockHandler)


class MockHandler(BaseHTTPRequestHandler):
    protocol_version = "HTTP/1.1"
    server: MockServer

    def log_message(self, _format: str, *args: object) -> None:
        return

    def _request_json(self) -> dict[str, Any] | None:
        try:
            length = int(self.headers.get("content-length", "0"))
            value = json.loads(self.rfile.read(length))
        except (ValueError, json.JSONDecodeError):
            self._send_json(400, {"error": {"type": "invalid_request", "message": "invalid JSON"}})
            return None
        if not isinstance(value, dict):
            self._send_json(400, {"error": {"type": "invalid_request", "message": "expected object"}})
            return None
        return value

    def _send(self, status: int, body: bytes, content_type: str) -> None:
        self.send_response(status)
        self.send_header("content-type", content_type)
        self.send_header("content-length", str(len(body)))
        self.send_header("connection", "close")
        self.end_headers()
        self.wfile.write(body)
        self.close_connection = True

    def _send_json(self, status: int, value: Any) -> None:
        self._send(status, _compact(value).encode(), "application/json")

    def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        request = self._request_json()
        if request is None:
            return
        if self.path == "/v1/messages":
            self._anthropic_messages(request)
        elif self.path == "/v1/messages/count_tokens":
            self.server.state.log({"path": self.path, "request": request})
            self._send_json(200, {"input_tokens": USAGE_IN})
        elif self.path == "/v1/responses":
            self._openai_responses(request)
        else:
            self._not_found()

    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        self._not_found()

    def _not_found(self) -> None:
        self.server.state.log({"path": self.path, "unhandled": True})
        self._send_json(
            404,
            {
                "type": "error",
                "error": {
                    "type": "not_found_error",
                    "message": f"mockmodel: no route for {self.path}",
                },
            },
        )

    def _anthropic_messages(self, request: dict[str, Any]) -> None:
        request_id = self.server.state.next_id()
        model = request.get("model") if isinstance(request.get("model"), str) else "mock-model"
        stream = bool(request.get("stream", False))
        script = _anthropic_script(request)
        self.server.state.log(
            {
                "path": self.path,
                "id": request_id,
                "stream": stream,
                "response_kind": script[0],
                "request": request,
            }
        )
        message_id = f"msg_mock_{request_id:04}"
        if stream:
            body = _anthropic_sse(message_id, model, script).encode()
            self._send(200, body, "text/event-stream")
            return
        if script[0] == "text":
            content = [{"type": "text", "text": script[1]}]
            stop_reason = "end_turn"
        else:
            content = [
                {
                    "type": "tool_use",
                    "id": f"toolu_mock_{request_id:04}",
                    "name": script[1],
                    "input": script[2],
                }
            ]
            stop_reason = "tool_use"
        self._send_json(
            200,
            {
                "id": message_id,
                "type": "message",
                "role": "assistant",
                "model": model,
                "content": content,
                "stop_reason": stop_reason,
                "stop_sequence": None,
                "usage": _anthropic_usage(),
            },
        )

    def _openai_responses(self, request: dict[str, Any]) -> None:
        request_id = self.server.state.next_id()
        model = request.get("model") if isinstance(request.get("model"), str) else "mock-model"
        stream = bool(request.get("stream", False))
        script = _openai_script(request)
        self.server.state.log(
            {
                "path": self.path,
                "id": request_id,
                "stream": stream,
                "response_kind": script[0],
                "request": request,
            }
        )
        response, item = _openai_completed_response(request_id, model, script)
        if not stream:
            self._send_json(200, response)
            return
        body = _openai_sse(request_id, model, script, response, item).encode()
        self._send(200, body, "text/event-stream")


def _anthropic_script(request: dict[str, Any]) -> tuple[str, Any, Any]:
    messages = request.get("messages") if isinstance(request.get("messages"), list) else []
    for message in messages:
        if not isinstance(message, dict) or not isinstance(message.get("content"), list):
            continue
        if any(isinstance(block, dict) and block.get("type") == "tool_result" for block in message["content"]):
            return (
                "text",
                "The tool ran successfully and printed: rosetta-mock. Scenario complete.",
                None,
            )

    wants_tool = False
    for message in messages:
        if not isinstance(message, dict):
            continue
        content = message.get("content")
        if isinstance(content, str) and "[use-tool]" in content:
            wants_tool = True
        elif isinstance(content, list) and any(
            isinstance(block, dict)
            and block.get("type") == "text"
            and isinstance(block.get("text"), str)
            and "[use-tool]" in block["text"]
            for block in content
        ):
            wants_tool = True
    if wants_tool:
        tools = request.get("tools") if isinstance(request.get("tools"), list) else []
        names = [tool.get("name") for tool in tools if isinstance(tool, dict) and isinstance(tool.get("name"), str)]
        name = next((candidate for candidate in names if candidate.lower() == "bash"), names[0] if names else "Bash")
        inputs = {"command": "echo rosetta-mock"} if name.lower() == "bash" else {}
        return ("tool_use", name, inputs)
    return (
        "text",
        "Hello from rosetta mockmodel. This is a deterministic scripted response.",
        None,
    )


def _anthropic_usage() -> dict[str, int]:
    return {
        "input_tokens": USAGE_IN,
        "output_tokens": USAGE_OUT,
        "cache_creation_input_tokens": 0,
        "cache_read_input_tokens": 0,
    }


def _event(name: str, value: Any) -> str:
    return f"event: {name}\ndata: {_compact(value)}\n\n"


def _anthropic_sse(message_id: str, model: str, script: tuple[str, Any, Any]) -> str:
    parts = [
        _event(
            "message_start",
            {
                "type": "message_start",
                "message": {
                    "id": message_id,
                    "type": "message",
                    "role": "assistant",
                    "model": model,
                    "content": [],
                    "stop_reason": None,
                    "stop_sequence": None,
                    "usage": {**_anthropic_usage(), "output_tokens": 1},
                },
            },
        )
    ]
    if script[0] == "text":
        text = script[1]
        parts.append(
            _event(
                "content_block_start",
                {
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {"type": "text", "text": ""},
                },
            )
        )
        midpoint = len(text) // 2
        for chunk in (text[:midpoint], text[midpoint:]):
            if chunk:
                parts.append(
                    _event(
                        "content_block_delta",
                        {
                            "type": "content_block_delta",
                            "index": 0,
                            "delta": {"type": "text_delta", "text": chunk},
                        },
                    )
                )
        stop_reason = "end_turn"
    else:
        name, inputs = script[1], script[2]
        parts.append(
            _event(
                "content_block_start",
                {
                    "type": "content_block_start",
                    "index": 0,
                    "content_block": {
                        "type": "tool_use",
                        "id": f"{message_id}_tool",
                        "name": name,
                        "input": {},
                    },
                },
            )
        )
        arguments = _compact(inputs)
        midpoint = len(arguments) // 2
        for chunk in (arguments[:midpoint], arguments[midpoint:]):
            if chunk:
                parts.append(
                    _event(
                        "content_block_delta",
                        {
                            "type": "content_block_delta",
                            "index": 0,
                            "delta": {"type": "input_json_delta", "partial_json": chunk},
                        },
                    )
                )
        stop_reason = "tool_use"
    parts.extend(
        [
            _event("content_block_stop", {"type": "content_block_stop", "index": 0}),
            _event(
                "message_delta",
                {
                    "type": "message_delta",
                    "delta": {"stop_reason": stop_reason, "stop_sequence": None},
                    "usage": {"output_tokens": USAGE_OUT},
                },
            ),
            _event("message_stop", {"type": "message_stop"}),
        ]
    )
    return "".join(parts)


def _input_texts(request: dict[str, Any]) -> list[str]:
    value = request.get("input")
    if isinstance(value, str):
        return [value]
    texts: list[str] = []
    if not isinstance(value, list):
        return texts
    for item in value:
        if not isinstance(item, dict):
            continue
        content = item.get("content")
        if isinstance(content, str):
            texts.append(content)
        elif isinstance(content, list):
            texts.extend(
                block["text"]
                for block in content
                if isinstance(block, dict) and isinstance(block.get("text"), str)
            )
    return texts


def _openai_script(request: dict[str, Any]) -> tuple[str, str, str | None]:
    items = request.get("input") if isinstance(request.get("input"), list) else []
    if any(isinstance(item, dict) and item.get("type") == "function_call_output" for item in items):
        return (
            "text",
            "The tool ran successfully and printed: rosetta-mock. Scenario complete.",
            None,
        )
    if any("[use-tool]" in text for text in _input_texts(request)):
        tools = request.get("tools") if isinstance(request.get("tools"), list) else []
        names = [tool.get("name") for tool in tools if isinstance(tool, dict) and isinstance(tool.get("name"), str)]
        name = next((candidate for candidate in names if candidate.lower() == "exec_command"), None)
        name = name or next((candidate for candidate in names if candidate.lower() == "shell"), None)
        name = name or (names[0] if names else "shell")
        if name.lower() == "exec_command":
            arguments = _compact({"cmd": "echo rosetta-mock"})
        elif name.lower() == "shell":
            arguments = _compact({"command": ["echo", "rosetta-mock"]})
        else:
            arguments = "{}"
        return ("function_call", name, arguments)
    return (
        "text",
        "Hello from rosetta mockmodel. This is a deterministic scripted response.",
        None,
    )


def _openai_usage() -> dict[str, Any]:
    return {
        "input_tokens": USAGE_IN,
        "input_tokens_details": {"cached_tokens": 0},
        "output_tokens": USAGE_OUT,
        "output_tokens_details": {"reasoning_tokens": 0},
        "total_tokens": USAGE_IN + USAGE_OUT,
    }


def _openai_completed_response(
    request_id: int,
    model: str,
    script: tuple[str, str, str | None],
) -> tuple[dict[str, Any], dict[str, Any]]:
    if script[0] == "text":
        item = {
            "type": "message",
            "id": f"msg_mock_{request_id:04}",
            "status": "completed",
            "role": "assistant",
            "content": [{"type": "output_text", "annotations": [], "text": script[1]}],
        }
    else:
        item = {
            "type": "function_call",
            "id": f"fc_mock_{request_id:04}",
            "status": "completed",
            "call_id": f"call_mock_{request_id:04}",
            "name": script[1],
            "arguments": script[2],
        }
    response = {
        "id": f"resp_mock_{request_id:04}",
        "object": "response",
        "status": "completed",
        "created_at": 0,
        "model": model,
        "output": [item],
        "usage": _openai_usage(),
    }
    return response, item


def _openai_sse(
    request_id: int,
    model: str,
    script: tuple[str, str, str | None],
    completed: dict[str, Any],
    item: dict[str, Any],
) -> str:
    response_id = f"resp_mock_{request_id:04}"
    created = {
        "id": response_id,
        "object": "response",
        "status": "in_progress",
        "created_at": 0,
        "model": model,
        "output": [],
        "usage": None,
    }
    parts = [
        _event("response.created", {"type": "response.created", "response": created}),
        _event("response.in_progress", {"type": "response.in_progress", "response": created}),
    ]
    if script[0] == "text":
        item_id = f"msg_mock_{request_id:04}"
        parts.extend(
            [
                _event(
                    "response.output_item.added",
                    {
                        "type": "response.output_item.added",
                        "output_index": 0,
                        "item": {
                            "type": "message",
                            "id": item_id,
                            "status": "in_progress",
                            "role": "assistant",
                            "content": [],
                        },
                    },
                ),
                _event(
                    "response.content_part.added",
                    {
                        "type": "response.content_part.added",
                        "item_id": item_id,
                        "output_index": 0,
                        "content_index": 0,
                        "part": {"type": "output_text", "annotations": [], "text": ""},
                    },
                ),
            ]
        )
        text = script[1]
        midpoint = len(text) // 2
        for chunk in (text[:midpoint], text[midpoint:]):
            if chunk:
                parts.append(
                    _event(
                        "response.output_text.delta",
                        {
                            "type": "response.output_text.delta",
                            "item_id": item_id,
                            "output_index": 0,
                            "content_index": 0,
                            "delta": chunk,
                        },
                    )
                )
        parts.append(
            _event(
                "response.output_text.done",
                {
                    "type": "response.output_text.done",
                    "item_id": item_id,
                    "output_index": 0,
                    "content_index": 0,
                    "text": text,
                },
            )
        )
    else:
        item_id = f"fc_mock_{request_id:04}"
        parts.extend(
            [
                _event(
                    "response.output_item.added",
                    {
                        "type": "response.output_item.added",
                        "output_index": 0,
                        "item": {
                            "type": "function_call",
                            "id": item_id,
                            "status": "in_progress",
                            "call_id": f"call_mock_{request_id:04}",
                            "name": script[1],
                            "arguments": "",
                        },
                    },
                ),
                _event(
                    "response.function_call_arguments.delta",
                    {
                        "type": "response.function_call_arguments.delta",
                        "item_id": item_id,
                        "output_index": 0,
                        "delta": script[2],
                    },
                ),
                _event(
                    "response.function_call_arguments.done",
                    {
                        "type": "response.function_call_arguments.done",
                        "item_id": item_id,
                        "output_index": 0,
                        "arguments": script[2],
                    },
                ),
            ]
        )
    parts.extend(
        [
            _event(
                "response.output_item.done",
                {"type": "response.output_item.done", "output_index": 0, "item": item},
            ),
            _event(
                "response.completed",
                {"type": "response.completed", "response": completed},
            ),
        ]
    )
    return "".join(parts)


def start_server(port: int = 0, log_path: Path | None = None) -> tuple[MockServer, threading.Thread]:
    server = MockServer(("127.0.0.1", port), log_path)
    thread = threading.Thread(target=server.serve_forever, name="rosetta-mockmodel", daemon=True)
    thread.start()
    return server, thread


def main(argv: list[str] | None = None) -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("port", nargs="?", type=int, default=0)
    default_log = os.environ.get("MOCKMODEL_LOG")
    parser.add_argument("--log", type=Path, default=Path(default_log) if default_log else None)
    args = parser.parse_args(argv)
    server = MockServer(("127.0.0.1", args.port), args.log)
    host, port = server.server_address
    print(f"mockmodel listening on http://{host}:{port}", flush=True)
    try:
        server.serve_forever()
    except KeyboardInterrupt:
        pass
    finally:
        server.server_close()
    return 0
