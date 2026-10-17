mod invocation;
mod plan;
mod selection;

pub use invocation::{contains_dry_run_flag, create_run_generation, run_build_with_dry_run};
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
