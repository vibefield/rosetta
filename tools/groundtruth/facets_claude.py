"""Privacy-safe parsing for Claude's embedded collaboration envelopes."""

from __future__ import annotations

import json
import re
from typing import Any

Facet = dict[str, Any]

_MESSAGE_TAGS = ("agent-message", "teammate-message", "task-notification")
_MESSAGE_PATTERNS = {
    tag: re.compile(rf"<{tag}([^>]*)>(.*?)</{tag}>", re.DOTALL)
    for tag in _MESSAGE_TAGS
}
_TOP_LEVEL_MESSAGE = re.compile(
    r"<(teammate-message|task-notification)([^>]*)>",
    re.DOTALL,
)
_ATTRIBUTE = re.compile(
    r"([A-Za-z_:][A-Za-z0-9_:.-]*)\s*=\s*(['\"])(.*?)\2",
    re.DOTALL,
)
_TASK_NOTIFICATION_FIELDS = (
    "task-id",
    "tool-use-id",
    "output-file",
    "status",
    "summary",
    "result",
    "paths",
    "usage",
    "total_tokens",
    "tool_uses",
    "duration_ms",
    "event",
    "note",
    "subagent_tokens",
    "agent_count",
    "diagnostics",
    "agents_done",
    "agents_error",
    "agents_skipped",
    "agents_empty_result",
    "worktree",
    "task-type",
    "failures",
)
_TASK_NUMERIC_FIELDS = {
    "total_tokens",
    "tool_uses",
    "duration_ms",
    "subagent_tokens",
    "agent_count",
    "agents_done",
    "agents_error",
    "agents_skipped",
    "agents_empty_result",
}


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


def safe_protocol_value(value: Any, field: str | None = None) -> Any:
    """Retain protocol enums and JSON kinds while blanking message payloads."""

    if isinstance(value, str):
        normalized = re.sub(r"[^a-z0-9]", "", (field or "").lower())
        if normalized in {"type", "status", "idlereason", "color", "kind"}:
            return value
        return ""
    if isinstance(value, list):
        return [safe_protocol_value(item, field) for item in value[:1]]
    if isinstance(value, dict):
        return {key: safe_protocol_value(item, key) for key, item in value.items()}
    return value


def _task_notification(body: str) -> dict[str, Any]:
    notification: dict[str, Any] = {}
    for field in _TASK_NOTIFICATION_FIELDS:
        match = re.search(
            rf"<{re.escape(field)}>(.*?)</{re.escape(field)}>",
            body,
            re.DOTALL,
        )
        if match is None:
            continue
        raw = match.group(1).strip()
        if field == "status":
            notification[field] = raw
        elif field in _TASK_NUMERIC_FIELDS:
            try:
                notification[field] = int(raw)
            except ValueError:
                notification[field] = ""
        elif field in {"usage", "diagnostics", "paths"}:
            try:
                parsed = json.loads(raw)
            except json.JSONDecodeError:
                notification[field] = ""
            else:
                notification[field] = safe_protocol_value(parsed)
        else:
            notification[field] = ""
    return notification


def message_envelope_hits(
    record: dict[str, Any], tool_context: dict[str, Any] | None = None
) -> list[Facet]:
    """Extract only native top-level delivery envelopes from a user record."""

    message = record.get("message")
    content = message.get("content") if isinstance(message, dict) else None
    if not isinstance(content, str):
        return []

    origin = record.get("origin")
    origin_kind = origin.get("kind") if isinstance(origin, dict) else None
    envelopes: list[tuple[str, str, str]] = []
    if origin_kind == "peer":
        envelopes.extend(
            ("agent-message", match.group(1), match.group(2))
            for match in _MESSAGE_PATTERNS["agent-message"].finditer(content)
        )

    # Teammate/task envelopes can contain copied envelope text in their bodies.
    # Walking only adjacent root siblings keeps those nested copies as provenance
    # instead of counting them as new timeline deliveries.
    position = len(content) - len(content.lstrip())
    while position < len(content):
        opening = _TOP_LEVEL_MESSAGE.match(content, position)
        if opening is None:
            break
        tag = opening.group(1)
        closing = f"</{tag}>"
        end = content.find(closing, opening.end())
        if end < 0:
            break
        envelopes.append((tag, opening.group(2), content[opening.end() : end]))
        position = end + len(closing)
        while position < len(content) and content[position].isspace():
            position += 1

    hits: list[Facet] = []
    for tag, raw_attributes, raw_body in envelopes:
        if tag != "agent-message" and origin_kind == "human":
            continue
        attributes = {
            name: raw if name.lower().endswith("color") else ""
            for name, _, raw in _ATTRIBUTE.findall(raw_attributes)
        }
        body = raw_body.strip()
        try:
            payload = json.loads(body)
        except json.JSONDecodeError:
            payload = None
        envelope = {
            "attributes": attributes,
            "bodyKind": "json" if payload is not None else "text",
        }
        hits.append(
            _hit(
                "message-envelopes",
                "user.message.content (top-level embedded native envelopes)",
                "type",
                tag,
                envelope,
            )
        )

        if tag == "teammate-message":
            if isinstance(payload, dict):
                kind = (
                    payload.get("type")
                    if isinstance(payload.get("type"), str)
                    else "<none>"
                )
                value = safe_protocol_value(payload)
            else:
                kind = "<plain>"
                value = {"bodyKind": "text"}
            hits.append(
                _hit(
                    "teammate-message-payloads",
                    "top-level teammate-message JSON body (plain prose is <plain>)",
                    "type",
                    kind,
                    value,
                )
            )
        elif tag == "task-notification":
            notification = _task_notification(body)
            status = (
                notification.get("status")
                if isinstance(notification.get("status"), str)
                and notification["status"]
                else "<none>"
            )
            hits.append(
                _hit(
                    "task-notifications",
                    "top-level task-notification body (by status)",
                    "status",
                    status,
                    notification,
                )
            )
            tool_use_match = re.search(
                r"<tool-use-id>(.*?)</tool-use-id>", body, re.DOTALL
            )
            tool_use_id = tool_use_match.group(1).strip() if tool_use_match else None
            if (
                tool_context is not None
                and tool_use_id is not None
                and tool_context.get(tool_use_id) == "Workflow"
            ):
                hits.append(
                    _hit(
                        "workflow-task-notifications",
                        "top-level Workflow task-notification body (by status)",
                        "status",
                        status,
                        notification,
                    )
                )
                result_match = re.search(r"<result>(.*?)</result>", body, re.DOTALL)
                result_state = (
                    "absent"
                    if result_match is None
                    else "truncated"
                    if re.search(
                        r"\(truncated\s+\d+\s+chars?,\s*full result in\s+[^)]+\)",
                        result_match.group(1),
                        re.IGNORECASE,
                    )
                    else "complete"
                )
                hits.append(
                    _hit(
                        "workflow-task-notification-results",
                        "top-level Workflow task-notification result body",
                        "state",
                        result_state,
                        {"state": result_state},
                    )
                )
    return hits
