from __future__ import annotations

import hashlib
import json
import subprocess
import tempfile
import unittest
from pathlib import Path
from unittest import mock

from tools.groundtruth.farm import (
    AGY_DISABLE_AUTO_UPDATE,
    LOOPBACK_ONLY_PROFILE,
    ExecutableGuard,
    _antigravity_environment,
    _detect_version,
    _generate_fixtures,
    _install_antigravity,
    _scenario_invocation,
    _write_provenance,
)
from tools.groundtruth.model import GroundTruthError


class AntigravityFarmTests(unittest.TestCase):
    def setUp(self) -> None:
        self._tmp = tempfile.TemporaryDirectory()
        self.root = Path(self._tmp.name)
        self.binary = self.root / "bin" / "agy"
        self.binary.parent.mkdir()
        self.binary.write_bytes(b"#!/bin/sh\necho 1.4.12\n")
        self.binary.chmod(0o755)
        self.digest = hashlib.sha256(self.binary.read_bytes()).hexdigest()

    def tearDown(self) -> None:
        self._tmp.cleanup()

    def test_updater_kill_switch_is_the_literal_true(self) -> None:
        # `1` leaves the updater running; only lowercase `true` disables it.
        self.assertEqual(AGY_DISABLE_AUTO_UPDATE, "true")
        _, environment, _ = _scenario_invocation(
            "antigravity",
            self.binary,
            "hello",
            0,
            (),
            self.root / "home",
            self.root / "home" / ".gemini" / "antigravity-cli",
            "http://127.0.0.1:9",
        )
        self.assertEqual(environment["AGY_CLI_DISABLE_AUTO_UPDATE"], "true")
        self.assertEqual(environment["HOME"], str(self.root / "home"))
        self.assertEqual(environment["GOOGLE_GEMINI_BASE_URL"], "http://127.0.0.1:9")

    def test_version_probe_environment_is_disposable_and_has_no_credentials(self) -> None:
        home = self.root / "version-home"
        environment = _antigravity_environment(self.binary, home)
        self.assertEqual(environment["HOME"], str(home))
        self.assertEqual(environment["AGY_CLI_DISABLE_AUTO_UPDATE"], "true")
        self.assertNotIn("GEMINI_API_KEY", environment)
        self.assertNotIn("GOOGLE_GEMINI_BASE_URL", environment)

    def test_version_probe_runs_with_the_given_environment(self) -> None:
        home = self.root / "version-home"
        environment = _antigravity_environment(self.binary, home)
        completed = subprocess.CompletedProcess([], 0, stdout="1.4.12\n", stderr="")
        with mock.patch("tools.groundtruth.farm.subprocess.run", return_value=completed) as run:
            version = _detect_version(
                self.binary, env=environment, cwd=home, prefix=("sandbox-exec", "-f", "p.sb")
            )
        self.assertEqual(version, "1.4.12")
        args, kwargs = run.call_args
        self.assertEqual(args[0], ["sandbox-exec", "-f", "p.sb", str(self.binary), "--version"])
        self.assertEqual(kwargs["env"]["HOME"], str(home))
        self.assertEqual(kwargs["cwd"], home)

    def test_guard_rejects_an_unpinned_digest(self) -> None:
        with self.assertRaisesRegex(GroundTruthError, "does not match the pinned"):
            ExecutableGuard(self.binary, "0" * 64)

    def test_guard_detects_a_replaced_executable(self) -> None:
        guard = ExecutableGuard(self.binary, self.digest.upper())
        guard.verify("before step 0")
        self.assertEqual(guard.checks, 2)
        self.binary.write_bytes(b"#!/bin/sh\necho 1.5.0\n")
        with self.assertRaisesRegex(GroundTruthError, "not version-bound evidence"):
            guard.verify("during step 0")

    def test_local_binary_is_placed_without_download(self) -> None:
        with mock.patch("tools.groundtruth.farm.subprocess.run", wraps=subprocess.run) as run:
            placed = _install_antigravity(self.root / "install", "1.4.12", self.binary)
        self.assertEqual(placed, self.root / "install" / "agy")
        self.assertEqual(hashlib.sha256(placed.read_bytes()).hexdigest(), self.digest)
        for call in run.call_args_list:
            self.assertNotIn("curl", call.args[0])

    def test_local_binary_requires_a_pinned_digest(self) -> None:
        with self.assertRaisesRegex(GroundTruthError, "needs --binary-sha256"):
            _generate_fixtures(
                "antigravity", self.root, "1.4.12", False, local_binary=self.binary
            )

    def test_local_binary_is_antigravity_only(self) -> None:
        with self.assertRaisesRegex(GroundTruthError, "antigravity only"):
            _generate_fixtures(
                "grok",
                self.root,
                "1.0.0",
                False,
                local_binary=self.binary,
                binary_sha256=self.digest,
            )

    def test_loopback_profile_denies_all_but_loopback_egress(self) -> None:
        self.assertIn("(deny network-outbound)", LOOPBACK_ONLY_PROFILE)
        self.assertIn('(allow network-outbound (remote ip "localhost:*"))', LOOPBACK_ONLY_PROFILE)

    def test_provenance_records_the_run_without_secrets(self) -> None:
        guard = ExecutableGuard(self.binary, self.digest)
        guard.verify("after")
        destination = self.root / "scenario"
        destination.mkdir()
        _write_provenance(
            destination,
            agent="antigravity",
            version="1.4.12",
            scenario="basic-chat",
            dialect="gemini-generatecontent",
            guard=guard,
            executable_source="local-binary",
            loopback_only=True,
            mock_requests=3,
            started_at="2026-11-04T19:00:00Z",
            finished_at="2026-11-04T19:00:09Z",
        )
        text = (destination / "provenance.json").read_text()
        record = json.loads(text)
        self.assertEqual(record["executable"]["sha256"], self.digest)
        self.assertEqual(record["executable"]["digest_checks"], 2)
        self.assertEqual(record["environment"]["AGY_CLI_DISABLE_AUTO_UPDATE"], "true")
        self.assertEqual(record["network"], "loopback-only (macOS sandbox-exec)")
        self.assertNotIn("rosetta-mock-key", text)
        self.assertNotIn("GOOGLE_GEMINI_BASE_URL", text)
        self.assertNotIn(str(self.root), text)


if __name__ == "__main__":
    unittest.main()
