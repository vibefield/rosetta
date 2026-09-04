from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from tools.groundtruth.census import FACET_MERGE_CAP, SHAPE_MERGE_CAP, capture


class CensusTests(unittest.TestCase):
    def test_claude_workflow_facets_keep_nested_workers_and_attempt_surfaces(self) -> None:
        repo = Path(__file__).resolve().parents[3]
        agent_dir = repo / "groundtruth" / "claude-code"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            session = root / "projects" / "project" / "session"
            workflow_children = session / "subagents" / "workflows" / "wf_test"
            workflow_runs = session / "workflows"
            workflow_children.mkdir(parents=True)
            workflow_runs.mkdir()

            tool_id = "toolu_workflow_test"
            transcript = [
                {
                    "type": "assistant",
                    "message": {
                        "model": "claude-opus-test",
                        "content": [
                            {
                                "type": "tool_use",
                                "id": tool_id,
                                "name": "Workflow",
                                "input": {"script": "private"},
                            }
                        ],
                    },
                },
                {
                    "type": "user",
                    "message": {
                        "content": [
                            {
                                "type": "tool_result",
                                "tool_use_id": tool_id,
                                "content": "plain-text launch receipt",
                            }
                        ]
                    },
                    "toolUseResult": {
                        "status": "async_launched",
                        "runId": "wf_test",
                    },
                },
                {
                    "type": "user",
                    "message": {
                        "content": (
                            "<task-notification><task-id>task</task-id>"
                            f"<tool-use-id>{tool_id}</tool-use-id>"
                            "<status>completed</status><result>{}</result>"
                            "</task-notification>"
                        )
                    },
                },
            ]
            (session.parent / "main.jsonl").write_text(
                "".join(json.dumps(record) + "\n" for record in transcript)
            )
            (workflow_runs / "wf_test.json").write_text(
                json.dumps(
                    {
                        "runId": "wf_test",
                        "defaultModel": "claude-opus-test",
                        "result": {"ok": True},
                        "workflowProgress": [
                            {"type": "workflow_phase", "index": 0, "title": "phase"},
                            {
                                "type": "workflow_agent",
                                "state": "done",
                                "model": "claude-opus-test",
                                "cached": True,
                                "index": 0,
                                "label": "worker",
                            },
                            {
                                "type": "workflow_agent",
                                "state": "error",
                                "model": "claude-sonnet-test",
                                "index": 1,
                                "label": "worker",
                            },
                        ],
                    }
                )
            )
            (workflow_children / "agent-worker.jsonl").write_text(
                json.dumps(
                    {
                        "type": "assistant",
                        "effort": "max",
                        "message": {"model": "claude-opus-test", "content": []},
                    }
                )
                + "\n"
            )
            (workflow_children / "agent-worker.meta.json").write_text(
                json.dumps({"agentType": "workflow-subagent", "spawnDepth": 1})
            )
            (workflow_children / "journal.jsonl").write_text(
                json.dumps(
                    {
                        "type": "result",
                        "key": "worker",
                        "agentId": "worker",
                        "result": "private",
                    }
                )
                + "\n"
            )

            document, _ = capture(agent_dir, root, "test")

        streams = {stream["id"]: stream for stream in document["streams"]}
        run_facets = {
            facet["id"]: facet for facet in streams["workflow-run"]["facets"]
        }
        progress = run_facets["workflow-progress-items"]["record_types"]
        self.assertEqual(progress["workflow_phase"]["observed"], 1)
        self.assertEqual(progress["workflow_agent"]["observed"], 2)
        states = run_facets["workflow-agent-states"]["record_types"]
        self.assertEqual(states["done"]["observed"], 1)
        self.assertEqual(states["error"]["observed"], 1)
        cache = run_facets["workflow-agent-cache-states"]["record_types"]
        self.assertEqual(cache["cached"]["observed"], 1)
        self.assertEqual(cache["not-marked-cached"]["observed"], 1)
        models = run_facets["workflow-agent-models"]["record_types"]
        self.assertIn("claude-opus-test", models)

        transcript_facets = {
            facet["id"]: facet for facet in streams["transcript"]["facets"]
        }
        self.assertEqual(
            transcript_facets["workflow-tool-result-blocks"]["record_types"][
                "launch-receipt"
            ]["observed"],
            1,
        )
        self.assertEqual(
            transcript_facets["workflow-task-notifications"]["record_types"][
                "completed"
            ]["observed"],
            1,
        )
        self.assertEqual(
            transcript_facets["workflow-task-notification-results"]["record_types"][
                "complete"
            ]["observed"],
            1,
        )

        child_facets = {
            facet["id"]: facet
            for facet in streams["subagent-transcript"]["facets"]
        }
        self.assertEqual(
            child_facets["workflow-child-records"]["record_types"]["assistant"][
                "observed"
            ],
            1,
        )
        self.assertIn(
            "claude-opus-test.max",
            child_facets["workflow-child-response-profiles"]["record_types"],
        )
        journal_facets = {
            facet["id"]: facet for facet in streams["workflow-journal"]["facets"]
        }
        self.assertEqual(
            journal_facets["workflow-journal-results"]["record_types"]["string"][
                "observed"
            ],
            1,
        )
        metadata_facets = {
            facet["id"]: facet for facet in streams["subagent-metadata"]["facets"]
        }
        self.assertEqual(
            metadata_facets["workflow-worker-metadata"]["record_types"][
                "workflow-subagent"
            ]["observed"],
            1,
        )

    def test_json_array_items_feed_nested_facets(self) -> None:
        repo = Path(__file__).resolve().parents[3]
        agent_dir = repo / "groundtruth" / "claude-code"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            inbox = root / "teams" / "team" / "inboxes"
            projects = root / "projects"
            inbox.mkdir(parents=True)
            projects.mkdir()
            (inbox / "worker.json").write_text(
                json.dumps(
                    [
                        {
                            "type": "task_assignment",
                            "from": "lead",
                            "text": "private",
                            "read": False,
                        },
                        {
                            "from": "worker",
                            "text": "private",
                            "read": True,
                        },
                    ]
                )
            )
            document, count = capture(agent_dir, root, "test")

        self.assertEqual(count, 1)
        stream = next(
            stream for stream in document["streams"] if stream["id"] == "team-inbox"
        )
        facets = {facet["id"]: facet for facet in stream["facets"]}
        self.assertEqual(
            facets["team-inbox-messages"]["record_types"]["task_assignment"]["observed"],
            1,
        )
        self.assertEqual(
            facets["team-inbox-messages"]["record_types"]["<plain>"]["observed"],
            1,
        )
        self.assertEqual(
            facets["team-inbox-delivery"]["record_types"]["unread"]["observed"],
            1,
        )

    def test_json_metadata_keeps_a_rare_field_beyond_the_jsonl_sample_cap(self) -> None:
        repo = Path(__file__).resolve().parents[3]
        agent_dir = repo / "groundtruth" / "claude-code"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            sidecars = root / "projects" / "project" / "parent" / "subagents"
            sidecars.mkdir(parents=True)
            for index in range(SHAPE_MERGE_CAP + 1):
                (sidecars / f"agent-{index:04}.meta.json").write_text(
                    json.dumps({"agentType": "Explore"})
                )
            (sidecars / "agent-late.meta.json").write_text(
                json.dumps(
                    {
                        "agentType": "Explore",
                        "worktreePath": "/tmp/project/.claude/worktrees/late",
                    }
                )
            )
            document, count = capture(agent_dir, root, "test")

        self.assertEqual(count, SHAPE_MERGE_CAP + 2)
        stream = next(
            stream
            for stream in document["streams"]
            if stream["id"] == "subagent-metadata"
        )
        fields = stream["record_types"][""]["shape"]["fields"]
        self.assertIn("worktreePath", fields)
        self.assertEqual(fields["worktreePath"]["observed"], 1)

    def test_late_nested_variant_is_not_hidden_by_envelope_sample_cap(self) -> None:
        records = [
            {
                "type": "context.append_loop_event",
                "event": {"type": "step.begin", "uuid": f"step-{index}"},
                "time": index + 1,
            }
            for index in range(FACET_MERGE_CAP + 1)
        ]
        records.append(
            {
                "type": "context.append_loop_event",
                "event": {
                    "type": "content.part",
                    "stepUuid": "step-media",
                    "part": {
                        "type": "image_url",
                        "imageUrl": {"url": "blobref:image"},
                    },
                },
                "time": len(records) + 1,
            }
        )

        repo = Path(__file__).resolve().parents[3]
        agent_dir = repo / "groundtruth" / "kimi-code"
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            wire = root / "sessions" / "wd_test" / "session-1" / "agents" / "main"
            wire.mkdir(parents=True)
            (wire / "wire.jsonl").write_text(
                "".join(json.dumps(record) + "\n" for record in records)
            )
            document, count = capture(agent_dir, root, "test")

        self.assertEqual(count, len(records))
        stream = next(stream for stream in document["streams"] if stream["id"] == "wire")
        facets = {facet["id"]: facet for facet in stream["facets"]}
        self.assertEqual(
            facets["loop-events"]["record_types"]["content.part"]["observed"],
            1,
        )
        self.assertEqual(
            facets["content-parts"]["record_types"]["image_url"]["observed"],
            1,
        )


if __name__ == "__main__":
    unittest.main()
