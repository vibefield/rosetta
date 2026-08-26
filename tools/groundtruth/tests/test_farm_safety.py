import contextlib
import io
import os
import unittest
from unittest import mock

from tools.groundtruth.farm import ISOLATED_RUNNER_ENV, _require_isolated_runner
from tools.groundtruth.model import GroundTruthError


class FarmSafetyTests(unittest.TestCase):
    @mock.patch("tools.groundtruth.farm._running_in_container", return_value=False)
    def test_fixture_execution_fails_closed_on_host(self, _container: mock.Mock) -> None:
        with mock.patch.dict(os.environ, {}, clear=True):
            with self.assertRaisesRegex(GroundTruthError, "downloaded agent code"):
                _require_isolated_runner(allow_host_execution=False)

    @mock.patch("tools.groundtruth.farm._running_in_container", return_value=False)
    def test_explicit_isolated_runner_attestation_is_accepted(
        self, _container: mock.Mock
    ) -> None:
        with mock.patch.dict(os.environ, {ISOLATED_RUNNER_ENV: "1"}, clear=True):
            _require_isolated_runner(allow_host_execution=False)

    @mock.patch("tools.groundtruth.farm._running_in_container", return_value=False)
    def test_host_override_is_noisy(self, _container: mock.Mock) -> None:
        stderr = io.StringIO()
        with mock.patch.dict(os.environ, {}, clear=True), contextlib.redirect_stderr(stderr):
            _require_isolated_runner(allow_host_execution=True)
        self.assertIn("WARNING", stderr.getvalue())


if __name__ == "__main__":
    unittest.main()
