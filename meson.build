# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0
project(
    'vex-automation',
    meson_version: '>=1.8.5',
    version: run_command(
        find_program('python3', 'python'),
        files('tools/build_config.py')[0].full_path(),
        'version',
        check: true,
    ).stdout().strip(),
)

python = import('python').find_installation()
if run_command(
    python,
    '-c',
    'import sys; sys.exit(sys.version_info < (3, 11))',
    check: false,
).returncode() != 0
    error('Vex automation requires Python 3.11 or newer')
endif
fs = import('fs')
# Register configuration inputs so changing Cargo version/platforms regenerates
# the build definition. Rust source freshness remains exclusively Cargo-owned.
fs.read('Cargo.toml')
fs.read('rust-toolchain.toml')
fs.read('platforms.json')
config = files('tools/build_config.py')[0].full_path()
artifacts = [python, files('tools/release_artifacts.py')[0].full_path()]
executor = [python, files('tools/automation_step.py')[0].full_path()]
step_runner = executor + ['--stamp', '@OUTPUT@', '--']
cargo = get_option('cargo')
if cargo.length() == 0
    error('The cargo command must not be empty')
endif
version = run_command(python, config, 'version', check: true).stdout().strip()
known_targets = run_command(python, config, 'targets', check: true).stdout().strip().split(
    '\n',
)
release_targets = get_option('release_targets')
if release_targets.length() == 0
    release_targets = [
        run_command(python, config, 'host', check: true).stdout().strip(),
    ]
endif
selected = []
foreach triple : release_targets
    if not known_targets.contains(triple)
        error(
            'Unsupported release target: ' + triple + '\nKnown targets: ' + ', '.join(
                known_targets,
            ),
        )
    endif
    if not selected.contains(triple)
        selected += triple
    endif
endforeach
release_targets = selected

test_args = []
if get_option('test_target') != ''
    if not known_targets.contains(get_option('test_target'))
        error('Unsupported test target: ' + get_option('test_target'))
    endif
    test_args = ['--target', get_option('test_target')]
endif
python_tests = ['-m', 'unittest', 'discover', '-s', 'tests/tooling', '-v']
rust_tests = cargo + ['test', '--workspace', '--locked'] + test_args
test(
    'tooling',
    python,
    args: python_tests,
    workdir: meson.project_source_root(),
    suite: 'tooling',
    timeout: 1800,
    is_parallel: false,
)
test(
    'rust',
    python,
    args: [files('tools/automation_step.py')[0].full_path(), '--'] + cargo + [
        'test',
        '--workspace',
        '--locked',
    ] + test_args,
    workdir: meson.project_source_root(),
    suite: 'rust',
    timeout: 7200,
    is_parallel: false,
)
debug_build = custom_target(
    'build-debug',
    output: 'build-debug.stamp',
    command: step_runner + cargo + ['build', '--workspace', '--locked'],
    build_always_stale: true,
    build_by_default: true,
    console: true,
)
run_target(
    'test-compile',
    command: executor + ['--'] + rust_tests + ['--no-run'],
)

# Explicit dependency chains preserve fail-fast ordering. No stamp file may
# suppress a Cargo invocation: Cargo itself owns incremental compilation.
check_steps = [
    {'name': 'format', 'command': cargo + ['fmt', '--check']},
    {
        'name': 'tooling',
        'command': [
            find_program('meson'),
            'test',
            '-C',
            meson.project_build_root(),
            '--no-rebuild',
            '--suite',
            'tooling',
            '--print-errorlogs',
        ],
    },
    {
        'name': 'rust',
        'command': [
            find_program('meson'),
            'test',
            '-C',
            meson.project_build_root(),
            '--no-rebuild',
            '--suite',
            'rust',
            '--print-errorlogs',
        ],
    },
    {
        'name': 'clippy',
        'command': cargo + [
            'clippy',
            '--workspace',
            '--locked',
            '--all-targets',
            '--',
            '-D',
            'warnings',
        ],
    },
    {'name': 'build', 'command': cargo + ['build', '--workspace', '--locked']},
]
verify = custom_target(
    'verify-release',
    output: 'verify-release.stamp',
    command: step_runner + artifacts + ['verify-source'],
    build_always_stale: true,
    console: true,
)
foreach pipeline : ['check', 'release']
    previous = []
    if pipeline == 'release'
        # A development opt-in must never weaken the official release entry point.
        source_command = artifacts + ['verify-source']
        if get_option('allow_unverified')
            source_command = [
                python,
                '-c',
                'raise SystemExit("error: official releases require -Dallow_unverified=false")',
            ]
        endif
        previous = [
            custom_target(
                'release-source',
                output: 'release-source.stamp',
                command: step_runner + source_command,
                build_always_stale: true,
                console: true,
            ),
        ]
    endif
    foreach step : check_steps
        name = pipeline + '-' + step['name']
        phase = custom_target(
            name,
            output: name + '.stamp',
            command: step_runner + step['command'],
            depends: previous,
            build_always_stale: true,
            console: true,
        )
        previous = [phase]
    endforeach
    if pipeline == 'check'
        alias_target('check', previous)
    else
        foreach triple : release_targets
            name = 'release-build-' + triple
            phase = custom_target(
                name,
                output: name + '.stamp',
                command: step_runner + cargo + [
                    'build',
                    '--locked',
                    '--release',
                    '--target',
                    triple,
                ],
                depends: previous,
                build_always_stale: true,
                console: true,
            )
            previous = [phase]
        endforeach
        run_target(
            'release',
            command: artifacts + ['package'] + release_targets,
            depends: previous,
        )
    endif
endforeach

builds = []
foreach triple : release_targets
    name = 'build-' + triple
    phase = custom_target(
        name,
        output: name + '.stamp',
        command: step_runner + cargo + [
            'build',
            '--locked',
            '--release',
            '--target',
            triple,
        ],
        depends: builds,
        build_always_stale: true,
        console: true,
    )
    builds = [phase]
endforeach
alias_target('build-release', builds)
package_args = get_option('allow_unverified') ? ['--allow-unverified'] : []
run_target(
    'package',
    command: artifacts + ['package'] + package_args + release_targets,
)
run_target('checksum', command: artifacts + ['checksum'] + release_targets)
run_target('list-targets', command: [python, config, 'list-targets'])
run_target('version', command: [python, config, 'version'])
clean_cargo = custom_target(
    'clean-cargo',
    output: 'clean-cargo.stamp',
    command: step_runner + cargo + ['clean'],
    build_always_stale: true,
    console: true,
)
run_target(
    'clean-artifacts',
    command: artifacts + ['clean'],
    depends: clean_cargo,
)

quality_steps = [
    {
        'name': 'source',
        'command': [python, files('tools/source_policy.py')[0].full_path()],
    },
    {
        'name': 'repository',
        'command': [python, files('tools/repository_checks.py')[0].full_path()],
    },
    {
        'name': 'links',
        'command': [
            python,
            files('tools/check_links.py')[0].full_path(),
            '--external',
        ],
    },
    {
        'name': 'audit',
        'command': [python, files('tools/dependency_audit.py')[0].full_path()],
    },
    {
        'name': 'parser',
        'command': cargo + [
            'run',
            '--locked',
            '--example',
            'parser_fuzz',
            '--',
            '20261005',
            '2000',
        ],
    },
    {'name': 'format', 'command': cargo + ['fmt', '--check']},
    {
        'name': 'clippy',
        'command': cargo + [
            'clippy',
            '--workspace',
            '--locked',
            '--all-targets',
            '--',
            '-D',
            'warnings',
        ],
    },
]
previous = []
foreach step : quality_steps
    name = 'quality-' + step['name']
    phase = custom_target(
        name,
        output: name + '.stamp',
        command: step_runner + step['command'],
        depends: previous,
        build_always_stale: true,
        console: true,
    )
    previous = [phase]
endforeach
alias_target('quality', previous)

platform = get_option('acceptance_platform')
if platform != ''
    known_platforms = run_command(python, config, 'platforms', check: true).stdout().strip().split(
        '\n',
    )
    if not known_platforms.contains(platform)
        error('Unsupported acceptance platform: ' + platform)
    endif
    script = ['linux-riscv64', 'linux-loongarch64'].contains(platform) ? 'tools/ci_cross.py' : 'tools/ci_platform.py'
    run_target(
        'acceptance',
        command: [python, files(script)[0].full_path(), platform],
    )
endif
summary(
    {
        'Cargo version': version,
        'release targets': release_targets,
        'test target': get_option('test_target'),
        'development unverified packaging': get_option('allow_unverified'),
    },
)

# Supplemental development acceptance; this never substitutes for the pinned
# public compiler or the nine mandatory platform/release lanes.
if get_option('contract_wavec') != '' and get_option('contract_std') != ''
    run_target(
        'consumer-acceptance',
        command: [
            python,
            files('tests/external_builds.py'),
            '--vex',
            meson.project_source_root() / (host_machine.system() == 'windows' ? 'target/debug/vex.exe' : 'target/debug/vex'),
            '--wavec',
            get_option('contract_wavec'),
            '--std',
            get_option('contract_std'),
            '--cross-formats',
        ],
        depends: debug_build,
    )
endif
