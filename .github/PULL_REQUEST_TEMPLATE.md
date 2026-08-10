## Summary

<!-- What does this change? -->

## Why

<!-- Why is the change needed? -->

## Behavior

<!-- Describe observable behavior, compatibility, and migration impact. -->

## Validation

<!-- List commands and relevant manual checks. -->

- [ ] `cargo fmt --check`
- [ ] `cargo test --locked`
- [ ] `cargo clippy --locked --all-targets -- -D warnings`
- [ ] `cargo build --locked`

## Checklist

- [ ] The change is focused and targets `wavefnd/Vex:master`.
- [ ] User-facing behavior and recovery instructions are documented.
- [ ] Dependency changes preserve lockfile reproducibility.
- [ ] `--locked` and `--offline` behavior remains correct.
- [ ] No raw `wavec` flags were added to the Vex CLI.
- [ ] New commits include a DCO `Signed-off-by:` line.
