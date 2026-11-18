from __future__ import annotations

import json
import sqlite3
import tempfile
import unittest
from pathlib import Path

from tools.groundtruth.farm import (
    WITHHELD_INVENTORY,
    _redact,
    _redact_bytes,
    _redact_sqlite,
    _withhold_antigravity_implicit,
)
from tools.groundtruth.model import GroundTruthError


def _length_delimited(field: int, payload: bytes) -> bytes:
    assert len(payload) < 128
    return bytes([(field << 3) | 2, len(payload)]) + payload


class BinaryRedactionTests(unittest.TestCase):
    def test_bytes_keep_their_length_so_protobuf_framing_survives(self) -> None:
        secret = b"/var/folders/ab/cdef/T/rosetta-farm-x-1/scenarios/s"
        message = _length_delimited(1, secret + b"/home/.gemini") + _length_delimited(2, b"ok")
        redacted = _redact_bytes(message, [(secret.decode(), "<SCENARIO_DIR>")])

        self.assertEqual(len(redacted), len(message))
        self.assertNotIn(secret, redacted)
        self.assertIn(b"<SCENARIO_DIR", redacted)
        self.assertEqual(redacted[1], message[1])
        self.assertTrue(redacted.endswith(_length_delimited(2, b"ok")))

    def test_blob_columns_are_redacted_whatever_their_declared_type(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "conversation.db"
            secret = "/private/tmp/rosetta-farm/tool-use/project"
            payload = _length_delimited(1, b"Command Working Directory: " + secret.encode()[:60])
            connection = sqlite3.connect(path)
            connection.execute("PRAGMA journal_mode = WAL")
            connection.execute("CREATE TABLE steps (id INTEGER PRIMARY KEY, step_payload blob, note TEXT)")
            connection.execute("INSERT INTO steps (step_payload, note) VALUES (?, ?)", (payload, secret))
            connection.commit()
            connection.close()

            _redact_sqlite(path, [(secret, "<PROJECT>")])

            connection = sqlite3.connect(path)
            try:
                blob, note = connection.execute("SELECT step_payload, note FROM steps").fetchone()
                self.assertIsInstance(blob, bytes)
                self.assertEqual(len(blob), len(payload))
                self.assertEqual(note, "<PROJECT>")
                self.assertEqual(connection.execute("PRAGMA integrity_check").fetchone(), ("ok",))
            finally:
                connection.close()
            self.assertNotIn(b"rosetta-farm", path.read_bytes())


class RedactTreeTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.work = Path(self._tmp.name) / "rosetta-farm-antigravity-1"
        self.scenario_dir = self.work / "scenarios" / "basic-chat"
        self.project = self.scenario_dir / "project"
        self.destination = Path(self._tmp.name) / "fixture"
        self.destination.mkdir(parents=True)

    def test_binary_files_and_blobs_lose_every_operator_path(self) -> None:
        home = f"{self.scenario_dir}/home/.gemini/antigravity-cli"
        (self.destination / "jetbox_summaries_proto.pb").write_bytes(
            b"\xff\xfe" + _length_delimited(1, f"file://{self.project}".encode()[:120])
        )
        database = self.destination / "conversation.db"
        connection = sqlite3.connect(database)
        connection.execute("CREATE TABLE steps (step_payload blob)")
        connection.execute(
            "INSERT INTO steps VALUES (?)", (_length_delimited(1, f"App Data Directory: {home}".encode()[:120]),)
        )
        connection.commit()
        connection.close()
        (self.destination / "transcript.jsonl").write_text(f'{{"cwd": "{self.project}"}}\n')

        _redact(self.destination, self.scenario_dir, self.work, self.project)

        temp_root = tempfile.gettempdir().encode()
        for path in self.destination.rglob("*"):
            if path.is_file():
                data = path.read_bytes()
                self.assertNotIn(str(self.work).encode(), data, path.name)
                self.assertNotIn(temp_root, data, path.name)
        self.assertEqual((self.destination / "transcript.jsonl").read_text(), '{"cwd": "<PROJECT>"}\n')

    def test_a_surviving_per_user_temp_directory_fails_the_harvest(self) -> None:
        (self.destination / "state.pb").write_bytes(b"\xff\xfe/var/folders/zz/someoneelse/T/x")

        with self.assertRaisesRegex(GroundTruthError, "per-user temp directory"):
            _redact(self.destination, self.scenario_dir, self.work, self.project)


class WithheldBlobTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.addCleanup(self._tmp.cleanup)
        self.destination = Path(self._tmp.name) / "basic-chat"
        self.implicit = self.destination / "antigravity-cli" / "implicit"
        self.implicit.mkdir(parents=True)

    def test_opaque_implicit_blobs_leave_only_their_name_and_size(self) -> None:
        # Redaction cannot look inside an encrypted blob, so none is published.
        (self.implicit / "593c96d3.pb").write_bytes(b"\xbc\x10" + b"\x00" * 272)

        _withhold_antigravity_implicit(self.destination, "antigravity-cli")

        self.assertFalse(self.implicit.exists())
        inventory = json.loads((self.destination / WITHHELD_INVENTORY).read_text())
        self.assertEqual(
            inventory["files"],
            [{"path": "antigravity-cli/implicit/593c96d3.pb", "bytes": 274}],
        )

    def test_a_harvest_without_implicit_blobs_writes_no_inventory(self) -> None:
        self.implicit.rmdir()

        _withhold_antigravity_implicit(self.destination, "antigravity-cli")

        self.assertFalse((self.destination / WITHHELD_INVENTORY).exists())


if __name__ == "__main__":
    unittest.main()
