from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from tools.groundtruth.census import FACET_MERGE_CAP, capture


class CensusTests(unittest.TestCase):
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
