use super::build::{build, BuildMode};

pub fn check(args: &[String]) {
    build(BuildMode::Check, args);
}
