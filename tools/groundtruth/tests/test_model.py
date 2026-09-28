from __future__ import annotations

import unittest

from pathlib import Path

from tools.groundtruth.model import (
    ShapeAccumulator,
    glob_match,
    jsonl_record_type,
    merge_docs,
    merge_shapes,
    sanitize_census_document,
    shape_of,
    sqlite_schema_name,
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

    def test_uuid_and_redaction_keys_collapse(self) -> None:
        shape = shape_of(
            {
                "3baa3d39-d12f-4ac7-89fd-c2eb6369cd09": True,
                "<PROJECT>": "bd6ccc47-c565-40ec-a71a-ae53ce0b3481",
            }
        )
        self.assertEqual(list(shape["fields"]), ["<dynamic>"])
        self.assertEqual(shape["fields"]["<dynamic>"]["observed"], 2)

    def test_native_id_keys_collapse(self) -> None:
        shape = shape_of(
            {
                "toolu_014Q1xingyToyWpN3CCfCcdt": {"command": "ls"},
                "toolu_01ARoTaMy3L4gh1adUHm8Pqz": {"file_path": "a"},
                "0123456789abcdef0123": {"cwd": "b"},
            }
        )
        self.assertEqual(list(shape["fields"]), ["<dynamic>"])
        self.assertEqual(shape["fields"]["<dynamic>"]["observed"], 3)
        self.assertEqual(
            sorted(shape["fields"]["<dynamic>"]["shape"]["fields"]),
            ["command", "cwd", "file_path"],
        )

    def test_id_named_schema_fields_are_kept(self) -> None:
        shape = shape_of({"tool_use_id": "a", "msg_id": "b", "session_id": "c"})
        self.assertEqual(
            list(shape["fields"]), ["msg_id", "session_id", "tool_use_id"]
        )

    def test_wide_schema_field_names_are_kept(self) -> None:
        row = {f"col_{index:02d}": index for index in range(56)}
        shape = shape_of(row)
        self.assertEqual(len(shape["fields"]), 56)
        self.assertIn("col_00", shape["fields"])
        self.assertNotIn("<dynamic>", shape["fields"])

    def test_sqlite_schema_name_collapses_uuid_files(self) -> None:
        path = Path("conversations/3baa3d39-d12f-4ac7-89fd-c2eb6369cd09.db")
        self.assertEqual(sqlite_schema_name(path, path.as_posix()), "conversations.db")
        self.assertEqual(
            sqlite_schema_name(
                Path("conversation_summaries.db"), "conversation_summaries.db"
            ),
            "conversation_summaries.db",
        )

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
        self.assertTrue(
            glob_match(
                "projects/*/*/subagents/workflows/*/journal.jsonl",
                "projects/p/s/subagents/workflows/wf-1/journal.jsonl",
            )
        )
        self.assertTrue(
            glob_match(
                "projects/*/*/workflows/wf_*.json",
                "projects/p/s/workflows/wf_1.json",
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
            [
                document("observed", "organic", {"x": 1}),
                document("fixture", "1.0", {"x": 2}),
                document("writer", "1.0", {"x": 3}),
            ]
        )
        self.assertEqual(merged["agent_version"], "1.0")
        self.assertEqual(merged["evidence"], "writer")
        self.assertEqual(merged["streams"][0]["evidence"], "writer")
        record = merged["streams"][0]["record_types"]["event"]
        self.assertEqual(record["observed"], 3)
        self.assertEqual(record["evidence"], "writer")

    def test_census_keeps_protocol_enums_but_not_payload_values(self) -> None:
        document = {
            "format": "rosetta-groundtruth/1",
            "agent": "a",
            "agent_version": "1.0.0",
            "evidence": "observed",
            "generator": "tools.groundtruth census 1",
            "data_root": "~/.a",
            "streams": [
                {
                    "id": "events",
                    "discriminator": "type",
                    "record_types": {
                        "assistant": {
                            "observed": 1,
                            "shape": shape_of(
                                {
                                    "type": "assistant",
                                    "role": "assistant",
                                    "id": "msg_abcdefgh",
                                    "title": "private-project",
                                    "model": "claude-opus-4-8[1m]",
                                    "toolName": "mcp__private__deploy",
                                    "tools": ["Read", "mcp__private__deploy"],
                                    "byName": {"spaghetti-channel": True},
                                }
                            ),
                        }
                    },
                    "facets": [
                        {
                            "id": "tool-input",
                            "discriminator": "name",
                            "record_types": {
                                "Bash": {"observed": 1, "shape": shape_of({"x": 1})},
                                "mcp__private__deploy": {
                                    "observed": 1,
                                    "shape": shape_of({"y": 1}),
                                },
                            },
                        }
                    ],
                }
            ],
        }
        sanitized = sanitize_census_document(document)
        stream = sanitized["streams"][0]
        fields = stream["record_types"]["assistant"]["shape"]["fields"]
        self.assertEqual(fields["type"]["shape"]["values"], {"assistant": 1})
        self.assertEqual(fields["role"]["shape"]["values"], {"assistant": 1})
        self.assertNotIn("values", fields["id"]["shape"])
        self.assertNotIn("values", fields["title"]["shape"])
        self.assertEqual(
            fields["model"]["shape"]["values"],
            {"claude-opus-4-8[1m]": 1},
        )
        self.assertEqual(fields["toolName"]["shape"]["values"], {"mcp__*": 1})
        self.assertEqual(
            fields["tools"]["shape"]["items"]["values"],
            {"Read": 1, "mcp__*": 1},
        )
        self.assertEqual(
            list(fields["byName"]["shape"]["fields"]), ["<dynamic>"]
        )
        self.assertEqual(
            list(stream["facets"][0]["record_types"]), ["Bash", "mcp__*"]
        )

    def test_census_drops_epoch_stamped_identifiers(self) -> None:
        document = {
            "format": "rosetta-groundtruth/1",
            "agent": "a",
            "agent_version": "1.0.0",
            "evidence": "observed",
            "generator": "tools.groundtruth census 2",
            "data_root": "~/.a",
            "streams": [
                {
                    "id": "events",
                    # Values as an older census committed them, before the
                    # policy existed: the sanitizer `check` runs must drop them.
                    "record_types": {
                        "": {
                            "observed": 3,
                            "shape": {
                                "kind": "object",
                                "fields": {
                                    name: {
                                        "required": True,
                                        "observed": 3,
                                        "shape": {"kind": "string", "values": values},
                                    }
                                    for name, values in {
                                        "version": {
                                            "1790555900-d9dc": 1,
                                            "1790561051-5714": 1,
                                            "2.1.283": 1,
                                        },
                                        "model": {
                                            "claude-3-5-sonnet-20241022": 1,
                                            "opus": 2,
                                        },
                                        "status": {"1790555900123": 1, "done": 2},
                                    }.items()
                                },
                            },
                        }
                    },
                }
            ],
        }
        fields = sanitize_census_document(document)["streams"][0]["record_types"][""][
            "shape"
        ]["fields"]
        self.assertEqual(fields["version"]["shape"]["values"], {"2.1.283": 1})
        self.assertEqual(
            fields["model"]["shape"]["values"],
            {"claude-3-5-sonnet-20241022": 1, "opus": 2},
        )
        self.assertEqual(fields["status"]["shape"]["values"], {"done": 2})
        self.assertNotIn(
            "values", shape_of({"version": "1790555900-d9dc"})["fields"]["version"]["shape"]
        )

    def test_census_keeps_sqlite_table_identities(self) -> None:
        document = {
            "format": "rosetta-groundtruth/1",
            "agent": "a",
            "agent_version": "1.0.0",
            "evidence": "observed",
            "generator": "tools.groundtruth census 1",
            "data_root": "~/.a",
            "streams": [
                {
                    "id": "database",
                    "discriminator": "table",
                    "record_types": {
                        "state.db:messages": {
                            "observed": 1,
                            "shape": shape_of({"role": "assistant"}),
                        }
                    },
                }
            ],
        }
        sanitized = sanitize_census_document(document)
        self.assertEqual(
            list(sanitized["streams"][0]["record_types"]),
            ["state.db:messages"],
        )

    def test_long_protocol_enum_is_not_mistaken_for_payload(self) -> None:
        shape = shape_of({"type": "permission.record_approval_result"})
        self.assertEqual(
            shape["fields"]["type"]["shape"]["values"],
            {"permission.record_approval_result": 1},
        )

    def test_census_keeps_session_update_and_tool_completed_types(self) -> None:
        document = {
            "format": "rosetta-groundtruth/1",
            "agent": "grok",
            "agent_version": "observed",
            "evidence": "observed",
            "generator": "tools.groundtruth census 2",
            "data_root": "~/.grok",
            "streams": [
                {
                    "id": "updates",
                    "discriminator": "sessionUpdate",
                    "record_types": {
                        "tool_call": {
                            "observed": 1,
                            "shape": shape_of({"sessionUpdate": "tool_call"}),
                        },
                        "agent_thought_chunk": {
                            "observed": 1,
                            "shape": shape_of({"sessionUpdate": "agent_thought_chunk"}),
                        },
                    },
                },
                {
                    "id": "events",
                    "discriminator": "type",
                    "record_types": {
                        "tool_completed": {
                            "observed": 1,
                            "shape": shape_of({"type": "tool_completed"}),
                        }
                    },
                },
            ],
        }
        sanitized = sanitize_census_document(document)
        self.assertEqual(
            set(sanitized["streams"][0]["record_types"]),
            {"tool_call", "agent_thought_chunk"},
        )
        self.assertIn("tool_completed", sanitized["streams"][1]["record_types"])

    def test_jsonl_record_type_prefers_session_update(self) -> None:
        self.assertEqual(jsonl_record_type({"type": "user"}), "user")
        self.assertEqual(
            jsonl_record_type(
                {
                    "method": "session/update",
                    "params": {"update": {"sessionUpdate": "tool_call"}},
                }
            ),
            "tool_call",
        )
        self.assertEqual(jsonl_record_type({"prompt_index": 0}), "<untyped>")
        self.assertEqual(
            jsonl_record_type({"role": "assistant", "content": "hi"}),
            "assistant",
        )
        self.assertEqual(
            jsonl_record_type(
                {"role": "meta", "type": "system.version", "version": "0.38.0"}
            ),
            "system.version",
        )


if __name__ == "__main__":
    unittest.main()
