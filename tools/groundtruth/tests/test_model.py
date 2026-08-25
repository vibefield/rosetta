from __future__ import annotations

import unittest

from tools.groundtruth.model import (
    ShapeAccumulator,
    glob_match,
    merge_docs,
    merge_shapes,
    shape_of,
)


class ShapeTests(unittest.TestCase):
    def test_object_merge_tracks_optionality_and_counts(self) -> None:
        merged = merge_shapes(shape_of({"x": 1, "y": "s"}), shape_of({"x": None}))
        fields = merged["fields"]
        self.assertTrue(fields["x"]["required"])
        self.assertEqual(fields["x"]["observed"], 2)
        self.assertEqual(
            fields["x"]["shape"],
            {"kind": "any_of", "variants": [{"kind": "number"}, {"kind": "null"}]},
        )
        self.assertFalse(fields["y"]["required"])
        self.assertEqual(fields["y"]["observed"], 1)

    def test_dynamic_keys_collapse(self) -> None:
        shape = shape_of({"a path/that is a value": 1, "another": 2})
        self.assertEqual(list(shape["fields"]), ["<dynamic>"])
        self.assertEqual(shape["fields"]["<dynamic>"]["observed"], 2)

    def test_accumulator(self) -> None:
        accumulator = ShapeAccumulator()
        accumulator.absorb("event", {"ok": True})
        accumulator.absorb("event", {"ok": False})
        record = accumulator.into_types()["event"]
        self.assertEqual(record["observed"], 2)
        self.assertEqual(record["shape"]["fields"]["ok"]["observed"], 2)

    def test_layout_globs(self) -> None:
        self.assertTrue(glob_match("projects/*/*.jsonl", "projects/p/a.jsonl"))
        self.assertFalse(glob_match("projects/*/*.jsonl", "projects/a.jsonl"))
        self.assertTrue(
            glob_match(
                "projects/*/*/subagents/**/agent-*.jsonl",
                "projects/p/s/subagents/x/y/agent-7.jsonl",
            )
        )
        self.assertTrue(glob_match("**/*.jsonl", "c.jsonl"))
        self.assertTrue(glob_match("todos/*-agent-*.json", "todos/1-agent-2.json"))

    def test_canonical_merge_records_evidence(self) -> None:
        def document(evidence: str, version: str, value: object) -> dict[str, object]:
            return {
                "format": "rosetta-groundtruth/1",
                "agent": "a",
                "agent_version": version,
                "evidence": evidence,
                "generator": evidence,
                "data_root": "~/.a",
                "streams": [
                    {
                        "id": "events",
                        "glob": "*.jsonl",
                        "file_format": "jsonl",
                        "discipline": "append_only",
                        "evidence": evidence,
                        "record_types": {
                            "event": {"observed": 1, "shape": shape_of(value)}
                        },
                    }
                ],
            }

        merged = merge_docs(
            [document("observed", "organic", {"x": 1}), document("fixture", "1.0", {"x": 2})]
        )
        self.assertEqual(merged["agent_version"], "1.0")
        record = merged["streams"][0]["record_types"]["event"]
        self.assertEqual(record["observed"], 2)
        self.assertEqual(record["evidence"], "fixture")


if __name__ == "__main__":
    unittest.main()
