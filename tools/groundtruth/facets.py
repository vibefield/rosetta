"""Agent-specific nested schema extraction used by the evidence tools."""

from __future__ import annotations

import json
import re
from typing import Any

from .facets_claude import message_envelope_hits as _claude_message_envelope_hits
from .facets_claude import safe_protocol_value as _claude_safe_protocol_value

Facet = dict[str, Any]


def extract(
    agent: str,
    stream_id: str,
    record: dict[str, Any],
    context: dict[str, Any],
) -> list[Facet]:
    if agent == "claude-code":
        return _claude_code(stream_id, record, context)
    if agent == "codex":
        return _codex(stream_id, record)
    if agent == "kimi-code":
        return _kimi_code(stream_id, record, context)
    if agent == "copilot":
        return _copilot(stream_id, record, context)
    if agent == "grok":
        return _grok(stream_id, record, context)
    if agent == "pi":
        return _pi(stream_id, record, context)
    if agent == "opencode":
        return _opencode(stream_id, record)
    if agent == "antigravity":
        return _antigravity(stream_id, record)
    if agent == "hermes":
        return _hermes(stream_id, record)
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


def _mcp_parameters(value: Any) -> Any:
    """A configured MCP server's input: parameter names and JSON kinds only.

    The server, not the agent, defines everything below its parameters, and
    nested payloads can be keyed by per-call values (document block ids, for
    example) that a structure census must not publish.
    """

    if not isinstance(value, dict):
        return value
    return {
        key: {} if isinstance(item, dict) else [] if isinstance(item, list) else item
        for key, item in value.items()
    }


def _claude_code(
    stream_id: str,
    record: dict[str, Any],
    context: dict[str, Any],
) -> list[Facet]:
    relative_path = context.get("__relative_path__", "")
    is_workflow_child = (
        isinstance(relative_path, str)
        and "/subagents/workflows/" in f"/{relative_path}"
    )

    if stream_id == "workflow-run":
        hits: list[Facet] = []
        default_model = record.get("defaultModel")
        if isinstance(default_model, str):
            hits.append(
                _hit(
                    "workflow-run-default-models",
                    "workflow-run.defaultModel",
                    "model",
                    default_model,
                    {"model": default_model},
                )
            )
        result = record.get("result")
        result_kind = (
            "null"
            if result is None
            else "array"
            if isinstance(result, list)
            else "object"
            if isinstance(result, dict)
            else "string"
            if isinstance(result, str)
            else "other"
        )
        hits.append(
            _hit(
                "workflow-run-results",
                "workflow-run.result (by JSON kind)",
                "kind",
                result_kind,
                {"kind": result_kind, "result": result},
            )
        )
        progress = record.get("workflowProgress")
        if isinstance(progress, list):
            for item in progress:
                if not isinstance(item, dict):
                    continue
                item_type = (
                    item.get("type")
                    if isinstance(item.get("type"), str)
                    else "<untyped>"
                )
                hits.append(
                    _hit(
                        "workflow-progress-items",
                        "workflow-run.workflowProgress[]",
                        "type",
                        item_type,
                        item,
                    )
                )
                if item_type != "workflow_agent":
                    continue
                state = (
                    item.get("state")
                    if isinstance(item.get("state"), str)
                    else "<none>"
                )
                hits.append(
                    _hit(
                        "workflow-agent-states",
                        "workflow-run.workflowProgress[type=workflow_agent]",
                        "state",
                        state,
                        item,
                    )
                )
                cache_state = "cached" if item.get("cached") is True else "not-marked-cached"
                hits.append(
                    _hit(
                        "workflow-agent-cache-states",
                        "workflow-run.workflowProgress[type=workflow_agent]",
                        "state",
                        cache_state,
                        item,
                    )
                )
                model = item.get("model")
                if isinstance(model, str):
                    hits.append(
                        _hit(
                            "workflow-agent-models",
                            "workflow-run.workflowProgress[type=workflow_agent]",
                            "model",
                            model,
                            {"model": model},
                        )
                    )
        return hits

    if stream_id == "workflow-journal":
        if record.get("type") != "result":
            return []
        result = record.get("result")
        result_kind = (
            "null"
            if result is None
            else "array"
            if isinstance(result, list)
            else "object"
            if isinstance(result, dict)
            else "string"
            if isinstance(result, str)
            else "other"
        )
        return [
            _hit(
                "workflow-journal-results",
                "workflow-journal[type=result].result (by JSON kind)",
                "kind",
                result_kind,
                {"kind": result_kind, "result": result},
            )
        ]

    if stream_id == "subagent-metadata" and is_workflow_child:
        agent_type = (
            record.get("agentType")
            if isinstance(record.get("agentType"), str)
            else "<none>"
        )
        return [
            _hit(
                "workflow-worker-metadata",
                "subagents/workflows/*/agent-*.meta.json (by agentType)",
                "agentType",
                agent_type,
                record,
            )
        ]

    if stream_id == "team-config":
        lead_id = record.get("leadAgentId")
        members = record.get("members")
        if not isinstance(members, list):
            return []
        hits: list[Facet] = []
        for member in members:
            if not isinstance(member, dict):
                continue
            hits.append(
                _hit(
                    "team-members",
                    "team-config.members[] (by membership role)",
                    "role",
                    "lead" if member.get("agentId") == lead_id else "member",
                    member,
                )
            )
            color = (
                member.get("color")
                if isinstance(member.get("color"), str)
                else "<none>"
            )
            hits.append(
                _hit(
                    "team-member-colors",
                    "team-config.members[] (by native color)",
                    "color",
                    color,
                    member,
                )
            )
            model = (
                member.get("model")
                if isinstance(member.get("model"), str)
                else "<none>"
            )
            hits.append(
                _hit(
                    "team-member-models",
                    "team-config.members[] (by configured model)",
                    "model",
                    model,
                    member,
                )
            )
            backend = member.get("backendType")
            if not isinstance(backend, str):
                pane = member.get("tmuxPaneId")
                backend = (
                    pane
                    if pane in {"leader", "in-process"}
                    else "external-pane"
                    if isinstance(pane, str) and pane
                    else "<none>"
                )
            hits.append(
                _hit(
                    "team-member-backends",
                    "team-config.members[] (by backend or pane class)",
                    "backend",
                    backend,
                    member,
                )
            )
        return hits

    if stream_id == "team-inbox":
        payload = None
        text = record.get("text")
        if isinstance(text, str):
            try:
                parsed = json.loads(text)
            except json.JSONDecodeError:
                pass
            else:
                if isinstance(parsed, dict):
                    payload = parsed
        kind = (
            record.get("type")
            if isinstance(record.get("type"), str)
            else payload.get("type")
            if isinstance(payload, dict) and isinstance(payload.get("type"), str)
            else "<plain>"
        )
        read = record.get("read")
        delivery = "read" if read is True else "unread" if read is False else "unknown"
        safe_record = _claude_safe_protocol_value(record)
        if payload is not None:
            safe_record["textPayload"] = _claude_safe_protocol_value(payload)
        return [
            _hit(
                "team-inbox-messages",
                "team-inbox[] (by message type)",
                "type",
                kind,
                safe_record,
            ),
            _hit(
                "team-inbox-delivery",
                "team-inbox[] (by read state)",
                "state",
                delivery,
                safe_record,
            ),
        ]

    if stream_id not in {"transcript", "subagent-transcript"}:
        return []
    hits: list[Facet] = []
    record_type = record.get("type", "")

    if stream_id == "subagent-transcript" and is_workflow_child:
        child_type = record_type if isinstance(record_type, str) and record_type else "<untyped>"
        hits.append(
            _hit(
                "workflow-child-records",
                "subagents/workflows/*/agent-*.jsonl (by record type)",
                "type",
                child_type,
                record,
            )
        )

    if record_type == "assistant":
        message = record.get("message")
        message = message if isinstance(message, dict) else {}
        model = (
            message.get("model")
            if isinstance(message.get("model"), str)
            else "missing-model"
        )
        effort = (
            record.get("effort")
            if isinstance(record.get("effort"), str)
            else "missing-effort"
        )
        profile = f"{model}.{effort}"
        if re.fullmatch(r"[A-Za-z0-9_.-]{1,96}", profile):
            hits.append(
                _hit(
                    "assistant-response-profiles",
                    "assistant response (by recorded model and effort)",
                    "profile",
                    profile,
                    {"model": model, "effort": effort},
                )
            )
            if stream_id == "subagent-transcript" and is_workflow_child:
                hits.append(
                    _hit(
                        "workflow-child-response-profiles",
                        "Workflow child assistant response (by recorded model and effort)",
                        "profile",
                        profile,
                        {"model": model, "effort": effort},
                    )
                )
        blocks = message.get("content")
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
                    tool_input = block.get("input")
                    hits.append(
                        _hit(
                            "tool-input",
                            "assistant.message.content[type=tool_use].input",
                            "name",
                            key,
                            _mcp_parameters(tool_input) if key == "mcp__*" else tool_input,
                        )
                    )
                    if name == "Agent" and isinstance(tool_input, dict):
                        operation = "resume" if "resume" in tool_input else "launch"
                        if isinstance(block.get("id"), str):
                            context[f"claude-agent-operation:{block['id']}"] = operation
                        hits.append(
                            _hit(
                                "agent-requests",
                                "Agent input (by operation)",
                                "operation",
                                operation,
                                tool_input,
                            )
                        )
                    elif name == "SendMessage" and isinstance(tool_input, dict):
                        message_type = (
                            tool_input.get("type")
                            if isinstance(tool_input.get("type"), str)
                            else "<none>"
                        )
                        hits.append(
                            _hit(
                                "send-message-requests",
                                "SendMessage input (by type)",
                                "type",
                                message_type,
                                tool_input,
                            )
                        )
                    elif name == "Workflow" and isinstance(tool_input, dict):
                        operation = "resume" if "resumeFromRunId" in tool_input else "launch"
                        hits.append(
                            _hit(
                                "workflow-requests",
                                "Workflow input (by operation)",
                                "operation",
                                operation,
                                tool_input,
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
            tool_use_id = None
            tool_result_block = None
            if isinstance(blocks, list):
                for block in blocks:
                    if isinstance(block, dict) and block.get("type") == "tool_result":
                        tool_result_block = block
                        candidate = block.get("tool_use_id")
                        if isinstance(candidate, str):
                            tool_use_id = candidate
                            tool = context.get(candidate, "<unmatched>")
                            break
            result = record["toolUseResult"]
            hits.append(
                _hit(
                    "tool-result",
                    "user.toolUseResult (keyed by producing tool)",
                    "tool",
                    tool,
                    result,
                )
            )
            if tool == "Workflow" and tool_result_block is not None:
                outcome = (
                    "error"
                    if tool_result_block.get("is_error") is True
                    else "launch-receipt"
                )
                hits.append(
                    _hit(
                        "workflow-tool-result-blocks",
                        "user.message.content[type=tool_result] for Workflow",
                        "outcome",
                        outcome,
                        {
                            "content": tool_result_block.get("content"),
                            "is_error": tool_result_block.get("is_error"),
                        },
                    )
                )
            if tool == "Agent" and isinstance(result, dict):
                status = (
                    result.get("status")
                    if isinstance(result.get("status"), str)
                    else "<none>"
                )
                operation = (
                    context.get(f"claude-agent-operation:{tool_use_id}", "unknown-request")
                    if tool_use_id is not None
                    else "unknown-request"
                )
                hits.append(
                    _hit(
                        "agent-results",
                        "Agent toolUseResult (by request operation and result status)",
                        "status",
                        f"{operation}.{status}",
                        result,
                    )
                )
            elif tool == "SendMessage" and isinstance(result, dict):
                outcome = (
                    "resumed"
                    if "resumedAgentId" in result
                    else "routed"
                    if "routing" in result
                    else "acknowledged"
                    if "success" in result
                    else "other"
                )
                hits.append(
                    _hit(
                        "send-message-results",
                        "SendMessage toolUseResult (by result shape)",
                        "outcome",
                        outcome,
                        result,
                    )
                )
            elif tool == "Workflow" and isinstance(result, dict):
                status = (
                    result.get("status")
                    if isinstance(result.get("status"), str)
                    else "<none>"
                )
                hits.append(
                    _hit(
                        "workflow-results",
                        "Workflow toolUseResult (by status)",
                        "status",
                        status,
                        result,
                    )
                )
        origin = record.get("origin")
        if isinstance(origin, dict):
            kind = origin.get("kind") if isinstance(origin.get("kind"), str) else "<none>"
            hits.append(
                _hit(
                    "user-origins",
                    "user.origin (by kind)",
                    "kind",
                    kind,
                    _claude_safe_protocol_value(origin),
                )
            )
        hits.extend(_claude_message_envelope_hits(record, context))
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


def _antigravity(stream_id: str, record: dict[str, Any]) -> list[Facet]:
    if stream_id not in {
        "transcript",
        "transcript-full",
        "transcript-chunks",
        "transcript-full-chunks",
    }:
        return []
    record_type = record.get("type")
    hits: list[Facet] = []
    if record_type == "PLANNER_RESPONSE":
        calls = record.get("tool_calls")
        if isinstance(calls, list):
            for call in calls:
                if not isinstance(call, dict):
                    continue
                name = call.get("name") if isinstance(call.get("name"), str) else "<unnamed>"
                hits.append(
                    _hit(
                        "tool-call",
                        "PLANNER_RESPONSE.tool_calls[]",
                        "name",
                        name,
                        call.get("args"),
                    )
                )
        return hits
    if isinstance(record_type, str) and record_type in {
        "RUN_COMMAND",
        "LIST_DIRECTORY",
        "VIEW_FILE",
        "ERROR_MESSAGE",
    }:
        hits.append(
            _hit(
                "tool-result",
                f"{record_type} (by type)",
                "type",
                record_type,
                record.get("content", record.get("error")),
            )
        )
    return hits


def _pi(stream_id: str, record: dict[str, Any], context: dict[str, str]) -> list[Facet]:
    if stream_id != "session":
        return []
    if record.get("type") != "message":
        return []
    message = record.get("message")
    message = message if isinstance(message, dict) else {}
    hits: list[Facet] = []
    role = message.get("role") if isinstance(message.get("role"), str) else "<none>"
    hits.append(_hit("message-roles", "message.message (by role)", "role", role, message))
    content = message.get("content")
    if isinstance(content, list):
        for block in content:
            if not isinstance(block, dict):
                continue
            kind = block.get("type") if isinstance(block.get("type"), str) else "<untyped>"
            hits.append(
                _hit(
                    "content-blocks",
                    "message.message.content[]",
                    "type",
                    kind,
                    block,
                )
            )
            if kind == "toolCall" and isinstance(block.get("name"), str):
                if isinstance(block.get("id"), str):
                    context[block["id"]] = block["name"]
                hits.append(
                    _hit(
                        "tool-call-arguments",
                        "message.message.content[type=toolCall].arguments",
                        "name",
                        block["name"],
                        block.get("arguments"),
                    )
                )
    if role == "toolResult":
        tool = message.get("toolName") if isinstance(message.get("toolName"), str) else "<unnamed>"
        if isinstance(message.get("toolCallId"), str):
            tool = context.get(message["toolCallId"], tool)
        hits.append(
            _hit(
                "tool-result",
                "message.message[role=toolResult] (by toolName)",
                "tool",
                tool,
                message.get("details", message.get("content")),
            )
        )
    return hits


def _grok_tool_name(update: dict[str, Any]) -> str:
    """Grok 1.0.5 persists title + `_meta['x.ai/tool'].name`, not ACP toolName."""

    meta = update.get("_meta") if isinstance(update.get("_meta"), dict) else {}
    tool = meta.get("x.ai/tool") if isinstance(meta.get("x.ai/tool"), dict) else {}
    for candidate in (tool.get("name"), update.get("toolName"), update.get("title")):
        if isinstance(candidate, str) and candidate:
            return candidate
    return "<unnamed>"


def _grok(
    stream_id: str,
    record: dict[str, Any],
    context: dict[str, str],
) -> list[Facet]:
    hits: list[Facet] = []
    if stream_id == "updates":
        params = record.get("params") if isinstance(record.get("params"), dict) else {}
        update = params.get("update") if isinstance(params.get("update"), dict) else {}
        kind = (
            update.get("sessionUpdate")
            if isinstance(update.get("sessionUpdate"), str)
            else "<none>"
        )
        hits.append(
            _hit(
                "session-updates",
                "params.update (by sessionUpdate)",
                "sessionUpdate",
                kind,
                update or record,
            )
        )
        if kind == "tool_call":
            name = _grok_tool_name(update)
            tool_id = update.get("toolCallId")
            if isinstance(tool_id, str):
                context[tool_id] = name
            hits.append(
                _hit(
                    "tool-input",
                    "updates tool_call.rawInput (by tool name)",
                    "name",
                    name,
                    update.get("rawInput"),
                )
            )
        elif kind == "tool_call_update":
            tool_id = update.get("toolCallId")
            name = (
                context.get(tool_id, "<unmatched>")
                if isinstance(tool_id, str)
                else "<unmatched>"
            )
            if name == "<unmatched>":
                name = _grok_tool_name(update)
            hits.append(
                _hit(
                    "tool-result",
                    "updates tool_call_update.rawOutput (by producing tool)",
                    "name",
                    name,
                    update.get("rawOutput"),
                )
            )
        return hits

    if stream_id != "chat-history":
        return hits
    kind = record.get("type") if isinstance(record.get("type"), str) else "<untyped>"
    if kind == "assistant":
        calls = record.get("tool_calls")
        if isinstance(calls, list):
            for call in calls:
                if not isinstance(call, dict):
                    continue
                name = call.get("name") if isinstance(call.get("name"), str) else "<unnamed>"
                call_id = call.get("id")
                if isinstance(call_id, str):
                    context[call_id] = name
                arguments = call.get("arguments")
                if isinstance(arguments, str):
                    try:
                        arguments = json.loads(arguments)
                    except json.JSONDecodeError:
                        pass
                hits.append(
                    _hit(
                        "tool-input",
                        "assistant.tool_calls[] (by name)",
                        "name",
                        name,
                        arguments,
                    )
                )
    elif kind == "tool_result":
        call_id = record.get("tool_call_id")
        name = (
            context.get(call_id, "<unmatched>")
            if isinstance(call_id, str)
            else "<unmatched>"
        )
        hits.append(
            _hit(
                "tool-result",
                "tool_result (keyed by producing tool)",
                "name",
                name,
                record.get("content"),
            )
        )
    return hits


def _copilot(
    stream_id: str,
    record: dict[str, Any],
    context: dict[str, str],
) -> list[Facet]:
    if stream_id != "events":
        return []
    hits: list[Facet] = []
    event_type = record.get("type") if isinstance(record.get("type"), str) else "<untyped>"
    data = record.get("data") if isinstance(record.get("data"), dict) else {}

    if event_type == "assistant.message":
        requests = data.get("toolRequests")
        if isinstance(requests, list):
            for request in requests:
                if not isinstance(request, dict):
                    continue
                name = request.get("name") if isinstance(request.get("name"), str) else "<unnamed>"
                call_id = request.get("toolCallId")
                if isinstance(call_id, str):
                    context[call_id] = name
                hits.append(
                    _hit(
                        "tool-request",
                        "assistant.message.data.toolRequests[] (by name)",
                        "name",
                        name,
                        request.get("arguments"),
                    )
                )
    elif event_type == "tool.execution_start":
        name = (
            data.get("toolName") if isinstance(data.get("toolName"), str) else "<unnamed>"
        )
        call_id = data.get("toolCallId")
        if isinstance(call_id, str):
            context[call_id] = name
        hits.append(
            _hit(
                "tool-start-arguments",
                "tool.execution_start.data.arguments (by toolName)",
                "name",
                name,
                data.get("arguments"),
            )
        )
    elif event_type == "tool.execution_complete":
        call_id = data.get("toolCallId")
        name = (
            context.get(call_id, "<unmatched>")
            if isinstance(call_id, str)
            else "<unmatched>"
        )
        hits.append(
            _hit(
                "tool-result",
                "tool.execution_complete.data.result (keyed by producing tool)",
                "tool",
                name,
                data.get("result") if "result" in data else data.get("error"),
            )
        )
    elif event_type == "permission.requested":
        permission = (
            data.get("permissionRequest")
            if isinstance(data.get("permissionRequest"), dict)
            else {}
        )
        kind = (
            permission.get("kind")
            if isinstance(permission.get("kind"), str)
            else "<none>"
        )
        hits.append(
            _hit(
                "permission-kinds",
                "permission.requested.data.permissionRequest (by kind)",
                "kind",
                kind,
                permission,
            )
        )
    return hits


def _opencode(stream_id: str, record: dict[str, Any]) -> list[Facet]:
    if stream_id != "opencode-db":
        return []
    data = record.get("data")
    if not isinstance(data, dict):
        return []
    table = record.get("table")
    hits: list[Facet] = []
    if table == "message":
        role = data.get("role") if isinstance(data.get("role"), str) else "<untyped>"
        hits.append(_hit("message-data", "message.data (by role)", "role", role, data))
    elif table == "part":
        kind = data.get("type") if isinstance(data.get("type"), str) else "<untyped>"
        hits.append(_hit("part-data", "part.data (by type)", "type", kind, data))
        if kind == "tool":
            tool = data.get("tool") if isinstance(data.get("tool"), str) else "<unnamed>"
            hits.append(
                _hit(
                    "tool-part",
                    "part.data[type=tool] (by tool)",
                    "tool",
                    tool,
                    data,
                )
            )
    elif table == "event":
        event_type = (
            record.get("event_type")
            if isinstance(record.get("event_type"), str)
            else "<untyped>"
        )
        hits.append(
            _hit("event-data", "event.data (by event.type)", "type", event_type, data)
        )
        info = data.get("info") if isinstance(data.get("info"), dict) else None
        if info is not None and isinstance(info.get("role"), str):
            hits.append(
                _hit(
                    "event-message-info",
                    "event.data.info (by role)",
                    "role",
                    info["role"],
                    info,
                )
            )
        part = data.get("part") if isinstance(data.get("part"), dict) else None
        if part is not None:
            kind = part.get("type") if isinstance(part.get("type"), str) else "<untyped>"
            hits.append(
                _hit(
                    "event-part",
                    "event.data.part (by type)",
                    "type",
                    kind,
                    part,
                )
            )
    elif table == "session_message":
        kind = data.get("type") if isinstance(data.get("type"), str) else "<untyped>"
        hits.append(
            _hit(
                "session-message-data",
                "session_message.data (by type)",
                "type",
                kind,
                data,
            )
        )
    return hits


def _kimi_code(
    stream_id: str, record: dict[str, Any], context: dict[str, str]
) -> list[Facet]:
    if stream_id != "wire":
        return []
    hits: list[Facet] = []

    def content_parts(value: Any) -> None:
        parts = value if isinstance(value, list) else [value]
        for part in parts:
            if not isinstance(part, dict):
                continue
            kind = part.get("type") if isinstance(part.get("type"), str) else "<untyped>"
            hits.append(
                _hit(
                    "content-parts",
                    "ContentPart in turn.prompt.input[], context.append_message.message.content[], "
                    "content.part.part, or tool.result.result.output[]",
                    "type",
                    kind,
                    part,
                )
            )

    record_type = record.get("type")
    if record_type == "turn.prompt":
        content_parts(record.get("input"))
        return hits
    if record_type == "context.append_message":
        message = record.get("message")
        if isinstance(message, dict):
            content_parts(message.get("content"))
        return hits
    if record_type != "context.append_loop_event":
        return hits
    event = record.get("event")
    if not isinstance(event, dict):
        return hits
    kind = event.get("type") if isinstance(event.get("type"), str) else "<untyped>"
    hits.append(
        _hit(
            "loop-events",
            "context.append_loop_event.event (by event.type)",
            "type",
            kind,
            event,
        )
    )
    call_id = event.get("toolCallId")
    if kind == "content.part":
        content_parts(event.get("part"))
    elif kind == "tool.call" and isinstance(event.get("name"), str):
        if isinstance(call_id, str):
            context[call_id] = event["name"]
        hits.append(
            _hit(
                "tool-call",
                "context.append_loop_event.event[type=tool.call].args (by tool name)",
                "name",
                event["name"],
                event.get("args"),
            )
        )
    elif kind == "tool.result":
        tool = context.get(call_id, "<unmatched>") if isinstance(call_id, str) else "<unmatched>"
        result = event.get("result")
        if isinstance(result, dict) and isinstance(result.get("output"), list):
            content_parts(result["output"])
        hits.append(
            _hit(
                "tool-result",
                "context.append_loop_event.event[type=tool.result].result (keyed by producing tool)",
                "name",
                tool,
                event.get("result"),
            )
        )
    return hits


def _hermes_tool_calls(data: Any) -> list[Facet]:
    items = data if isinstance(data, list) else [data]
    hits: list[Facet] = []
    for item in items:
        if not isinstance(item, dict):
            continue
        name = "<unnamed>"
        function = item.get("function")
        if isinstance(function, dict) and isinstance(function.get("name"), str):
            name = function["name"]
        elif isinstance(item.get("name"), str):
            name = item["name"]
        hits.append(
            _hit(
                "tool-calls",
                "messages.tool_calls[] (by tool name)",
                "name",
                name,
                item,
            )
        )
    return hits


def _hermes_tool_result(data: Any, tool_name: Any) -> list[Facet]:
    if not isinstance(data, dict):
        return []
    name = tool_name if isinstance(tool_name, str) and tool_name else "<unnamed>"
    return [
        _hit(
            "tool-result",
            "messages.content (tool role, keyed by tool_name)",
            "tool",
            name,
            data,
        )
    ]


def _hermes(stream_id: str, record: dict[str, Any]) -> list[Facet]:
    hits: list[Facet] = []
    if stream_id == "state-db":
        column = record.get("column")
        data = record.get("data")
        if record.get("table") == "messages" and column == "tool_calls":
            hits.extend(_hermes_tool_calls(data))
        elif record.get("table") == "messages" and column == "content":
            hits.extend(_hermes_tool_result(data, record.get("tool_name")))
        return hits
    if stream_id != "farm:messages":
        return hits
    raw_calls = record.get("tool_calls")
    if isinstance(raw_calls, str) and raw_calls.lstrip()[:1] in "{[":
        try:
            parsed = json.loads(raw_calls)
        except json.JSONDecodeError:
            parsed = None
        if parsed is not None:
            hits.extend(_hermes_tool_calls(parsed))
    if record.get("role") == "tool" and isinstance(record.get("content"), str):
        try:
            payload = json.loads(record["content"])
        except json.JSONDecodeError:
            payload = None
        if isinstance(payload, dict):
            hits.extend(_hermes_tool_result(payload, record.get("tool_name")))
    return hits
