# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0
import datetime as dt
import importlib.util
import json
from pathlib import Path
import tempfile
import unittest
from unittest import mock

SPEC = importlib.util.spec_from_file_location("dependency_audit", Path(__file__).resolve().parents[2] / "tools/dependency_audit.py")
audit = importlib.util.module_from_spec(SPEC)
SPEC.loader.exec_module(audit)

class AuditTests(unittest.TestCase):
    def setUp(self):
        self.lock = {"package": [dict(name="example", version="1.0.0", source="registry+https://github.com/rust-lang/crates.io-index")]}

    def test_findings_are_distinct_from_incomplete_results(self):
        self.assertEqual(audit.audit(self.lock, {}, lambda _: {"results": [{}]})["status"], "clean")
        self.assertEqual(audit.audit(self.lock, {}, lambda _: {"results": [{"vulns": [{"id": "TEST-1"}]}]})["status"], "findings")
        for result in [{}, {"results": []}, {"results": [{"vulns": [{}]}]}, {"results": [{"error": "unavailable"}]}]:
            with self.assertRaises(ValueError):
                audit.audit(self.lock, {}, lambda _: result)

    def test_pagination_and_exact_exceptions(self):
        query = mock.Mock(side_effect=[{"results": [{"next_page_token": "page2"}]}, {"results": [{"vulns": [{"id": "TEST-1"}]}]}])
        report = audit.audit(self.lock, {("example", "1.0.0", "TEST-1"): {"reason": "temporary"}}, query)
        self.assertEqual(report["status"], "clean")
        self.assertEqual(len(report["exceptions"]), 1)
        self.assertEqual(query.call_args.args[0]["queries"][0]["page_token"], "page2")
        with self.assertRaises(ValueError):
            audit.audit(self.lock, {}, lambda _: {"results": [{"next_page_token": "same"}]})

    def test_expired_and_unbounded_exceptions_fail_the_audit(self):
        today = dt.date(2026, 9, 27)
        with tempfile.TemporaryDirectory() as directory:
            path = Path(directory) / "exceptions.json"
            item = dict(id="TEST-1", package="example", version="1.0.0", expires="2026-09-28", reason="tracked repair", owner="maintainer")
            for expiry in ["2026-09-26", "2026-09-27", "2027-09-27"]:
                path.write_text(json.dumps(dict(version=1, exceptions=[dict(item, expires=expiry)])))
                with self.assertRaises(ValueError): audit.exceptions(path, today)
            path.write_text(json.dumps(dict(version=1, exceptions=[item])))
            self.assertEqual(len(audit.exceptions(path, today)), 1)
