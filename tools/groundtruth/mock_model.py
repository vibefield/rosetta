"""Deterministic Anthropic Messages, OpenAI Responses, Chat Completions, and Gemini generateContent mock."""

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
HIGH_USAGE_IN = 900
USAGE_MARKER = "[usage]"


def usage_numbers(turn: int) -> dict[str, int]:
    """Distinct non-zero counts for the `[usage]` scenario (RESEARCH_PLAN §5.13).

    Every bucket is a different prime-ish value so a fixture shows exactly how
    an agent folds each wire field: which subsets it subtracts, which it keeps
    inside a total, and which it drops. `turn` (0 before a tool result, 1
    after) shifts every value so two requests in one session stay distinct.
    """
    uncached = 1009 + 100 * turn
    cache_read = 5003 + 1000 * turn
    write_5m = 1201 + 10 * turn
    write_1h = 809 + 10 * turn
    visible = 197 + 10 * turn
    reasoning = 211 + 10 * turn
    return {
        "uncached": uncached,
        "cache_read": cache_read,
        "write_5m": write_5m,
        "write_1h": write_1h,
        "visible": visible,
        "reasoning": reasoning,
        "web_searches": 2,
        "web_fetches": 1,
        # xAI's billed amount, 10^10 ticks per USD.
        "cost_ticks": 1_234_500 + turn,
    }


def wants_usage(request: dict[str, Any]) -> bool:
    return USAGE_MARKER in _compact(request)
GREETING = "Hello from rosetta mockmodel. This is a deterministic scripted response."
TOOL_DONE = "The tool ran successfully and printed: rosetta-mock. Scenario complete."


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
        try:
            self.wfile.write(body)
        except (BrokenPipeError, ConnectionResetError):
            pass
        self.close_connection = True

    def _send_json(self, status: int, value: Any) -> None:
        self._send(status, _compact(value).encode(), "application/json")

    def do_POST(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        request = self._request_json()
        if request is None:
            return
        path = self.path.split("?", 1)[0].rstrip("/") or "/"
        if path == "/v1/messages":
            self._anthropic_messages(request)
        elif path == "/v1/messages/count_tokens":
            self.server.state.log({"path": self.path, "request": request})
            self._send_json(200, {"input_tokens": USAGE_IN})
        elif path == "/v1/responses":
            self._openai_responses(request)
        elif path.endswith("/chat/completions"):
            self._openai_chat(request)
        elif ":countTokens" in path:
            self._gemini_count_tokens(request)
        elif ":streamGenerateContent" in path or ":generateContent" in path:
            query = self.path.split("?", 1)[1] if "?" in self.path else ""
            streamed = ":streamGenerateContent" in path or "alt=sse" in query
            self._gemini_generate(request, streamed)
        else:
            self._not_found()

    def do_GET(self) -> None:  # noqa: N802 - BaseHTTPRequestHandler API
        path = self.path.split("?", 1)[0].rstrip("/") or "/"
        if path in {"/v1/models", "/models"}:
            self.server.state.log({"path": self.path, "method": "GET"})
            self._send_json(200, _models_list())
            return
        if path.startswith("/v1/models/"):
            model_id = path[len("/v1/models/") :]
            self.server.state.log({"path": self.path, "method": "GET"})
            self._send_json(200, _model_object(model_id or "mock-model"))
            return
        if path.startswith("/models/") and not path.startswith("/v1beta/"):
            model_id = path[len("/models/") :]
            self.server.state.log({"path": self.path, "method": "GET"})
            self._send_json(200, _model_object(model_id or "mock-model"))
            return
        if path == "/v1beta/models" or path.startswith("/v1beta/models/"):
            self._gemini_models(path)
            return
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
            turn = _turn(request, script[1] == TOOL_DONE)
            body = _anthropic_sse(message_id, model, script, turn).encode()
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
                "usage": _anthropic_usage(_turn(request, script[1] == TOOL_DONE)),
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
        response, item = _openai_completed_response(
            request_id, model, script, _turn(request, script[1] == TOOL_DONE)
        )
        if not stream:
            self._send_json(200, response)
            return
        body = _openai_sse(request_id, model, script, response, item).encode()
        self._send(200, body, "text/event-stream")

    def _gemini_count_tokens(self, request: dict[str, Any]) -> None:
        self.server.state.log({"path": self.path, "request": request})
        self._send_json(200, {"totalTokens": USAGE_IN})

    def _gemini_models(self, path: str) -> None:
        self.server.state.log({"path": self.path, "method": "GET"})
        model = {
            "name": "models/mock-model",
            "version": "mock",
            "displayName": "Rosetta mock model",
            "supportedGenerationMethods": ["generateContent", "countTokens"],
        }
        if path.endswith("/models"):
            self._send_json(200, {"models": [model]})
            return
        self._send_json(200, model)

    def _gemini_generate(self, request: dict[str, Any], streamed: bool) -> None:
        request_id = self.server.state.next_id()
        model = _gemini_model_name(self.path, request)
        script = _gemini_script(request)
        self.server.state.log(
            {
                "path": self.path,
                "id": request_id,
                "stream": streamed,
                "response_kind": script[0],
                "request": request,
            }
        )
        turn = _turn(request, _gemini_has_function_response(request))
        if streamed:
            body = _gemini_sse(request_id, model, script, turn).encode()
            self._send(200, body, "text/event-stream")
            return
        self._send_json(200, _gemini_response(request_id, model, script, turn))

    def _openai_chat(self, request: dict[str, Any]) -> None:
        request_id = self.server.state.next_id()
        model = request.get("model") if isinstance(request.get("model"), str) else "mock-model"
        stream = bool(request.get("stream", False))
        script = _openai_chat_script(request)
        usage_in = HIGH_USAGE_IN if "[high-usage]" in _compact(request) else USAGE_IN
        turn = _turn(request, script[1] == TOOL_DONE)
        self.server.state.log(
            {
                "path": self.path,
                "id": request_id,
                "stream": stream,
                "response_kind": script[0],
                "request": request,
            }
        )
        if stream:
            body = _openai_chat_sse(request_id, model, script, usage_in, turn).encode()
            self._send(200, body, "text/event-stream")
            return
        self._send_json(
            200, _openai_chat_completion(request_id, model, script, usage_in, turn)
        )


def _anthropic_script(request: dict[str, Any]) -> tuple[str, Any, Any]:
    messages = request.get("messages") if isinstance(request.get("messages"), list) else []
    for message in messages:
        if not isinstance(message, dict) or not isinstance(message.get("content"), list):
            continue
        if any(isinstance(block, dict) and block.get("type") == "tool_result" for block in message["content"]):
            return (
                "text",
                TOOL_DONE,
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
        GREETING,
        None,
    )


def _anthropic_usage(turn: int | None = None) -> dict[str, Any]:
    if turn is None:
        return {
            "input_tokens": USAGE_IN,
            "output_tokens": USAGE_OUT,
            "cache_creation_input_tokens": 0,
            "cache_read_input_tokens": 0,
        }
    n = usage_numbers(turn)
    # Anthropic: input excludes cache; cache creation is the 5m + 1h sum;
    # output includes thinking.
    return {
        "input_tokens": n["uncached"],
        "cache_read_input_tokens": n["cache_read"],
        "cache_creation_input_tokens": n["write_5m"] + n["write_1h"],
        "cache_creation": {
            "ephemeral_5m_input_tokens": n["write_5m"],
            "ephemeral_1h_input_tokens": n["write_1h"],
        },
        "output_tokens": n["visible"] + n["reasoning"],
        "output_tokens_details": {"thinking_tokens": n["reasoning"]},
        "server_tool_use": {
            "web_search_requests": n["web_searches"],
            "web_fetch_requests": n["web_fetches"],
        },
        "service_tier": "standard",
    }


def _turn(request: dict[str, Any], after_tool: bool) -> int | None:
    """The `[usage]` request index, or None for the ordinary scenarios."""
    if not wants_usage(request):
        return None
    return 1 if after_tool else 0


def _event(name: str, value: Any) -> str:
    return f"event: {name}\ndata: {_compact(value)}\n\n"


def _anthropic_sse(
    message_id: str,
    model: str,
    script: tuple[str, Any, Any],
    turn: int | None = None,
) -> str:
    usage = _anthropic_usage(turn)
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
                    "usage": {**usage, "output_tokens": 1},
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
                    # The current API repeats the complete, final usage here.
                    "usage": usage if turn is not None else {"output_tokens": USAGE_OUT},
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
            TOOL_DONE,
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
        GREETING,
        None,
    )


def _openai_usage(turn: int | None = None) -> dict[str, Any]:
    if turn is None:
        return {
            "input_tokens": USAGE_IN,
            "input_tokens_details": {"cached_tokens": 0},
            "output_tokens": USAGE_OUT,
            "output_tokens_details": {"reasoning_tokens": 0},
            "total_tokens": USAGE_IN + USAGE_OUT,
        }
    n = usage_numbers(turn)
    # OpenAI: input includes cached reads and cache writes; output includes
    # reasoning. The Responses API has no 1-hour TTL.
    prompt = n["uncached"] + n["cache_read"] + n["write_5m"]
    completion = n["visible"] + n["reasoning"]
    return {
        "input_tokens": prompt,
        "input_tokens_details": {
            "cached_tokens": n["cache_read"],
            "cache_write_tokens": n["write_5m"],
        },
        "output_tokens": completion,
        "output_tokens_details": {"reasoning_tokens": n["reasoning"]},
        "total_tokens": prompt + completion,
    }


def _openai_completed_response(
    request_id: int,
    model: str,
    script: tuple[str, str, str | None],
    turn: int | None = None,
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
        "usage": _openai_usage(turn),
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


def _model_object(model_id: str) -> dict[str, Any]:
    return {
        "id": model_id,
        "object": "model",
        "created": 0,
        "owned_by": "rosetta",
        "context_length": 128000,
        "max_model_len": 128000,
    }


def _models_list() -> dict[str, Any]:
    return {
        "object": "list",
        "data": [_model_object(name) for name in ("mock-model", "grok-4.5", "grok-build")],
    }


def _chat_message_texts(request: dict[str, Any]) -> list[str]:
    messages = request.get("messages") if isinstance(request.get("messages"), list) else []
    texts: list[str] = []
    for message in messages:
        if not isinstance(message, dict):
            continue
        content = message.get("content")
        if isinstance(content, str):
            texts.append(content)
        elif isinstance(content, list):
            for block in content:
                if isinstance(block, str):
                    texts.append(block)
                elif isinstance(block, dict) and isinstance(block.get("text"), str):
                    texts.append(block["text"])
    return texts


def _chat_tool_names(request: dict[str, Any]) -> list[str]:
    tools = request.get("tools") if isinstance(request.get("tools"), list) else []
    names: list[str] = []
    for tool in tools:
        if not isinstance(tool, dict):
            continue
        if isinstance(tool.get("name"), str):
            names.append(tool["name"])
        function = tool.get("function")
        if isinstance(function, dict) and isinstance(function.get("name"), str):
            names.append(function["name"])
    return names


def _openai_chat_script(request: dict[str, Any]) -> tuple[str, str, str | None]:
    messages = request.get("messages") if isinstance(request.get("messages"), list) else []
    if any(isinstance(message, dict) and message.get("role") == "tool" for message in messages):
        return ("text", TOOL_DONE, None)
    for message in messages:
        if not isinstance(message, dict) or not isinstance(message.get("content"), list):
            continue
        if any(
            isinstance(block, dict) and block.get("type") == "tool_result"
            for block in message["content"]
        ):
            return ("text", TOOL_DONE, None)
    if any("[error]" in text for text in _chat_message_texts(request)):
        return ("error", "Rosetta mockmodel refused this request.", None)
    if any("[use-agent]" in text for text in _chat_message_texts(request)):
        names = _chat_tool_names(request)
        name = next(
            (candidate for candidate in names if candidate.lower() == "agent"),
            "Agent",
        )
        arguments = _compact(
            {
                "prompt": "Reply with a short greeting.",
                "description": "greet briefly",
                "subagent_type": "explore",
            }
        )
        return ("function_call", name, arguments)
    if any("[use-tool]" in text for text in _chat_message_texts(request)):
        names = _chat_tool_names(request)
        preferred = (
            "run_terminal_command",
            "run_terminal_cmd",
            "terminal",
            "bash",
            "shell",
            "exec_command",
            "execute",
        )
        name = next(
            (
                candidate
                for wanted in preferred
                for candidate in names
                if candidate.lower() == wanted
            ),
            names[0] if names else "run_terminal_command",
        )
        lowered = name.lower()
        if lowered == "run_terminal_command":
            arguments = _compact(
                {
                    "command": "echo rosetta-mock",
                    "description": "echo mock output",
                }
            )
        elif lowered in {
            "bash",
            "shell",
            "execute",
            "run_terminal_cmd",
            "terminal",
        }:
            arguments = _compact({"command": "echo rosetta-mock"})
        elif lowered == "exec_command":
            arguments = _compact({"cmd": "echo rosetta-mock"})
        else:
            arguments = "{}"
        return ("function_call", name, arguments)
    return ("text", GREETING, None)


def _openai_chat_usage(input_tokens: int = USAGE_IN, turn: int | None = None) -> dict[str, Any]:
    if turn is None:
        return {
            "prompt_tokens": input_tokens,
            "completion_tokens": USAGE_OUT,
            "total_tokens": input_tokens + USAGE_OUT,
        }
    n = usage_numbers(turn)
    # Chat Completions: prompt includes cached reads and writes; completion
    # includes reasoning. `cost_in_usd_ticks` is xAI's billed extension.
    prompt = n["uncached"] + n["cache_read"] + n["write_5m"]
    completion = n["visible"] + n["reasoning"]
    return {
        "prompt_tokens": prompt,
        "completion_tokens": completion,
        "total_tokens": prompt + completion,
        "prompt_tokens_details": {
            "cached_tokens": n["cache_read"],
            "cache_write_tokens": n["write_5m"],
        },
        "completion_tokens_details": {"reasoning_tokens": n["reasoning"]},
        "cost_in_usd_ticks": n["cost_ticks"],
    }


def _openai_chat_completion(
    request_id: int,
    model: str,
    script: tuple[str, str, str | None],
    usage_in: int = USAGE_IN,
    turn: int | None = None,
) -> dict[str, Any]:
    completion_id = f"chatcmpl-mock_{request_id:04}"
    if script[0] == "text":
        message: dict[str, Any] = {"role": "assistant", "content": script[1]}
        finish_reason = "stop"
    elif script[0] == "error":
        message = {"role": "assistant", "content": script[1]}
        finish_reason = "content_filter"
    else:
        message = {
            "role": "assistant",
            "content": None,
            "tool_calls": [
                {
                    "id": f"call_mock_{request_id:04}",
                    "type": "function",
                    "function": {"name": script[1], "arguments": script[2]},
                }
            ],
        }
        finish_reason = "tool_calls"
    return {
        "id": completion_id,
        "object": "chat.completion",
        "created": 0,
        "model": model,
        "choices": [
            {
                "index": 0,
                "message": message,
                "finish_reason": finish_reason,
            }
        ],
        "usage": _openai_chat_usage(usage_in, turn),
    }


def _sse_data(value: Any) -> str:
    return f"data: {_compact(value)}\n\n"


def _openai_chat_sse(
    request_id: int,
    model: str,
    script: tuple[str, str, str | None],
    usage_in: int = USAGE_IN,
    turn: int | None = None,
) -> str:
    completion_id = f"chatcmpl-mock_{request_id:04}"

    def chunk(delta: dict[str, Any], finish_reason: str | None = None) -> str:
        return _sse_data(
            {
                "id": completion_id,
                "object": "chat.completion.chunk",
                "created": 0,
                "model": model,
                "choices": [
                    {
                        "index": 0,
                        "delta": delta,
                        "finish_reason": finish_reason,
                    }
                ],
            }
        )

    parts: list[str] = []
    if script[0] == "text":
        text = script[1]
        midpoint = len(text) // 2
        parts.append(chunk({"role": "assistant", "content": ""}))
        for piece in (text[:midpoint], text[midpoint:]):
            if piece:
                parts.append(chunk({"content": piece}))
        parts.append(chunk({}, "stop"))
    elif script[0] == "error":
        text = script[1] or "error"
        parts.append(chunk({"role": "assistant", "content": ""}))
        parts.append(chunk({"content": text}))
        parts.append(chunk({}, "content_filter"))
    else:
        # Emit the complete tool call in the first delta. Clients that do not
        # accumulate argument fragments (Grok 1.0.5) otherwise persist `{}`.
        parts.append(
            chunk(
                {
                    "role": "assistant",
                    "content": None,
                    "tool_calls": [
                        {
                            "index": 0,
                            "id": f"call_mock_{request_id:04}",
                            "type": "function",
                            "function": {
                                "name": script[1],
                                "arguments": script[2] or "{}",
                            },
                        }
                    ],
                }
            )
        )
        parts.append(chunk({}, "tool_calls"))
    usage_chunk = {
        "id": completion_id,
        "object": "chat.completion.chunk",
        "created": 0,
        "model": model,
        "choices": [],
        "usage": _openai_chat_usage(usage_in, turn),
    }
    parts.append(_sse_data(usage_chunk))
    parts.append("data: [DONE]\n\n")
    return "".join(parts)


def _gemini_model_name(path: str, request: dict[str, Any]) -> str:
    if isinstance(request.get("model"), str) and request["model"]:
        return request["model"]
    route = path.split("?", 1)[0]
    marker = "/models/"
    if marker in route:
        rest = route.split(marker, 1)[1]
        return rest.split(":", 1)[0] or "mock-model"
    return "mock-model"


def _gemini_parts(value: Any) -> list[dict[str, Any]]:
    if not isinstance(value, dict):
        return []
    parts = value.get("parts", value.get("Parts"))
    if isinstance(parts, dict):
        return [parts]
    if isinstance(parts, list):
        return [part for part in parts if isinstance(part, dict)]
    return []


def _gemini_contents(request: dict[str, Any]) -> list[dict[str, Any]]:
    contents = request.get("contents")
    if isinstance(contents, dict):
        return [contents]
    if isinstance(contents, list):
        return [item for item in contents if isinstance(item, dict)]
    return []


def _gemini_texts(request: dict[str, Any]) -> list[str]:
    texts: list[str] = []
    instruction = request.get("systemInstruction", request.get("system_instruction"))
    for part in _gemini_parts(instruction):
        if isinstance(part.get("text"), str):
            texts.append(part["text"])
    for content in _gemini_contents(request):
        for part in _gemini_parts(content):
            if isinstance(part.get("text"), str):
                texts.append(part["text"])
    return texts


def _gemini_has_function_response(request: dict[str, Any]) -> bool:
    for content in _gemini_contents(request):
        for part in _gemini_parts(content):
            if part.get("functionResponse") or part.get("function_response"):
                return True
    return False


def _gemini_tool_names(request: dict[str, Any]) -> list[str]:
    names: list[str] = []
    tools = request.get("tools") if isinstance(request.get("tools"), list) else []
    for tool in tools:
        if not isinstance(tool, dict):
            continue
        if isinstance(tool.get("name"), str):
            names.append(tool["name"])
        declarations = tool.get("functionDeclarations", tool.get("function_declarations", []))
        if isinstance(declarations, dict):
            declarations = [declarations]
        if not isinstance(declarations, list):
            continue
        for declaration in declarations:
            if isinstance(declaration, dict) and isinstance(declaration.get("name"), str):
                names.append(declaration["name"])
    return names


def _gemini_script(request: dict[str, Any]) -> tuple[str, Any, Any]:
    if _gemini_has_function_response(request):
        return ("text", TOOL_DONE, None)
    if any("[use-tool]" in text for text in _gemini_texts(request)):
        names = _gemini_tool_names(request)
        preferred = ("run_command", "run_terminal_cmd", "shell", "bash", "exec_command")
        name = next(
            (
                candidate
                for wanted in preferred
                for candidate in names
                if candidate.lower() == wanted
            ),
            names[0] if names else "run_command",
        )
        lowered = name.lower()
        if lowered == "run_command":
            arguments: dict[str, Any] = {"commandLine": "echo rosetta-mock"}
        elif lowered in {"bash", "shell", "execute", "run_terminal_cmd"}:
            arguments = {"command": "echo rosetta-mock"}
        elif lowered == "list_dir":
            arguments = {"DirectoryPath": "."}
        else:
            arguments = {}
        return ("function_call", name, arguments)
    return ("text", GREETING, None)


def _gemini_usage(turn: int | None = None) -> dict[str, int]:
    if turn is None:
        return {
            "promptTokenCount": USAGE_IN,
            "candidatesTokenCount": USAGE_OUT,
            "totalTokenCount": USAGE_IN + USAGE_OUT,
        }
    n = usage_numbers(turn)
    # Gemini: the prompt count includes cached content; thoughts are reported
    # beside the candidates, not inside them. There are no cache writes.
    prompt = n["uncached"] + n["cache_read"]
    return {
        "promptTokenCount": prompt,
        "cachedContentTokenCount": n["cache_read"],
        "candidatesTokenCount": n["visible"],
        "thoughtsTokenCount": n["reasoning"],
        "totalTokenCount": prompt + n["visible"] + n["reasoning"],
    }


def _gemini_candidate(script: tuple[str, Any, Any]) -> dict[str, Any]:
    if script[0] == "text":
        parts: list[dict[str, Any]] = [{"text": script[1]}]
    else:
        parts = [{"functionCall": {"name": script[1], "args": script[2]}}]
    return {
        "content": {"role": "model", "parts": parts},
        "finishReason": "STOP",
        "index": 0,
    }


def _gemini_response(
    request_id: int, model: str, script: tuple[str, Any, Any], turn: int | None = None
) -> dict[str, Any]:
    del request_id
    return {
        "candidates": [_gemini_candidate(script)],
        "usageMetadata": _gemini_usage(turn),
        "modelVersion": model,
    }


def _gemini_sse(
    request_id: int, model: str, script: tuple[str, Any, Any], turn: int | None = None
) -> str:
    del request_id
    if script[0] == "text":
        text = script[1]
        midpoint = len(text) // 2
        first, second = text[:midpoint], text[midpoint:]
        chunks = []
        if first:
            chunks.append(
                _sse_data(
                    {
                        "candidates": [
                            {
                                "content": {"role": "model", "parts": [{"text": first}]},
                                "index": 0,
                            }
                        ]
                    }
                )
            )
        chunks.append(
            _sse_data(
                {
                    "candidates": [
                        {
                            "content": {"role": "model", "parts": [{"text": second}]},
                            "finishReason": "STOP",
                            "index": 0,
                        }
                    ],
                    "usageMetadata": _gemini_usage(turn),
                    "modelVersion": model,
                }
            )
        )
        return "".join(chunks)
    return _sse_data(
        {
            "candidates": [_gemini_candidate(script)],
            "usageMetadata": _gemini_usage(turn),
            "modelVersion": model,
        }
    )


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
