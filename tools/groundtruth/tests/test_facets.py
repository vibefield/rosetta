from __future__ import annotations

import unittest

from tools.groundtruth.facets import extract


class FacetTests(unittest.TestCase):
    def test_claude_tool_call_and_result_correlate(self) -> None:
        context: dict[str, str] = {}
        calls = extract(
            "claude-code",
            "transcript",
            {
                "type": "assistant",
                "message": {
                    "content": [
                        {
                            "type": "tool_use",
                            "id": "call-1",
                            "name": "Bash",
                            "input": {"command": "true"},
                        }
                    ]
                },
            },
            context,
        )
        self.assertEqual(context, {"call-1": "Bash"})
        self.assertIn("tool-input", {hit["facet_id"] for hit in calls})
        results = extract(
            "claude-code",
            "transcript",
            {
                "type": "user",
                "message": {"content": [{"type": "tool_result", "tool_use_id": "call-1"}]},
                "toolUseResult": {"stdout": "ok"},
            },
            context,
        )
        result = next(hit for hit in results if hit["facet_id"] == "tool-result")
        self.assertEqual(result["key"], "Bash")

    def test_codex_arguments_are_parsed(self) -> None:
        hits = extract(
            "codex",
            "rollout",
            {
                "type": "response_item",
                "payload": {
                    "type": "function_call",
                    "name": "exec_command",
                    "arguments": '{"cmd":"true"}',
                },
            },
            {},
        )
        arguments = next(hit for hit in hits if hit["facet_id"] == "tool-call-arguments")
        self.assertEqual(arguments["value"], {"cmd": "true"})

    def test_kimi_loop_tool_call(self) -> None:
        context: dict[str, str] = {}
        hits = extract(
            "kimi-code",
            "wire",
            {
                "type": "context.append_loop_event",
                "event": {
                    "type": "tool.call",
                    "toolCallId": "call-1",
                    "name": "Bash",
                    "args": {"command": "echo rosetta-mock"},
                },
            },
            context,
        )
        kinds = {hit["facet_id"]: hit for hit in hits}
        self.assertEqual(kinds["loop-events"]["key"], "tool.call")
        self.assertEqual(kinds["tool-call"]["key"], "Bash")
        self.assertEqual(kinds["tool-call"]["value"], {"command": "echo rosetta-mock"})
        self.assertEqual(context, {"call-1": "Bash"})
        results = extract(
            "kimi-code",
            "wire",
            {
                "type": "context.append_loop_event",
                "event": {
                    "type": "tool.result",
                    "toolCallId": "call-1",
                    "result": {"output": "rosetta-mock\n"},
                },
            },
            context,
        )
        result = next(hit for hit in results if hit["facet_id"] == "tool-result")
        self.assertEqual(result["key"], "Bash")
        self.assertEqual(result["value"], {"output": "rosetta-mock\n"})

    def test_antigravity_planner_tool_calls(self) -> None:
        hits = extract(
            "antigravity",
            "transcript",
            {
                "type": "PLANNER_RESPONSE",
                "tool_calls": [
                    {
                        "name": "run_command",
                        "args": {"commandLine": "echo rosetta-mock"},
                    }
                ],
            },
            {},
        )
        call = next(hit for hit in hits if hit["facet_id"] == "tool-call")
        self.assertEqual(call["key"], "run_command")
        self.assertEqual(call["value"], {"commandLine": "echo rosetta-mock"})
        results = extract(
            "antigravity",
            "transcript",
            {
                "type": "RUN_COMMAND",
                "content": "rosetta-mock\n",
                "exit_code": 0,
            },
            {},
        )
        result = next(hit for hit in results if hit["facet_id"] == "tool-result")
        self.assertEqual(result["key"], "RUN_COMMAND")
        self.assertEqual(result["value"], "rosetta-mock\n")

    def test_opencode_part_and_tool_facets(self) -> None:
        hits = extract(
            "opencode",
            "opencode-db",
            {
                "table": "part",
                "data": {
                    "type": "tool",
                    "tool": "bash",
                    "callID": "call_1",
                    "state": {"status": "completed"},
                },
            },
            {},
        )
        kinds = {hit["facet_id"]: hit["key"] for hit in hits}
        self.assertEqual(kinds["part-data"], "tool")
        self.assertEqual(kinds["tool-part"], "bash")

    def test_opencode_event_payload_is_keyed_by_type(self) -> None:
        hits = extract(
            "opencode",
            "opencode-db",
            {
                "table": "event",
                "event_type": "message.part.updated.1",
                "data": {
                    "sessionID": "ses_1",
                    "part": {"type": "text", "text": "hi"},
                    "time": 1,
                },
            },
            {},
        )
        kinds = {hit["facet_id"]: hit["key"] for hit in hits}
        self.assertEqual(kinds["event-data"], "message.part.updated.1")
        self.assertEqual(kinds["event-part"], "text")

    def test_pi_tool_call_and_result_correlate(self) -> None:
        context: dict[str, str] = {}
        calls = extract(
            "pi",
            "session",
            {
                "type": "message",
                "message": {
                    "role": "assistant",
                    "content": [
                        {
                            "type": "toolCall",
                            "id": "call-1",
                            "name": "bash",
                            "arguments": {"command": "true"},
                        }
                    ],
                },
            },
            context,
        )
        self.assertEqual(context, {"call-1": "bash"})
        arguments = next(hit for hit in calls if hit["facet_id"] == "tool-call-arguments")
        self.assertEqual(arguments["value"], {"command": "true"})
        results = extract(
            "pi",
            "session",
            {
                "type": "message",
                "message": {
                    "role": "toolResult",
                    "toolCallId": "call-1",
                    "toolName": "bash",
                    "content": [{"type": "text", "text": "ok"}],
                    "details": {"exitCode": 0},
                },
            },
            context,
        )
        result = next(hit for hit in results if hit["facet_id"] == "tool-result")
        self.assertEqual(result["key"], "bash")
        self.assertEqual(result["value"], {"exitCode": 0})

    def test_grok_updates_correlate_tool_io(self) -> None:
        context: dict[str, str] = {}
        calls = extract(
            "grok",
            "updates",
            {
                "method": "session/update",
                "params": {
                    "update": {
                        "sessionUpdate": "tool_call",
                        "toolCallId": "call-1",
                        "title": "run_terminal_command",
                        "rawInput": {"command": "true"},
                        "_meta": {
                            "x.ai/tool": {
                                "name": "run_terminal_command",
                                "kind": "execute",
                            }
                        },
                    }
                },
            },
            context,
        )
        self.assertEqual(context, {"call-1": "run_terminal_command"})
        self.assertIn("tool-input", {hit["facet_id"] for hit in calls})
        results = extract(
            "grok",
            "updates",
            {
                "params": {
                    "update": {
                        "sessionUpdate": "tool_call_update",
                        "toolCallId": "call-1",
                        "rawOutput": {"output": "ok"},
                    }
                }
            },
            context,
        )
        result = next(hit for hit in results if hit["facet_id"] == "tool-result")
        self.assertEqual(result["key"], "run_terminal_command")

    def test_copilot_tool_start_and_result_correlate(self) -> None:
        context: dict[str, str] = {}
        starts = extract(
            "copilot",
            "events",
            {
                "type": "tool.execution_start",
                "data": {
                    "toolCallId": "call-1",
                    "toolName": "bash",
                    "arguments": {"command": "true"},
                },
            },
            context,
        )
        self.assertEqual(context, {"call-1": "bash"})
        arguments = next(hit for hit in starts if hit["facet_id"] == "tool-start-arguments")
        self.assertEqual(arguments["value"], {"command": "true"})
        results = extract(
            "copilot",
            "events",
            {
                "type": "tool.execution_complete",
                "data": {
                    "toolCallId": "call-1",
                    "success": True,
                    "result": {"content": "ok"},
                },
            },
            context,
        )
        result = next(hit for hit in results if hit["facet_id"] == "tool-result")
        self.assertEqual(result["key"], "bash")

    def test_hermes_tool_calls_are_keyed_by_name(self) -> None:
        hits = extract(
            "hermes",
            "state-db",
            {
                "table": "messages",
                "column": "tool_calls",
                "data": [
                    {
                        "id": "call_mock_0000",
                        "type": "function",
                        "function": {
                            "name": "terminal",
                            "arguments": '{"command":"echo rosetta-mock"}',
                        },
                    }
                ],
            },
            {},
        )
        call = next(hit for hit in hits if hit["facet_id"] == "tool-calls")
        self.assertEqual(call["key"], "terminal")

    def test_hermes_farm_message_row_parses_tool_json_strings(self) -> None:
        hits = extract(
            "hermes",
            "farm:messages",
            {
                "role": "tool",
                "tool_name": "terminal",
                "tool_calls": None,
                "content": '{"output": "rosetta-mock", "exit_code": 0, "error": null}',
            },
            {},
        )
        result = next(hit for hit in hits if hit["facet_id"] == "tool-result")
        self.assertEqual(result["key"], "terminal")
        self.assertEqual(result["value"]["output"], "rosetta-mock")


if __name__ == "__main__":
    unittest.main()
