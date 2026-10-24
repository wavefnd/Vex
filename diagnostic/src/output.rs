// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
//! Vex-owned output only; compiler and user-program stdio remain inherited.
use std::fmt;
use std::io::{self, Write};
use std::sync::atomic::{AtomicBool, Ordering};

static STDOUT_CLOSED: AtomicBool = AtomicBool::new(false);

fn write(writer: &mut impl Write, args: fmt::Arguments<'_>) -> io::Result<()> {
    writer.write_fmt(args)?;
    writer.flush()
}

pub fn stdout(args: fmt::Arguments<'_>) -> Result<(), crate::Error> {
    if STDOUT_CLOSED.load(Ordering::Relaxed) {
        return Ok(());
    }
    match write(&mut io::stdout().lock(), args) {
        Err(error) if error.kind() == io::ErrorKind::BrokenPipe => {
            STDOUT_CLOSED.store(true, Ordering::Relaxed);
            Ok(())
        }
        result => result
            .map_err(|error| crate::Error::environment(format!("cannot write stdout: {error}"))),
    }
}

pub fn stderr(args: fmt::Arguments<'_>) -> Result<(), crate::Error> {
    write(&mut io::stderr().lock(), args)
        .map_err(|error| crate::Error::environment(format!("cannot write stderr: {error}")))
}

/// Fallible CLI output; a closed consumer stops further stdout writes.
#[macro_export]
macro_rules! out {
    ($($arg:tt)*) => { $crate::output::stdout(format_args!($($arg)*))? };
}

#[macro_export]
macro_rules! outln {
    () => { $crate::output::stdout(format_args!("\n"))? };
    ($($arg:tt)*) => { $crate::output::stdout(format_args!("{}\n", format_args!($($arg)*)))? };
}
