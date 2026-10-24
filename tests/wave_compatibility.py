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



def target_profile_smoke(vex, root, env, host_target):
    """Exercise format 3 through the selected real compiler on each CI host."""
    app = root / "multiple-targets"
    dependency = root / "multi-support"
    for package in (app, dependency):
        (package / "src").mkdir(parents=True)
    (dependency / "vex.ws").write_text(
        '{format=3,name="support",targets={lib={},bins=['
        '{name="unused",path="src/missing-binary.wave"}]}}\n', encoding="utf-8")
    (dependency / "src/lib.wave").write_text(
        'pub fun value() -> i32 { return 42; }\n', encoding="utf-8")
    (app / "vex.ws").write_text(
        '{format=3,name="multi",targets={lib={},bins=['
        '{name="multi",path="src/main.wave"},'
        '{name="tools",path="src/tools.wave"}]},default_run="multi",'
        'profiles={fast={inherits="release",opt_level=3}},'
        'dependencies=[{name="support",path="../multi-support"}]}\n', encoding="utf-8")
    (app / "src/lib.wave").write_text(
        'import("support")::{value};\npub fun answer() -> i32 { return value(); }\n'
        'fun hidden() -> i32 { return 9; }\n', encoding="utf-8")
    (app / "src/main.wave").write_text(
        'import("multi")::{answer};\nfun main() -> i32 { '
        'var result: i32 = answer(); println("default {}", result); return result - 42; }\n',
        encoding="utf-8")
    tool_source = ('import("multi")::{answer};\nfun main() -> i32 { '
                   'var result: i32 = answer(); println("tools {}", result); return result - 42; }\n')
    (app / "src/tools.wave").write_text(tool_source, encoding="utf-8")
    run([vex, "fetch", "--offline"], app, env)
    locked = (app / "vex.lock").read_bytes()

    def snapshot():
        return {str(path.relative_to(app)): hashlib.sha256(path.read_bytes()).hexdigest()
                for path in app.rglob("*") if path.is_file()}

    before = snapshot()
    planned = run([vex, "build", "--profile", "fast", "--dry-run", "--locked", "--offline"], app, env)
    envelope = json.loads(planned.stdout)
    plans = envelope.get("plans", [])
    if (envelope.get("schema") != "vex.build-plan" or envelope.get("schema_version") != 1
            or envelope.get("target") != host_target
            or envelope.get("profile") != {"name": "fast", "opt_level": 3}
            or [item["package_target"] for item in plans] != [
                {"name": "multi", "kind": "library"},
                {"name": "multi", "kind": "binary"},
                {"name": "tools", "kind": "binary"}]):
        raise RuntimeError("mixed targets or custom profile were lost in the Vex build-plan")
    if snapshot() != before:
        raise RuntimeError("format 3 dry-run changed project files")
    expected_root = Path("target") / host_target / "fast"
    expected_paths = []
    for item in plans:
        plan = item["compiler_plan"]
        target = item["package_target"]
        if plan["target"] != host_target:
            raise RuntimeError("mixed compiler plan selected the wrong target")
        if target["kind"] == "library":
            output = Path(plan["compile"][0]["output"])
            expected = expected_root / "lib" / ("multi.obj" if "windows" in host_target else "multi.o")
        else:
            output = Path(plan["link"]["output"])
            expected = expected_root / "bin" / (target["name"] + (".exe" if "windows" in host_target else ""))
        if output != expected:
            raise RuntimeError(f"unexpected unified output path: {output}, expected {expected}")
        expected_paths.append(app / output)
    run([vex, "build", "--profile", "fast", "--locked", "--offline"], app, env)
    if not all(path.is_file() for path in expected_paths):
        raise RuntimeError("mixed-target build omitted a planned artifact")
    for selection, marker in [([], "default 42"), (["--bin", "tools"], "tools 42")]:
        result = run([vex, "run", *selection, "--profile", "fast", "--locked", "--offline"], app, env)
        if marker not in result.stdout.splitlines():
            raise RuntimeError("default_run or explicit binary selection used the wrong target")
    run([vex, "check", "--profile", "fast", "--locked", "--offline"], app, env)
    if (dependency / "src/missing-binary.wave").exists() or (dependency / "target").exists():
        raise RuntimeError("dependency binary targets were built instead of consuming its library")
    rejected = run([vex, "metadata", "--locked", "--offline"], app, env, succeeds=False)
    if rejected.returncode != 3 or "--format-version 2" not in rejected.stderr:
        raise RuntimeError("metadata schema 1 did not reject a mixed-target package")
    metadata = json.loads(run([vex, "metadata", "--format-version", "2", "--locked", "--offline"], app, env).stdout)
    if (metadata["schema_version"] != 2 or len(metadata["root"]["targets"]) != 3
            or metadata["root"]["default_run"] != "multi"
            or metadata["root"]["profiles"]["fast"]["opt_level"] != 3):
        raise RuntimeError("metadata schema 2 lost targets, default_run, or custom profiles")
    (app / "src/tools.wave").write_text(
        'import("multi")::{hidden};\nfun main() -> i32 { return hidden(); }\n', encoding="utf-8")
    rejected = run([vex, "check", "--bin", "tools", "--locked", "--offline"], app, env, succeeds=False)
    if rejected.returncode != 4 or "hidden" not in rejected.stderr or "private" not in rejected.stderr:
        raise RuntimeError("private self-import did not retain the compiler failure contract")
    (app / "src/tools.wave").write_text(tool_source, encoding="utf-8")
    if (app / "vex.lock").read_bytes() != locked:
        raise RuntimeError("format 3 locked/offline operations changed vex.lock")
    print("Format 3 targets, output layout, custom profiles, self-import and metadata passed.", flush=True)


def package_identity_smoke(vex, root, env):
    for directory, version, symbol, value in (("alias_one", "1.2.3", "old_value", 11), ("alias_two", "2.0.0", "new_value", 22)):
        package = root / directory
        (package / "src").mkdir(parents=True)
        (package / "vex.ws").write_text(
            '{format=4,name="math",version="' + version + '",targets={lib={}}}', encoding="utf-8")
        (package / "src/lib.wave").write_text(
            f"pub fun {symbol}() -> i32 {{ return {value}; }}\n", encoding="utf-8")
    app = root / "alias_app"
    (app / "src").mkdir(parents=True)
    (app / "vex.ws").write_text(
        '{format=4,name="app",targets={bins=[{name="app",path="src/main.wave"}]},dependencies=['
        '{name="old_math",package="math",path="../alias_one",version_req="^1"},'
        '{name="new_math",package="math",path="../alias_two",version_req="^2"}]}', encoding="utf-8")
    (app / "src/main.wave").write_text(
        'import("old_math")::{old_value};\nimport("new_math")::{new_value};\n'
        'fun main() { println("{}", old_value() + new_value()); }\n', encoding="utf-8")
    run([vex, "fetch"], app, env)
    result = run([vex, "run", "--locked", "--offline"], app, env)
    if result.stdout.strip() != "33":
        raise RuntimeError("two package versions were not imported through their explicit aliases")


def feature_graph_smoke(vex, root, env, host_target):
    for directory, name, version in (("feature_active", "platform", "1.0.0"), ("feature_inactive", "platform", "2.0.0"), ("feature_extra", "extras", "1.0.0")):
        package = root / directory
        (package / "src").mkdir(parents=True)
        compiler = ',compiler="9.0.0"' if directory == "feature_inactive" else ""
        (package / "vex.ws").write_text(
            '{format=4,name="' + name + '",version="' + version + '",targets={lib={}}' + compiler + '}', encoding="utf-8")
        (package / "src/lib.wave").write_text('pub fun value() -> i32 { return 11; }\n', encoding="utf-8")
    app = root / "feature_app"
    (app / "src").mkdir(parents=True)
    other = "wasm32-unknown-unknown"
    (app / "vex.ws").write_text(
        '{format=4,name="app",targets={bins=[{name="app",path="src/main.wave"}]},dependencies=['
        '{name="platform",path="../feature_active",target={triple="' + host_target + '"}},'
        '{name="platform",path="../feature_inactive",target={triple="' + other + '"}},'
        '{name="extras",path="../feature_extra",optional=true}],features={default=["dep:extras"]}}', encoding="utf-8")
    (app / "src/main.wave").write_text('import("platform")::{value};\nfun main() { println("{}", value()); }\n', encoding="utf-8")
    run([vex, "fetch", "--target", host_target], app, env)
    locked = (app / "vex.lock").read_bytes()
    for flags in ([], ["--no-default-features"]):
        result = run([vex, "run", "--locked", "--offline", *flags], app, env)
        if result.stdout.strip() != "11":
            raise RuntimeError("selected target/feature graph did not execute correctly")
    result = run([vex, "metadata", "--format-version", "2", "--target", other, "--locked"], app, env)
    packages = json.loads(result.stdout)["packages"]
    if not any(package["name"] == "platform" and package["version"] == "2.0.0" for package in packages):
        raise RuntimeError("offline target selection did not expose its locked platform package")
    if (app / "vex.lock").read_bytes() != locked:
        raise RuntimeError("target or feature selection rewrote the union lockfile")


def workspace_smoke(vex, root, env):
    workspace = root / "workspace"
    workspace.mkdir()
    (workspace / "vex.ws").write_text(
        '{format=4,compiler="0.2.1-pre-beta",workspace={members=["alpha","beta"],default_members=["beta"]}}', encoding="utf-8")
    for directory, value in (("one", 11), ("two", 22)):
        package = workspace / directory
        (package / "src").mkdir(parents=True)
        (package / "vex.ws").write_text('{format=4,name="common",version="1.0.0",targets={lib={}}}', encoding="utf-8")
        (package / "src/lib.wave").write_text(f"pub fun value() -> i32 {{ return {value}; }}\n", encoding="utf-8")
    for name, source in (("alpha", "one"), ("beta", "two")):
        package = workspace / name
        (package / "src").mkdir(parents=True)
        library = "lib={}," if name == "alpha" else ""
        (package / "vex.ws").write_text(
            '{format=4,name="' + name + '",targets={' + library + 'bins=[{name="tool",path="src/main.wave"}]},dependencies=[{name="common",path="../' + source + '"}]}', encoding="utf-8")
        if name == "alpha":
            (package / "src/lib.wave").write_text('pub fun own() -> i32 { return 100; }\n', encoding="utf-8")
        own = 'import("alpha")::{own};\n' if name == "alpha" else ""
        expression = "own() + value()" if name == "alpha" else "value()"
        (package / "src/main.wave").write_text(
            'import("common")::{value};\n' + own + 'fun main() { println("{}", ' + expression + '); }\n', encoding="utf-8")
    run([vex, "fetch", "--workspace"], workspace, env)
    locked = (workspace / "vex.lock").read_bytes()
    run([vex, "build", "--workspace", "--locked", "--offline"], workspace, env)
    for name, expected in (("alpha", "111"), ("beta", "22")):
        result = run([vex, "run", "-p", name, "--locked", "--offline"], workspace, env)
        if result.stdout.strip() != expected:
            raise RuntimeError("workspace compiler mapping or isolated output failed for " + name)
    result = run([vex, "metadata", "--format-version", "2", "--workspace", "--locked"], workspace, env)
    metadata = json.loads(result.stdout)
    if metadata["root"]["kind"] != "workspace" or len(metadata["workspace_members"]) != 2:
        raise RuntimeError("workspace metadata did not describe both members")
    if (workspace / "vex.lock").read_bytes() != locked or any((workspace / name / ".vex").exists() for name in ("alpha", "beta")):
        raise RuntimeError("workspace command wrote member state or changed its locked graph")


def executable_test_smoke(vex, root, env):
    helper = root / "test_helper"
    (helper / "src").mkdir(parents=True)
    (helper / "vex.ws").write_text('{format=4,name="helper",targets={lib={}}}', encoding="utf-8")
    (helper / "src/lib.wave").write_text('pub fun value() -> i32 { return 42; }\n', encoding="utf-8")
    app = root / "test_app"
    (app / "tests").mkdir(parents=True)
    (app / "vex.ws").write_text('{format=4,name="tests",targets={tests=[{name="smoke",path="tests/smoke.wave"}]},test_dependencies=[{name="helper",path="../test_helper"}]}', encoding="utf-8")
    (app / "tests/smoke.wave").write_text('import("helper")::{value};\nfun main() { println("{}", value()); }\n', encoding="utf-8")
    run([vex, "fetch"], app, env)
    result = run([vex, "--message-file", "results.jsonl", "test", "--locked", "--offline"], app, env)
    if result.stdout.strip() != "42":
        raise RuntimeError("explicit test executable did not import its test dependency")
    events = [json.loads(line) for line in (app / "results.jsonl").read_text(encoding="utf-8").splitlines()]
    if not any(event.get("event") == "test_result" and event.get("exit_code") == 0 for event in events):
        raise RuntimeError("missing executable test result")


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
        env["VEX_CACHE_HOME"] = str(root / "cache")
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
        target_profile_smoke(vex, root, env, host_target)
        package_identity_smoke(vex, root, env)
        feature_graph_smoke(vex, root, env, host_target)
        workspace_smoke(vex, root, env)
        executable_test_smoke(vex, root, env)
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
        inverse = run([vex, "tree", "--invert", "leaf", "--locked", "--offline"], app, env)
        if not all(name in inverse.stdout for name in ("leaf", "middle", "app")):
            raise RuntimeError("inverse tree omitted the dependent path")
        depth = run([vex, "tree", "--depth", "0", "--locked", "--offline"], app, env)
        if len(depth.stdout.splitlines()) != 1:
            raise RuntimeError("depth zero did not limit output to the root")
        doctor = run([vex, "doctor", "--format=json"], app, env)
        health = json.loads(doctor.stdout)
        if health["schema_version"] != 1 or not health["success"] or not any(
                check["id"] == "dry_run" and check["status"] == "ok" for check in health["checks"]):
            raise RuntimeError("doctor did not verify the prepared compiler plan")
        executable = app / artifacts[0]["executable"]
        unknown = app / "target/user-file"
        unknown.write_text("preserve me", encoding="utf-8")
        preview = run([vex, "--quiet", "clean", "--dry-run"], app, env)
        if not executable.is_file() or "Would remove" not in preview.stdout:
            raise RuntimeError("clean preview changed or omitted generated artifacts")
        run([vex, "clean"], app, env)
        if executable.exists() or unknown.read_text(encoding="utf-8") != "preserve me":
            raise RuntimeError("clean failed to distinguish owned output from user data")
        quiet = run([vex, "--quiet", "run", "--locked", "--offline"], app, env)
        if "42" not in quiet.stdout.splitlines() or "Compiling" in quiet.stderr or "Running" in quiet.stderr:
            raise RuntimeError("quiet changed program output or retained Vex progress")
        run([vex, "prune"], app, env)
        if (app / "vex.lock").read_bytes() != locked:
            raise RuntimeError("operational inspection or clean changed vex.lock")
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


        # A removed Git dependency remains until explicit prune --apply.
        (app / "vex.ws").write_text('{format=2,name="app",dependencies=[]}\n', encoding="utf-8")
        run([vex, "fetch", "--offline"], app, env)
        checkouts = list((app / ".vex/deps").iterdir())
        if not checkouts:
            raise RuntimeError("fetch implicitly removed unused dependency state")
        preview = run([vex, "prune"], app, env)
        if "Would remove" not in preview.stdout or not all(path.exists() for path in checkouts):
            raise RuntimeError("prune did not preview retained unused checkouts")
        run([vex, "prune", "--apply"], app, env)
        if any(path.exists() for path in checkouts):
            raise RuntimeError("prune did not remove the verified unused checkout")


if __name__ == "__main__":
    main()
