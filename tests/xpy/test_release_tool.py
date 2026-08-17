# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0

from __future__ import annotations

import hashlib
import importlib.util
import os
import re
import subprocess
import sys
import tarfile
import tempfile
import tomllib
import unittest
import zipfile
from pathlib import Path
from unittest import mock


ROOT = Path(__file__).resolve().parents[2]
with (ROOT / "Cargo.toml").open("rb") as manifest_file:
    EXPECTED_VERSION = tomllib.load(manifest_file)["package"]["version"]
MODULE_NAME = "vex_release_tool"
SPEC = importlib.util.spec_from_file_location(MODULE_NAME, ROOT / "x.py")
if SPEC is None or SPEC.loader is None:  # pragma: no cover - import setup failure
    raise RuntimeError("could not load x.py")
release_tool = importlib.util.module_from_spec(SPEC)
sys.modules[MODULE_NAME] = release_tool
SPEC.loader.exec_module(release_tool)


class ReleaseToolTests(unittest.TestCase):
    def test_load_version_reads_cargo_manifest(self) -> None:
        self.assertEqual(release_tool.load_version(), EXPECTED_VERSION)

    def test_load_version_accepts_full_semver(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            manifest = Path(temporary) / "Cargo.toml"
            manifest.write_text(
                '[package]\nname = "fixture"\nversion = "1.2.3-rc.1+build.7"\n',
                encoding="utf-8",
            )
            self.assertEqual(release_tool.load_version(manifest), "1.2.3-rc.1+build.7")

    def test_select_targets_deduplicates_without_reordering(self) -> None:
        selected = release_tool.select_targets(
            [
                "aarch64-unknown-linux-gnu",
                "x86_64-unknown-linux-gnu",
                "aarch64-unknown-linux-gnu",
            ]
        )
        self.assertEqual(
            [target.triple for target in selected],
            ["aarch64-unknown-linux-gnu", "x86_64-unknown-linux-gnu"],
        )

    def test_select_targets_rejects_unknown_target_with_known_targets(self) -> None:
        with self.assertRaisesRegex(
            release_tool.ReleaseError,
            r"(?s)unsupported target: imaginary-target.*Known targets",
        ):
            release_tool.select_targets(["imaginary-target"])

    def test_default_target_uses_explicit_host_override(self) -> None:
        with mock.patch.dict(
            os.environ,
            {"VEX_RELEASE_HOST": "aarch64-apple-darwin"},
            clear=False,
        ):
            selected = release_tool.select_targets([])
        self.assertEqual(selected, [release_tool.SUPPORTED_TARGETS["aarch64-apple-darwin"]])

    def test_source_date_epoch_rejects_invalid_value(self) -> None:
        with mock.patch.dict(os.environ, {"SOURCE_DATE_EPOCH": "yesterday"}):
            with self.assertRaisesRegex(
                release_tool.ReleaseError,
                "SOURCE_DATE_EPOCH must be a non-negative integer",
            ):
                release_tool.source_date_epoch()

    def test_tar_archive_is_deterministic_and_complete(self) -> None:
        target = release_tool.SUPPORTED_TARGETS["x86_64-unknown-linux-gnu"]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.make_package_inputs(root, target, b"linux binary")
            stage = release_tool.prepare_stage("0.0.1", target, root=root)
            first = root / "first.tar.gz"
            second = root / "second.tar.gz"
            release_tool.create_tar_archive(stage, first, 1_700_000_000)
            for entry in stage.iterdir():
                os.utime(entry, (1_800_000_000, 1_800_000_000))
            release_tool.create_tar_archive(stage, second, 1_700_000_000)

            self.assertEqual(first.read_bytes(), second.read_bytes())
            names, binary = release_tool.read_binary_from_tar(
                first, f"{stage.name}/{target.executable_name}"
            )
            self.assertEqual(
                names,
                release_tool.expected_archive_entries(stage.name, target),
            )
            self.assertEqual(binary, b"linux binary")
            with tarfile.open(first, "r:gz") as packaged:
                self.assertEqual(
                    packaged.getmember(f"{stage.name}/vex").mode,
                    0o755,
                )

    def test_zip_archive_is_deterministic_and_complete(self) -> None:
        target = release_tool.SUPPORTED_TARGETS["x86_64-pc-windows-msvc"]
        with tempfile.TemporaryDirectory() as temporary:
            root = Path(temporary)
            self.make_package_inputs(root, target, b"windows binary")
            stage = release_tool.prepare_stage("0.0.1", target, root=root)
            first = root / "first.zip"
            second = root / "second.zip"
            release_tool.create_zip_archive(stage, first, 1_700_000_000)
            release_tool.create_zip_archive(stage, second, 1_700_000_000)

            self.assertEqual(first.read_bytes(), second.read_bytes())
            names, binary = release_tool.read_binary_from_zip(
                first, f"{stage.name}/{target.executable_name}"
            )
            self.assertEqual(
                names,
                release_tool.expected_archive_entries(stage.name, target),
            )
            self.assertEqual(binary, b"windows binary")
            with zipfile.ZipFile(first) as packaged:
                mode = packaged.getinfo(f"{stage.name}/vex.exe").external_attr >> 16
                self.assertEqual(mode, 0o100755)

    def test_checksums_are_sorted_and_limited_to_requested_archives(self) -> None:
        with tempfile.TemporaryDirectory() as temporary:
            dist = Path(temporary)
            current_zip = dist / "vex-v0.0.1-z-target.zip"
            current_tar = dist / "vex-v0.0.1-a-target.tar.gz"
            old_tar = dist / "vex-v0.0.0-old-target.tar.gz"
            current_zip.write_bytes(b"zip")
            current_tar.write_bytes(b"tar")
            old_tar.write_bytes(b"old")

            checksum_path = release_tool.write_checksums(
                [current_zip, current_tar], dist
            )
            lines = checksum_path.read_text(encoding="utf-8").splitlines()
            self.assertEqual(
                lines,
                [
                    f"{hashlib.sha256(b'tar').hexdigest()}  {current_tar.name}",
                    f"{hashlib.sha256(b'zip').hexdigest()}  {current_zip.name}",
                ],
            )
            self.assertNotIn(str(dist), "\n".join(lines))

    def test_collect_release_archives_requires_exact_requested_set(self) -> None:
        linux = release_tool.SUPPORTED_TARGETS["x86_64-unknown-linux-gnu"]
        windows = release_tool.SUPPORTED_TARGETS["x86_64-pc-windows-msvc"]
        with tempfile.TemporaryDirectory() as temporary:
            dist = Path(temporary)
            linux_archive = release_tool.archive_path("0.0.1", linux, dist)
            windows_archive = release_tool.archive_path("0.0.1", windows, dist)
            linux_archive.write_bytes(b"linux")

            with self.assertRaisesRegex(
                release_tool.ReleaseError,
                rf"missing: {re.escape(windows_archive.name)}",
            ):
                release_tool.collect_release_archives(
                    [linux, windows], "0.0.1", dist
                )

            windows_archive.write_bytes(b"windows")
            self.assertEqual(
                release_tool.collect_release_archives(
                    [linux, windows], "0.0.1", dist
                ),
                [linux_archive, windows_archive],
            )

            extra = dist / "vex-v0.0.1-imaginary-target.tar.gz"
            extra.write_bytes(b"extra")
            with self.assertRaisesRegex(
                release_tool.ReleaseError,
                rf"unexpected: {re.escape(extra.name)}",
            ):
                release_tool.collect_release_archives(
                    [linux, windows], "0.0.1", dist
                )

    def test_release_requires_version_tag_at_head(self) -> None:
        with mock.patch.object(
            release_tool,
            "capture_command",
            side_effect=["v0.0.1\nother-tag", "tag"],
        ):
            release_tool.require_release_tag("0.0.1")

        with mock.patch.object(release_tool, "capture_command", return_value="other-tag"):
            with self.assertRaisesRegex(
                release_tool.ReleaseError,
                "official release must run from tag `v0.0.1`",
            ):
                release_tool.require_release_tag("0.0.1")

        with mock.patch.object(
            release_tool,
            "capture_command",
            side_effect=["v0.0.1", "commit"],
        ):
            with self.assertRaisesRegex(
                release_tool.ReleaseError,
                "must be annotated, not lightweight",
            ):
                release_tool.require_release_tag("0.0.1")

    def test_release_workflow_covers_every_supported_target(self) -> None:
        workflow = (ROOT / ".github/workflows/release.yml").read_text(
            encoding="utf-8"
        )
        for target in release_tool.SUPPORTED_TARGETS:
            self.assertIn(f"target: {target}", workflow)
            self.assertGreaterEqual(workflow.count(target), 2)
        self.assertIn("python x.py checksum", workflow)
        self.assertIn("uses: actions/attest@v4", workflow)
        self.assertIn("--draft", workflow)

    @staticmethod
    def make_package_inputs(root: Path, target: object, binary: bytes) -> None:
        executable_name = target.executable_name
        binary_path = root / "target" / target.triple / "release" / executable_name
        binary_path.parent.mkdir(parents=True)
        binary_path.write_bytes(binary)
        for document in release_tool.PACKAGE_DOCUMENTS:
            (root / document).write_text(f"{document}\n", encoding="utf-8")


class ReleaseToolCliTests(unittest.TestCase):
    def run_xpy(self, *arguments: str) -> subprocess.CompletedProcess[str]:
        return subprocess.run(
            [sys.executable, str(ROOT / "x.py"), *arguments],
            cwd=ROOT,
            check=False,
            text=True,
            stdout=subprocess.PIPE,
            stderr=subprocess.PIPE,
        )

    def test_version_output_uses_manifest_version(self) -> None:
        result = self.run_xpy("--version")
        self.assertEqual(result.returncode, 0)
        self.assertEqual(result.stdout.strip(), f"x.py {EXPECTED_VERSION}")

    def test_list_targets_reports_all_supported_targets(self) -> None:
        result = self.run_xpy("list-targets")
        self.assertEqual(result.returncode, 0)
        for target in release_tool.SUPPORTED_TARGETS:
            self.assertIn(target, result.stdout)

    def test_unknown_target_is_actionable(self) -> None:
        result = self.run_xpy("build", "imaginary-target")
        self.assertEqual(result.returncode, 1)
        self.assertIn("error: unsupported target: imaginary-target", result.stderr)
        self.assertIn("Known targets:", result.stderr)


if __name__ == "__main__":
    unittest.main()
