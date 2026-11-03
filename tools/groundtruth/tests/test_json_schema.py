from __future__ import annotations

import tempfile
import unittest
from pathlib import Path

from tools.groundtruth.json_schema import (
    copilot_schema_candidates,
    persisted_session_events,
    schema_shape,
    session_events_writer_doc,
)


class JsonSchemaTests(unittest.TestCase):
    def test_any_of_null_string(self) -> None:
        shape = schema_shape(
            {"anyOf": [{"type": "string"}, {"type": "null"}]},
            {},
        )
        self.assertEqual(shape["kind"], "any_of")
        kinds = {variant["kind"] for variant in shape["variants"]}
        self.assertEqual(kinds, {"string", "null"})

    def test_persisted_events_drop_ephemeral(self) -> None:
        schema = {
            "definitions": {
                "SessionEvent": {
                    "anyOf": [
                        {"$ref": "#/definitions/StartEvent"},
                        {"$ref": "#/definitions/IdleEvent"},
                    ]
                },
                "StartEvent": {
                    "type": "object",
                    "required": ["id", "timestamp", "parentId", "type", "data"],
                    "properties": {
                        "id": {"type": "string"},
                        "timestamp": {"type": "string"},
                        "parentId": {"anyOf": [{"type": "string"}, {"type": "null"}]},
                        "type": {"const": "session.start"},
                        "data": {
                            "type": "object",
                            "required": ["sessionId"],
                            "properties": {"sessionId": {"type": "string"}, "alreadyInUse": {"type": "boolean"}},
                        },
                    },
                },
                "IdleEvent": {
                    "type": "object",
                    "required": ["id", "timestamp", "parentId", "ephemeral", "type", "data"],
                    "properties": {
                        "id": {"type": "string"},
                        "type": {"const": "session.idle"},
                        "ephemeral": {"type": "boolean"},
                        "data": {"type": "object"},
                    },
                },
            }
        }
        records = persisted_session_events(schema)
        self.assertEqual(list(records), ["session.start"])
        fields = records["session.start"]["shape"]["fields"]
        self.assertTrue(fields["id"]["required"])
        self.assertFalse(fields["data"]["shape"]["fields"]["alreadyInUse"]["required"])
        self.assertTrue(fields["data"]["shape"]["fields"]["sessionId"]["required"])
        document = session_events_writer_doc(schema, agent="copilot", version="1.0.80")
        self.assertEqual(document["evidence"], "writer")
        self.assertEqual(document["streams"][0]["id"], "events")

    def test_copilot_schema_from_shipped_or_self_extracted_payload(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            install = Path(directory)
            self.assertEqual(copilot_schema_candidates(install, "1.0.105"), [])
            extracted = (
                install
                / "extract-cache/pkg/linux-x64/1.0.105/schemas/session-events.schema.json"
            )
            extracted.parent.mkdir(parents=True)
            extracted.write_text("{}")
            other = install / "extract-cache/pkg/linux-x64/1.0.104/schemas"
            other.mkdir(parents=True)
            (other / "session-events.schema.json").write_text("{}")
            self.assertEqual(copilot_schema_candidates(install, "1.0.105"), [extracted])
            shipped = (
                install
                / "node_modules/@github/copilot-darwin-arm64/schemas/session-events.schema.json"
            )
            shipped.parent.mkdir(parents=True)
            shipped.write_text("{}")
            self.assertEqual(
                copilot_schema_candidates(install, "1.0.105"), [shipped, extracted]
            )


if __name__ == "__main__":
    unittest.main()
