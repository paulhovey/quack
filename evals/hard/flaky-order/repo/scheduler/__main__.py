import sys

from .jobs import load_jobs
from .report import describe

print(describe(load_jobs(sys.argv[1] if len(sys.argv) > 1 else "jobs.json")))
