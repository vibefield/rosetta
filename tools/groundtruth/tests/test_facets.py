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


if __name__ == "__main__":
    unittest.main()
