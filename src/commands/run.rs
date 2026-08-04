use super::build::{build, BuildMode};

pub fn run(args: &[String]) {
    build(BuildMode::Run, args);
}
