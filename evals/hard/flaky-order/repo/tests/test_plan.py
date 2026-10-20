import unittest
from pathlib import Path

from scheduler.jobs import Job, load_jobs
from scheduler.plan import CycleError, UnknownJob, plan

ROOT = Path(__file__).resolve().parent.parent


class PlanTest(unittest.TestCase):
    def test_dependencies_come_first(self):
        order = plan([Job("b", after=("a",)), Job("a")])
        self.assertEqual(order, ["a", "b"])

    def test_priority_wins_among_ready_jobs(self):
        order = plan([Job("slow", 1), Job("fast", 9)])
        self.assertEqual(order, ["fast", "slow"])

    def test_the_project_plan(self):
        order = plan(load_jobs(ROOT / "jobs.json"))
        self.assertEqual(
            order, ["fetch", "unit", "docs", "lint", "typecheck", "package", "publish"]
        )

    def test_cycles_and_unknown_jobs(self):
        with self.assertRaises(CycleError):
            plan([Job("a", after=("b",)), Job("b", after=("a",))])
        with self.assertRaises(UnknownJob):
            plan([Job("a", after=("nope",))])
