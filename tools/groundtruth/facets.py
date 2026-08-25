"""Agent-specific nested schema extraction used by the evidence tools."""

from __future__ import annotations

import json
from typing import Any

Facet = dict[str, Any]


def extract(
    agent: str,
    stream_id: str,
    record: dict[str, Any],
    context: dict[str, str],
) -> list[Facet]:
    if agent == "claude-code":
        return _claude_code(stream_id, record, context)
    if agent == "codex":
        return _codex(stream_id, record)
    return []


def _hit(
    facet_id: str,
    path: str,
    discriminator: str,
    key: str,
    value: Any,
) -> Facet:
    return {
        "facet_id": facet_id,
        "path": path,
        "discriminator": discriminator,
        "key": key,
        "value": value,
    }


def _claude_code(
    stream_id: str,
    record: dict[str, Any],
    context: dict[str, str],
) -> list[Facet]:
    if stream_id not in {"transcript", "subagent-transcript"}:
        return []
    hits: list[Facet] = []
    record_type = record.get("type", "")

    if record_type == "assistant":
        blocks = record.get("message", {}).get("content")
        if isinstance(blocks, list):
            for block in blocks:
                if not isinstance(block, dict):
                    continue
                kind = block.get("type") if isinstance(block.get("type"), str) else "<untyped>"
                hits.append(
                    _hit(
                        "assistant-content-blocks",
                        "assistant.message.content[]",
                        "type",
                        kind,
                        block,
                    )
                )
                if kind == "tool_use":
                    name = block.get("name") if isinstance(block.get("name"), str) else "<unnamed>"
                    key = "mcp__*" if name.startswith("mcp__") else name
                    if isinstance(block.get("id"), str) and isinstance(block.get("name"), str):
                        context[block["id"]] = key
                    hits.append(
                        _hit(
                            "tool-input",
                            "assistant.message.content[type=tool_use].input",
                            "name",
                            key,
                            block.get("input"),
                        )
                    )
    elif record_type == "user":
        blocks = record.get("message", {}).get("content")
        if isinstance(blocks, list):
            for block in blocks:
                if not isinstance(block, dict):
                    continue
                kind = block.get("type") if isinstance(block.get("type"), str) else "<untyped>"
                hits.append(
                    _hit(
                        "user-content-blocks",
                        "user.message.content[]",
                        "type",
                        kind,
                        block,
                    )
                )
        if "toolUseResult" in record:
            tool = "<unmatched>"
            if isinstance(blocks, list):
                for block in blocks:
                    if isinstance(block, dict) and block.get("type") == "tool_result":
                        tool_use_id = block.get("tool_use_id")
                        if isinstance(tool_use_id, str):
                            tool = context.get(tool_use_id, "<unmatched>")
                            break
            hits.append(
                _hit(
                    "tool-result",
                    "user.toolUseResult (keyed by producing tool)",
                    "tool",
                    tool,
                    record["toolUseResult"],
                )
            )
    elif record_type == "system":
        subtype = record.get("subtype") if isinstance(record.get("subtype"), str) else "<none>"
        hits.append(_hit("system-subtypes", "system (by subtype)", "subtype", subtype, record))
    elif record_type == "progress":
        data = record.get("data")
        data = data if isinstance(data, dict) else {}
        kind = data.get("type") if isinstance(data.get("type"), str) else "<none>"
        hits.append(_hit("progress-data", "progress.data (by data.type)", "data.type", kind, data))
    return hits


def _codex(stream_id: str, record: dict[str, Any]) -> list[Facet]:
    if stream_id != "rollout":
        return []
    payload = record.get("payload")
    if not isinstance(payload, dict):
        payload = {}
    hits: list[Facet] = []

    if record.get("type") == "event_msg":
        kind = payload.get("type") if isinstance(payload.get("type"), str) else "<untyped>"
        hits.append(_hit("event-msg", "event_msg.payload (by payload.type)", "type", kind, payload))
        item = payload.get("item")
        if kind == "item_completed" and isinstance(item, dict):
            item_kind = item.get("type") or item.get("item_type")
            item_kind = item_kind if isinstance(item_kind, str) else "<untyped>"
            hits.append(
                _hit(
                    "thread-items",
                    "event_msg[item_completed].item (by item type)",
                    "type",
                    item_kind,
                    item,
                )
            )
    elif record.get("type") == "response_item":
        kind = payload.get("type") if isinstance(payload.get("type"), str) else "<untyped>"
        hits.append(
            _hit("response-items", "response_item.payload (by payload.type)", "type", kind, payload)
        )
        if kind == "function_call" and isinstance(payload.get("name"), str):
            arguments = payload.get("arguments")
            if isinstance(arguments, str):
                try:
                    parsed = json.loads(arguments)
                except json.JSONDecodeError:
                    pass
                else:
                    hits.append(
                        _hit(
                            "tool-call-arguments",
                            "response_item[function_call].arguments (parsed, by tool name)",
                            "name",
                            payload["name"],
                            parsed,
                        )
                    )
    return hits
