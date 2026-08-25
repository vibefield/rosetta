from __future__ import annotations

import json
import tempfile
import unittest
from pathlib import Path

from tools.groundtruth.check import _validate_manifest


class ManifestTests(unittest.TestCase):
    def test_manifest_is_exact_and_fixable(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            scenario = Path(temporary) / "basic-chat"
            data = scenario / "home" / "events.jsonl"
            data.parent.mkdir(parents=True)
            data.write_text("{}\n")
            manifest = scenario / "manifest.json"
            manifest.write_text(
                json.dumps(
                    {
                        "agent": "agent",
                        "agent_version": "1.0.0",
                        "scenario": "basic-chat",
                        "generator": "tools.groundtruth farm 1",
                        "mock_dialect": "openai-chat",
                        "files": [],
                    }
                )
            )
            args = {
                "agent": "agent",
                "version": "1.0.0",
                "scenario": "basic-chat",
                "dialect": "openai-chat",
            }
            errors = _validate_manifest(manifest, fix=False, **args)
            self.assertEqual(len(errors), 1)
            self.assertIn("missing=home/events.jsonl", errors[0])

            self.assertEqual(_validate_manifest(manifest, fix=True, **args), [])
            repaired = json.loads(manifest.read_text())
            self.assertEqual(
                repaired["files"], [{"path": "home/events.jsonl", "bytes": 3}]
            )
            self.assertEqual(_validate_manifest(manifest, fix=False, **args), [])


if __name__ == "__main__":
    unittest.main()
