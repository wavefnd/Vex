// This Source Code Form is subject to the terms of the Mozilla Public
// License, v. 2.0. If a copy of the MPL was not distributed with this
// file, You can obtain one at https://mozilla.org/MPL/2.0/.
// SPDX-License-Identifier: MPL-2.0
pub mod freshness;
mod invocation;
mod plan;
mod selection;

pub use invocation::{
    contains_dry_run_flag, create_run_generation, prepare_build, run_build_with_dry_run,
    PreparedBuild,
};
pub use selection::Compiler;

/// Validate the compiler protocol without starting a process.
pub fn validate_plan(bytes: &[u8]) -> Result<serde_json::Value, String> {
    plan::validate_dry_run_json_output(bytes, &[])
}

/// Check compiler destinations and protected native inputs without creating
/// outputs or ownership records. Call after validating the schema.
pub fn validate_plan_outputs(
    plan: &serde_json::Value,
    root: &std::path::Path,
    compiler_args: &[String],
) -> Result<(), diagnostic::Error> {
    invocation::validate_plan_outputs(plan, root, compiler_args).map(|_| ())
}
