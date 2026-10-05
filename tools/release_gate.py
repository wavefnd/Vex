#!/usr/bin/env python3
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0
"""Fail closed unless the release SHA is current master with successful native CI."""

import argparse
import json
import re
import subprocess
from pathlib import Path

REPOSITORY = "wavefnd/Vex"
REQUIRED_JOBS = frozenset({"Quality / Linux amd64"} | {
    "Platform / " + p["id"] for p in json.loads(
        (Path(__file__).resolve().parents[1] / "platforms.json").read_text()
    )["platforms"]
})


def api(path):
    result = subprocess.run(["gh", "api", f"repos/{REPOSITORY}/{path}"],
                            check=True, text=True, capture_output=True)
    return json.loads(result.stdout)


def validate_run(commit, run, jobs):
    if (run.get("head_sha") != commit or run.get("head_branch") != "master"
            or run.get("event") != "push" or run.get("status") != "completed"
            or run.get("conclusion") != "success"):
        raise ValueError("the latest master CI run for the release commit is not successful")
    for name in sorted(REQUIRED_JOBS):
        matches = [job for job in jobs if job.get("name") == name]
        if (len(matches) != 1 or matches[0].get("head_sha") != commit
                or matches[0].get("run_id") != run.get("id")
                or matches[0].get("status") != "completed"
                or matches[0].get("conclusion") != "success"):
            raise ValueError(f"required CI job is missing, ambiguous, or unsuccessful: {name}")


def check(commit, fetch=api):
    if not re.fullmatch(r"[0-9a-f]{40}", commit):
        raise ValueError("expected a complete release commit SHA")
    if fetch("git/ref/heads/master")["object"]["sha"] != commit:
        raise ValueError("upstream master advanced; prepare a release from current master")
    runs = fetch(f"actions/workflows/ci.yml/runs?head_sha={commit}&branch=master&event=push&per_page=100")["workflow_runs"]
    if not runs:
        raise ValueError("the release commit has no master CI run")
    run = max(runs, key=lambda item: item["id"])
    # An attempt-specific endpoint prevents jobs from different reruns being mixed.
    jobs = []
    page = 1
    while True:
        batch = fetch(f"actions/runs/{run['id']}/attempts/{run['run_attempt']}/jobs?per_page=100&page={page}")["jobs"]
        jobs.extend(batch)
        if len(batch) < 100:
            break
        page += 1
    validate_run(commit, run, jobs)
    # Recheck after reading CI, immediately before the caller creates the release.
    if fetch("git/ref/heads/master")["object"]["sha"] != commit:
        raise ValueError("upstream master advanced during release verification; retry from current master")
    print(f"Verified {REPOSITORY}@{commit}, CI run {run['id']} attempt {run['run_attempt']}")
    for name in sorted(REQUIRED_JOBS):
        print(f"  passed: {name}")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--commit", required=True)
    args = parser.parse_args()
    try:
        check(args.commit)
    except (ValueError, KeyError, subprocess.CalledProcessError) as error:
        parser.exit(1, f"error: release gate failed: {error}\nhelp: wait for all master CI jobs to pass, then dispatch a release from that commit\n")
