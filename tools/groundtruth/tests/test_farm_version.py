from __future__ import annotations

import unittest

from tools.groundtruth.farm import _parse_agent_version


class FarmVersionTests(unittest.TestCase):
    def test_hermes_v_prefix_is_not_eaten(self) -> None:
        output = (
            "Hermes Agent v0.20.5 (2026.8.19)\n"
            "Python: 3.13.11\n"
            "OpenAI SDK: 2.24.0\n"
        )
        self.assertEqual(_parse_agent_version(output), "0.20.5")

    def test_space_delimited_semver(self) -> None:
        self.assertEqual(_parse_agent_version("codex-cli 0.149.1"), "0.149.1")


if __name__ == "__main__":
    unittest.main()
