from __future__ import annotations

import shutil
import sys
import tempfile
import unittest
from pathlib import Path

_HERE = Path(__file__).resolve().parent
if str(_HERE) not in sys.path:
    sys.path.insert(0, str(_HERE))

from sweep import (  # noqa: E402
    COLUMNS,
    RunResult,
    SweepError,
    build_command,
    csv_row,
    format_table,
    parse_budget,
    parse_budgets,
    parse_milestones,
    resolve_home,
)

FULL_RUN = """\
rosettad: serve starting
milestone: starter-drained elapsed_s=1.25 files=42 paused=7
milestone: cold-drained elapsed_s=18.50
milestone: canonical-ready elapsed_s=19.02
milestone: search-ready elapsed_s=24.75
rosettad: serve done
"""


class BudgetTests(unittest.TestCase):
    def test_plain_integer(self) -> None:
        self.assertEqual(parse_budget("65536"), 65536)
        self.assertEqual(parse_budget("  262144  "), 262144)

    def test_kilo_and_mega_suffixes(self) -> None:
        self.assertEqual(parse_budget("64k"), 65536)
        self.assertEqual(parse_budget("256k"), 262144)
        self.assertEqual(parse_budget("1m"), 1048576)
        self.assertEqual(parse_budget("1M"), 1048576)
        self.assertEqual(parse_budget("2K"), 2048)

    def test_zero_disables_slicing(self) -> None:
        self.assertEqual(parse_budget("0"), 0)

    def test_garbage_is_rejected(self) -> None:
        for token in ("", "  ", "k", "m", "-1", "1.5", "64kb", "64g", "one", "1 2"):
            with self.subTest(token=token):
                with self.assertRaises(SweepError):
                    parse_budget(token)

    def test_list_keeps_input_order(self) -> None:
        self.assertEqual(
            parse_budgets("1m, 64k,0"),
            [("1m", 1048576), ("64k", 65536), ("0", 0)],
        )

    def test_list_rejects_a_repeated_byte_count(self) -> None:
        with self.assertRaises(SweepError) as caught:
            parse_budgets("64k,65536")
        self.assertIn("duplicate", str(caught.exception))


class MilestoneTests(unittest.TestCase):
    def test_full_set(self) -> None:
        found = parse_milestones(FULL_RUN)
        self.assertEqual(
            sorted(found),
            ["canonical-ready", "cold-drained", "search-ready", "starter-drained"],
        )
        self.assertEqual(found["starter-drained"]["elapsed_s"], 1.25)
        self.assertEqual(found["starter-drained"]["files"], 42)
        self.assertEqual(found["starter-drained"]["paused"], 7)
        self.assertEqual(found["cold-drained"]["elapsed_s"], 18.50)
        self.assertEqual(found["canonical-ready"]["elapsed_s"], 19.02)
        self.assertEqual(found["search-ready"]["elapsed_s"], 24.75)

    def test_partial_set_from_an_interrupted_run(self) -> None:
        found = parse_milestones(
            "milestone: starter-drained elapsed_s=0.90 files=3 paused=0\n"
            "milestone: cold-drained elapsed_s=5.00\n"
        )
        self.assertEqual(sorted(found), ["cold-drained", "starter-drained"])
        self.assertNotIn("canonical-ready", found)
        self.assertNotIn("search-ready", found)

    def test_slicing_disabled_has_no_starter_milestone(self) -> None:
        found = parse_milestones(
            "milestone: cold-drained elapsed_s=17.00\n"
            "milestone: canonical-ready elapsed_s=17.40\n"
            "milestone: search-ready elapsed_s=22.10\n"
        )
        self.assertNotIn("starter-drained", found)
        self.assertEqual(len(found), 3)

    def test_interleaved_noise_is_ignored(self) -> None:
        found = parse_milestones(
            "warning: skipped a source\n"
            "milestone: cold-drained elapsed_s=2.00\n"
            "claude-code: watched 0 ticks — 0 events\n"
            "milestone: something-else elapsed_s=3.00\n"
            "milestone: search-ready files=9\n"
            "milestone: canonical-ready elapsed_s=not-a-number\n"
            "elapsed_s=99.00 without any milestone prefix\n"
            "milestone: search-ready elapsed_s=4.00\n"
        )
        self.assertEqual(sorted(found), ["cold-drained", "search-ready"])
        self.assertEqual(found["search-ready"]["elapsed_s"], 4.00)

    def test_duplicate_lines_keep_the_first(self) -> None:
        found = parse_milestones(
            "milestone: cold-drained elapsed_s=8.00\n"
            "milestone: cold-drained elapsed_s=9.00\n"
            "milestone: starter-drained elapsed_s=1.00 files=5 paused=1\n"
            "milestone: starter-drained elapsed_s=2.00 files=6 paused=2\n"
        )
        self.assertEqual(found["cold-drained"]["elapsed_s"], 8.00)
        self.assertEqual(found["starter-drained"]["elapsed_s"], 1.00)
        self.assertEqual(found["starter-drained"]["files"], 5)
        self.assertEqual(found["starter-drained"]["paused"], 1)

    def test_empty_output(self) -> None:
        self.assertEqual(parse_milestones(""), {})


class RowTests(unittest.TestCase):
    def test_successful_run(self) -> None:
        result = RunResult(
            budget="256k",
            milestones=parse_milestones(FULL_RUN),
            wall_s=26.5,
            ok=True,
            status="exit 0",
        )
        self.assertEqual(
            csv_row(result),
            ["256k", "1.25", "18.50", "19.02", "24.75", "26.50", "42", "7", "true"],
        )

    def test_failed_run_leaves_every_missing_cell_empty(self) -> None:
        result = RunResult(
            budget="1m",
            milestones=parse_milestones("milestone: starter-drained elapsed_s=1.00 files=4 paused=0\n"),
            wall_s=3600.0,
            ok=False,
            status="timed out after 3600s",
        )
        self.assertEqual(
            csv_row(result),
            ["1m", "1.00", "", "", "", "3600.00", "4", "0", "false"],
        )

    def test_failed_run_with_no_output_at_all(self) -> None:
        result = RunResult(budget="0", wall_s=0.5, ok=False, status="exit 101")
        self.assertEqual(
            csv_row(result), ["0", "", "", "", "", "0.50", "", "", "false"]
        )

    def test_table_renders_missing_cells_as_dashes(self) -> None:
        table = format_table(
            [
                RunResult(
                    budget="64k",
                    milestones=parse_milestones(FULL_RUN),
                    wall_s=26.5,
                    ok=True,
                ),
                RunResult(budget="0", wall_s=0.5, ok=False),
            ]
        )
        lines = table.splitlines()
        self.assertEqual(len(lines), 4)
        for line in lines:
            self.assertEqual(line.count("|"), len(COLUMNS) + 1)
            self.assertEqual(len(line), len(lines[0]))
        self.assertIn("budget", lines[0])
        self.assertIn("—", lines[3])
        self.assertIn("false", lines[3])


class CommandTests(unittest.TestCase):
    def test_all_agents_is_the_default(self) -> None:
        command = build_command(
            "256k",
            db_path=Path("/tmp/x/projection.sqlite"),
            home=Path("/snapshots/frozen"),
            agents=[],
            release=True,
        )
        self.assertEqual(
            command,
            [
                "cargo", "run", "--release", "-p", "rosettad", "--", "serve",
                "--db", "/tmp/x/projection.sqlite",
                "--home", "/snapshots/frozen",
                "--all-agents",
                "--ticks", "0",
                "--slice-budget", "256k",
            ],
        )

    def test_agent_kinds_replace_all_agents(self) -> None:
        command = build_command(
            "0",
            db_path=Path("/tmp/x/projection.sqlite"),
            home=Path("/snapshots/frozen"),
            agents=["claude-code", "codex"],
            release=False,
        )
        self.assertNotIn("--release", command)
        self.assertNotIn("--all-agents", command)
        self.assertEqual(
            command[command.index("--home") + 2 :],
            [
                "--agent", "claude-code",
                "--agent", "codex",
                "--ticks", "0",
                "--slice-budget", "0",
            ],
        )


class HomeTests(unittest.TestCase):
    """`--home` refusals, exercised against temp dirs standing in for a real home."""

    def setUp(self) -> None:
        self.root = Path(tempfile.mkdtemp(prefix="slice-sweep-home-")).resolve()
        self.addCleanup(shutil.rmtree, self.root, ignore_errors=True)
        self.live = self.root / "live-home"
        (self.live / ".claude" / "projects").mkdir(parents=True)
        self.snapshot = self.root / "snapshot"
        (self.snapshot / ".claude").mkdir(parents=True)

    def _alias_of_live(self) -> Path:
        """The live home spelled through a symlink and a `..` hop."""

        alias = self.root / "by-another-name"
        alias.symlink_to(self.live, target_is_directory=True)
        return alias / ".." / "by-another-name"

    def test_frozen_snapshot_is_accepted(self) -> None:
        self.assertEqual(
            resolve_home(str(self.snapshot), live_home=self.live), self.snapshot
        )

    def test_live_home_is_refused_however_it_is_spelled(self) -> None:
        spelling = self._alias_of_live()
        self.assertNotEqual(str(spelling), str(self.live))
        with self.assertRaises(SweepError) as caught:
            resolve_home(str(self.live), live_home=spelling)
        self.assertIn("is the live home directory", str(caught.exception))

    def test_live_home_reached_through_a_symlinked_parent_is_refused(self) -> None:
        with self.assertRaises(SweepError) as caught:
            resolve_home(str(self._alias_of_live()), live_home=self.live)
        self.assertIn("is the live home directory", str(caught.exception))

    def test_child_symlink_into_the_live_home_is_refused(self) -> None:
        link = self.snapshot / ".codex"
        link.symlink_to(self.live / ".claude", target_is_directory=True)
        with self.assertRaises(SweepError) as caught:
            resolve_home(str(self.snapshot), live_home=self.live)
        self.assertIn(str(link), str(caught.exception))

    def test_home_that_is_itself_a_symlink_into_the_live_home_is_refused(self) -> None:
        alias = self.root / "looks-frozen"
        alias.symlink_to(self.live / ".claude", target_is_directory=True)
        with self.assertRaises(SweepError) as caught:
            resolve_home(str(alias), live_home=self.live)
        self.assertIn(str(alias), str(caught.exception))

    def test_child_symlink_pointing_elsewhere_is_accepted(self) -> None:
        elsewhere = self.root / "elsewhere"
        elsewhere.mkdir()
        (self.snapshot / ".codex").symlink_to(elsewhere, target_is_directory=True)
        self.assertEqual(
            resolve_home(str(self.snapshot), live_home=self.live), self.snapshot
        )

    def test_a_real_directory_inside_the_live_home_is_accepted(self) -> None:
        frozen = self.live / "snapshots" / "2026-08-30"
        frozen.mkdir(parents=True)
        self.assertEqual(resolve_home(str(frozen), live_home=self.live), frozen)

    def test_symlinks_below_the_immediate_children_are_not_scanned(self) -> None:
        # The documented bound: this check catches the common accident, it does
        # not prove the snapshot is frozen.
        deep = self.snapshot / ".claude" / "projects"
        deep.mkdir(parents=True)
        (deep / "live").symlink_to(self.live / ".claude", target_is_directory=True)
        self.assertEqual(
            resolve_home(str(self.snapshot), live_home=self.live), self.snapshot
        )

    def test_a_missing_directory_is_refused(self) -> None:
        with self.assertRaises(SweepError) as caught:
            resolve_home(str(self.root / "absent"), live_home=self.live)
        self.assertIn("not a directory", str(caught.exception))


if __name__ == "__main__":
    unittest.main()
