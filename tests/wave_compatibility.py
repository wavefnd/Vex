#!/usr/bin/env python3
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0
"""Real-compiler smoke, deliberately separate from the network-free Rust suite."""

import argparse
import hashlib
import json
import os
from pathlib import Path
import subprocess
import tempfile


def run(command, cwd, env, *, succeeds=True):
    result = subprocess.run(command, cwd=cwd, env=env, text=True, capture_output=True)
    print(f"$ {' '.join(map(str, command))}", flush=True)
    print(result.stdout, end="", flush=True)
    print(result.stderr, end="", flush=True)
    if (result.returncode == 0) != succeeds:
        raise RuntimeError(f"unexpected exit {result.returncode}: {command}")
    return result


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vex", type=Path, required=True)
    parser.add_argument("--wavec-bin", type=Path, required=True)
    parser.add_argument("--reexports", action="store_true")
    parser.add_argument("--expected-version", help="Require this exact compiler version token")
    parser.add_argument("--expected-sha256", help="Require this compiler executable SHA-256 (not the archive digest)")
    args = parser.parse_args()
    vex = str(args.vex.resolve())
    compiler = args.wavec_bin.resolve() / ("wavec.exe" if os.name == "nt" else "wavec")
    digest = hashlib.sha256(compiler.read_bytes()).hexdigest()
    print(f"wavec executable: {compiler}\nwavec executable SHA-256: {digest}", flush=True)
    if args.expected_sha256 and digest != args.expected_sha256.lower():
        raise RuntimeError("compiler executable SHA-256 does not match the selected artifact")
    env = os.environ.copy()
    env.pop("VEX_WAVEC", None)
    env["PATH"] = str(args.wavec_bin.resolve()) + os.pathsep + env.get("PATH", "")
    with tempfile.TemporaryDirectory(prefix="vex-wave-compat-") as temporary:
        root = Path(temporary)
        # Python's Windows executable search does not use a replacement env PATH.
        # Probe this exact compiler; Vex itself still selects it through PATH.
        version = run([str(compiler), "--version"], root, dict(env, NO_COLOR="1"))
        if args.expected_version and version.stdout.split()[:2] != ["wavec", args.expected_version]:
            raise RuntimeError("compiler version does not match the selected release")
        capabilities = run([str(compiler), "print", "supported-targets", "--format=json"], root, env)
        targets = json.loads(capabilities.stdout)
        if not isinstance(targets, list) or not targets or not all(isinstance(t, str) and t for t in targets):
            raise RuntimeError("invalid supported-targets response")
        app, middle, leaf = [root / name for name in ("app", "middle", "leaf")]
        for package in (app, middle, leaf):
            package.mkdir()
            run([vex, "init", *(["--lib"] if package != app else [])], package, env)
        for command in ("build", "check", "run"):
            result = run([vex, command, "--locked", "--offline"], app, env)
            if command == "run" and "Hello World" not in result.stdout:
                raise RuntimeError("Hello World was not printed through PATH wavec")
        nested = app / "src/nested"
        nested.mkdir()
        result = run([vex, "run", "--locked", "--offline"], nested, env)
        if "Hello World" not in result.stdout:
            raise RuntimeError("ancestor-selected run did not execute the root package")
        result = run([vex, "--manifest-path", str(app / "vex.ws"), "check", "--locked", "--offline"], root, env)
        for command in ("build", "check", "run"):
            result = run([vex, command, "--dry-run", "--locked", "--offline"], nested, env)
            plan = json.loads(result.stdout)
            if plan["schema_version"] != 1 or plan["target"] not in targets:
                raise RuntimeError("compiler plan disagrees with capabilities")
            if command == "build":
                host_target = plan["target"]
        run([vex, "check", "--target", host_target, "--locked", "--offline"], nested, env)
        rejected = run([vex, "build", "--emit=obj"], app, env, succeeds=False)
        if "unknown Vex option" not in rejected.stderr:
            raise RuntimeError("raw compiler option was not rejected by Vex")

        # The first-project guide consumes the unmodified generated library.
        (app / "vex.ws").write_text(
            '{format=2,name="app",compiler="0.2.1-pre-beta",dependencies=[{name="middle",path="../middle"}]}\n', encoding="utf-8")
        (app / "src/main.wave").write_text('import("middle")::{greet};\nfun main() { greet(); }\n', encoding="utf-8")
        run([vex, "fetch"], app, env)
        generated = run([vex, "run", "--locked", "--offline"], app, env)
        if "Hello from library" not in generated.stdout:
            raise RuntimeError("generated library first-project example did not run")

        (leaf / "src/lib.wave").write_text(
            "pub fun value() -> i32 { return 42; }\nfun hidden() -> i32 { return 9; }\n",
            encoding="utf-8",
        )
        run(["git", "init", "-q", "-b", "master"], leaf, env)
        run(["git", "add", "."], leaf, env)
        run(["git", "-c", "user.name=Vex Test", "-c", "user.email=vex@example.invalid", "commit", "-qm", "fixture"], leaf, env)
        (middle / "vex.ws").write_text(
            '{ name = "middle", version = 0.1.0, lib = true, dependencies = ['
            f'{{ name = "leaf", git = "{leaf.as_uri()}" }}] }}\n', encoding="utf-8",
        )
        (middle / "src/lib.wave").write_text(
            'pub import("leaf")::{value};\n' if args.reexports else
            'import("leaf");\npub fun forwarded() -> i32 { return value(); }\n',
            encoding="utf-8",
        )
        symbol = "value" if args.reexports else "forwarded"
        (app / "vex.ws").write_text(
            '{ name = "app", version = 0.1.0, dependencies = [{ name = "middle", path = "../middle" }] }\n',
            encoding="utf-8",
        )
        import_line = f'import("middle")::{{{symbol}}};' if args.reexports else 'import("middle");'
        (app / "src/main.wave").write_text(
            f'{import_line}\nfun main() {{ var result: i32 = {symbol}(); println("{{}}", result); }}\n',
            encoding="utf-8",
        )
        run([vex, "fetch"], app, env)
        locked = (app / "vex.lock").read_bytes()
        # Take the original source offline: successful reuse cannot clone/fetch it.
        leaf.rename(root / "offline_leaf")
        for command in ("fetch", "build", "check", "run"):
            result = run([vex, command, "--locked", "--offline"], app, env)
            if command == "run" and "42" not in result.stdout.splitlines():
                raise RuntimeError("dependency graph did not produce 42")
            if (app / "vex.lock").read_bytes() != locked:
                raise RuntimeError("locked/offline command changed vex.lock")
        message_path = root / "run-events.jsonl"
        result = run([vex, "--message-file", str(message_path), "run", "--locked", "--offline"], app, env)
        events = [json.loads(line) for line in message_path.read_text().splitlines()]
        artifacts = [event for event in events if event["event"] == "artifact"]
        if len(artifacts) != 1 or not artifacts[0]["paths"] or not artifacts[0]["executable"]:
            raise RuntimeError("run did not report its artifact and executable")
        if events[-1]["origin"] != "program" or events[-1]["exit_code"] != 0:
            raise RuntimeError("runtime outcome was not preserved")
        metadata = run([vex, "metadata", "--locked", "--offline"], nested, env)
        graph = json.loads(metadata.stdout)
        repeated = run([vex, "--manifest-path", str(app / "vex.ws"), "metadata"], root, env)
        if metadata.stdout != repeated.stdout or graph["schema_version"] != 1:
            raise RuntimeError("metadata changed with project-selection method")
        if graph["root"]["dependencies"] != ["middle"] or [p["name"] for p in graph["packages"]] != ["leaf", "middle"]:
            raise RuntimeError("metadata omitted the transitive graph")
        if (app / "vex.lock").read_bytes() != locked:
            raise RuntimeError("metadata changed vex.lock")
        if args.reexports:
            (middle / "src/lib.wave").write_text('pub import("leaf")::{hidden};\n', encoding="utf-8")
            (app / "src/main.wave").write_text('import("middle")::{hidden};\nfun main() { var result: i32 = hidden(); println("{}", result); }\n', encoding="utf-8")
            error_report = root / "compiler-errors.jsonl"
            rejected = run([vex, "--message-file", str(error_report), "check", "--locked", "--offline"], app, env, succeeds=False)
            errors = [json.loads(line) for line in error_report.read_text().splitlines()]
            compiler_errors = [item['compiler'] for item in errors if item['event'] == 'compiler-diagnostic']
            if not compiler_errors or not any('span' in json.dumps(item) and 'hidden' in json.dumps(item) for item in compiler_errors):
                raise RuntimeError("compiler source diagnostic payload was lost")
            if errors[-1]['origin'] != 'vex' or errors[-1]['exit_code'] != 4:
                raise RuntimeError("compiler failure outcome was lost")
            if "hidden" not in rejected.stderr or "private" not in rejected.stderr:
                raise RuntimeError("private import failed for an unexpected reason")
            if (app / "vex.lock").read_bytes() != locked:
                raise RuntimeError("private-symbol failure changed vex.lock")


if __name__ == "__main__":
    main()
