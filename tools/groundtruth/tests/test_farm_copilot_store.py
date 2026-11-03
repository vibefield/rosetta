import sqlite3
import tempfile
import unittest
from pathlib import Path

from tools.groundtruth.farm import _redact_sqlite, _reduce_copilot_store


class CopilotStoreReductionTests(unittest.TestCase):
    def test_fts5_index_is_emptied_through_its_module(self) -> None:
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "session-store.db"
            connection = sqlite3.connect(path)
            connection.executescript(
                """
                CREATE TABLE schema_version (version INTEGER NOT NULL);
                INSERT INTO schema_version VALUES (7);
                CREATE TABLE sessions (id TEXT PRIMARY KEY, cwd TEXT, created_at TEXT);
                INSERT INTO sessions VALUES ('s1', '/secret/project', 't0');
                CREATE TABLE turns (id INTEGER PRIMARY KEY, body TEXT);
                INSERT INTO turns (body) VALUES ('private prompt');
                CREATE VIRTUAL TABLE search_index USING fts5(content, session_id UNINDEXED);
                INSERT INTO search_index VALUES ('private prompt', 's1');
                """
            )
            connection.commit()
            connection.close()

            _reduce_copilot_store(path)
            _redact_sqlite(path, [("/secret", "<PROJECT>")])

            connection = sqlite3.connect(path)
            try:
                self.assertEqual(connection.execute("SELECT * FROM schema_version").fetchall(), [(7,)])
                self.assertEqual(
                    connection.execute("SELECT id, cwd, created_at FROM sessions").fetchall(),
                    [("s1", None, "t0")],
                )
                self.assertEqual(connection.execute("SELECT count(*) FROM turns").fetchone(), (0,))
                self.assertEqual(connection.execute("SELECT count(*) FROM search_index").fetchone(), (0,))
                self.assertEqual(
                    connection.execute("SELECT count(*) FROM search_index_content").fetchone(), (0,)
                )
                connection.execute("INSERT INTO search_index(search_index) VALUES ('integrity-check')")
                blocks = b"".join(
                    row[0] for row in connection.execute("SELECT block FROM search_index_data")
                )
                self.assertNotIn(b"private", blocks)
                self.assertNotIn(b"secret", path.read_bytes())
            finally:
                connection.close()


if __name__ == "__main__":
    unittest.main()
