# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0
"""Audit exact Cargo.lock versions against OSV; findings=1, audit failure=2."""
from __future__ import annotations

import argparse
import datetime as dt
import hashlib
import json
from pathlib import Path
import sys
import tomllib
import urllib.request

ROOT = Path(__file__).resolve().parents[1]
API = "https://api.osv.dev/v1/querybatch"


def request(payload: dict) -> dict:
    req = urllib.request.Request(API, data=json.dumps(payload).encode(), headers={
        "Content-Type": "application/json", "User-Agent": "Vex-dependency-audit/1",
    })
    with urllib.request.urlopen(req, timeout=45) as response:
        raw = response.read(32 * 1024 * 1024 + 1)
    if len(raw) > 32 * 1024 * 1024:
        raise ValueError("OSV response exceeds 32 MiB")
    result = json.loads(raw)
    if not isinstance(result, dict):
        raise ValueError("OSV response must be an object")
    return result


def exceptions(path: Path, today: dt.date) -> dict:
    config = json.loads(path.read_text(encoding="utf-8"))
    if set(config) != {"version", "exceptions"} or config["version"] != 1 or not isinstance(config["exceptions"], list):
        raise ValueError("invalid audit exception policy")
    active = {}
    for item in config["exceptions"]:
        if not isinstance(item, dict) or set(item) != {"id", "package", "version", "expires", "reason", "owner"}:
            raise ValueError("exceptions require id/package/version/expires/reason/owner")
        if any(not isinstance(value, str) or not value.strip() for value in item.values()):
            raise ValueError("exception fields must be nonempty strings")
        expiry = dt.date.fromisoformat(item["expires"])
        if not today < expiry <= today + dt.timedelta(days=90):
            raise ValueError(f"exception {item['id']} is expired or exceeds the 90-day maximum")
        key = (item["package"], item["version"], item["id"])
        if key in active:
            raise ValueError(f"duplicate exception: {key}")
        active[key] = item
    return active


def audit(lock: dict, allowed: dict, query=request) -> dict:
    packages = []
    for package in lock["package"]:
        source = package.get("source")
        if source is None:
            continue  # Workspace packages are the code being reviewed.
        if source != "registry+https://github.com/rust-lang/crates.io-index":
            raise ValueError(f"unsupported audit source for {package['name']}; audit is incomplete")
        packages.append(package)
    findings, suppressed = [], []
    for start in range(0, len(packages), 100):
        batch = packages[start:start + 100]
        queries = [{"package": {"ecosystem": "crates.io", "name": p["name"]}, "version": p["version"]} for p in batch]
        pending = list(range(len(batch)))
        seen_pages = set()
        seen_findings = set()
        for _ in range(100):
            if not pending:
                break
            data = query({"queries": [queries[i] for i in pending]})
            results = data.get("results")
            if not isinstance(results, list) or len(results) != len(pending):
                raise ValueError("OSV returned incomplete batch results")
            following = []
            for index, result in zip(pending, results):
                if not isinstance(result, dict) or set(result) - {"vulns", "next_page_token"}:
                    raise ValueError("OSV returned an invalid result")
                vulns = result.get("vulns", [])
                if not isinstance(vulns, list):
                    raise ValueError("OSV vulnerabilities must be a list")
                package = batch[index]
                for vuln in vulns:
                    if not isinstance(vuln, dict) or not isinstance(vuln.get("id"), str) or not vuln["id"]:
                        raise ValueError("OSV vulnerability is missing its ID")
                    key = (package["name"], package["version"], vuln["id"])
                    if key in seen_findings:
                        continue
                    seen_findings.add(key)
                    item = dict(package=key[0], version=key[1], id=key[2])
                    if key in allowed:
                        item["exception"] = allowed[key]
                        suppressed.append(item)
                    else:
                        findings.append(item)
                token = result.get("next_page_token")
                if token:
                    if not isinstance(token, str) or (index, token) in seen_pages:
                        raise ValueError("OSV pagination did not advance")
                    seen_pages.add((index, token))
                    queries[index]["page_token"] = token
                    following.append(index)
            pending = following
        if pending:
            raise ValueError("OSV pagination limit exceeded")
    return dict(status="findings" if findings else "clean", scanned_packages=len(packages), findings=findings, exceptions=suppressed)


def main() -> int:
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--lockfile", type=Path, default=ROOT / "Cargo.lock")
    parser.add_argument("--exceptions", type=Path, default=ROOT / "audit-exceptions.json")
    args = parser.parse_args()
    now = dt.datetime.now(dt.timezone.utc)
    try:
        raw = args.lockfile.read_bytes()
        result = audit(tomllib.loads(raw.decode()), exceptions(args.exceptions, now.date()))
        result.update(observed_at=now.isoformat(), lock_sha256=hashlib.sha256(raw).hexdigest())
        print(json.dumps(result, indent=2))
        return 1 if result["findings"] else 0
    except (OSError, ValueError, KeyError, TypeError) as error:
        print(json.dumps(dict(status="audit-failed", error=str(error), observed_at=now.isoformat())))
        print("error: dependency audit could not complete; this is not a clean audit or a vulnerability finding", file=sys.stderr)
        return 2


if __name__ == "__main__":
    sys.exit(main())
