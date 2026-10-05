mod invocation;
mod plan;
mod selection;

pub use invocation::{contains_dry_run_flag, create_run_generation, run_build_with_dry_run};
pub use selection::Compiler;

/// Validate the compiler protocol without starting a process.
pub fn validate_plan(bytes: &[u8]) -> Result<serde_json::Value, String> {
    plan::validate_dry_run_json_output(bytes, &[])
}
