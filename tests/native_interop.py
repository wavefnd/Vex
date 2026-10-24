#!/usr/bin/env python3
# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0
"""Native C ABI acceptance against a verified Wave release on the current host.

Requires cc/c++/ar and rustc on Unix, or Visual Studio C++ tools and rustc on
Windows. Foreign sources are built only by this fixture; Vex receives prebuilt
archives/objects. Every language and both Wave output modes are required.
"""

import argparse
import hashlib
import json
import os
from pathlib import Path
import shutil
import subprocess
import tempfile


def run(command, cwd, env, expected=0, *, native_tool=False):
    command = list(map(str, command))
    if env.get("VEX_TEST_TARGET_RUNNER") and not native_tool:
        command.insert(0, env["VEX_TEST_TARGET_RUNNER"])
    print("$", " ".join(command), flush=True)
    result = subprocess.run(command, cwd=cwd, env=env, text=True,
                            capture_output=True, timeout=300)
    print(result.stdout, end="", flush=True)
    print(result.stderr, end="", flush=True)
    if result.returncode != expected:
        raise RuntimeError(f"expected exit {expected}, got {result.returncode}: {command}")
    return result


def required_tool(name, env):
    tool = shutil.which(name, path=env.get("PATH"))
    if not tool:
        raise RuntimeError(f"native interoperability acceptance requires {name}")
    return tool


def msvc_environment(root, env, arch):
    """Select matching native tools even outside a Developer Command Prompt."""
    installer = Path(env.get("PROGRAMFILES(X86)", env.get("ProgramFiles(x86)",
                                                        r"C:\Program Files (x86)")))
    vswhere = installer / "Microsoft Visual Studio/Installer/vswhere.exe"
    if not vswhere.is_file():
        raise RuntimeError("native Windows acceptance requires Visual Studio's vswhere.exe")
    installation = run([vswhere, "-latest", "-products", "*", "-property",
                        "installationPath"], root, env).stdout.strip()
    devcmd = Path(installation) / "Common7/Tools/VsDevCmd.bat"
    if not installation or not devcmd.is_file():
        raise RuntimeError("cannot locate Visual Studio C++ developer environment")
    batch = root / "msvc-environment.cmd"
    batch.write_text(
        '@echo off\ncall "%VEX_INTEROP_VSDEVCMD%" -no_logo '
        '-arch=%VEX_INTEROP_ARCH% >nul\n'
        'if errorlevel 1 exit /b %errorlevel%\nset\n', encoding="ascii")
    selection = dict(env, VEX_INTEROP_VSDEVCMD=str(devcmd),
                     VEX_INTEROP_ARCH="arm64" if arch == "aarch64" else "amd64")
    # `set` contains the runner's environment: capture it without printing it.
    result = subprocess.run([env.get("COMSPEC", "cmd.exe"), "/d", "/c", str(batch)],
                            cwd=root, env=selection, text=True, capture_output=True,
                            timeout=60)
    if result.returncode:
        raise RuntimeError(f"Visual Studio environment setup failed: {result.stderr}")
    selected = dict(env)
    for line in result.stdout.splitlines():
        key, separator, value = line.partition("=")
        if separator and key and not key.startswith("="):
            # Windows environment keys are case insensitive; normalize the ones
            # used by Python executable lookup instead of keeping two PATHs.
            if key.upper() in {"PATH", "INCLUDE", "LIB", "LIBPATH"}:
                key = key.upper()
            selected[key] = value
    return selected


class NativeTools:
    def __init__(self, root, env, spec):
        self.windows = spec["os"] == "windows"
        self.env = msvc_environment(root, env, spec["arch"]) if self.windows else env
        self.cc = required_tool("cl.exe" if self.windows else self.env.get("CC", "cc"), self.env)
        self.cxx = self.cc if self.windows else required_tool(self.env.get("CXX", "c++"), self.env)
        self.ar = required_tool("lib.exe" if self.windows else self.env.get("AR", "ar"), self.env)
        self.rustc = required_tool(self.env.get("RUSTC", "rustc"), self.env)
        self.object_extension = ".obj" if self.windows else ".o"

    def archive(self, root, name, source, cpp=False):
        path = root / (name + (".cpp" if cpp else ".c"))
        path.write_text(source, encoding="utf-8")
        obj = root / (name + self.object_extension)
        archive = root / (name + ".lib" if self.windows else "lib" + name + ".a")
        if self.windows:
            run([self.cc, "/nologo", "/MD", "/c", "/TP" if cpp else "/TC",
                 path, "/Fo" + str(obj)], root, self.env)
            run([self.ar, "/NOLOGO", "/OUT:" + str(archive), obj], root, self.env)
        else:
            run([self.cxx if cpp else self.cc, "-fPIC", "-c", path, "-o", obj], root, self.env, native_tool=True)
            run([self.ar, "rcs", archive, obj], root, self.env, native_tool=True)
        return archive

    def rust_object(self, root):
        source = root / "native_rust.rs"
        source.write_text(
            '#![no_std]\n#[no_mangle]\npub extern "C" fn native_rust(a: i32, b: i32) '
            '-> i32 { a.wrapping_add(b) }\n', encoding="utf-8")
        obj = root / ("native_rust" + self.object_extension)
        run([self.rustc, "--edition=2021", "--crate-type=lib", "--emit=obj",
             "-C", "panic=abort", source, "-o", obj], root, self.env, native_tool=True)
        return obj

    def consume(self, root, library, shared=False):
        source = root / "consumer.c"
        source.write_text('extern int wave_add(int, int);\n'
                          'int main(void) { return wave_add(20, 22) == 42 ? 0 : 1; }\n',
                          encoding="utf-8")
        executable = root / ("consumer.exe" if self.windows else "consumer")
        if self.windows:
            link_input = library.with_suffix(".lib") if shared else library
            if not link_input.is_file():
                raise RuntimeError(f"Wave did not publish the required import library: {link_input}")
            run([self.cc, "/nologo", "/MD", source, link_input,
                 "/Fe" + str(executable), "/link", "/INCREMENTAL:NO"], root, self.env)
        else:
            args = [self.cc, source, library, "-o", executable]
            if shared:
                args.append("-Wl,-rpath," + str(library.parent))
            run(args, root, self.env, native_tool=True)
        env = dict(self.env)
        if shared and self.windows:
            # This is the external consumer's loader setup, not a Vex policy.
            env["PATH"] = str(library.parent) + os.pathsep + env.get("PATH", "")
        run([executable], root, env)


def wson(value):
    if isinstance(value, dict):
        return "{" + ",".join(key + "=" + wson(item) for key, item in value.items()) + "}"
    if isinstance(value, list):
        return "[" + ",".join(wson(item) for item in value) + "]"
    return json.dumps(value)


def package(path, name, *, lib=False, native=None, dependencies=None):
    (path / "src").mkdir(parents=True, exist_ok=True)
    document = dict(format=2, name=name, version="0.1.0", lib=lib)
    if native is not None:
        document["native"] = native
    if dependencies is not None:
        document["dependencies"] = dependencies
    (path / "vex.ws").write_text(wson(document) + "\n", encoding="utf-8")


def snapshot(root):
    return {str(path.relative_to(root)): hashlib.sha256(path.read_bytes()).hexdigest()
            for path in root.rglob("*") if path.is_file()}


def events(path):
    return [json.loads(line) for line in path.read_text(encoding="utf-8").splitlines()]


def build(vex, root, env, report):
    run([vex, "--message-file", report, "build", "--locked", "--offline"], root, env)
    records = [event for event in events(report) if event["event"] == "artifact"]
    if len(records) != 1 or not records[0]["paths"]:
        raise RuntimeError("native build did not report its artifacts")
    outputs = [root / path for path in records[0]["paths"]]
    if not all(path.is_file() for path in outputs):
        raise RuntimeError("artifact event listed a missing native output")
    return outputs


def inbound(vex, root, tools, spec):
    app, middle, bridge = [root / name for name in ("native app", "middle", "bridge")]
    native_dir = bridge / "native files"
    native_dir.mkdir(parents=True)
    c_archive = tools.archive(native_dir, "vex_c", "int native_c(int a, int b) { return a+b; }\n")
    tools.archive(native_dir, "vex_cpp", 'extern "C" int native_cpp(int a, int b) { return a+b; }\n', cpp=True)
    rust_obj = tools.rust_object(native_dir)
    other_target = ("aarch64-unknown-linux-gnu" if spec["triple"] != "aarch64-unknown-linux-gnu"
                    else "x86_64-unknown-linux-gnu")
    package(bridge, "bridge", lib=True, native=dict(
        libraries=[dict(path="native files/" + c_archive.name),
                   dict(name="vex_cpp", target=spec["triple"]),
                   dict(path="native files/" + rust_obj.name),
                   dict(path="unselected/missing.a", target=other_target)],
        search_paths=[dict(path="native files", target=spec["triple"])]))
    (bridge / "src/lib.wave").write_text(
        # Public Wave 0.2.1 needs explicit linker names in imported modules:
        # the module resolver renames local declarations, not these symbols.
        'extern(c, "native_c") fun native_c(a: i32, b: i32) -> i32;\n'
        'extern(c, "native_cpp") fun native_cpp(a: i32, b: i32) -> i32;\n'
        'extern(c, "native_rust") fun native_rust(a: i32, b: i32) -> i32;\n'
        'pub fun answer() -> i32 { return native_c(10, 1) + native_cpp(10, 2) + native_rust(10, 9); }\n',
        encoding="utf-8")
    package(middle, "middle", lib=True, dependencies=[dict(name="bridge", path="../bridge")])
    (middle / "src/lib.wave").write_text('pub import("bridge")::{answer};\n', encoding="utf-8")
    package(app, "native_app", dependencies=[dict(name="middle", path="../middle")])
    (app / "src/main.wave").write_text(
        'import("middle")::{answer};\nfun main() -> i32 { '
        'var result: i32 = answer(); println("{}", result); return result - 42; }\n', encoding="utf-8")
    run([vex, "fetch", "--offline"], app, tools.env)
    locked = (app / "vex.lock").read_bytes()
    before = snapshot(app)
    build_plan = json.loads(run([vex, "build", "--dry-run", "--locked", "--offline"], app, tools.env).stdout)
    plan = build_plan["plans"][0]["compiler_plan"]
    if snapshot(app) != before or plan["target"] != spec["triple"]:
        raise RuntimeError("native dry-run mutated the project or selected the wrong target")
    outputs = build(vex, app, tools.env, root / "inbound-build.jsonl")
    executable = app / plan["link"]["output"]
    if executable not in outputs:
        raise RuntimeError("build artifact did not match the planned native executable")
    if "42" not in run([executable], app, tools.env).stdout.splitlines():
        raise RuntimeError("C/C++/Rust native inputs did not produce the expected result")
    run([vex, "check", "--locked", "--offline"], app, tools.env)
    report = root / "inbound-run.jsonl"
    result = run([vex, "--message-file", report, "run", "--locked", "--offline"], app, tools.env)
    if "42" not in result.stdout.splitlines() or events(report)[-1].get("origin") != "program":
        raise RuntimeError("native run lost the program result or structured origin")
    metadata = json.loads(run([vex, "metadata", "--locked", "--offline"], app, tools.env).stdout)
    bridge_metadata = next(item for item in metadata["packages"] if item["name"] == "bridge")
    if c_archive.name not in json.dumps(bridge_metadata.get("native")):
        raise RuntimeError("metadata omitted transitive native build inputs")
    if (app / "vex.lock").read_bytes() != locked:
        raise RuntimeError("locked native operations changed the lockfile")

    # A native link error must retain the previous usable image and its cause.
    previous = executable.read_bytes()
    (app / "src/main.wave").write_text(
        'extern(c) fun missing_native_symbol() -> i32;\n'
        'fun main() -> i32 { return missing_native_symbol(); }\n', encoding="utf-8")
    report = root / "inbound-failure.jsonl"
    failed = run([vex, "--message-file", report, "build", "--locked", "--offline"],
                 app, tools.env, expected=4)
    outcome = events(report)[-1]
    if ("missing_native_symbol" not in failed.stderr or outcome.get("origin") != "vex"
            or outcome.get("exit_code") != 4 or executable.read_bytes() != previous):
        raise RuntimeError("native linker failure lost its cause, category, or previous output")


def outbound(vex, root, tools, spec):
    project = root / "wave exports"
    package(project, "wave_exports", lib=True)
    (project / "src/lib.wave").write_text(
        'export(c) fun wave_add(a: i32, b: i32) -> i32 { return a + b; }\n', encoding="utf-8")
    run([vex, "fetch", "--offline"], project, tools.env)
    output_dir = project / "target" / spec["triple"] / "dev" / "lib"
    obj = output_dir / ("wave_exports" + tools.object_extension)
    outputs = build(vex, project, tools.env, root / "outbound-object.jsonl")
    if obj not in outputs:
        raise RuntimeError("library default output is not the named target/profile object")
    consumer = root / "object consumer"
    consumer.mkdir()
    tools.consume(consumer, obj)
    unknown = output_dir / "user-file"
    unknown.write_text("preserve me", encoding="utf-8")
    run([vex, "clean"], project, tools.env)
    if obj.exists() or unknown.read_text(encoding="utf-8") != "preserve me":
        raise RuntimeError("clean failed to distinguish native output from user data")

    package(project, "wave_exports", lib=True,
            native=dict(output="shared", exports=["wave_add"]))
    outputs = build(vex, project, tools.env, root / "outbound-shared.jsonl")
    name = {"coff": "wave_exports.dll", "macho": "libwave_exports.dylib",
            "elf": "libwave_exports.so"}[spec["object_format"]]
    library = output_dir / name
    if library not in outputs:
        raise RuntimeError("shared artifact path does not match the target object format")
    if tools.windows and library.with_suffix(".lib") not in outputs:
        raise RuntimeError("shared artifact event omitted the Windows import library")
    consumer = root / "shared consumer"
    consumer.mkdir()
    tools.consume(consumer, library, shared=True)
    if tools.windows:
        # A nonexistent export must fail linking rather than publish an empty
        # import library or replace the previously usable DLL/import pair.
        previous = {path: path.read_bytes()
                    for path in (library, library.with_suffix(".lib"))}
        package(project, "wave_exports", lib=True,
                native=dict(output="shared", exports=["missing_export"]))
        report = root / "outbound-failure.jsonl"
        failed = run([vex, "--message-file", report, "build", "--locked", "--offline"],
                     project, tools.env, expected=4)
        outcome = events(report)[-1]
        if ("missing_export" not in failed.stderr or outcome.get("origin") != "vex"
                or outcome.get("exit_code") != 4
                or any(path.read_bytes() != content for path, content in previous.items())):
            raise RuntimeError("undefined DLL export lost its error or previous library pair")
        package(project, "wave_exports", lib=True,
                native=dict(output="shared", exports=["wave_add"]))
    run([vex, "clean", "--dry-run"], project, tools.env)
    if not all(path.is_file() for path in outputs):
        raise RuntimeError("clean preview removed a shared output")
    run([vex, "clean"], project, tools.env)
    if any(path.exists() for path in outputs) or unknown.read_text(encoding="utf-8") != "preserve me":
        raise RuntimeError("clean did not remove the owned shared artifact set safely")


def format_three_exports(vex, root, tools, spec):
    """The explicit library model retains the real C ABI on every CI host."""
    project = root / "format three exports"
    (project / "src").mkdir(parents=True)
    (project / "src/lib.wave").write_text(
        'export(c) fun wave_add(a: i32, b: i32) -> i32 { return a + b; }\n', encoding="utf-8")
    for kind in ("object", "shared"):
        library = dict(output=kind)
        if kind == "shared":
            library["exports"] = ["wave_add"]
        manifest = dict(format=3, name="fmt3_exports", targets=dict(lib=library))
        (project / "vex.ws").write_text(wson(manifest) + "\n", encoding="utf-8")
        if kind == "object":
            run([vex, "fetch", "--offline"], project, tools.env)
            locked = (project / "vex.lock").read_bytes()
        outputs = build(vex, project, tools.env, root / ("fmt3-" + kind + ".jsonl"))
        name = ("fmt3_exports" + tools.object_extension if kind == "object" else
                {"coff": "fmt3_exports.dll", "macho": "libfmt3_exports.dylib",
                 "elf": "libfmt3_exports.so"}[spec["object_format"]])
        artifact = project / "target" / spec["triple"] / "dev" / "lib" / name
        if artifact not in outputs:
            raise RuntimeError("format 3 library artifact escaped the unified lib directory")
        if tools.windows and kind == "shared" and artifact.with_suffix(".lib") not in outputs:
            raise RuntimeError("format 3 shared artifact omitted its Windows import library")
        consumer = root / ("format three " + kind + " consumer")
        consumer.mkdir()
        tools.consume(consumer, artifact, shared=kind == "shared")
        if (project / "vex.lock").read_bytes() != locked:
            raise RuntimeError("format 3 native output selection changed vex.lock")


def archive_order(vex, root, tools):
    """A name/path mix must preserve the manifest's static-library order."""
    project = root / "library order"
    native_dir = project / "native files"
    native_dir.mkdir(parents=True)
    tools.archive(native_dir, "order_first", "int chosen(void) { return 1; }\n")
    second = tools.archive(native_dir, "order_second", "int chosen(void) { return 2; }\n")
    libraries = [dict(name="order_first"), dict(path="native files/" + second.name)]
    for expected, ordered in [(1, libraries), (2, list(reversed(libraries)))]:
        package(project, "library_order", native=dict(
            libraries=ordered, search_paths=[dict(path="native files")]))
        (project / "src/main.wave").write_text(
            'extern(c) fun chosen() -> i32;\n'
            f'fun main() -> i32 {{ return chosen() - {expected}; }}\n', encoding="utf-8")
        if expected == 1:
            run([vex, "fetch", "--offline"], project, tools.env)
        # Both archives define chosen; only the earlier definition should be
        # extracted. The reversed case prevents an accidental fixed preference.
        run([vex, "run", "--locked", "--offline"], project, tools.env)


def main():
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("--vex", type=Path, required=True)
    parser.add_argument("--wavec-bin", type=Path, required=True)
    parser.add_argument("--expected-version")
    parser.add_argument("--expected-sha256", help="Compiler executable digest, not the archive digest")
    args = parser.parse_args()
    vex = args.vex.resolve()
    compiler = args.wavec_bin.resolve() / ("wavec.exe" if os.name == "nt" else "wavec")
    digest = hashlib.sha256(compiler.read_bytes()).hexdigest()
    if args.expected_sha256 and digest != args.expected_sha256.lower():
        raise RuntimeError("compiler executable SHA-256 does not match the verified release")
    print(f"native interop compiler: {compiler}\nSHA-256: {digest}", flush=True)
    env = dict(os.environ)
    env.pop("VEX_WAVEC", None)
    env.pop("GH_TOKEN", None)
    env.pop("GITHUB_TOKEN", None)
    env["PATH"] = str(compiler.parent) + os.pathsep + env.get("PATH", "")
    with tempfile.TemporaryDirectory(prefix="vex native interop ") as temporary:
        root = Path(temporary)
        version = run([compiler, "--version"], root, dict(env, NO_COLOR="1"))
        if args.expected_version and version.stdout.split()[:2] != ["wavec", args.expected_version]:
            raise RuntimeError("native interop compiler version differs from the verified release")
        spec = json.loads(run([compiler, "print", "target-spec", "--format=json"], root, env).stdout)
        if not spec["hosted"] or spec["object_format"] not in {"elf", "macho", "coff"}:
            raise RuntimeError("native acceptance requires a supported hosted target")
        tools = NativeTools(root, env, spec)
        inbound(vex, root, tools, spec)
        archive_order(vex, root, tools)
        outbound(vex, root, tools, spec)
        format_three_exports(vex, root, tools, spec)
        print("Native C/C++/Rust inputs and legacy/format 3 Wave object/shared consumers passed.", flush=True)


if __name__ == "__main__":
    main()
