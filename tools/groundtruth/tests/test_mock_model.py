from __future__ import annotations

import unittest

from tools.groundtruth.mock_model import (
    USAGE_MARKER,
    _anthropic_script,
    _anthropic_usage,
    _gemini_usage,
    _openai_chat_usage,
    _openai_usage,
    usage_numbers,
    _anthropic_sse,
    _gemini_response,
    _gemini_script,
    _gemini_sse,
    _openai_chat_completion,
    _openai_chat_script,
    _openai_chat_sse,
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

    def test_openai_chat_non_streaming_text(self) -> None:
        script = _openai_chat_script(
            {"model": "mock", "messages": [{"role": "user", "content": "hello"}]}
        )
        self.assertEqual(script[0], "text")
        completion = _openai_chat_completion(0, "mock", script)
        self.assertEqual(completion["object"], "chat.completion")
        self.assertEqual(completion["choices"][0]["finish_reason"], "stop")
        self.assertIn("rosetta mockmodel", completion["choices"][0]["message"]["content"])
        self.assertEqual(completion["usage"]["prompt_tokens"], 128)
        self.assertEqual(completion["usage"]["completion_tokens"], 32)

    def test_openai_chat_can_force_high_usage_for_compaction(self) -> None:
        script = _openai_chat_script(
            {
                "model": "mock",
                "messages": [{"role": "user", "content": "[high-usage]"}],
            }
        )
        completion = _openai_chat_completion(0, "mock", script, 900)
        self.assertEqual(completion["usage"]["prompt_tokens"], 900)
        self.assertEqual(completion["usage"]["total_tokens"], 932)

    def test_openai_chat_streaming_tool_call(self) -> None:
        script = _openai_chat_script(
            {
                "model": "mock",
                "stream": True,
                "messages": [{"role": "user", "content": "please [use-tool]"}],
                "tools": [{"type": "function", "function": {"name": "Bash"}}],
            }
        )
        self.assertEqual(script[0], "function_call")
        self.assertEqual(script[1], "Bash")
        text = _openai_chat_sse(0, "mock", script)
        self.assertIn('"object":"chat.completion.chunk"', text)
        self.assertIn("tool_calls", text)
        self.assertIn('"finish_reason":"tool_calls"', text)
        self.assertIn("data: [DONE]", text)

    def test_openai_chat_prefers_terminal_tool(self) -> None:
        script = _openai_chat_script(
            {
                "messages": [{"role": "user", "content": "[use-tool]"}],
                "tools": [
                    {"type": "function", "function": {"name": "web_search"}},
                    {"type": "function", "function": {"name": "terminal"}},
                ],
            }
        )
        self.assertEqual(script[0], "function_call")
        self.assertEqual(script[1], "terminal")
        self.assertIn("echo rosetta-mock", script[2] or "")

    def test_openai_chat_tool_result_closes_loop(self) -> None:
        script = _openai_chat_script(
            {
                "messages": [
                    {"role": "user", "content": "please [use-tool]"},
                    {"role": "assistant", "tool_calls": [{"id": "call_mock_0000"}]},
                    {"role": "tool", "tool_call_id": "call_mock_0000", "content": "ok"},
                ]
            }
        )
        self.assertEqual(script[0], "text")
        self.assertIn("Scenario complete", script[1])

    def test_openai_chat_use_agent(self) -> None:
        script = _openai_chat_script(
            {
                "messages": [
                    {"role": "user", "content": "Delegate a greeting. [use-agent]"}
                ],
                "tools": [
                    {"type": "function", "function": {"name": "Bash"}},
                    {"type": "function", "function": {"name": "Agent"}},
                ],
            }
        )
        self.assertEqual(script[0], "function_call")
        self.assertEqual(script[1], "Agent")
        self.assertIn("subagent_type", script[2] or "")
        self.assertIn("explore", script[2] or "")

    def test_openai_chat_error_finish_reason(self) -> None:
        script = _openai_chat_script(
            {"messages": [{"role": "user", "content": "please [error]"}]}
        )
        self.assertEqual(script[0], "error")
        completion = _openai_chat_completion(0, "mock", script)
        self.assertEqual(completion["choices"][0]["finish_reason"], "content_filter")
        stream = _openai_chat_sse(0, "mock", script)
        self.assertIn('"finish_reason":"content_filter"', stream)

    def test_openai_chat_prefers_run_terminal_cmd(self) -> None:
        script = _openai_chat_script(
            {
                "messages": [{"role": "user", "content": "[use-tool]"}],
                "tools": [
                    {"type": "function", "function": {"name": "read_file"}},
                    {"type": "function", "function": {"name": "run_terminal_command"}},
                ],
            }
        )
        self.assertEqual(script[1], "run_terminal_command")
        self.assertIn("echo rosetta-mock", script[2] or "")
        self.assertIn("description", script[2] or "")

    def test_gemini_non_streaming_text(self) -> None:
        script = _gemini_script(
            {"contents": [{"role": "user", "parts": [{"text": "hello"}]}]}
        )
        self.assertEqual(script[0], "text")
        response = _gemini_response(0, "mock-model", script)
        self.assertEqual(response["candidates"][0]["finishReason"], "STOP")
        self.assertIn(
            "rosetta mockmodel",
            response["candidates"][0]["content"]["parts"][0]["text"],
        )
        self.assertEqual(response["usageMetadata"]["promptTokenCount"], 128)
        self.assertEqual(response["usageMetadata"]["candidatesTokenCount"], 32)

    def test_gemini_streaming_tool_call(self) -> None:
        script = _gemini_script(
            {
                "contents": [{"role": "user", "parts": [{"text": "please [use-tool]"}]}],
                "tools": [
                    {
                        "functionDeclarations": [
                            {"name": "run_command"},
                            {"name": "list_dir"},
                        ]
                    }
                ],
            }
        )
        self.assertEqual(script[0], "function_call")
        self.assertEqual(script[1], "run_command")
        self.assertEqual(script[2]["commandLine"], "echo rosetta-mock")
        text = _gemini_sse(0, "mock-model", script)
        self.assertIn("functionCall", text)
        self.assertIn("run_command", text)

    def test_gemini_function_response_closes_loop(self) -> None:
        script = _gemini_script(
            {
                "contents": [
                    {"role": "user", "parts": [{"text": "please [use-tool]"}]},
                    {
                        "role": "model",
                        "parts": [{"functionCall": {"name": "run_command"}}],
                    },
                    {
                        "role": "user",
                        "parts": [
                            {
                                "functionResponse": {
                                    "name": "run_command",
                                    "response": {"output": "ok"},
                                }
                            }
                        ],
                    },
                ]
            }
        )
        self.assertEqual(script[0], "text")
        self.assertIn("Scenario complete", script[1])



class UsageMarkerTests(unittest.TestCase):
    """The `[usage]` marker's wire values, per dialect (RESEARCH_PLAN §5.13)."""

    def test_every_bucket_is_distinct_and_nonzero(self) -> None:
        for turn in (0, 1):
            numbers = usage_numbers(turn)
            tokens = [numbers[key] for key in ("uncached", "cache_read", "write_5m", "write_1h", "visible", "reasoning")]
            self.assertTrue(all(value > 0 for value in tokens))
            self.assertEqual(len(set(tokens)), len(tokens))
        self.assertNotEqual(usage_numbers(0), usage_numbers(1))

    def test_anthropic_reports_exclusive_input_and_inclusive_output(self) -> None:
        numbers = usage_numbers(0)
        usage = _anthropic_usage(0)
        self.assertEqual(usage["input_tokens"], numbers["uncached"])
        self.assertEqual(
            usage["cache_creation_input_tokens"],
            usage["cache_creation"]["ephemeral_5m_input_tokens"]
            + usage["cache_creation"]["ephemeral_1h_input_tokens"],
        )
        self.assertEqual(usage["output_tokens"], numbers["visible"] + numbers["reasoning"])
        self.assertEqual(usage["server_tool_use"]["web_search_requests"], 2)

    def test_openai_dialects_report_inclusive_totals(self) -> None:
        numbers = usage_numbers(1)
        prompt = numbers["uncached"] + numbers["cache_read"] + numbers["write_5m"]
        responses = _openai_usage(1)
        self.assertEqual(responses["input_tokens"], prompt)
        self.assertEqual(responses["input_tokens_details"]["cache_write_tokens"], numbers["write_5m"])
        self.assertEqual(responses["output_tokens_details"]["reasoning_tokens"], numbers["reasoning"])
        chat = _openai_chat_usage(128, 1)
        self.assertEqual(chat["prompt_tokens"], prompt)
        self.assertEqual(chat["completion_tokens"], numbers["visible"] + numbers["reasoning"])
        self.assertEqual(chat["cost_in_usd_ticks"], numbers["cost_ticks"])

    def test_gemini_reports_thoughts_beside_candidates(self) -> None:
        numbers = usage_numbers(0)
        usage = _gemini_usage(0)
        self.assertEqual(usage["candidatesTokenCount"], numbers["visible"])
        self.assertEqual(usage["thoughtsTokenCount"], numbers["reasoning"])

    def test_the_marker_switches_a_chat_completion_on(self) -> None:
        request = {"model": "mock", "messages": [{"role": "user", "content": f"hi {USAGE_MARKER}"}]}
        script = _openai_chat_script(request)
        completion = _openai_chat_completion(0, "mock", script, 128, 0)
        self.assertIn("prompt_tokens_details", completion["usage"])
        plain = _openai_chat_completion(0, "mock", script)
        self.assertEqual(plain["usage"]["prompt_tokens"], 128)

    def test_every_tool_use_scenario_has_a_usage_twin(self) -> None:
        from tools.groundtruth.farm import SCENARIOS

        for agent, scenarios in SCENARIOS.items():
            names = [scenario["name"] for scenario in scenarios]
            if "tool-use" in names:
                self.assertIn("usage", names, agent)
                twin = next(scenario for scenario in scenarios if scenario["name"] == "usage")
                self.assertTrue(all(USAGE_MARKER in prompt for prompt in twin["prompts"]))


if __name__ == "__main__":
    unittest.main()
