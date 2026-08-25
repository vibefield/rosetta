from __future__ import annotations

import unittest

from tools.groundtruth.mock_model import (
    _anthropic_script,
    _anthropic_sse,
    _openai_completed_response,
    _openai_script,
    _openai_sse,
)


class MockModelTests(unittest.TestCase):
    def test_anthropic_non_streaming(self) -> None:
        script = _anthropic_script(
            {"model": "mock", "messages": [{"role": "user", "content": "hello"}]}
        )
        self.assertEqual(script[0], "text")
        stream = _anthropic_sse("msg_mock_0000", "mock", script)
        self.assertIn("event: message_start", stream)
        self.assertIn("event: message_stop", stream)

    def test_openai_streaming_tool_call(self) -> None:
        script = _openai_script(
            {
                "model": "mock",
                "stream": True,
                "input": "please [use-tool]",
                "tools": [{"name": "exec_command"}],
            }
        )
        self.assertEqual(script[0], "function_call")
        response, item = _openai_completed_response(0, "mock", script)
        text = _openai_sse(0, "mock", script, response, item)
        self.assertIn("response.function_call_arguments.delta", text)
        self.assertIn("response.completed", text)


if __name__ == "__main__":
    unittest.main()
