from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from tools.groundtruth.census import FACET_MERGE_CAP, SHAPE_MERGE_CAP, capture


class CensusTests(unittest.TestCase):
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
