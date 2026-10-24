// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
use std::sync::atomic::{AtomicU8, Ordering};

// 0: quiet, 1: normal, 2: verbose decisions, 3: diagnostic detail.
static OUTPUT_LEVEL: AtomicU8 = AtomicU8::new(1);

pub fn configure(quiet: bool, verbosity: u8) {
    OUTPUT_LEVEL.store(
        if quiet { 0 } else { 1 + verbosity.min(2) },
        Ordering::Relaxed,
    );
}

pub fn status(action: &str, message: impl AsRef<str>) {
    let required = match action {
        "Detail" => 2,
        "Trace" => 3,
        _ => 1,
    };
    if OUTPUT_LEVEL.load(Ordering::Relaxed) < required {
        return;
    }
    let _ = diagnostic::output::stderr(format_args!(
        "{action:>12} {}\n",
        source::redact(message.as_ref())
    ));
}

/// Command results stay visible in quiet mode, unlike progress.
pub fn result(message: impl AsRef<str>) {
    let _ = diagnostic::output::stderr(format_args!("{}\n", source::redact(message.as_ref())));
}

pub fn verbose(message: impl AsRef<str>) {
    status("Detail", message);
}

pub fn trace(message: impl AsRef<str>) {
    status("Trace", message);
}

pub fn error(message: impl AsRef<str>) {
    let _ = diagnostic::output::stderr(format_args!(
        "error: {}\n",
        source::redact(message.as_ref())
    ));
}

pub fn diagnostic(failure: &diagnostic::Error) {
    // Learn credentials from the whole diagnostic before printing any context
    // or cause that might repeat one without its original URL.
    let mut current = Some(failure);
    while let Some(item) = current {
        let _ = source::redact(item.as_ref());
        for (key, value) in &item.context_fields {
            let _ = source::redact(&format!("{key}: {value}"));
        }
        current = item.cause.as_deref();
    }
    error(failure);
    trace(format!("error category: {}", failure.category.name()));
    let mut current = Some(failure);
    while let Some(item) = current {
        for (key, value) in &item.context_fields {
            trace(format!("{key}: {value}"));
        }
        if !std::ptr::eq(item, failure) {
            trace(format!("caused by: {item}"));
        }
        current = item.cause.as_deref();
    }
}

pub fn warning(message: impl AsRef<str>) {
    let _ = diagnostic::output::stderr(format_args!(
        "warning: {}\n",
        source::redact(message.as_ref())
    ));
}
