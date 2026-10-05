use diagnostic::{Category, Error};
use std::process::ExitStatus;

pub struct Outcome {
    pub code: i32,
    pub origin: &'static str,
    pub category: &'static str,
    pub signal: Option<i32>,
}
impl Outcome {
    pub fn success() -> Self {
        Self {
            code: 0,
            origin: "vex",
            category: "success",
            signal: None,
        }
    }
    pub fn program(status: ExitStatus) -> Self {
        #[cfg(unix)]
        let signal = {
            use std::os::unix::process::ExitStatusExt;
            status.signal()
        };
        #[cfg(not(unix))]
        let signal = None;
        Self {
            code: process::exit_code(status),
            origin: "program",
            category: "program",
            signal,
        }
    }
    pub fn error(error: &Error) -> Self {
        Self {
            code: error.category.code(),
            origin: "vex",
            category: error.category.name(),
            signal: None,
        }
    }
}
/// Supervision takes precedence over the operation interrupted by it.
pub fn supervised(mut error: Error) -> Error {
    match process::failure_code() {
        130 => error.category = Category::Cancelled,
        124 => error.category = Category::Timeout,
        _ => {}
    }
    error
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn typed_results_map_without_exiting_the_process() {
        assert_eq!(Outcome::success().code, 0);
        for (category, code) in [
            (Category::Internal, 1),
            (Category::Usage, 2),
            (Category::Resolution, 3),
            (Category::Compiler, 4),
            (Category::Environment, 5),
            (Category::Timeout, 124),
            (Category::Cancelled, 130),
        ] {
            let result = Outcome::error(&Error::new(category, "fixture"));
            assert_eq!(result.code, code);
            assert_eq!(result.origin, "vex");
            assert_eq!(result.category, category.name());
        }
    }
}
