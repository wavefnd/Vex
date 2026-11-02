# This Source Code Form is subject to the terms of the Mozilla Public
# License, v. 2.0. If a copy of the MPL was not distributed with this
# file, You can obtain one at https://mozilla.org/MPL/2.0/.
# SPDX-License-Identifier: MPL-2.0
project(
    'vex-consumer',
    'c',
    'cpp',
    'rust',
    meson_version: '>=1.8.5',
    default_options: ['rust_std=2021'],
)
python = import('python').find_installation()
vex = find_program(get_option('vex'), native: true)
bridge_name = host_machine.system() == 'windows' ? 'wave_bridge.lib' : 'libwave_bridge.a'
adapter = get_option('adapter')
if adapter == ''
    adapter = meson.current_source_dir() / '../../tools/meson_consumer.py'
endif
command = [
    python,
    adapter,
    '--vex',
    vex,
    '--manifest',
    meson.current_source_dir() / 'wave/vex.ws',
    '--build-dir',
    meson.current_build_dir() / 'vex',
    '--profile',
    get_option('wave_profile'),
    '--kind',
    'static',
    '--output',
    '@OUTPUT0@',
    '--depfile',
    '@DEPFILE@',
    '--rust-main',
    '@OUTPUT1@',
    '--rust-source',
    meson.current_source_dir() / 'caller.rs',
]
if get_option('wave_target') != ''
    command += ['--target', get_option('wave_target')]
endif
foreach feature : get_option('wave_features')
    command += ['--features', feature]
endforeach
# Always run the bounded freshness scan, including new/deleted imports. The
# adapter leaves the bridge mtime unchanged when verified bytes are unchanged.
wave = custom_target(
    'wave',
    output: [bridge_name, 'caller_wave.rs'],
    depfile: 'wave.d',
    command: command,
    build_always_stale: true,
)
foreach language : ['c', 'cpp', 'rs']
    if language == 'rs'
        # The generated Rust entry includes caller.rs and the verified digest.
        # It is a real compiler input, so Meson 1.8 schedules and relinks it.
        caller = executable(
            'caller_rs',
            wave[1],
            rust_args: ['-Clink-arg=' + wave[0].full_path()],
        )
    else
        caller = executable(
            'caller_' + language,
            'caller.' + language,
            link_with: wave[0],
        )
    endif
    test('wave-' + language, caller)
endforeach
